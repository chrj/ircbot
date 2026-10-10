//! The `ops` plugin: channel operator commands.
//!
//! | Command | What it does |
//! |---|---|
//! | `!op [nick]` | Give channel operator status, to the sender by default |
//! | `!deop [nick]` | Take channel operator status, from the sender by default |
//! | `!kick <nick> [reason]` | Kick a nick from this channel |
//! | `!ban <nick or mask> [duration]` | Ban, for example `!ban spammer 2h` |
//! | `!unban <nick or mask>` | Lift a ban |
//!
//! # Who can use it
//!
//! Each command needs the role [`ROLE`] (`op`). Define it on the bot with
//! `with_role`, as a hostmask role, an account role, or both. Without it,
//! `main_loop` refuses to start. The commands work in a channel only, and only
//! on that channel.
//!
//! # Bans
//!
//! A nick without `!` or `@` is banned as `nick!*@*`. A duration is a number
//! and a unit: `s`, `m`, `h`, `d` or `w`, up to [`MAX_BAN_DAYS`] days. The
//! plugin stores a timed ban, and lifts it when it expires, also after a
//! restart. It checks the stored bans each minute.
//!
//! The plugin refuses a mask that matches everyone, such as `*!*@*`, and a
//! mask that matches the bot by its nick alone, such as `test*` for the nick
//! `testbot`. The plugin does not know the host of the bot, so a mask with a
//! host part is the operator's responsibility.
//!
//! # When the bot is not a channel operator
//!
//! The server refuses the command with numeric 482. The plugin then says in
//! the channel that the bot is not a channel operator there.
//!
//! The plugin changes its stored bans when it queues a command for the
//! server, not when the server accepts it. IRC gives no answer that ties a 482
//! to one command, so after a 482 the stored bans can differ from the bans of
//! the channel:
//!
//! * A timed ban that the server refused is still lifted at its time. This
//!   does nothing, unless someone set the same ban after it.
//! * A timed ban that the bot could not lift when it expired stays in the
//!   channel until an operator lifts it.
//!
//! When the connection is lost before the plugin queues a command, the plugin
//! does not change its stored bans, and the next run lifts the expired bans
//! that are left. A command that is queued but lost with the connection is
//! handled as a refused one.
//!
//! The data is in the store namespace `ops`, in the table `ops_bans`.

use ircbot::store::{Namespace, Store, StoreError};
use ircbot::{plugin, Context, ModeError, Plugin, Result};

/// The role that each command of the plugin needs.
pub const ROLE: &str = "op";

/// The longest duration of a timed ban, in days.
pub const MAX_BAN_DAYS: i64 = 365;

