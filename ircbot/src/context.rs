use irc_proto::Message;
use tokio::sync::mpsc::UnboundedSender;

/// IRC mandates that no message line (including the trailing `\r\n`) may
/// exceed 512 bytes.  We budget 2 bytes for `\r\n`, leaving 510 bytes for
/// the command text itself.
const MAX_IRC_LINE: usize = 510;

/// A user on IRC (nick!user@host).
#[derive(Debug, Clone, Default)]
pub struct User {
    pub nick: String,
    pub user: String,
    pub host: String,
}

/// Per-message context passed to every handler.
pub struct Context {
    pub(crate) tx: UnboundedSender<String>,
    /// The channel or nick this message was directed to.
    pub target: String,
    pub is_channel: bool,
    /// The user who sent the message (if available).
    pub sender: Option<User>,
    /// The underlying parsed IRC message.
    pub raw: Message,
    /// The bot's own nick (for self-detection).
    pub bot_nick: String,
    /// Wildcard or regex captures from the matched trigger pattern.
    pub captures: Vec<String>,
}

/// Strip characters that could be used for IRC message injection.
fn sanitize(s: &str) -> String {
    s.chars()
        .filter(|&c| c != '\r' && c != '\n' && c != '\0')
        .collect()
}

/// Split `text` into one or more complete IRC lines of the form
/// `"{header}{chunk}{suffix}\r\n"` where each line is at most 512 bytes.
///
/// When a split is necessary the function prefers to break at the last ASCII
/// space within the available window (word-wrapping); if no space exists the
/// text is hard-split at the byte limit, taking care to stay on a valid UTF-8
/// character boundary.
///
/// Returns at least one entry even when `text` is empty.
pub fn make_messages(header: &str, text: &str, suffix: &str) -> Vec<String> {
    // bytes available for `text` inside each line
    let overhead = header.len() + suffix.len() + 2; // +2 for \r\n
    let available = MAX_IRC_LINE.saturating_sub(overhead);

    if text.is_empty() || available == 0 {
        return vec![format!("{header}{suffix}\r\n")];
    }
    if text.len() <= available {
        return vec![format!("{header}{text}{suffix}\r\n")];
    }

    let mut messages = Vec::new();
    let mut remaining = text;

    while !remaining.is_empty() {
        if remaining.len() <= available {
            messages.push(format!("{header}{remaining}{suffix}\r\n"));
            break;
        }

        // Find the largest valid UTF-8 boundary that fits.
        let mut end = available;
        while end > 0 && !remaining.is_char_boundary(end) {
            end -= 1;
        }

        // Prefer breaking at a space; fall back to the hard limit.
        let split_at = remaining[..end]
            .rfind(' ')
            .filter(|&p| p > 0)
            .unwrap_or(end.max(1));

        messages.push(format!("{header}{}{suffix}\r\n", &remaining[..split_at]));
        remaining = remaining[split_at..].trim_start_matches(' ');
    }

    messages
}

/// Send one or more (split) messages through `tx`.
fn send_chunked(
    tx: &UnboundedSender<String>,
    header: &str,
    text: &str,
    suffix: &str,
) -> crate::Result {
    for line in make_messages(header, text, suffix) {
        tx.send(line).map_err(|e| Box::new(e) as crate::BoxError)?;
    }
    Ok(())
}

impl Context {
    /// Reply to the sender.  In a channel, prefixes the nick; in a query, PMs back.
    ///
    /// If the formatted message would exceed the IRC 512-byte line limit it is
    /// automatically split across multiple messages.
    ///
    /// # Errors
    ///
    /// Returns an error if the write channel is closed.
    pub fn reply(&self, msg: impl std::fmt::Display) -> crate::Result {
        let msg = sanitize(&msg.to_string());
        if self.is_channel {
            let prefix = self
                .sender
                .as_ref()
                .map(|u| format!("{}, ", u.nick))
                .unwrap_or_default();
            let header = format!("PRIVMSG {} :{prefix}", self.target);
            send_chunked(&self.tx, &header, &msg, "")
        } else {
            let to = self
                .sender
                .as_ref()
                .map_or(self.target.as_str(), |u| u.nick.as_str());
            let header = format!("PRIVMSG {to} :");
            send_chunked(&self.tx, &header, &msg, "")
        }
    }

