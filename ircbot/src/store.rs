//! Persistent data for a bot, in one SQLite file.
//!
//! This module needs the `store` feature.
//!
//! A [`Store`] is one open SQLite database. A bot gives each part of itself
//! (for example, each group of handlers) its own [`Namespace`]. A namespace has
//! two functions:
//!
//! * [`Namespace::sql`] runs a closure with the real
//!   [`rusqlite::Connection`]. There is no query layer between the closure and
//!   SQLite.
//! * [`Namespace::migrate`] applies a list of schema changes. The store
//!   records the version of each namespace, so each step runs one time only.
//!
//! SQLite calls block the thread. Both functions run the call on Tokio's
//! blocking pool, so a slow query does not stop the runtime.
//!
//! # Example
//!
//! ```rust
//! use ircbot::store::Store;
//!
//! # #[tokio::main(flavor = "current_thread")]
//! # async fn main() -> Result<(), ircbot::store::StoreError> {
//! // A real bot gives a file path to `Store::open`.
//! let store = Store::memory()?;
//! let quotes = store.namespace("quotes")?;
//!
//! // Add new steps at the end of the list. Do not change or remove a step
//! // that a database already has.
//! quotes
//!     .migrate(&["CREATE TABLE quotes_quote (id INTEGER PRIMARY KEY, text TEXT NOT NULL)"])
//!     .await?;
//!
//! quotes
//!     .sql(|conn| conn.execute("INSERT INTO quotes_quote (text) VALUES (?1)", ["hello"]))
//!     .await?;
//! let count: i64 = quotes
//!     .sql(|conn| conn.query_row("SELECT count(*) FROM quotes_quote", [], |row| row.get(0)))
//!     .await?;
//! assert_eq!(count, 1);
//! # Ok(())
//! # }
//! ```
//!
//! To use a store in handlers, put the namespace in the bot state. An open
//! database has no useful `Default`, so use `no_default` and start the bot
//! with `new_with_state`:
//!
//! ```rust,ignore
//! struct Data { quotes: Namespace }
//!
//! #[bot(state = Data, no_default)]
//! impl QuoteBot { /* … */ }
//!
//! let store = Store::open("bot.db")?;
//! let quotes = store.namespace("quotes")?;
//! quotes.migrate(QUOTE_MIGRATIONS).await?;
//! QuoteBot::new_with_state("quotebot", "irc.example.net:6667", ["#rust"], Data { quotes })
//!     .await?
//!     .main_loop()
//!     .await
//! ```
//!
//! # Names
//!
//! A namespace name starts with a lowercase ASCII letter. The other characters
//! are lowercase ASCII letters, digits, and `_`. Thus a name is safe to use in
//! SQL as a prefix of a table name.
//!
//! All namespaces share one database. Give each table the name of its
//! namespace as a prefix (`quotes_quote`), so two namespaces do not use the
//! same table. The store cannot make sure of this, because `sql` gives the
//! full connection.
//!
//! The store keeps its own data in tables whose names start with `_ircbot`.
//! Do not change these tables.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

/// The `rusqlite` crate that [`Namespace::sql`] gives its connection from.
///
/// Use this re-export, so the types match the version of this crate.
pub use rusqlite;
use rusqlite::hooks::{AuthAction, AuthContext, Authorization};
use rusqlite::{Connection, OptionalExtension};
use tokio::sync::Mutex;

/// How long a write waits for a lock that another connection holds, before
/// it fails with `SQLITE_BUSY`.
///
/// One store uses one connection. A second connection to the same file, for
/// example from a second process, can hold a lock.
pub const BUSY_TIMEOUT: Duration = Duration::from_secs(5);

/// The table that records the migration version of each namespace.
const MIGRATIONS_TABLE: &str = "
    CREATE TABLE IF NOT EXISTS _ircbot_migrations (
        namespace TEXT PRIMARY KEY,
        version   INTEGER NOT NULL
    );
";

