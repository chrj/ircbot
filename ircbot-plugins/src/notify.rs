//! The `notify` plugin: leave a message for a nick that is not here.
//!
//! `!notify <nick> <message>` keeps the message. The bot gives it to the nick
//! the next time that the nick speaks where the bot can see it, or joins a
//! channel of the bot:
//!
//! ```text
//! <bob> !notify alice the build is green again
//! <bot> bob, I will give alice your message.
//!       … later, alice joins …
//! -bot- (to alice) bob left a message at 2026-10-07 10:00:00 UTC: the build is green again
//! ```
//!
//! # Privacy
//!
//! The bot gives a message in a private message to the nick, never in a
//! channel. Thus a message that was left in one channel cannot show in
//! another one.
//!
//! # Limits
//!
//! * One sender can have up to [`MAX_PER_SENDER`] messages that wait, so one
//!   nick cannot fill the store.
//! * One nick can get up to [`MAX_PER_RECIPIENT`] messages that wait.
//! * A message that waits longer than [`LIFETIME_DAYS`] days is deleted.
//! * A nick cannot leave a message for itself.
//!
//! The data is in the store namespace `notify`, in the table `notify_pending`.

use ircbot::store::{Namespace, Store, StoreError};
use ircbot::{plugin, Context, Plugin, Result};

/// How many messages one sender can have that wait.
pub const MAX_PER_SENDER: i64 = 5;

/// How many messages can wait for one nick.
pub const MAX_PER_RECIPIENT: i64 = 10;

/// How many days a message waits before it is deleted.
pub const LIFETIME_DAYS: i64 = 30;