    /// Send a message to the channel / private target without a nick prefix.
    ///
    /// If the formatted message would exceed the IRC 512-byte line limit it is
    /// automatically split across multiple messages.
    ///
    /// # Errors
    ///
    /// Returns an error if the write channel is closed.
    pub fn say(&self, msg: impl std::fmt::Display) -> crate::Result {
        let msg = sanitize(&msg.to_string());
        let target = if self.is_channel {
            self.target.clone()
        } else {
            self.sender
                .as_ref()
                .map_or_else(|| self.target.clone(), |u| u.nick.clone())
        };
        let header = format!("PRIVMSG {target} :");
        send_chunked(&self.tx, &header, &msg, "")
    }

    /// Send a `/me` action.
    ///
    /// If the formatted message would exceed the IRC 512-byte line limit it is
    /// automatically split across multiple messages.
    ///
    /// # Errors
    ///
    /// Returns an error if the write channel is closed.
    pub fn action(&self, msg: impl std::fmt::Display) -> crate::Result {
        let msg = sanitize(&msg.to_string());
        let target = if self.is_channel {
            self.target.clone()
        } else {
            self.sender
                .as_ref()
                .map_or_else(|| self.target.clone(), |u| u.nick.clone())
        };
        // CTCP ACTION: header is "PRIVMSG target :\x01ACTION ", suffix is "\x01"
        let header = format!("PRIVMSG {target} :\x01ACTION ");
        send_chunked(&self.tx, &header, &msg, "\x01")
    }

    /// The trailing text of the underlying IRC message.
    #[must_use]
    pub fn message_text(&self) -> &str {
        match &self.raw.command {
            irc_proto::Command::PRIVMSG(_, text) | irc_proto::Command::NOTICE(_, text) => text,
            irc_proto::Command::PING(server, _) => server,
            irc_proto::Command::PONG(_, Some(token)) => token,
            irc_proto::Command::PONG(server, None) => server,
            irc_proto::Command::JOIN(channel, _, _) => channel,
            irc_proto::Command::PART(_, Some(reason)) => reason,
            irc_proto::Command::PART(channel, None) => channel,
            irc_proto::Command::QUIT(Some(message)) => message,
            irc_proto::Command::KICK(_, _, Some(reason)) => reason,
            irc_proto::Command::TOPIC(_, Some(topic)) => topic,
            irc_proto::Command::TOPIC(channel, None) => channel,
            irc_proto::Command::Response(_, args) => args.last().map(String::as_str).unwrap_or(""),
            irc_proto::Command::Raw(_, args) => args.last().map(String::as_str).unwrap_or(""),
            _ => "",
        }
    }

    /// Send an IRC NOTICE to the channel / private target.
    ///
    /// NOTICEs are typically displayed without triggering audible alerts and
    /// must never be replied to automatically (by convention), making them
    /// suitable for bot status messages or one-shot notifications.
    ///
    /// If the formatted message would exceed the IRC 512-byte line limit it is
    /// automatically split across multiple messages.
    pub async fn notice(&self, msg: impl std::fmt::Display) -> crate::Result {
        let msg = sanitize(&msg.to_string());
        let target = if self.is_channel {
            self.target.clone()
        } else {
            self.sender
                .as_ref()
                .map(|u| u.nick.clone())
                .unwrap_or_else(|| self.target.clone())
        };
        let header = format!("NOTICE {target} :");
        send_chunked(&self.tx, &header, &msg, "")
    }