/// An error from the store.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum StoreError {
    /// The database file could not be opened or prepared.
    #[error("cannot open the store at {}: {source}", path.display())]
    Open {
        /// The path given to [`Store::open`], or `:memory:`.
        path: PathBuf,
        /// The error from SQLite.
        source: rusqlite::Error,
    },

    /// A namespace name does not obey the rules in the [module docs](self#names).
    #[error(
        "invalid namespace name {name:?}: start with a lowercase ASCII letter, \
         and use only lowercase ASCII letters, digits and `_`"
    )]
    InvalidName {
        /// The name that was refused.
        name: String,
    },

    /// A [`Namespace::sql`] closure returned an error, or the store could not
    /// read its own tables.
    #[error("SQL error in namespace {namespace:?}: {source}")]
    Sql {
        /// The namespace of the call.
        namespace: String,
        /// The error from SQLite.
        source: rusqlite::Error,
    },

    /// A migration step failed. No step of this [`Namespace::migrate`] call
    /// was applied.
    #[error(
        "migration step {step} of namespace {namespace:?} failed, so this run \
         applied no step: {source}"
    )]
    Migration {
        /// The namespace of the call.
        namespace: String,
        /// The number of the failed step. The first step is 1.
        step: usize,
        /// The error from SQLite.
        source: rusqlite::Error,
    },

    /// The database has more migration steps for a namespace than the bot
    /// gives. Usually the bot is older than the database, or a step was
    /// removed from the list.
    #[error(
        "the database has {applied} migration steps for namespace {namespace:?}, \
         but the bot gives only {given}: do not remove steps, add new steps at \
         the end of the list"
    )]
    UnknownMigrations {
        /// The namespace of the call.
        namespace: String,
        /// The number of steps that the database has.
        applied: usize,
        /// The number of steps that the bot gave.
        given: usize,
    },

    /// The blocking task that runs the database call did not complete. The
    /// usual cause is a panic in a [`Namespace::sql`] closure.
    #[error("the database call of namespace {namespace:?} did not complete: {source}")]
    Task {
        /// The namespace of the call.
        namespace: String,
        /// The error from Tokio.
        source: tokio::task::JoinError,
    },
}

/// One open SQLite database.
///
/// Clones share the same connection. Get a [`Namespace`] for each part of the
/// bot with [`Store::namespace`].
#[derive(Clone, Debug)]
pub struct Store {
    conn: Arc<Mutex<Connection>>,
}