/// The schema of the `notify` namespace. Add new steps at the end, and do not
/// change a step that a database already has.
///
/// IRC nicks are case-insensitive, so `recipient` and `sender` use
/// `COLLATE NOCASE`. This ignores the RFC 1459 rule for `[]\~` and `{}|^`,
/// which is enough here. The limits count the rows of one sender and of one
/// recipient, so each of these columns has an index.
const MIGRATIONS: &[&str] = &["
    CREATE TABLE notify_pending (
        id         INTEGER PRIMARY KEY,
        recipient  TEXT NOT NULL COLLATE NOCASE,
        sender     TEXT NOT NULL COLLATE NOCASE,
        message    TEXT NOT NULL,
        created_at INTEGER NOT NULL DEFAULT (unixepoch())
    );
    CREATE INDEX notify_pending_recipient ON notify_pending (recipient);
    CREATE INDEX notify_pending_sender ON notify_pending (sender);
"];

/// What happened to a message that a sender wants to leave.
#[derive(Debug, PartialEq, Eq)]
enum Stored {
    /// The message waits for the nick.
    Kept,
    /// The sender has [`MAX_PER_SENDER`] messages that wait.
    SenderFull,
    /// The nick has [`MAX_PER_RECIPIENT`] messages that wait.
    RecipientFull,
}

/// The `notify` plugin. Make it with [`Notify::open`].
///
/// See the [module docs](self) for what it does.
#[plugin(name = "notify", state = Namespace)]
impl Notify {
    /// Leave a message for a nick, for when it is next here.
    #[command("notify")]
    async fn notify(&self, ctx: Context, nick: String, message: String) -> Result {
        let Some(sender) = ctx.sender.as_ref().map(|s| s.nick.to_string()) else {
            return Ok(());
        };
        if message.trim().is_empty() {
            return ctx.reply("usage: !notify <nick> <message>");
        }
        if nick.eq_ignore_ascii_case(&sender) {
            return ctx.reply("You cannot leave a message for yourself.");
        }
        if nick.eq_ignore_ascii_case(ctx.bot_nick.as_str()) {
            return ctx.reply("I cannot leave a message for myself.");
        }

        let stored = self.store(&sender, &nick, message.trim()).await?;
        match stored {
            Stored::Kept => ctx.reply(format!("I will give {nick} your message.")),
            Stored::SenderFull => ctx.reply(format!(
                "You already have {MAX_PER_SENDER} messages that wait. Leave a new one \
                 when one of them is given."
            )),
            Stored::RecipientFull => ctx.reply(format!(
                "{nick} already has {MAX_PER_RECIPIENT} messages that wait. Try again \
                 when {nick} has read them."
            )),
        }
    }

    /// Give the messages of a nick that speaks.
    #[on(message = "*")]
    async fn deliver_on_message(&self, ctx: Context) -> Result {
        self.deliver(&ctx).await
    }

    /// Give the messages of a nick that joins a channel.
    #[on(event = "JOIN")]
    async fn deliver_on_join(&self, ctx: Context) -> Result {
        self.deliver(&ctx).await
    }
}

impl Notify {
    /// Open the plugin on `store`: take the namespace `notify`, and apply its
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

    /// Keep `message` from `sender` for `recipient`, if the limits allow it.
    async fn store(
        &self,
        sender: &str,
        recipient: &str,
        message: &str,
    ) -> std::result::Result<Stored, StoreError> {
        let row = (
            sender.to_string(),
            recipient.to_string(),
            message.to_string(),
        );
        self.state
            .sql(move |conn| {
                let (sender, recipient, message) = row;
                let tx = conn.transaction()?;
                delete_expired(&tx)?;
                let count = |column: &str, nick: &str| -> rusqlite_result::Result<i64> {
                    tx.query_row(
                        &format!("SELECT count(*) FROM notify_pending WHERE {column} = ?1"),
                        [nick],
                        |r| r.get(0),
                    )
                };
                let stored = if count("sender", &sender)? >= MAX_PER_SENDER {
                    Stored::SenderFull
                } else if count("recipient", &recipient)? >= MAX_PER_RECIPIENT {
                    Stored::RecipientFull
                } else {
                    tx.execute(
                        "INSERT INTO notify_pending (recipient, sender, message)
                         VALUES (?1, ?2, ?3)",
                        (&recipient, &sender, &message),
                    )?;
                    Stored::Kept
                };
                tx.commit()?;
                Ok(stored)
            })
            .await
    }

    /// Give the sender of `ctx` its messages that wait, in private messages,
    /// and delete them.
    async fn deliver(&self, ctx: &Context) -> Result {
        let Some(nick) = ctx.sender.as_ref().map(|s| s.nick.to_string()) else {
            return Ok(());
        };
        let notes = self
            .state
            .sql(move |conn| {
                let tx = conn.transaction()?;
                delete_expired(&tx)?;
                let notes = {
                    let mut stmt = tx.prepare(
                        "SELECT sender, datetime(created_at, 'unixepoch'), message
                         FROM notify_pending WHERE recipient = ?1 ORDER BY id",
                    )?;
                    let notes = stmt
                        .query_map([&nick], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
                        .collect::<rusqlite_result::Result<Vec<(String, String, String)>>>()?;
                    notes
                };
                tx.execute("DELETE FROM notify_pending WHERE recipient = ?1", [&nick])?;
                tx.commit()?;
                Ok(notes)
            })
            .await?;
        for (sender, at, message) in notes {
            ctx.whisper(format!("{sender} left a message at {at} UTC: {message}"))?;
        }
        Ok(())
    }
}

/// The `Result` of `rusqlite`, under a name that does not clash with
/// `ircbot::Result`.
mod rusqlite_result {
    pub(super) use ircbot::store::rusqlite::Result;
}

/// Delete each message that waits longer than [`LIFETIME_DAYS`] days.
fn delete_expired(conn: &ircbot::store::rusqlite::Connection) -> rusqlite_result::Result<usize> {
    conn.execute(
        "DELETE FROM notify_pending WHERE created_at < unixepoch() - ?1 * 86400",
        [LIFETIME_DAYS],
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use ircbot::testing::{TestBot, TestContext};

    /// A plugin with an empty in-memory store.
    async fn plugin() -> Notify {
        let store = Store::memory().expect("open in-memory store");
        Notify::open(&store).await.expect("apply schema")
    }

    /// Run `!notify <nick> <message>` from `sender` in #rust, and return the
    /// reply.
    async fn leave(notify: &Notify, sender: &str, nick: &str, message: &str) -> String {
        let mut tc = TestContext::channel("#rust", sender, &format!("!notify {nick} {message}"));
        notify
            .notify(tc.take_ctx(), nick.to_string(), message.to_string())
            .await
            .unwrap();
        tc.next_reply().expect("a reply")
    }

    /// Store a message with a known time, so the delivery is the same on each
    /// run.
    async fn insert_note(notify: &Notify, recipient: &str, sender: &str, message: &str, at: i64) {
        let row = (
            recipient.to_string(),
            sender.to_string(),
            message.to_string(),
            at,
        );
        notify
            .state
            .sql(move |conn| {
                conn.execute(
                    "INSERT INTO notify_pending (recipient, sender, message, created_at)
                     VALUES (?1, ?2, ?3, ?4)",
                    row,
                )
            })
            .await
            .expect("insert note");
    }

    /// How many messages wait for `recipient`.
    async fn waiting(notify: &Notify, recipient: &str) -> i64 {
        let recipient = recipient.to_string();
        notify
            .state
            .sql(move |conn| {
                conn.query_row(
                    "SELECT count(*) FROM notify_pending WHERE recipient = ?1",
                    [&recipient],
                    |r| r.get(0),
                )
            })
            .await
            .expect("count notes")
    }

    /// A time a few minutes ago, so the message has not expired.
    fn recently() -> i64 {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("time after 1970")
            .as_secs();
        i64::try_from(now).expect("time fits i64") - 300
    }

    #[test]
    fn the_plugin_and_its_namespace_are_named_notify() {
        assert_eq!(Notify::NAME, "notify");
    }

    // ── !notify ──────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn notify_keeps_the_message_and_says_so() {
        let notify = plugin().await;

        let reply = leave(&notify, "bob", "alice", "the build is green").await;

        assert_eq!(
            reply,
            "PRIVMSG #rust :bob, I will give alice your message.\r\n"
        );
        assert_eq!(waiting(&notify, "ALICE").await, 1);
    }

    #[tokio::test]
    async fn notify_refuses_a_message_for_the_sender() {
        let notify = plugin().await;

        let reply = leave(&notify, "bob", "Bob", "remember the milk").await;

        assert_eq!(
            reply,
            "PRIVMSG #rust :bob, You cannot leave a message for yourself.\r\n"
        );
        assert_eq!(waiting(&notify, "bob").await, 0);
    }

    #[tokio::test]
    async fn notify_refuses_a_message_for_the_bot() {
        let notify = plugin().await;

        // `TestContext` uses the bot nick `testbot`.
        let reply = leave(&notify, "bob", "TestBot", "hello bot").await;

        assert_eq!(
            reply,
            "PRIVMSG #rust :bob, I cannot leave a message for myself.\r\n"
        );
        assert_eq!(waiting(&notify, "testbot").await, 0);
    }

    #[tokio::test]
    async fn notify_through_the_dispatch_keeps_all_words_of_the_message() {
        let bot = TestBot::new(plugin().await);

        let stored = bot
            .deliver(":bob!b@h PRIVMSG #rust :!notify alice the build is green again")
            .await
            .unwrap();
        let given = bot.deliver(":alice!a@h PRIVMSG #rust :hi").await.unwrap();

        assert_eq!(
            stored,
            vec!["PRIVMSG #rust :bob, I will give alice your message.\r\n".to_string()]
        );
        assert_eq!(given.len(), 1);
        assert!(
            given[0].starts_with("PRIVMSG alice :bob left a message at ")
                && given[0].ends_with(" UTC: the build is green again\r\n"),
            "got {given:?}"
        );
    }

    #[tokio::test]
    async fn notify_works_in_a_private_message_to_the_bot() {
        let bot = TestBot::new(plugin().await);

        let stored = bot
            .deliver(":bob!b@h PRIVMSG testbot :!notify alice a secret plan")
            .await
            .unwrap();
        let given = bot.deliver(":alice!a@h JOIN #rust").await.unwrap();

        assert_eq!(
            stored,
            vec!["PRIVMSG bob :I will give alice your message.\r\n".to_string()]
        );
        assert!(
            given.len() == 1 && given[0].ends_with(" UTC: a secret plan\r\n"),
            "got {given:?}"
        );
    }

    #[tokio::test]
    async fn the_schema_has_an_index_for_each_limit() {
        let notify = plugin().await;

        let indexes: Vec<String> = notify
            .state
            .sql(|conn| {
                let mut stmt = conn.prepare(
                    "SELECT name FROM sqlite_master
                     WHERE type = 'index' AND tbl_name = 'notify_pending' ORDER BY name",
                )?;
                let names = stmt
                    .query_map([], |r| r.get(0))?
                    .collect::<rusqlite_result::Result<Vec<String>>>()?;
                Ok(names)
            })
            .await
            .unwrap();

        assert_eq!(
            indexes,
            vec![
                "notify_pending_recipient".to_string(),
                "notify_pending_sender".to_string()
            ]
        );
    }

    #[tokio::test]
    async fn notify_refuses_an_empty_message() {
        let notify = plugin().await;

        let reply = leave(&notify, "bob", "alice", "   ").await;

        assert_eq!(
            reply,
            "PRIVMSG #rust :bob, usage: !notify <nick> <message>\r\n"
        );
        assert_eq!(waiting(&notify, "alice").await, 0);
    }

    #[tokio::test]
    async fn notify_refuses_more_messages_from_one_sender() {
        let notify = plugin().await;
        for i in 0..MAX_PER_SENDER {
            leave(&notify, "bob", &format!("nick{i}"), "hello").await;
        }

        let reply = leave(&notify, "bob", "alice", "one too many").await;

        assert_eq!(
            reply,
            "PRIVMSG #rust :bob, You already have 5 messages that wait. Leave a new one when \
             one of them is given.\r\n"
        );
        assert_eq!(waiting(&notify, "alice").await, 0);
    }

    #[tokio::test]
    async fn notify_refuses_more_messages_for_one_nick() {
        let notify = plugin().await;
        for i in 0..MAX_PER_RECIPIENT {
            leave(&notify, &format!("sender{i}"), "alice", "hello").await;
        }

        let reply = leave(&notify, "bob", "alice", "one too many").await;

        assert_eq!(
            reply,
            "PRIVMSG #rust :bob, alice already has 10 messages that wait. Try again when \
             alice has read them.\r\n"
        );
        assert_eq!(waiting(&notify, "alice").await, MAX_PER_RECIPIENT);
    }

    // ── delivery ─────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn a_nick_that_speaks_gets_its_messages_in_private() {
        let notify = plugin().await;
        // The time must be inside the lifetime, so it changes on each run.
        // `the_delivery_shows_the_time_of_the_message` checks the time.
        let at = recently();
        insert_note(&notify, "alice", "bob", "first", at).await;
        insert_note(&notify, "alice", "carol", "second", at).await;
        let bot = TestBot::new(notify);

        let replies = bot
            .deliver(":Alice!a@h PRIVMSG #rust :hello all")
            .await
            .unwrap();

        assert_eq!(replies.len(), 2);
        assert!(replies[0].starts_with("PRIVMSG Alice :bob left a message at "));
        assert!(replies[0].ends_with(" UTC: first\r\n"), "got {replies:?}");
        assert!(replies[1].starts_with("PRIVMSG Alice :carol left a message at "));
        assert!(replies[1].ends_with(" UTC: second\r\n"), "got {replies:?}");
    }

    #[tokio::test]
    async fn a_nick_that_joins_gets_its_messages_in_private() {
        let notify = plugin().await;
        insert_note(&notify, "alice", "bob", "welcome back", recently()).await;
        let bot = TestBot::new(notify);

        let replies = bot.deliver(":alice!a@h JOIN #rust").await.unwrap();

        assert_eq!(replies.len(), 1);
        assert!(
            replies[0].starts_with("PRIVMSG alice :bob left a message at "),
            "got {replies:?}"
        );
    }

    #[tokio::test]
    async fn a_message_is_given_one_time() {
        let notify = plugin().await;
        insert_note(&notify, "alice", "bob", "only once", recently()).await;
        let bot = TestBot::new(notify);

        bot.deliver(":alice!a@h PRIVMSG #rust :hi").await.unwrap();
        let second = bot
            .deliver(":alice!a@h PRIVMSG #rust :hi again")
            .await
            .unwrap();

        assert_eq!(second, Vec::<String>::new());
    }

    #[tokio::test]
    async fn a_nick_without_messages_gets_nothing() {
        let notify = plugin().await;
        insert_note(&notify, "alice", "bob", "for alice", recently()).await;
        let bot = TestBot::new(notify);

        let replies = bot.deliver(":carol!c@h PRIVMSG #rust :hi").await.unwrap();

        assert_eq!(replies, Vec::<String>::new());
    }

    #[tokio::test]
    async fn an_expired_message_is_not_given() {
        let notify = plugin().await;
        let expired = recently() - (LIFETIME_DAYS + 1) * 86_400;
        insert_note(&notify, "alice", "bob", "too old", expired).await;
        let bot = TestBot::new(notify);

        let replies = bot.deliver(":alice!a@h PRIVMSG #rust :hi").await.unwrap();

        assert_eq!(replies, Vec::<String>::new());
    }

    #[tokio::test]
    async fn the_delivery_shows_the_time_of_the_message() {
        let notify = plugin().await;
        // A fixed time inside the lifetime: one day ago, at a known second.
        let at = recently() - 86_400;
        insert_note(&notify, "alice", "bob", "dated", at).await;
        let expected_at = notify
            .state
            .sql(move |conn| {
                conn.query_row("SELECT datetime(?1, 'unixepoch')", [at], |r| {
                    r.get::<_, String>(0)
                })
            })
            .await
            .unwrap();
        let bot = TestBot::new(notify);

        let replies = bot.deliver(":alice!a@h PRIVMSG #rust :hi").await.unwrap();

        assert_eq!(
            replies,
            vec![format!(
                "PRIVMSG alice :bob left a message at {expected_at} UTC: dated\r\n"
            )]
        );
    }

    #[tokio::test]
    async fn help_has_the_usage_and_summary_of_notify() {
        let help = <Notify as ircbot::Bot>::help();

        assert_eq!(help.len(), 1);
        assert_eq!(help[0].usage, "!notify <nick> <message>");
        assert_eq!(
            help[0].summary.as_deref(),
            Some("Leave a message for a nick, for when it is next here.")
        );
    }
}