    /// Send a private message directly to the sender, regardless of whether
    /// the original message arrived in a channel or a query window.
    ///
    /// Useful for sending sensitive or verbose information out of a public
    /// channel without flooding it.
    ///
    /// If the formatted message would exceed the IRC 512-byte line limit it is
    /// automatically split across multiple messages.
    pub async fn whisper(&self, msg: impl std::fmt::Display) -> crate::Result {
        let msg = sanitize(&msg.to_string());
        let to = self
            .sender
            .as_ref()
            .map(|u| u.nick.as_str())
            .unwrap_or(self.target.as_str());
        let header = format!("PRIVMSG {to} :");
        send_chunked(&self.tx, &header, &msg, "")
    }

    /// Make the bot join `channel`.
    ///
    /// Sends a raw `JOIN` command.  The channel name is sanitized to strip the
    /// `\r`, `\n`, and `\0` characters that could be used for command
    /// injection.
    ///
    /// # Errors
    ///
    /// Returns an error if the write channel is closed.
    pub fn join(&self, channel: impl std::fmt::Display) -> crate::Result {
        let channel = sanitize(&channel.to_string());
        self.tx
            .send(format!("JOIN {channel}\r\n"))
            .map_err(|e| Box::new(e) as crate::BoxError)?;
        Ok(())
    }

    /// Make the bot leave `channel`.
    ///
    /// Sends a raw `PART` command.  The channel name is sanitized to strip the
    /// `\r`, `\n`, and `\0` characters that could be used for command
    /// injection.
    ///
    /// # Errors
    ///
    /// Returns an error if the write channel is closed.
    pub fn part(&self, channel: impl std::fmt::Display) -> crate::Result {
        let channel = sanitize(&channel.to_string());
        self.tx
            .send(format!("PART {channel}\r\n"))
            .map_err(|e| Box::new(e) as crate::BoxError)?;
        Ok(())
    }

    /// Send a raw IRC protocol line.
    ///
    /// This is a low-level escape hatch for commands the framework does not
    /// wrap with a dedicated helper (`KICK`, `MODE`, `INVITE`, `WHOIS`, …).
    /// `line` is sanitized to strip the `\r`, `\n`, and `\0` characters — so a
    /// caller cannot smuggle additional lines — and a single trailing `\r\n` is
    /// appended.
    ///
    /// The caller is responsible for the command's IRC syntax and for keeping
    /// the line within the 512-byte protocol limit; unlike [`Context::say`],
    /// `raw` does not split or wrap its argument.
    ///
    /// # Errors
    ///
    /// Returns an error if the write channel is closed.
    pub fn raw(&self, line: impl std::fmt::Display) -> crate::Result {
        let line = sanitize(&line.to_string());
        self.tx
            .send(format!("{line}\r\n"))
            .map_err(|e| Box::new(e) as crate::BoxError)?;
        Ok(())
    }

    /// Set the topic of the current channel ([`Context::target`]).
    ///
    /// Sends a `TOPIC` command for the channel this message arrived in,
    /// mirroring how [`Context::say`] and [`Context::reply`] act on the
    /// current target.  The topic text is sanitized to strip the `\r`, `\n`,
    /// and `\0` injection characters.  It is sent as a single line; topics
    /// longer than the 512-byte protocol limit may be truncated by the server.
    ///
    /// # Errors
    ///
    /// Returns an error if the write channel is closed.
    pub fn set_topic(&self, topic: impl std::fmt::Display) -> crate::Result {
        let topic = sanitize(&topic.to_string());
        self.tx
            .send(format!("TOPIC {} :{topic}\r\n", self.target))
            .map_err(|e| Box::new(e) as crate::BoxError)?;
        Ok(())
    }