/// The schema of the `ops` namespace. Add new steps at the end, and do not
/// change a step that a database already has.
///
/// Channel names and masks are case-insensitive, so both use
/// `COLLATE NOCASE`.
const MIGRATIONS: &[&str] = &["
    CREATE TABLE ops_bans (
        channel    TEXT NOT NULL COLLATE NOCASE,
        mask       TEXT NOT NULL COLLATE NOCASE,
        expires_at INTEGER NOT NULL,
        PRIMARY KEY (channel, mask)
    );
    CREATE INDEX ops_bans_expires_at ON ops_bans (expires_at);
"];

/// The `ops` plugin. Make it with [`Ops::open`].
///
/// See the [module docs](self) for what it does.
#[plugin(name = "ops", state = Namespace)]
impl Ops {
    /// Give channel operator status to a nick, or to yourself.
    #[command("op", role = "op", scope = "channel")]
    async fn op(&self, ctx: Context, nick: Option<String>) -> Result {
        let nick = nick.or_else(|| ctx.nick().map(str::to_string));
        let Some(nick) = nick else {
            return Ok(());
        };
        send_mode(&ctx, "+o", &nick).map(|_| ())
    }

    /// Take channel operator status from a nick, or from yourself.
    #[command("deop", role = "op", scope = "channel")]
    async fn deop(&self, ctx: Context, nick: Option<String>) -> Result {
        let nick = nick.or_else(|| ctx.nick().map(str::to_string));
        let Some(nick) = nick else {
            return Ok(());
        };
        if nick.eq_ignore_ascii_case(ctx.bot_nick.as_str()) {
            return ctx.reply("I will not take my own channel operator status.");
        }
        send_mode(&ctx, "-o", &nick).map(|_| ())
    }

    /// Kick a nick from this channel.
    #[command("kick", role = "op", scope = "channel")]
    async fn kick(&self, ctx: Context, nick: String, reason: Option<String>) -> Result {
        if nick.eq_ignore_ascii_case(ctx.bot_nick.as_str()) {
            return ctx.reply("I will not kick myself.");
        }
        let by = ctx.nick().unwrap_or("an operator").to_string();
        let reason = reason.unwrap_or_else(|| format!("Kicked by {by}"));
        ctx.kick(nick, reason)
    }

    /// Ban a nick or a mask from this channel, for a time or until an unban.
    #[command("ban", role = "op", scope = "channel")]
    async fn ban(&self, ctx: Context, target: String, duration: Option<String>) -> Result {
        let mask = match ban_mask(&target, ctx.bot_nick.as_str()) {
            Ok(mask) => mask,
            Err(refusal) => return ctx.reply(refusal),
        };
        let seconds = match duration.as_deref().map(parse_duration) {
            None => None,
            Some(Ok(seconds)) => Some(seconds),
            Some(Err(refusal)) => return ctx.reply(refusal),
        };

        // Send first: the stored bans change only when the line is queued.
        if !send_mode(&ctx, "+b", &mask)? {
            return Ok(());
        }
        let channel = ctx.target.to_string();
        match seconds {
            Some(seconds) => self.store_ban(&channel, &mask, seconds).await?,
            // A ban without a duration replaces a timed ban of the same mask.
            None => self.forget_ban(&channel, &mask).await?,
        }
        match duration {
            Some(duration) => ctx.reply(format!("Banned {mask} for {duration}.")),
            None => ctx.reply(format!("Banned {mask}.")),
        }
    }

    /// Lift the ban of a nick or a mask in this channel.
    #[command("unban", role = "op", scope = "channel")]
    async fn unban(&self, ctx: Context, target: String) -> Result {
        let mask = normalize_mask(&target);
        // Send first: the timed ban stays stored when the line is not queued.
        if !send_mode(&ctx, "-b", &mask)? {
            return Ok(());
        }
        self.forget_ban(&ctx.target.to_string(), &mask).await?;
        Ok(())
    }

    /// Lift each timed ban that has expired.
    #[on(cron = "0 * * * * *")]
    async fn lift_expired_bans(&self, ctx: Context) -> Result {
        let expired = self
            .state
            .sql(|conn| {
                let mut stmt = conn.prepare(
                    "SELECT channel, mask FROM ops_bans WHERE expires_at <= unixepoch()
                     ORDER BY channel, mask",
                )?;
                let rows = stmt
                    .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
                    .collect::<ircbot::store::rusqlite::Result<Vec<_>>>()?;
                Ok(rows)
            })
            .await?;
        for (channel, mask) in expired {
            // The mask passed `ban_mask` before it was stored, so it is one
            // IRC parameter, and the channel came from the server. Forget a
            // ban only when its line went out; the others stay for the next
            // run.
            ctx.raw(format!("MODE {channel} -b {mask}"))?;
            self.forget_ban(&channel, &mask).await?;
        }
        Ok(())
    }

    /// Say that the bot is not a channel operator, when the server refuses a
    /// command for that reason.
    #[on(event = "482")]
    async fn not_channel_operator(&self, ctx: Context) -> Result {
        // 482 <bot> <channel> :You're not channel operator
        let params = ctx.params();
        let Some(channel) = params.get(1) else {
            return Ok(());
        };
        ctx.raw(format!(
            "PRIVMSG {channel} :I am not a channel operator in {channel}, so I cannot do that."
        ))
    }
}

impl Ops {
    /// Open the plugin on `store`: take the namespace `ops`, and apply its
    /// schema.
    ///
    /// # Errors
    ///
    /// Returns a [`StoreError`] if the store cannot apply the schema.
    pub async fn open(store: &Store) -> std::result::Result<Self, StoreError> {
        let namespace = store.namespace(Self::NAME)?;
        namespace.migrate(MIGRATIONS).await?;
        Ok(Self::from_state(namespace))
    }

    /// Store a ban of `mask` in `channel` that expires after `seconds`.
    async fn store_ban(
        &self,
        channel: &str,
        mask: &str,
        seconds: i64,
    ) -> std::result::Result<(), StoreError> {
        let row = (channel.to_string(), mask.to_string(), seconds);
        self.state
            .sql(move |conn| {
                conn.execute(
                    "INSERT INTO ops_bans (channel, mask, expires_at)
                     VALUES (?1, ?2, unixepoch() + ?3)
                     ON CONFLICT (channel, mask) DO UPDATE SET expires_at = excluded.expires_at",
                    row,
                )
            })
            .await?;
        Ok(())
    }

    /// Forget a timed ban of `mask` in `channel`, if there is one.
    async fn forget_ban(&self, channel: &str, mask: &str) -> std::result::Result<(), StoreError> {
        let key = (channel.to_string(), mask.to_string());
        self.state
            .sql(move |conn| {
                conn.execute("DELETE FROM ops_bans WHERE channel = ?1 AND mask = ?2", key)
            })
            .await?;
        Ok(())
    }

    /// When the timed ban of `mask` in `channel` expires, in seconds since
    /// 1970, if there is one.
    #[cfg(test)]
    async fn ban_expiry(&self, channel: &str, mask: &str) -> Option<i64> {
        use ircbot::store::rusqlite::OptionalExtension;

        let key = (channel.to_string(), mask.to_string());
        self.state
            .sql(move |conn| {
                conn.query_row(
                    "SELECT expires_at FROM ops_bans WHERE channel = ?1 AND mask = ?2",
                    key,
                    |r| r.get(0),
                )
                .optional()
            })
            .await
            .expect("read ban")
    }
}

/// Send the mode change `change` with `arg` to the channel of `ctx`. When
/// `ircbot` refuses the line, for example because it is too long, tell the
/// sender why, and return `false`.
fn send_mode(
    ctx: &Context,
    change: &str,
    arg: &str,
) -> std::result::Result<bool, ircbot::BoxError> {
    let Err(error) = ctx.mode(change, [arg]) else {
        return Ok(true);
    };
    let Some(refusal) = error.downcast_ref::<ModeError>() else {
        return Err(error);
    };
    ctx.reply(refusal)?;
    Ok(false)
}

/// `target` as a ban mask: a nick without `!` or `@` becomes `nick!*@*`.
fn normalize_mask(target: &str) -> String {
    if target.contains('!') || target.contains('@') {
        target.to_string()
    } else {
        format!("{target}!*@*")
    }
}

/// The ban mask for `target`, or the reason to refuse it.
fn ban_mask(target: &str, bot_nick: &str) -> std::result::Result<String, String> {
    let mask = normalize_mask(target);
    let wildcards_only = |part: &str| part.chars().all(|c| c == '*' || c == '?');
    // Read the mask as `nick!user@host`. A missing part matches anything, as
    // the server reads it, so `*@*` is `*!*@*`.
    let (left, host) = mask.rsplit_once('@').unwrap_or((&mask, ""));
    let (nick, user) = left.split_once('!').unwrap_or((left, ""));
    if wildcards_only(nick) && wildcards_only(user) && wildcards_only(host) {
        return Err(format!(
            "{mask} matches everyone. Use a nick, or a mask with a host."
        ));
    }
    // The plugin only knows the nick of the bot, so it refuses a mask that
    // matches the bot by its nick alone.
    if wildcards_only(user)
        && wildcards_only(host)
        && ircbot::bot::glob_match(nick, bot_nick).is_some()
    {
        return Err(format!(
            "{mask} matches me. Use a mask that does not match {bot_nick}."
        ));
    }
    Ok(mask)
}

/// The number of seconds in `text`, for example `30m` or `2h`, or the reason
/// to refuse it.
fn parse_duration(text: &str) -> std::result::Result<i64, String> {
    let usage = || {
        format!(
            "{text} is not a duration. Use a number and s, m, h, d or w, for example 2h, up to \
             {MAX_BAN_DAYS}d."
        )
    };
    let unit = text.chars().last().ok_or_else(usage)?;
    let number: i64 = text[..text.len() - unit.len_utf8()]
        .parse()
        .map_err(|_| usage())?;
    let seconds_per_unit = match unit.to_ascii_lowercase() {
        's' => 1,
        'm' => 60,
        'h' => 3_600,
        'd' => 86_400,
        'w' => 604_800,
        _ => return Err(usage()),
    };
    let seconds = number.checked_mul(seconds_per_unit).ok_or_else(usage)?;
    if seconds <= 0 || seconds > MAX_BAN_DAYS * 86_400 {
        return Err(usage());
    }
    Ok(seconds)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ircbot::testing::TestBot;

    /// A bot with the plugin and an in-memory store, where `alice` has the role
    /// `op` by hostmask, and a second plugin value on the same namespace, to
    /// read and change the stored bans.
    async fn bot_and_store() -> (TestBot<Ops>, Ops) {
        let store = Store::memory().expect("open in-memory store");
        let ops = Ops::open(&store).await.expect("apply schema");
        let view = Ops::from_state(store.namespace(Ops::NAME).expect("namespace"));
        (
            TestBot::new(ops).with_role(ROLE, ["alice!*@ops.host"]),
            view,
        )
    }

    async fn bot() -> TestBot<Ops> {
        bot_and_store().await.0
    }

    /// The lines that the bot sends for `text` from alice in #rust.
    async fn as_op(bot: &TestBot<Ops>, text: &str) -> Vec<String> {
        bot.deliver(&format!(":alice!a@ops.host PRIVMSG #rust :{text}"))
            .await
            .unwrap()
    }

    #[test]
    fn the_plugin_and_its_namespace_are_named_ops() {
        assert_eq!(Ops::NAME, "ops");
    }

    // ── roles and scope ──────────────────────────────────────────────────────

    #[tokio::test]
    async fn a_sender_without_the_role_gets_no_answer() {
        let bot = bot().await;

        let lines = bot
            .deliver(":mallory!m@evil.host PRIVMSG #rust :!kick alice")
            .await
            .unwrap();

        assert_eq!(lines, Vec::<String>::new());
    }

    #[tokio::test]
    async fn the_commands_do_not_work_in_a_private_message() {
        let bot = bot().await;

        let lines = bot
            .deliver(":alice!a@ops.host PRIVMSG testbot :!op")
            .await
            .unwrap();

        assert_eq!(lines, Vec::<String>::new());
    }

    #[test]
    fn each_command_needs_the_op_role() {
        let roles: Vec<Option<String>> = <Ops as ircbot::Bot>::handlers()
            .into_iter()
            .filter_map(|entry| match entry.trigger {
                ircbot::Trigger::Command { role, .. } => Some(role),
                _ => None,
            })
            .collect();

        assert_eq!(roles.len(), 5);
        assert!(roles.iter().all(|role| role.as_deref() == Some("op")));
    }

    // ── !op, !deop, !kick ────────────────────────────────────────────────────

    #[tokio::test]
    async fn op_without_a_nick_gives_op_to_the_sender() {
        assert_eq!(
            as_op(&bot().await, "!op").await,
            vec!["MODE #rust +o alice\r\n"]
        );
    }

    #[tokio::test]
    async fn op_and_deop_take_a_nick() {
        let bot = bot().await;

        assert_eq!(as_op(&bot, "!op bob").await, vec!["MODE #rust +o bob\r\n"]);
        assert_eq!(
            as_op(&bot, "!deop bob").await,
            vec!["MODE #rust -o bob\r\n"]
        );
    }

    #[tokio::test]
    async fn deop_refuses_the_bot() {
        assert_eq!(
            as_op(&bot().await, "!deop TestBot").await,
            vec!["PRIVMSG #rust :alice, I will not take my own channel operator status.\r\n"]
        );
    }

    #[tokio::test]
    async fn kick_sends_the_reason_or_names_the_operator() {
        let bot = bot().await;

        assert_eq!(
            as_op(&bot, "!kick bob stop the spam").await,
            vec!["KICK #rust bob :stop the spam\r\n"]
        );
        assert_eq!(
            as_op(&bot, "!kick bob").await,
            vec!["KICK #rust bob :Kicked by alice\r\n"]
        );
    }

    #[tokio::test]
    async fn kick_refuses_the_bot() {
        assert_eq!(
            as_op(&bot().await, "!kick testbot").await,
            vec!["PRIVMSG #rust :alice, I will not kick myself.\r\n"]
        );
    }

    // ── !ban, !unban ─────────────────────────────────────────────────────────

    #[tokio::test]
    async fn ban_of_a_nick_bans_its_mask() {
        assert_eq!(
            as_op(&bot().await, "!ban spammer").await,
            vec![
                "MODE #rust +b spammer!*@*\r\n",
                "PRIVMSG #rust :alice, Banned spammer!*@*.\r\n"
            ]
        );
    }

    #[tokio::test]
    async fn ban_with_a_duration_stores_when_it_expires() {
        let (bot, view) = bot_and_store().await;

        let lines = as_op(&bot, "!ban *!*@spam.host 2h").await;

        assert_eq!(
            lines,
            vec![
                "MODE #rust +b *!*@spam.host\r\n",
                "PRIVMSG #rust :alice, Banned *!*@spam.host for 2h.\r\n"
            ]
        );
        let expiry = view.ban_expiry("#rust", "*!*@spam.host").await;
        let now = i64::try_from(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs(),
        )
        .unwrap();
        let left = expiry.expect("a stored ban") - now;
        assert!((7_190..=7_200).contains(&left), "expires in {left} seconds");
    }

    #[tokio::test]
    async fn ban_refuses_a_mask_that_matches_everyone_or_the_bot() {
        let bot = bot().await;

        assert_eq!(
            as_op(&bot, "!ban *!*@*").await,
            vec!["PRIVMSG #rust :alice, *!*@* matches everyone. Use a nick, or a mask with a host.\r\n"]
        );
        // A host part makes the mask specific enough.
        assert_eq!(
            as_op(&bot, "!ban test*!*@spam.host").await,
            vec![
                "MODE #rust +b test*!*@spam.host\r\n",
                "PRIVMSG #rust :alice, Banned test*!*@spam.host.\r\n"
            ]
        );
        assert_eq!(
            as_op(&bot, "!ban test*").await,
            vec!["PRIVMSG #rust :alice, test*!*@* matches me. Use a mask that does not match testbot.\r\n"]
        );
    }

    #[tokio::test]
    async fn ban_of_a_mask_too_long_for_irc_says_so_and_stores_nothing() {
        let (bot, view) = bot_and_store().await;
        let nick = "a".repeat(600);

        let lines = as_op(&bot, &format!("!ban {nick} 1d")).await;

        assert_eq!(
            lines,
            vec![
                "PRIVMSG #rust :alice, the MODE line is 618 bytes, but IRC allows 510: use \
                 shorter arguments\r\n"
            ]
        );
        assert_eq!(view.ban_expiry("#rust", &format!("{nick}!*@*")).await, None);
    }

    #[tokio::test]
    async fn ban_refuses_a_bad_duration() {
        assert_eq!(
            as_op(&bot().await, "!ban spammer forever").await,
            vec![
                "PRIVMSG #rust :alice, forever is not a duration. Use a number and s, m, h, d or w, \
                 for example 2h, up to 365d.\r\n"
            ]
        );
    }

    #[tokio::test]
    async fn unban_lifts_the_ban_and_forgets_a_timed_one() {
        let (bot, view) = bot_and_store().await;
        as_op(&bot, "!ban spammer 1d").await;

        let lines = as_op(&bot, "!unban spammer").await;

        assert_eq!(lines, vec!["MODE #rust -b spammer!*@*\r\n"]);
        assert_eq!(view.ban_expiry("#rust", "spammer!*@*").await, None);
    }

    #[tokio::test]
    async fn an_expired_ban_is_lifted_and_forgotten() {
        let (_bot, ops) = bot_and_store().await;
        ops.state
            .sql(|conn| {
                conn.execute(
                    "INSERT INTO ops_bans (channel, mask, expires_at) VALUES
                     ('#rust', 'old!*@*', unixepoch() - 10),
                     ('#rust', 'new!*@*', unixepoch() + 3600)",
                    [],
                )
            })
            .await
            .unwrap();
        let mut tc = ircbot::testing::TestContext::channel("#rust", "cron", "");

        ops.lift_expired_bans(tc.take_ctx()).await.unwrap();

        assert_eq!(
            tc.next_reply().as_deref(),
            Some("MODE #rust -b old!*@*\r\n")
        );
        assert_eq!(tc.next_reply(), None);
        assert_eq!(ops.ban_expiry("#rust", "old!*@*").await, None);
        assert!(ops.ban_expiry("#rust", "new!*@*").await.is_some());
    }

    /// A context whose connection is gone: each send fails.
    fn closed_ctx(text: &str) -> Context {
        let mut tc = ircbot::testing::TestContext::channel("#rust", "alice", text);
        tc.take_ctx()
    }

    #[tokio::test]
    async fn a_timed_ban_that_cannot_be_sent_is_not_stored() {
        let (_bot, ops) = bot_and_store().await;

        let result = ops
            .ban(
                closed_ctx("!ban spammer 2h"),
                "spammer".to_string(),
                Some("2h".to_string()),
            )
            .await;

        assert!(result.is_err());
        assert_eq!(ops.ban_expiry("#rust", "spammer!*@*").await, None);
    }

    #[tokio::test]
    async fn an_unban_that_cannot_be_sent_keeps_the_timed_ban() {
        let (bot, ops) = bot_and_store().await;
        as_op(&bot, "!ban spammer 1d").await;

        let result = ops
            .unban(closed_ctx("!unban spammer"), "spammer".to_string())
            .await;

        assert!(result.is_err());
        assert!(ops.ban_expiry("#rust", "spammer!*@*").await.is_some());
    }

    #[tokio::test]
    async fn expired_bans_that_cannot_be_lifted_stay_for_the_next_run() {
        let (_bot, ops) = bot_and_store().await;
        ops.state
            .sql(|conn| {
                conn.execute(
                    "INSERT INTO ops_bans (channel, mask, expires_at) VALUES
                     ('#rust', 'a!*@*', unixepoch() - 10),
                     ('#rust', 'b!*@*', unixepoch() - 10)",
                    [],
                )
            })
            .await
            .unwrap();

        let result = ops.lift_expired_bans(closed_ctx("")).await;

        assert!(result.is_err());
        assert!(ops.ban_expiry("#rust", "a!*@*").await.is_some());
        assert!(ops.ban_expiry("#rust", "b!*@*").await.is_some());
    }

    #[tokio::test]
    async fn deop_without_a_nick_takes_op_from_the_sender() {
        assert_eq!(
            as_op(&bot().await, "!deop").await,
            vec!["MODE #rust -o alice\r\n"]
        );
    }

    // ── 482 ──────────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn a_refusal_for_missing_op_is_told_in_the_channel() {
        let lines = bot()
            .await
            .deliver(":srv 482 testbot #rust :You're not channel operator")
            .await
            .unwrap();

        assert_eq!(
            lines,
            vec!["PRIVMSG #rust :I am not a channel operator in #rust, so I cannot do that.\r\n"]
        );
    }

    #[test]
    fn ban_mask_refuses_each_form_that_matches_everyone() {
        for mask in ["*!*@*", "*@*", "*!*", "*", "?*!*@*", "*!?*@*?"] {
            assert!(
                ban_mask(mask, "testbot").is_err(),
                "mask {mask:?} must be refused"
            );
        }
    }

    #[test]
    fn ban_mask_refuses_a_question_mark_that_matches_the_bot() {
        assert!(ban_mask("testbo?", "testbot").is_err());
        assert!(ban_mask("test???", "testbot").is_err());
        assert_eq!(ban_mask("test?", "testbot"), Ok("test?!*@*".to_string()));
    }

    #[test]
    fn ban_mask_keeps_a_mask_with_a_host_or_a_nick() {
        assert_eq!(
            ban_mask("*@spam.host", "testbot"),
            Ok("*@spam.host".to_string())
        );
        assert_eq!(
            ban_mask("spammer", "testbot"),
            Ok("spammer!*@*".to_string())
        );
    }

    // ── parse_duration ───────────────────────────────────────────────────────

    #[test]
    fn parse_duration_reads_each_unit_and_refuses_the_rest() {
        let cases = [
            ("45s", Ok(45)),
            ("30m", Ok(1_800)),
            ("2h", Ok(7_200)),
            ("7d", Ok(604_800)),
            ("1w", Ok(604_800)),
            ("365d", Ok(31_536_000)),
        ];
        for (text, seconds) in cases {
            assert_eq!(parse_duration(text), seconds, "duration {text:?}");
        }
        for bad in [
            "0m",
            "366d",
            "2",
            "h",
            "2x",
            "-1h",
            "",
            "99999999999999999999w",
        ] {
            assert!(parse_duration(bad).is_err(), "duration {bad:?}");
        }
    }
}
