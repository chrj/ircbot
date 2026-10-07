//! The `seen` plugin: when and where a nick said its last line.
//!
//! The plugin records the last line of each nick in each channel. `!seen
//! <nick>` in a channel tells when that nick last spoke in this channel, and
//! what it said:
//!
//! ```text
//! <bob> !seen alice
//! <bot> bob, alice was last here at 2026-10-05 10:00:00 UTC, saying: hello all
//! ```
//!
//! # Privacy
//!
//! `!seen` never tells what a nick said in another channel. A channel can be
//! secret or private, and the people in one channel cannot always see another
//! one. Thus `!seen` only works in a channel, and only looks at that channel.
//! In a private message to the bot, it gives no answer.
//!
//! The data is in the store namespace `seen`, in the table `seen_last`.

use ircbot::store::rusqlite::OptionalExtension;
use ircbot::store::{Namespace, Store, StoreError};
use ircbot::{plugin, Context, Plugin, Result};

/// The schema of the `seen` namespace. Add new steps at the end, and do not
/// change a step that a database already has.
///
/// A row is the last line of one nick in one channel. IRC nicks and channel
/// names are case-insensitive, so both use `COLLATE NOCASE`. This ignores the
/// RFC 1459 rule for `[]\~` and `{}|^`, which is enough here.
const MIGRATIONS: &[&str] = &["
    CREATE TABLE seen_last (
        channel TEXT NOT NULL COLLATE NOCASE,
        nick    TEXT NOT NULL COLLATE NOCASE,
        message TEXT NOT NULL,
        seen_at INTEGER NOT NULL DEFAULT (unixepoch()),
        PRIMARY KEY (channel, nick)
    );
"];

/// The `seen` plugin. Make it with [`Seen::open`].
///
/// See the [module docs](self) for what it does.
#[plugin(name = "seen", state = Namespace)]
impl Seen {
    /// Record each channel message as the last line of its sender in this
    /// channel.
    #[on(message = "*", scope = "channel")]
    async fn record(&self, ctx: Context) -> Result {
        let Some(sender) = &ctx.sender else {
            return Ok(());
        };
        let nick = sender.nick.to_string();
        let channel = ctx.target.to_string();
        let message = ctx.plain_text();
        self.state
            .sql(move |conn| {
                conn.execute(
                    "INSERT INTO seen_last (nick, channel, message) VALUES (?1, ?2, ?3)
                     ON CONFLICT (channel, nick) DO UPDATE SET
                         message = excluded.message,
                         seen_at = unixepoch()",
                    (&nick, &channel, &message),
                )
            })
            .await?;
        Ok(())
    }

    /// Tell when a nick last spoke in this channel, and what it said.
    #[command("seen", scope = "channel")]
    async fn seen(&self, ctx: Context, nick: String) -> Result {
        let lookup = (ctx.target.to_string(), nick.clone());
        let row = self
            .state
            .sql(move |conn| {
                conn.query_row(
                    "SELECT message, datetime(seen_at, 'unixepoch')
                     FROM seen_last WHERE channel = ?1 AND nick = ?2",
                    lookup,
                    |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
                )
                .optional()
            })
            .await?;
        match row {
            Some((message, at)) => ctx.reply(format!(
                "{nick} was last here at {at} UTC, saying: {message}"
            )),
            None => ctx.reply(format!("I have not seen {nick} here.")),
        }
    }
}

impl Seen {
    /// Open the plugin on `store`: take the namespace `seen`, and apply its
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use ircbot::store::rusqlite;
    use ircbot::testing::TestContext;

    /// A plugin with an empty in-memory store.
    async fn plugin() -> Seen {
        let store = Store::memory().expect("open in-memory store");
        Seen::open(&store).await.expect("apply schema")
    }

    /// Store a row with a known time, so the reply is the same on each run.
    async fn insert_seen(seen: &Seen, nick: &str, channel: &str, message: &str, seen_at: i64) {
        let row = (
            nick.to_string(),
            channel.to_string(),
            message.to_string(),
            seen_at,
        );
        seen.state
            .sql(move |conn| {
                conn.execute(
                    "INSERT INTO seen_last (nick, channel, message, seen_at)
                     VALUES (?1, ?2, ?3, ?4)",
                    row,
                )
            })
            .await
            .expect("insert row");
    }

    /// The stored rows for `nick`, in each channel, without the time.
    async fn stored_rows(seen: &Seen, nick: &str) -> Vec<(String, String)> {
        let nick = nick.to_string();
        seen.state
            .sql(move |conn| {
                let mut stmt = conn.prepare(
                    "SELECT channel, message FROM seen_last WHERE nick = ?1 ORDER BY channel",
                )?;
                let rows = stmt
                    .query_map([&nick], |row| Ok((row.get(0)?, row.get(1)?)))?
                    .collect::<rusqlite::Result<Vec<(String, String)>>>()?;
                Ok(rows)
            })
            .await
            .expect("query rows")
    }

    #[test]
    fn the_plugin_and_its_namespace_are_named_seen() {
        assert_eq!(Seen::NAME, "seen");
    }

    #[tokio::test]
    async fn open_twice_on_one_store_applies_the_schema_one_time() {
        let store = Store::memory().unwrap();
        Seen::open(&store).await.unwrap();

        // A second `CREATE TABLE` would fail, so this passes only when the
        // step does not run again.
        assert!(Seen::open(&store).await.is_ok());
    }

    // ── record ───────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn record_stores_channel_and_message_of_sender() {
        let seen = plugin().await;

        let mut tc = TestContext::channel("#rust", "alice", "hello all");
        seen.record(tc.take_ctx()).await.unwrap();

        assert_eq!(
            stored_rows(&seen, "alice").await,
            vec![("#rust".to_string(), "hello all".to_string())],
        );
    }

    #[tokio::test]
    async fn record_replaces_the_previous_line_of_the_same_nick_in_a_channel() {
        let seen = plugin().await;

        let mut first = TestContext::channel("#rust", "alice", "first");
        seen.record(first.take_ctx()).await.unwrap();
        let mut second = TestContext::channel("#rust", "Alice", "second");
        seen.record(second.take_ctx()).await.unwrap();

        assert_eq!(
            stored_rows(&seen, "alice").await,
            vec![("#rust".to_string(), "second".to_string())],
        );
    }

    #[tokio::test]
    async fn record_keeps_one_line_for_each_channel() {
        let seen = plugin().await;

        let mut rust = TestContext::channel("#rust", "alice", "in rust");
        seen.record(rust.take_ctx()).await.unwrap();
        let mut secret = TestContext::channel("#secret", "alice", "in secret");
        seen.record(secret.take_ctx()).await.unwrap();

        assert_eq!(
            stored_rows(&seen, "alice").await,
            vec![
                ("#rust".to_string(), "in rust".to_string()),
                ("#secret".to_string(), "in secret".to_string()),
            ],
        );
    }

    #[tokio::test]
    async fn record_sends_no_reply() {
        let seen = plugin().await;

        let mut tc = TestContext::channel("#rust", "alice", "hello all");
        seen.record(tc.take_ctx()).await.unwrap();

        assert_eq!(tc.next_reply(), None);
    }

    // ── !seen ────────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn seen_reports_channel_time_and_message() {
        let seen = plugin().await;
        // 2026-10-05 10:00:00 UTC
        insert_seen(&seen, "alice", "#rust", "hello all", 1_791_194_400).await;

        let mut tc = TestContext::channel("#rust", "bob", "!seen alice");
        seen.seen(tc.take_ctx(), "alice".to_string()).await.unwrap();

        assert_eq!(
            tc.next_reply(),
            Some(
                "PRIVMSG #rust :bob, alice was last here at 2026-10-05 10:00:00 UTC, saying: hello all\r\n"
                    .to_string()
            ),
        );
    }

    #[tokio::test]
    async fn seen_ignores_the_case_of_the_nick() {
        let seen = plugin().await;
        insert_seen(&seen, "alice", "#rust", "hello all", 1_791_194_400).await;

        let mut tc = TestContext::channel("#rust", "bob", "!seen ALICE");
        seen.seen(tc.take_ctx(), "ALICE".to_string()).await.unwrap();

        assert_eq!(
            tc.next_reply(),
            Some(
                "PRIVMSG #rust :bob, ALICE was last here at 2026-10-05 10:00:00 UTC, saying: hello all\r\n"
                    .to_string()
            ),
        );
    }

    #[tokio::test]
    async fn seen_reports_an_unknown_nick() {
        let seen = plugin().await;

        let mut tc = TestContext::channel("#rust", "bob", "!seen carol");
        seen.seen(tc.take_ctx(), "carol".to_string()).await.unwrap();

        assert_eq!(
            tc.next_reply(),
            Some("PRIVMSG #rust :bob, I have not seen carol here.\r\n".to_string()),
        );
    }

    #[tokio::test]
    async fn seen_does_not_tell_what_a_nick_said_in_another_channel() {
        let seen = plugin().await;
        insert_seen(&seen, "alice", "#secret", "a secret", 1_791_194_400).await;

        let mut tc = TestContext::channel("#rust", "bob", "!seen alice");
        seen.seen(tc.take_ctx(), "alice".to_string()).await.unwrap();

        assert_eq!(
            tc.next_reply(),
            Some("PRIVMSG #rust :bob, I have not seen alice here.\r\n".to_string()),
        );
    }

    #[tokio::test]
    async fn seen_does_not_answer_in_a_private_message() {
        let seen = plugin().await;
        insert_seen(&seen, "alice", "#secret", "a secret", 1_791_194_400).await;

        let replies = ircbot::testing::TestBot::new(seen)
            .deliver(":bob!b@h PRIVMSG seenbot :!seen alice")
            .await
            .unwrap();

        assert_eq!(replies, Vec::<String>::new());
    }

    #[tokio::test]
    async fn help_has_the_usage_and_summary_of_seen() {
        let help = <Seen as ircbot::Bot>::help();

        assert_eq!(help.len(), 1);
        assert_eq!(help[0].usage, "!seen <nick>");
        assert_eq!(
            help[0].summary.as_deref(),
            Some("Tell when a nick last spoke in this channel, and what it said.")
        );
    }
}