impl Store {
    /// Open the database at `path`, or make a new one there.
    ///
    /// The store asks SQLite for the WAL journal mode, so a reader does not
    /// stop a writer, and sets [`BUSY_TIMEOUT`]. Some databases cannot use
    /// WAL, for example `:memory:` or a file on some network file systems.
    /// Then the store keeps the mode that SQLite gives and logs a warning.
    /// The store works correctly in each journal mode.
    ///
    /// This call blocks the thread, so call it before the bot starts.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Open`] if SQLite cannot open or prepare the file.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, StoreError> {
        let path = path.as_ref();
        let open_error = |source| StoreError::Open {
            path: path.to_path_buf(),
            source,
        };
        let conn = Connection::open(path).map_err(open_error)?;
        // The pragma returns the mode that SQLite uses after the change, so
        // it needs the `_and_check` form. This mode is not WAL when SQLite
        // cannot use WAL for this database.
        let journal_mode = conn
            .pragma_update_and_check(None, "journal_mode", "WAL", |row| row.get::<_, String>(0))
            .map_err(open_error)?;
        if !journal_mode.eq_ignore_ascii_case("wal") {
            tracing::warn!(
                path = %path.display(),
                %journal_mode,
                "SQLite cannot use WAL for this store, so a reader can make a writer wait"
            );
        }
        Self::prepare(conn).map_err(open_error)
    }

    /// Open a new, empty database in memory.
    ///
    /// The data is lost when the store, all its clones, and all its
    /// namespaces are dropped. This is for tests.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Open`] if SQLite cannot make the database.
    pub fn memory() -> Result<Self, StoreError> {
        let open_error = |source| StoreError::Open {
            path: PathBuf::from(":memory:"),
            source,
        };
        let conn = Connection::open_in_memory().map_err(open_error)?;
        Self::prepare(conn).map_err(open_error)
    }

    /// Set the busy timeout and make the tables of the store.
    fn prepare(conn: Connection) -> rusqlite::Result<Self> {
        conn.busy_timeout(BUSY_TIMEOUT)?;
        conn.execute_batch(MIGRATIONS_TABLE)?;
        Ok(Store {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    /// Get the namespace with the name `name`.
    ///
    /// Two calls with the same name give two handles to the same namespace.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::InvalidName`] if `name` does not obey the rules
    /// in the [module docs](self#names).
    pub fn namespace(&self, name: impl Into<String>) -> Result<Namespace, StoreError> {
        let name = name.into();
        if !is_valid_name(&name) {
            return Err(StoreError::InvalidName { name });
        }
        Ok(Namespace {
            name,
            conn: Arc::clone(&self.conn),
        })
    }
}

/// The part of a [`Store`] that one part of the bot uses.
///
/// Get one with [`Store::namespace`]. Clones share the same connection.
#[derive(Clone, Debug)]
pub struct Namespace {
    name: String,
    conn: Arc<Mutex<Connection>>,
}

impl Namespace {
    /// The name of the namespace.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Run `f` with the database connection, on Tokio's blocking pool.
    ///
    /// `f` gets the full [`rusqlite::Connection`], so it can prepare
    /// statements and start transactions. Calls from all namespaces of a store
    /// run one at a time. A call that waits for the connection does not
    /// occupy a thread of the blocking pool.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Sql`] if `f` returns an error, and
    /// [`StoreError::Task`] if `f` panics.
    pub async fn sql<R, F>(&self, f: F) -> Result<R, StoreError>
    where
        R: Send + 'static,
        F: FnOnce(&mut Connection) -> rusqlite::Result<R> + Send + 'static,
    {
        let namespace = self.name.clone();
        self.run(move |conn| f(conn).map_err(|source| StoreError::Sql { namespace, source }))
            .await
    }

    /// Apply the steps of `steps` that the database does not have yet.
    ///
    /// The store records how many steps each namespace has. A call applies
    /// only the steps after that number, in order, and records the new number.
    /// Thus it is safe to call this at each start of the bot with the full
    /// list.
    ///
    /// A step can hold more than one SQL statement. All steps of one call run
    /// in one transaction: if a step fails, the call applies no step. Thus a
    /// step cannot use a statement that SQLite refuses in a transaction, for
    /// example `VACUUM`. A step also cannot use `BEGIN`, `COMMIT`, `END` or
    /// `ROLLBACK`, because these end the transaction of the call. The call
    /// refuses such a step. `SAVEPOINT`, `RELEASE` and `ROLLBACK TO` are
    /// permitted.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Migration`] if a step fails or controls the
    /// transaction, and
    /// [`StoreError::UnknownMigrations`] if the database has more steps than
    /// `steps`.
    pub async fn migrate(&self, steps: &[&str]) -> Result<(), StoreError> {
        let steps: Vec<String> = steps.iter().map(|step| (*step).to_string()).collect();
        let namespace = self.name.clone();
        self.run(move |conn| apply_migrations(conn, &namespace, &steps))
            .await
    }

    /// Run `f` with the locked connection on Tokio's blocking pool.
    async fn run<R, F>(&self, f: F) -> Result<R, StoreError>
    where
        R: Send + 'static,
        F: FnOnce(&mut Connection) -> Result<R, StoreError> + Send + 'static,
    {
        // Wait for the connection before the blocking task starts. Thus a call
        // that waits does not occupy a thread of the blocking pool. The task
        // owns the guard, and releases it also when `f` panics.
        let mut conn = Arc::clone(&self.conn).lock_owned().await;
        tokio::task::spawn_blocking(move || f(&mut conn))
            .await
            .map_err(|source| StoreError::Task {
                namespace: self.name.clone(),
                source,
            })?
    }
}

/// Apply the steps of `steps` after the recorded version of `namespace`, in
/// one transaction.
fn apply_migrations(
    conn: &mut Connection,
    namespace: &str,
    steps: &[String],
) -> Result<(), StoreError> {
    let sql_error = |source| StoreError::Sql {
        namespace: namespace.to_string(),
        source,
    };
    // Dropping the transaction without `commit` rolls it back, so each early
    // return below leaves the database as it was.
    let tx = conn.transaction().map_err(sql_error)?;
    let recorded: i64 = tx
        .query_row(
            "SELECT version FROM _ircbot_migrations WHERE namespace = ?1",
            [namespace],
            |row| row.get(0),
        )
        .optional()
        .map_err(sql_error)?
        .unwrap_or(0);
    // SQLite stores integers as `i64`. A negative version is a changed
    // `_ircbot_migrations` table, so refuse it.
    let applied = usize::try_from(recorded)
        .map_err(|_| sql_error(rusqlite::Error::IntegralValueOutOfRange(0, recorded)))?;

    if applied > steps.len() {
        return Err(StoreError::UnknownMigrations {
            namespace: namespace.to_string(),
            applied,
            given: steps.len(),
        });
    }
    if applied == steps.len() {
        return Ok(());
    }

    // The authorizer makes SQLite refuse a statement that ends the
    // transaction, so a step cannot apply part of the run. Remove it before
    // each return: `commit`, and the rollback on drop, are also such
    // statements.
    tx.authorizer(Some(refuse_transaction_control))
        .map_err(sql_error)?;
    let result = steps
        .iter()
        .enumerate()
        .skip(applied)
        .try_for_each(|(index, step)| {
            tx.execute_batch(step)
                .map_err(|source| StoreError::Migration {
                    namespace: namespace.to_string(),
                    step: index + 1,
                    source,
                })
        });
    tx.authorizer(None::<fn(AuthContext<'_>) -> Authorization>)
        .map_err(sql_error)?;
    result?;

    let version = i64::try_from(steps.len())
        .map_err(|e| sql_error(rusqlite::Error::ToSqlConversionFailure(Box::new(e))))?;
    tx.execute(
        "INSERT INTO _ircbot_migrations (namespace, version) VALUES (?1, ?2)
         ON CONFLICT (namespace) DO UPDATE SET version = excluded.version",
        (namespace, version),
    )
    .map_err(sql_error)?;
    tx.commit().map_err(sql_error)
}

/// An SQLite authorizer that refuses `BEGIN`, `COMMIT`, `END` and `ROLLBACK`.
///
/// Savepoints stay permitted: in a transaction, they do not end it.
fn refuse_transaction_control(ctx: AuthContext<'_>) -> Authorization {
    match ctx.action {
        AuthAction::Transaction { .. } => Authorization::Deny,
        _ => Authorization::Allow,
    }
}

/// Whether `name` obeys the rules for a namespace name.
fn is_valid_name(name: &str) -> bool {
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    first.is_ascii_lowercase()
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The recorded migration version of `namespace`, or `None`.
    fn recorded_version(store: &Store, namespace: &str) -> Option<i64> {
        let conn = store.conn.try_lock().expect("no call holds the connection");
        conn.query_row(
            "SELECT version FROM _ircbot_migrations WHERE namespace = ?1",
            [namespace],
            |row| row.get(0),
        )
        .optional()
        .expect("read version")
    }

    /// Whether a table with the name `table` exists.
    fn table_exists(store: &Store, table: &str) -> bool {
        let conn = store.conn.try_lock().expect("no call holds the connection");
        conn.query_row(
            "SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
            [table],
            |row| row.get::<_, i64>(0),
        )
        .expect("read schema")
            == 1
    }

    /// A database file in the temporary directory, removed on drop.
    struct TempFile(PathBuf);

    impl TempFile {
        fn new(test: &str) -> Self {
            let path =
                std::env::temp_dir().join(format!("ircbot-store-{}-{test}.db", std::process::id()));
            let file = TempFile(path);
            file.remove();
            file
        }

        fn remove(&self) {
            for suffix in ["", "-wal", "-shm"] {
                let mut path = self.0.clone().into_os_string();
                path.push(suffix);
                // The file is not there when the test did not make it.
                let _ = std::fs::remove_file(path);
            }
        }
    }

    impl Drop for TempFile {
        fn drop(&mut self) {
            self.remove();
        }
    }

    // ── names ────────────────────────────────────────────────────────────────

    #[test]
    fn namespace_names_follow_the_rules() {
        let cases = [
            ("quotes", true),
            ("seen_v2", true),
            ("a", true),
            ("", false),
            ("Quotes", false),
            ("2quotes", false),
            ("_ircbot", false),
            ("quo-tes", false),
            ("quo tes", false),
            ("quotes;", false),
            ("cité", false),
        ];
        for (name, valid) in cases {
            assert_eq!(is_valid_name(name), valid, "name {name:?}");
        }
    }

    #[test]
    fn namespace_refuses_an_invalid_name() {
        let store = Store::memory().unwrap();

        let err = store.namespace("Bad-Name").unwrap_err();

        assert_eq!(
            err.to_string(),
            "invalid namespace name \"Bad-Name\": start with a lowercase ASCII \
             letter, and use only lowercase ASCII letters, digits and `_`"
        );
    }

    #[test]
    fn namespace_has_the_given_name() {
        let store = Store::memory().unwrap();

        let ns = store.namespace("quotes").unwrap();

        assert_eq!(ns.name(), "quotes");
    }

    // ── sql ──────────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn sql_returns_the_value_of_the_closure() {
        let ns = Store::memory().unwrap().namespace("calc").unwrap();

        let sum: i64 = ns
            .sql(|conn| conn.query_row("SELECT 2 + 3", [], |row| row.get(0)))
            .await
            .unwrap();

        assert_eq!(sum, 5);
    }

    #[tokio::test]
    async fn sql_error_names_the_namespace() {
        let ns = Store::memory().unwrap().namespace("quotes").unwrap();

        let err = ns
            .sql(|conn| conn.execute("SELECT * FROM no_such_table", []))
            .await
            .unwrap_err();

        assert!(
            matches!(&err, StoreError::Sql { namespace, .. } if namespace == "quotes"),
            "got {err:?}"
        );
    }

    #[tokio::test]
    async fn sql_reports_a_panic_as_a_task_error() {
        let ns = Store::memory().unwrap().namespace("quotes").unwrap();

        let err = ns
            .sql(|_conn| -> rusqlite::Result<()> { panic!("closure failed") })
            .await
            .unwrap_err();

        assert!(
            matches!(&err, StoreError::Task { namespace, .. } if namespace == "quotes"),
            "got {err:?}"
        );
    }

    #[tokio::test]
    async fn sql_works_after_a_closure_panicked() {
        let ns = Store::memory().unwrap().namespace("quotes").unwrap();
        let _ = ns
            .sql(|_conn| -> rusqlite::Result<()> { panic!("closure failed") })
            .await;

        let one: i64 = ns
            .sql(|conn| conn.query_row("SELECT 1", [], |row| row.get(0)))
            .await
            .unwrap();

        assert_eq!(one, 1);
    }

    /// Given a call that holds the connection, when a second call waits for
    /// it, then the second call does not occupy a blocking thread, so other
    /// blocking work still runs.
    #[test]
    fn a_waiting_call_leaves_the_blocking_pool_free() {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .max_blocking_threads(2)
            .enable_all()
            .build()
            .expect("build runtime");

        runtime.block_on(async {
            let ns = Store::memory().unwrap().namespace("pool").unwrap();
            let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
            let (started_tx, started_rx) = tokio::sync::oneshot::channel::<()>();

            // Call A holds the connection on a blocking thread until other
            // blocking work releases it.
            let holder = tokio::spawn({
                let ns = ns.clone();
                async move {
                    ns.sql(move |_conn| {
                        let _ = started_tx.send(());
                        let _ = release_rx.recv_timeout(Duration::from_secs(5));
                        Ok(())
                    })
                    .await
                }
            });
            started_rx.await.expect("call A started");

            // Call B waits for the connection.
            let waiter = tokio::spawn({
                let ns = ns.clone();
                async move { ns.sql(|_conn| Ok(())).await }
            });
            tokio::time::sleep(Duration::from_millis(50)).await;

            // Unrelated blocking work needs the second blocking thread.
            let other = tokio::task::spawn_blocking(move || release_tx.send(()));
            let finished = tokio::time::timeout(Duration::from_secs(2), other).await;

            assert!(
                finished.is_ok(),
                "other blocking work did not run while a call waited for the connection"
            );
            holder.await.unwrap().unwrap();
            waiter.await.unwrap().unwrap();
        });
    }

    #[tokio::test]
    async fn namespaces_of_one_store_share_the_database() {
        let store = Store::memory().unwrap();
        let writer = store.namespace("writer").unwrap();
        let reader = store.namespace("reader").unwrap();
        writer
            .sql(|conn| {
                conn.execute_batch(
                    "CREATE TABLE writer_t (v TEXT); INSERT INTO writer_t VALUES ('x');",
                )
            })
            .await
            .unwrap();

        let v: String = reader
            .sql(|conn| conn.query_row("SELECT v FROM writer_t", [], |row| row.get(0)))
            .await
            .unwrap();

        assert_eq!(v, "x");
    }

    // ── migrate ──────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn migrate_applies_all_steps_and_records_the_version() {
        let store = Store::memory().unwrap();
        let ns = store.namespace("quotes").unwrap();

        ns.migrate(&[
            "CREATE TABLE quotes_a (id INTEGER)",
            "CREATE TABLE quotes_b (id INTEGER)",
        ])
        .await
        .unwrap();

        assert!(table_exists(&store, "quotes_a"));
        assert!(table_exists(&store, "quotes_b"));
        assert_eq!(recorded_version(&store, "quotes"), Some(2));
    }

    #[tokio::test]
    async fn migrate_skips_the_steps_that_the_database_has() {
        let store = Store::memory().unwrap();
        let ns = store.namespace("quotes").unwrap();
        let steps = ["CREATE TABLE quotes_a (id INTEGER)"];
        ns.migrate(&steps).await.unwrap();

        // A second `CREATE TABLE` of the same table fails, so this passes only
        // when the step does not run again.
        ns.migrate(&steps).await.unwrap();

        assert_eq!(recorded_version(&store, "quotes"), Some(1));
    }

    #[tokio::test]
    async fn migrate_applies_only_the_new_steps_at_the_end() {
        let store = Store::memory().unwrap();
        let ns = store.namespace("quotes").unwrap();
        ns.migrate(&["CREATE TABLE quotes_a (id INTEGER)"])
            .await
            .unwrap();

        ns.migrate(&[
            "CREATE TABLE quotes_a (id INTEGER)",
            "ALTER TABLE quotes_a ADD COLUMN text TEXT",
        ])
        .await
        .unwrap();

        assert_eq!(recorded_version(&store, "quotes"), Some(2));
        let columns: i64 = ns
            .sql(|conn| {
                conn.query_row(
                    "SELECT count(*) FROM pragma_table_info('quotes_a')",
                    [],
                    |row| row.get(0),
                )
            })
            .await
            .unwrap();
        assert_eq!(columns, 2);
    }

    #[tokio::test]
    async fn migrate_with_a_failed_step_applies_no_step() {
        let store = Store::memory().unwrap();
        let ns = store.namespace("quotes").unwrap();

        let err = ns
            .migrate(&["CREATE TABLE quotes_a (id INTEGER)", "THIS IS NOT SQL"])
            .await
            .unwrap_err();

        assert!(
            matches!(&err, StoreError::Migration { namespace, step: 2, .. } if namespace == "quotes"),
            "got {err:?}"
        );
        assert!(!table_exists(&store, "quotes_a"));
        assert_eq!(recorded_version(&store, "quotes"), None);
    }

    #[tokio::test]
    async fn migrate_refuses_a_step_that_commits() {
        let store = Store::memory().unwrap();
        let ns = store.namespace("quotes").unwrap();

        let err = ns
            .migrate(&[
                "CREATE TABLE quotes_a (id INTEGER); COMMIT;",
                "THIS IS NOT SQL",
            ])
            .await
            .unwrap_err();

        assert!(
            matches!(&err, StoreError::Migration { step: 1, .. }),
            "got {err:?}"
        );
        assert!(!table_exists(&store, "quotes_a"));
        assert_eq!(recorded_version(&store, "quotes"), None);
    }

    #[tokio::test]
    async fn migrate_refuses_a_step_that_rolls_back() {
        let store = Store::memory().unwrap();
        let ns = store.namespace("quotes").unwrap();

        let err = ns
            .migrate(&[
                "CREATE TABLE quotes_a (id INTEGER)",
                "ROLLBACK; CREATE TABLE quotes_b (id INTEGER);",
            ])
            .await
            .unwrap_err();

        assert!(
            matches!(&err, StoreError::Migration { step: 2, .. }),
            "got {err:?}"
        );
        assert!(!table_exists(&store, "quotes_a"));
        assert!(!table_exists(&store, "quotes_b"));
        assert_eq!(recorded_version(&store, "quotes"), None);
    }

    #[tokio::test]
    async fn migrate_allows_a_savepoint_in_a_step() {
        let store = Store::memory().unwrap();
        let ns = store.namespace("quotes").unwrap();

        ns.migrate(&["SAVEPOINT s; CREATE TABLE quotes_a (id INTEGER); RELEASE s;"])
            .await
            .unwrap();

        assert!(table_exists(&store, "quotes_a"));
        assert_eq!(recorded_version(&store, "quotes"), Some(1));
    }

    #[tokio::test]
    async fn migrate_refuses_fewer_steps_than_the_database_has() {
        let store = Store::memory().unwrap();
        let ns = store.namespace("quotes").unwrap();
        ns.migrate(&[
            "CREATE TABLE quotes_a (id INTEGER)",
            "CREATE TABLE quotes_b (id INTEGER)",
        ])
        .await
        .unwrap();

        let err = ns
            .migrate(&["CREATE TABLE quotes_a (id INTEGER)"])
            .await
            .unwrap_err();

        assert_eq!(
            err.to_string(),
            "the database has 2 migration steps for namespace \"quotes\", but the \
             bot gives only 1: do not remove steps, add new steps at the end of \
             the list"
        );
    }

    #[tokio::test]
    async fn migrate_keeps_a_version_for_each_namespace() {
        let store = Store::memory().unwrap();
        let quotes = store.namespace("quotes").unwrap();
        let seen = store.namespace("seen").unwrap();

        quotes
            .migrate(&[
                "CREATE TABLE quotes_a (id INTEGER)",
                "CREATE TABLE quotes_b (id INTEGER)",
            ])
            .await
            .unwrap();
        seen.migrate(&["CREATE TABLE seen_a (id INTEGER)"])
            .await
            .unwrap();

        assert_eq!(recorded_version(&store, "quotes"), Some(2));
        assert_eq!(recorded_version(&store, "seen"), Some(1));
    }

    // ── open ─────────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn open_keeps_the_data_after_the_store_is_dropped() {
        let file = TempFile::new("keeps-data");
        {
            let ns = Store::open(&file.0).unwrap().namespace("quotes").unwrap();
            ns.migrate(&["CREATE TABLE quotes_a (text TEXT)"])
                .await
                .unwrap();
            ns.sql(|conn| conn.execute("INSERT INTO quotes_a VALUES ('kept')", []))
                .await
                .unwrap();
        }

        let ns = Store::open(&file.0).unwrap().namespace("quotes").unwrap();
        let text: String = ns
            .sql(|conn| conn.query_row("SELECT text FROM quotes_a", [], |row| row.get(0)))
            .await
            .unwrap();

        assert_eq!(text, "kept");
    }

    #[tokio::test]
    async fn open_uses_the_wal_journal_mode() {
        let file = TempFile::new("wal");
        let ns = Store::open(&file.0).unwrap().namespace("check").unwrap();

        let mode: String = ns
            .sql(|conn| conn.query_row("PRAGMA journal_mode", [], |row| row.get(0)))
            .await
            .unwrap();

        assert_eq!(mode, "wal");
    }

    #[test]
    fn open_warns_when_sqlite_keeps_another_journal_mode() {
        let capture = crate::test_capture::CaptureWriter::default();
        let subscriber = tracing_subscriber::fmt()
            .with_ansi(false)
            .with_writer(capture.clone())
            .finish();
        let _guard = tracing::subscriber::set_default(subscriber);

        // SQLite cannot use WAL for an in-memory database, and keeps the
        // `memory` journal mode.
        let opened = Store::open(":memory:");

        assert!(opened.is_ok(), "got {:?}", opened.err());
        let logs = capture.contents();
        assert!(
            logs.contains("WARN") && logs.contains("journal_mode=memory"),
            "got {logs:?}"
        );
    }

    #[test]
    fn open_reports_the_path_when_it_fails() {
        let path = std::env::temp_dir()
            .join(format!("ircbot-store-{}-no-such-dir", std::process::id()))
            .join("bot.db");

        let err = Store::open(&path).unwrap_err();

        assert!(
            matches!(&err, StoreError::Open { path: p, .. } if *p == path),
            "got {err:?}"
        );
    }
}