    /// Kick `nick` from the current channel ([`Context::target`]) with `reason`.
    ///
    /// Sends a `KICK` command for the channel this message arrived in.  Both
    /// `nick` and `reason` are sanitized to strip the `\r`, `\n`, and `\0`
    /// injection characters.
    ///
    /// # Errors
    ///
    /// Returns an error if the write channel is closed.
    pub fn kick(
        &self,
        nick: impl std::fmt::Display,
        reason: impl std::fmt::Display,
    ) -> crate::Result {
        let nick = sanitize(&nick.to_string());
        let reason = sanitize(&reason.to_string());
        self.tx
            .send(format!("KICK {} {nick} :{reason}\r\n", self.target))
            .map_err(|e| Box::new(e) as crate::BoxError)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::mpsc;

    /// Build a `Context` wired to an in-process channel for easy inspection.
    fn make_ctx(target: &str, is_channel: bool) -> (Context, mpsc::UnboundedReceiver<String>) {
        let (tx, rx) = mpsc::unbounded_channel();
        let raw = ":nick!u@h PRIVMSG #chan :hello"
            .parse::<irc_proto::Message>()
            .unwrap();
        let ctx = Context {
            tx,
            target: target.to_string(),
            is_channel,
            sender: Some(User {
                nick: "nick".to_string(),
                user: "u".to_string(),
                host: "h".to_string(),
            }),
            raw,
            bot_nick: "bot".to_string(),
            captures: vec![],
        };
        (ctx, rx)
    }

    // ── sanitize ─────────────────────────────────────────────────────────────

    #[test]
    fn sanitize_strips_carriage_return() {
        assert_eq!(sanitize("foo\rbar"), "foobar");
    }

    #[test]
    fn sanitize_strips_newline() {
        assert_eq!(sanitize("foo\nbar"), "foobar");
    }

    #[test]
    fn sanitize_strips_null_byte() {
        assert_eq!(sanitize("foo\0bar"), "foobar");
    }

    #[test]
    fn sanitize_keeps_normal_text() {
        assert_eq!(sanitize("hello world"), "hello world");
    }

    // ── message_text ─────────────────────────────────────────────────────────

    #[test]
    fn message_text_returns_trailing_param() {
        let (ctx, _rx) = make_ctx("#chan", true);
        assert_eq!(ctx.message_text(), "hello");
    }

    // ── say ──────────────────────────────────────────────────────────────────

    #[test]
    fn say_in_channel_sends_privmsg_to_channel() {
        let (ctx, mut rx) = make_ctx("#chan", true);
        ctx.say("hi there").unwrap();
        assert_eq!(rx.try_recv().unwrap(), "PRIVMSG #chan :hi there\r\n");
    }

    #[test]
    fn say_in_query_sends_privmsg_to_sender_nick() {
        let (ctx, mut rx) = make_ctx("bot", false);
        ctx.say("hi there").unwrap();
        assert_eq!(rx.try_recv().unwrap(), "PRIVMSG nick :hi there\r\n");
    }

    #[test]
    fn say_strips_injection_characters() {
        let (ctx, mut rx) = make_ctx("#chan", true);
        ctx.say("evil\r\nJOIN #other").unwrap();
        assert_eq!(rx.try_recv().unwrap(), "PRIVMSG #chan :evilJOIN #other\r\n");
    }

    // ── reply ────────────────────────────────────────────────────────────────

    #[test]
    fn reply_in_channel_prefixes_sender_nick() {
        let (ctx, mut rx) = make_ctx("#chan", true);
        ctx.reply("pong").unwrap();
        assert_eq!(rx.try_recv().unwrap(), "PRIVMSG #chan :nick, pong\r\n");
    }

    #[test]
    fn reply_in_query_sends_to_sender_nick() {
        let (ctx, mut rx) = make_ctx("bot", false);
        ctx.reply("pong").unwrap();
        assert_eq!(rx.try_recv().unwrap(), "PRIVMSG nick :pong\r\n");
    }

    // ── action ───────────────────────────────────────────────────────────────

    #[test]
    fn action_in_channel_wraps_in_ctcp() {
        let (ctx, mut rx) = make_ctx("#chan", true);
        ctx.action("waves").unwrap();
        assert_eq!(
            rx.try_recv().unwrap(),
            "PRIVMSG #chan :\x01ACTION waves\x01\r\n"
        );
    }

    // ── notice ───────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn notice_in_channel_sends_notice_command() {
        let (ctx, mut rx) = make_ctx("#chan", true);
        ctx.notice("status").await.unwrap();
        assert_eq!(rx.try_recv().unwrap(), "NOTICE #chan :status\r\n");
    }

