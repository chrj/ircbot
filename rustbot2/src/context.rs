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
}
