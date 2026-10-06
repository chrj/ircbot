//! Demonstrates a bot that keeps its data in SQLite, with the `store` module.
//!
//! The bot records the last line that each nick said in a channel. `!seen
//! <nick>` tells when and where that was.
//!
//! The state is a `Namespace` of a `Store`. An open database has no useful
//! `Default`, so the bot uses `#[bot(state = Namespace, no_default)]` and
//! starts with `new_with_state`. The namespace applies its own schema with
//! `migrate`, and each `sql` call runs on Tokio's blocking pool.
//!
//! Run with the `store` feature:
//!
//!     cargo run --example sqlite_bot --features store

use ircbot::store::rusqlite::OptionalExtension;
use ircbot::store::{Namespace, Store, StoreError};
use ircbot::{bot, Context, Result};

/// The schema of the `seen` namespace. Add new steps at the end, and do not
/// change a step that a database already has.
///
/// The table name starts with the name of the namespace, so it cannot clash
/// with the tables of another namespace. IRC nicks are case-insensitive, so
/// `nick` uses `COLLATE NOCASE`. This ignores the RFC 1459 rule for `[]\~` and
/// `{}|^`, which is enough here.
const MIGRATIONS: &[&str] = &["
    CREATE TABLE seen_last (
        nick    TEXT PRIMARY KEY COLLATE NOCASE,
        channel TEXT NOT NULL,
        message TEXT NOT NULL,
        seen_at INTEGER NOT NULL DEFAULT (unixepoch())
    );
"];

/// Get the `seen` namespace of `store`, with its schema applied.
async fn seen_namespace(store: &Store) -> std::result::Result<Namespace, StoreError> {
    let seen = store.namespace("seen")?;
    seen.migrate(MIGRATIONS).await?;
    Ok(seen)
}

#[bot(state = Namespace, no_default)]
impl SeenBot {
    /// Record each channel message as the last line of its sender.
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
                     ON CONFLICT (nick) DO UPDATE SET
                         channel = excluded.channel,
                         message = excluded.message,
                         seen_at = unixepoch()",
                    (&nick, &channel, &message),
                )
            })
            .await?;
        Ok(())
    }

    /// Tell when and where `nick` said their last line.
    #[command("seen")]
    async fn seen(&self, ctx: Context, nick: String) -> Result {
        let lookup = nick.clone();
        let row = self
            .state
            .sql(move |conn| {
                conn.query_row(
                    "SELECT channel, message, datetime(seen_at, 'unixepoch')
                     FROM seen_last WHERE nick = ?1",
                    [&lookup],
                    |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, String>(2)?,
                        ))
                    },
                )
                .optional()
            })
            .await?;
        match row {
            Some((channel, message, at)) => ctx.reply(format!(
                "{nick} was in {channel} at {at} UTC, saying: {message}"
            )),
            None => ctx.reply(format!("I have not seen {nick}.")),
        }
    }
}

#[tokio::main]
async fn main() -> std::result::Result<(), Box<dyn std::error::Error + Send + Sync>> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    // As with `basic_bot`, we do not connect here. The in-memory store shows
    // that the schema applies. A real bot opens a file, so the data stays
    // after a restart.
    let _seen = seen_namespace(&Store::memory()?).await?;

    println!("sqlite_bot example compiled successfully.");
    println!("To connect for real, open a store file and pass the namespace as the state:");
    println!("  let store = Store::open(\"bot.db\")?;");
    println!("  let seen = seen_namespace(&store).await?;");
    println!("  SeenBot::new_with_state(\"ircbot\", \"irc.libera.chat:6667\", [\"#rust\"], seen)");
    println!("      .main_loop()");
    println!("      .await?;");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ircbot::testing::TestContext;

    /// A bot with an empty in-memory store.
    async fn test_bot() -> SeenBot {
        let store = Store::memory().expect("open in-memory store");
        SeenBot::from_state(seen_namespace(&store).await.expect("apply schema"))
    }

    /// Store a row with a known time, so the reply is the same on each run.
    async fn insert_seen(bot: &SeenBot, nick: &str, channel: &str, message: &str, seen_at: i64) {
        let row = (
            nick.to_string(),
            channel.to_string(),
            message.to_string(),
            seen_at,
        );
        bot.state
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

    /// The stored row for `nick`, without the time.
    async fn stored_row(bot: &SeenBot, nick: &str) -> Option<(String, String)> {
        let nick = nick.to_string();
        bot.state
            .sql(move |conn| {
                conn.query_row(
                    "SELECT channel, message FROM seen_last WHERE nick = ?1",
                    [&nick],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()
            })
            .await
            .expect("query row")
    }

    // ── record ───────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn record_stores_channel_and_message_of_sender() {
        let bot = test_bot().await;

        let mut tc = TestContext::channel("#rust", "alice", "hello all");
        bot.record(tc.take_ctx()).await.unwrap();

        assert_eq!(
            stored_row(&bot, "alice").await,
            Some(("#rust".to_string(), "hello all".to_string())),
        );
    }

    #[tokio::test]
    async fn record_replaces_the_previous_line_of_the_same_nick() {
        let bot = test_bot().await;

        let mut first = TestContext::channel("#rust", "alice", "first");
        bot.record(first.take_ctx()).await.unwrap();
        let mut second = TestContext::channel("#tokio", "Alice", "second");
        bot.record(second.take_ctx()).await.unwrap();

        assert_eq!(
            stored_row(&bot, "alice").await,
            Some(("#tokio".to_string(), "second".to_string())),
        );
    }

    #[tokio::test]
    async fn record_sends_no_reply() {
        let bot = test_bot().await;

        let mut tc = TestContext::channel("#rust", "alice", "hello all");
        bot.record(tc.take_ctx()).await.unwrap();

        assert_eq!(tc.next_reply(), None);
    }

    // ── !seen ────────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn seen_reports_channel_time_and_message() {
        let bot = test_bot().await;
        // 2026-10-05 10:00:00 UTC
        insert_seen(&bot, "alice", "#rust", "hello all", 1_791_194_400).await;

        let mut tc = TestContext::channel("#rust", "bob", "!seen alice");
        bot.seen(tc.take_ctx(), "alice".to_string()).await.unwrap();

        assert_eq!(
            tc.next_reply(),
            Some(
                "PRIVMSG #rust :bob, alice was in #rust at 2026-10-05 10:00:00 UTC, saying: hello all\r\n"
                    .to_string()
            ),
        );
    }

    #[tokio::test]
    async fn seen_ignores_the_case_of_the_nick() {
        let bot = test_bot().await;
        insert_seen(&bot, "alice", "#rust", "hello all", 1_791_194_400).await;

        let mut tc = TestContext::channel("#rust", "bob", "!seen ALICE");
        bot.seen(tc.take_ctx(), "ALICE".to_string()).await.unwrap();

        assert_eq!(
            tc.next_reply(),
            Some(
                "PRIVMSG #rust :bob, ALICE was in #rust at 2026-10-05 10:00:00 UTC, saying: hello all\r\n"
                    .to_string()
            ),
        );
    }

    #[tokio::test]
    async fn seen_reports_an_unknown_nick() {
        let bot = test_bot().await;

        let mut tc = TestContext::channel("#rust", "bob", "!seen carol");
        bot.seen(tc.take_ctx(), "carol".to_string()).await.unwrap();

        assert_eq!(
            tc.next_reply(),
            Some("PRIVMSG #rust :bob, I have not seen carol.\r\n".to_string()),
        );
    }
}