    #[tokio::test]
    async fn notice_in_query_sends_to_sender_nick() {
        let (ctx, mut rx) = make_ctx("bot", false);
        ctx.notice("status").await.unwrap();
        assert_eq!(rx.try_recv().unwrap(), "NOTICE nick :status\r\n");
    }

    // ── whisper ──────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn whisper_sends_pm_to_sender() {
        let (ctx, mut rx) = make_ctx("#chan", true);
        ctx.whisper("secret").await.unwrap();
        assert_eq!(rx.try_recv().unwrap(), "PRIVMSG nick :secret\r\n");
    }

    // ── sender-less contexts ───────────────────────────────────────────────────

    /// Build a `Context` with no known sender (e.g. a server-origin message or
    /// a cron-fired context).
    fn make_ctx_no_sender(
        target: &str,
        is_channel: bool,
    ) -> (Context, mpsc::UnboundedReceiver<String>) {
        let (tx, rx) = mpsc::unbounded_channel();
        let raw = ":server PRIVMSG #chan :hello"
            .parse::<irc_proto::Message>()
            .unwrap();
        let ctx = Context {
            tx,
            target: target.to_string(),
            is_channel,
            sender: None,
            raw,
            bot_nick: "bot".to_string(),
            captures: vec![],
        };
        (ctx, rx)
    }

    #[test]
    fn reply_in_channel_without_sender_omits_prefix() {
        let (ctx, mut rx) = make_ctx_no_sender("#chan", true);
        ctx.reply("pong").unwrap();
        assert_eq!(rx.try_recv().unwrap(), "PRIVMSG #chan :pong\r\n");
    }

    #[test]
    fn reply_in_query_without_sender_uses_target() {
        let (ctx, mut rx) = make_ctx_no_sender("someone", false);
        ctx.reply("pong").unwrap();
        assert_eq!(rx.try_recv().unwrap(), "PRIVMSG someone :pong\r\n");
    }

    #[test]
    fn say_in_query_without_sender_uses_target() {
        let (ctx, mut rx) = make_ctx_no_sender("someone", false);
        ctx.say("hi").unwrap();
        assert_eq!(rx.try_recv().unwrap(), "PRIVMSG someone :hi\r\n");
    }

    #[test]
    fn action_in_query_without_sender_uses_target() {
        let (ctx, mut rx) = make_ctx_no_sender("someone", false);
        ctx.action("waves").unwrap();
        assert_eq!(
            rx.try_recv().unwrap(),
            "PRIVMSG someone :\x01ACTION waves\x01\r\n"
        );
    }

    #[tokio::test]
    async fn notice_in_query_without_sender_uses_target() {
        let (ctx, mut rx) = make_ctx_no_sender("someone", false);
        ctx.notice("status").await.unwrap();
        assert_eq!(rx.try_recv().unwrap(), "NOTICE someone :status\r\n");
    }

    #[tokio::test]
    async fn whisper_without_sender_uses_target() {
        let (ctx, mut rx) = make_ctx_no_sender("someone", false);
        ctx.whisper("secret").await.unwrap();
        assert_eq!(rx.try_recv().unwrap(), "PRIVMSG someone :secret\r\n");
    }

    #[test]
    fn action_in_query_with_sender_targets_sender_nick() {
        let (ctx, mut rx) = make_ctx("bot", false);
        ctx.action("waves").unwrap();
        assert_eq!(
            rx.try_recv().unwrap(),
            "PRIVMSG nick :\x01ACTION waves\x01\r\n"
        );
    }

    // ── automatic splitting through the public API ─────────────────────────────

    #[test]
    fn say_splits_long_message_across_multiple_lines() {
        let (ctx, mut rx) = make_ctx("#chan", true);
        // Well over the 510-byte line limit → must be split.
        let text = "a".repeat(1_200);
        ctx.say(&text).unwrap();

        let mut lines = Vec::new();
        while let Ok(line) = rx.try_recv() {
            lines.push(line);
        }
        assert!(lines.len() >= 2, "expected the message to be split");
        for line in &lines {
            assert!(line.len() <= 512, "line exceeds 512 bytes: {}", line.len());
            assert!(line.ends_with("\r\n"));
        }
        // Reassembling the bodies reproduces the original text.
        let recovered: String = lines
            .iter()
            .map(|l| {
                l.strip_prefix("PRIVMSG #chan :")
                    .unwrap()
                    .trim_end_matches("\r\n")
            })
            .collect();
        assert_eq!(recovered, text);
    }

    // ── join / part ────────────────────────────────────────────────────────────

    #[test]
    fn join_sends_join_command() {
        let (ctx, mut rx) = make_ctx("#chan", true);
        ctx.join("#other").unwrap();
        assert_eq!(rx.try_recv().unwrap(), "JOIN #other\r\n");
    }

    #[test]
    fn part_sends_part_command() {
        let (ctx, mut rx) = make_ctx("#chan", true);
        ctx.part("#other").unwrap();
        assert_eq!(rx.try_recv().unwrap(), "PART #other\r\n");
    }

    #[test]
    fn join_strips_injection_characters() {
        let (ctx, mut rx) = make_ctx("#chan", true);
        ctx.join("#evil\r\nQUIT").unwrap();
        assert_eq!(rx.try_recv().unwrap(), "JOIN #evilQUIT\r\n");
    }

    // ── raw ────────────────────────────────────────────────────────────────────

    #[test]
    fn raw_sends_the_exact_line_with_crlf() {
        let (ctx, mut rx) = make_ctx("#chan", true);
        ctx.raw("MODE #chan +o nick").unwrap();
        assert_eq!(rx.try_recv().unwrap(), "MODE #chan +o nick\r\n");
    }

    #[test]
    fn raw_strips_injection_characters() {
        let (ctx, mut rx) = make_ctx("#chan", true);
        ctx.raw("MODE #chan +o nick\r\nQUIT").unwrap();
        assert_eq!(rx.try_recv().unwrap(), "MODE #chan +o nickQUIT\r\n");
    }

    // ── set_topic / kick ───────────────────────────────────────────────────────

    #[test]
    fn set_topic_sends_topic_for_current_channel() {
        let (ctx, mut rx) = make_ctx("#chan", true);
        ctx.set_topic("welcome all").unwrap();
        assert_eq!(rx.try_recv().unwrap(), "TOPIC #chan :welcome all\r\n");
    }

    #[test]
    fn set_topic_strips_injection_characters() {
        let (ctx, mut rx) = make_ctx("#chan", true);
        ctx.set_topic("hi\r\nQUIT").unwrap();
        assert_eq!(rx.try_recv().unwrap(), "TOPIC #chan :hiQUIT\r\n");
    }

    #[test]
    fn kick_sends_kick_for_current_channel() {
        let (ctx, mut rx) = make_ctx("#chan", true);
        ctx.kick("baduser", "spamming").unwrap();
        assert_eq!(rx.try_recv().unwrap(), "KICK #chan baduser :spamming\r\n");
    }

    #[test]
    fn kick_strips_injection_characters_from_nick_and_reason() {
        let (ctx, mut rx) = make_ctx("#chan", true);
        ctx.kick("bad\r\nuser", "be\r\nnice").unwrap();
        assert_eq!(rx.try_recv().unwrap(), "KICK #chan baduser :benice\r\n");
    }
}
