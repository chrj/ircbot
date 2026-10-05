//! Demonstrates a bot that keeps its data in SQLite.
//!
//! The bot records the last line that each nick said in a channel. `!seen
//! <nick>` tells when and where that was.
//!
//! The state holds an open database, which has no useful `Default`. Thus the
//! bot uses `#[bot(state = Db, no_default)]` and starts with
//! `new_with_state`.
//!
//! SQLite calls block the thread. The `Db::run` helper moves each call to
//! Tokio's blocking pool with `spawn_blocking`, so a slow query does not stop
//! the runtime.
//!
//! Run with:
//!
//!     cargo run --example sqlite_bot

use std::sync::{Arc, Mutex};

use ircbot::{bot, BoxError, Context, Result};
use rusqlite::{Connection, OptionalExtension};

/// The schema. `IF NOT EXISTS` makes it safe to apply on every start.
///
/// IRC nicks are case-insensitive, so `nick` uses `COLLATE NOCASE`. This
/// ignores the RFC 1459 rule for `[]\~` and `{}|^`, which is enough here.
const SCHEMA: &str = "
    CREATE TABLE IF NOT EXISTS seen (
        nick    TEXT PRIMARY KEY COLLATE NOCASE,
        channel TEXT NOT NULL,
        message TEXT NOT NULL,
        seen_at INTEGER NOT NULL DEFAULT (unixepoch())
    );
";

/// A shared handle to one SQLite connection.
///
/// `Connection` is `Send` but not `Sync`, so a `Mutex` guards it. A bot
/// sends few queries, so one connection is enough.
#[derive(Clone)]
struct Db {
    conn: Arc<Mutex<Connection>>,
}

impl Db {
    /// Open the database at `path` and apply the schema.
    ///
    /// The path `":memory:"` opens a private in-memory database, for tests.
    fn open(path: &str) -> rusqlite::Result<Self> {
        let conn = Connection::open(path)?;
        conn.execute_batch(SCHEMA)?;
        Ok(Db {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    /// Run `f` with the connection on Tokio's blocking pool.
    async fn run<R: Send + 'static>(
        &self,
        f: impl FnOnce(&Connection) -> rusqlite::Result<R> + Send + 'static,
    ) -> std::result::Result<R, BoxError> {
        let conn = Arc::clone(&self.conn);
        let result = tokio::task::spawn_blocking(move || {
            // A handler that panicked while it held the lock does not make the
            // connection unusable, so continue with the poisoned lock.
            let conn = conn.lock().unwrap_or_else(|e| e.into_inner());
            f(&conn)
        })
        .await?;
        Ok(result?)
    }
}

#[bot(state = Db, no_default)]
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
            .run(move |conn| {
                conn.execute(
                    "INSERT INTO seen (nick, channel, message) VALUES (?1, ?2, ?3)
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
            .run(move |conn| {
                conn.query_row(
                    "SELECT channel, message, datetime(seen_at, 'unixepoch')
                     FROM seen WHERE nick = ?1",
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

    // As with `basic_bot`, we do not connect here. The in-memory database
    // shows that the schema applies. A real bot gives a file path, so the
    // data stays after a restart.
    let _db = Db::open(":memory:")?;

    println!("sqlite_bot example compiled successfully.");
    println!("To connect for real, open a database file and pass it as the state:");
    println!("  let db = Db::open(\"seen.db\")?;");
    println!("  SeenBot::new_with_state(\"ircbot\", \"irc.libera.chat:6667\", [\"#rust\"], db)");
    println!("      .await?");
    println!("      .main_loop()");
    println!("      .await?;");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ircbot::testing::TestContext;

    /// A bot with an empty in-memory database.
    fn test_bot() -> SeenBot {
        SeenBot::from_state(Db::open(":memory:").expect("open in-memory database"))
    }

    /// Store a row with a known time, so the reply is the same on each run.
    fn insert_seen(bot: &SeenBot, nick: &str, channel: &str, message: &str, seen_at: i64) {
        let conn = bot.state.conn.lock().expect("lock database");
        conn.execute(
            "INSERT INTO seen (nick, channel, message, seen_at) VALUES (?1, ?2, ?3, ?4)",
            (nick, channel, message, seen_at),
        )
        .expect("insert row");
    }

    /// The stored row for `nick`, without the time.
    fn stored_row(bot: &SeenBot, nick: &str) -> Option<(String, String)> {
        let conn = bot.state.conn.lock().expect("lock database");
        conn.query_row(
            "SELECT channel, message FROM seen WHERE nick = ?1",
            [nick],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .expect("query row")
    }

    // ── record ───────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn record_stores_channel_and_message_of_sender() {
        let bot = test_bot();

        let mut tc = TestContext::channel("#rust", "alice", "hello all");
        bot.record(tc.take_ctx()).await.unwrap();

        assert_eq!(
            stored_row(&bot, "alice"),
            Some(("#rust".to_string(), "hello all".to_string())),
        );
    }

    #[tokio::test]
    async fn record_replaces_the_previous_line_of_the_same_nick() {
        let bot = test_bot();

        let mut first = TestContext::channel("#rust", "alice", "first");
        bot.record(first.take_ctx()).await.unwrap();
        let mut second = TestContext::channel("#tokio", "Alice", "second");
        bot.record(second.take_ctx()).await.unwrap();

        assert_eq!(
            stored_row(&bot, "alice"),
            Some(("#tokio".to_string(), "second".to_string())),
        );
    }

    #[tokio::test]
    async fn record_sends_no_reply() {
        let bot = test_bot();

        let mut tc = TestContext::channel("#rust", "alice", "hello all");
        bot.record(tc.take_ctx()).await.unwrap();

        assert_eq!(tc.next_reply(), None);
    }

    // ── !seen ────────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn seen_reports_channel_time_and_message() {
        let bot = test_bot();
        // 2026-10-05 10:00:00 UTC
        insert_seen(&bot, "alice", "#rust", "hello all", 1_791_194_400);

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
        let bot = test_bot();
        insert_seen(&bot, "alice", "#rust", "hello all", 1_791_194_400);

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
        let bot = test_bot();

        let mut tc = TestContext::channel("#rust", "bob", "!seen carol");
        bot.seen(tc.take_ctx(), "carol".to_string()).await.unwrap();

        assert_eq!(
            tc.next_reply(),
            Some("PRIVMSG #rust :bob, I have not seen carol.\r\n".to_string()),
        );
    }
}
