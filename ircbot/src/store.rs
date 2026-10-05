//! Persistent data for a bot, in one SQLite file.
//!
//! This module needs the `store` feature.
//!
//! A [`Store`] is one open SQLite database. A bot gives each part of itself
//! (for example, each group of handlers) its own [`Namespace`]. A namespace
//! keeps data in two ways:
//!
//! * **Key/value data.** [`Namespace::set`] keeps a value of each type that
//!   implements `serde::Serialize`, as JSON. [`Namespace::get`] decodes it into
//!   a type that implements `serde::Deserialize`. [`Namespace::delete`] and
//!   [`Namespace::keys`] complete the set. This needs no schema.
//! * **SQL tables.** [`Namespace::sql`] runs a closure with the real
//!   [`rusqlite::Connection`]. There is no query layer between the closure and
//!   SQLite. [`Namespace::migrate`] applies a list of schema changes. The
//!   store records the version of each namespace, so each step runs one time
//!   only.
//!
//! SQLite calls block the thread. All these functions run the call on Tokio's
//! blocking pool, so a slow query does not stop the runtime.
//!
//! # Key/value example
//!
//! ```rust
//! use ircbot::store::Store;
//!
//! # #[tokio::main(flavor = "current_thread")]
//! # async fn main() -> Result<(), ircbot::store::StoreError> {
//! let store = Store::memory()?;
//! let karma = store.namespace("karma")?;
//!
//! karma.set("alice", &3_i64).await?;
//! let score: Option<i64> = karma.get("alice").await?;
//! assert_eq!(score, Some(3));
//! # Ok(())
//! # }
//! ```
//!
//! A value of a `#[derive(Serialize, Deserialize)]` struct works the same way.
//! If the struct gets a new field later, give the field `#[serde(default)]`,
//! so a value stored before the change still decodes.
//!
//! # SQL example
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
//! SQLite keeps table names that start with `sqlite_` for its own use. Thus a
//! namespace cannot have the name `sqlite`, or a name that starts with
//! `sqlite_`.
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
use serde::de::DeserializeOwned;
use serde::Serialize;
use tokio::sync::Mutex;

/// How long a write waits for a lock that another connection holds, before
/// it fails with `SQLITE_BUSY`.
///
/// One store uses one connection. A second connection to the same file, for
/// example from a second process, can hold a lock.
const BUSY_TIMEOUT: Duration = Duration::from_secs(5);

/// The table that records the migration version of each namespace.
const MIGRATIONS_TABLE: &str = "
    CREATE TABLE IF NOT EXISTS _ircbot_migrations (
        namespace TEXT PRIMARY KEY,
        version   INTEGER NOT NULL
    );
";

/// The namespace of the tables of the store itself. A user namespace cannot
/// have this name, because it starts with `_`.
const INTERNAL_NAMESPACE: &str = "_ircbot";

/// The migration steps of the tables of the store itself. Add new steps at the
/// end, as for a user namespace.
const INTERNAL_MIGRATIONS: &[&str] = &["
    CREATE TABLE _ircbot_kv (
        namespace TEXT NOT NULL,
        key       TEXT NOT NULL,
        value     TEXT NOT NULL,
        PRIMARY KEY (namespace, key)
    ) WITHOUT ROWID;
"];

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
         use only lowercase ASCII letters, digits and `_`, and do not use \
         `sqlite` or a name that starts with `sqlite_`"
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

    /// [`Namespace::set`] could not encode a value as JSON. For example, JSON
    /// refuses a map whose keys are not strings.
    #[error("cannot encode the value of key {key:?} in namespace {namespace:?}: {source}")]
    Encode {
        /// The namespace of the call.
        namespace: String,
        /// The key of the value.
        key: String,
        /// The error from `serde_json`.
        source: serde_json::Error,
    },

    /// [`Namespace::get`] could not decode a stored value into the type that
    /// the caller asked for. Usually the type changed after the value was
    /// stored. `#[serde(default)]` on a new field lets an old value decode.
    #[error(
        "cannot decode the value of key {key:?} in namespace {namespace:?} into the \
         requested type: {source}"
    )]
    Decode {
        /// The namespace of the call.
        namespace: String,
        /// The key of the value.
        key: String,
        /// The error from `serde_json`.
        source: serde_json::Error,
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
    /// stop a writer. Some databases cannot use WAL, for example `:memory:`
    /// or a file on some network file systems. Then the store keeps the mode
    /// that SQLite gives and logs a warning. The store works correctly in
    /// each journal mode.
    ///
    /// A write waits up to 5 seconds for a lock that another connection
    /// holds, for example from a second process. Then it fails with
    /// `SQLITE_BUSY`.
    ///
    /// This call blocks the thread, so call it before the bot starts.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Open`] if SQLite cannot open or prepare the file.
    /// Returns [`StoreError::UnknownMigrations`] for namespace `_ircbot` if a
    /// newer version of this crate made the file. Returns
    /// [`StoreError::Migration`] if the store cannot update its own tables.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, StoreError> {
        let path = path.as_ref();
        let open_error = |source| StoreError::Open {
            path: path.to_path_buf(),
            source,
        };
        let conn = Connection::open(path).map_err(open_error)?;
        // Set the timeout first: the journal mode change below also needs a
        // lock, so it must wait for another connection too.
        conn.busy_timeout(BUSY_TIMEOUT).map_err(open_error)?;
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
        Self::prepare(conn, path)
    }

    /// Open a new, empty database in memory.
    ///
    /// The data is lost when the store, all its clones, and all its
    /// namespaces are dropped. This is for tests.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Open`] if SQLite cannot make the database, and
    /// [`StoreError::Migration`] if the store cannot make its own tables.
    pub fn memory() -> Result<Self, StoreError> {
        let open_error = |source| StoreError::Open {
            path: PathBuf::from(":memory:"),
            source,
        };
        let conn = Connection::open_in_memory().map_err(open_error)?;
        Self::prepare(conn, Path::new(":memory:"))
    }

    /// Make the tables of the store, and apply its own migration steps.
    ///
    /// An in-memory database is private to its connection, so only
    /// [`Store::open`] needs `BUSY_TIMEOUT`.
    fn prepare(mut conn: Connection, path: &Path) -> Result<Self, StoreError> {
        conn.execute_batch(MIGRATIONS_TABLE)
            .map_err(|source| StoreError::Open {
                path: path.to_path_buf(),
                source,
            })?;
        apply_migrations(&mut conn, INTERNAL_NAMESPACE, INTERNAL_MIGRATIONS)?;
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
    /// All namespaces of a store share this connection. The store sets its
    /// connection-wide settings: the journal mode, the busy timeout, and the
    /// authorizer, which [`Namespace::migrate`] uses while it runs. Do not
    /// change these settings in `f`. For example, `migrate` removes an
    /// authorizer that `f` installs.
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

    /// Get the value of `key`, decoded from JSON into `T`.
    ///
    /// Returns `None` if the namespace has no value for `key`.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Decode`] if the stored value does not decode
    /// into `T`, and [`StoreError::Sql`] if SQLite cannot read it.
    pub async fn get<T: DeserializeOwned>(&self, key: &str) -> Result<Option<T>, StoreError> {
        let namespace = self.name.clone();
        let lookup = key.to_string();
        let json: Option<String> = self
            .sql(move |conn| {
                conn.query_row(
                    "SELECT value FROM _ircbot_kv WHERE namespace = ?1 AND key = ?2",
                    (&namespace, &lookup),
                    |row| row.get(0),
                )
                .optional()
            })
            .await?;
        // Decode here, not on the blocking thread, so `T` does not need to be
        // `Send + 'static`.
        json.map(|json| {
            serde_json::from_str(&json).map_err(|source| StoreError::Decode {
                namespace: self.name.clone(),
                key: key.to_string(),
                source,
            })
        })
        .transpose()
    }

    /// Set the value of `key` to `value`, encoded as JSON.
    ///
    /// A value that `key` already has is replaced.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Encode`] if `value` does not encode as JSON, and
    /// [`StoreError::Sql`] if SQLite cannot write it.
    pub async fn set<T: Serialize + ?Sized>(&self, key: &str, value: &T) -> Result<(), StoreError> {
        let json = serde_json::to_string(value).map_err(|source| StoreError::Encode {
            namespace: self.name.clone(),
            key: key.to_string(),
            source,
        })?;
        let namespace = self.name.clone();
        let key = key.to_string();
        self.sql(move |conn| {
            conn.execute(
                "INSERT INTO _ircbot_kv (namespace, key, value) VALUES (?1, ?2, ?3)
                 ON CONFLICT (namespace, key) DO UPDATE SET value = excluded.value",
                (&namespace, &key, &json),
            )
        })
        .await?;
        Ok(())
    }

    /// Delete the value of `key`.
    ///
    /// Returns `true` if `key` had a value.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Sql`] if SQLite cannot delete the value.
    pub async fn delete(&self, key: &str) -> Result<bool, StoreError> {
        let namespace = self.name.clone();
        let key = key.to_string();
        let deleted = self
            .sql(move |conn| {
                conn.execute(
                    "DELETE FROM _ircbot_kv WHERE namespace = ?1 AND key = ?2",
                    (&namespace, &key),
                )
            })
            .await?;
        Ok(deleted > 0)
    }

    /// The keys that start with `prefix`, in byte order.
    ///
    /// The comparison uses the bytes of `prefix`. Thus `%` and `_` have no
    /// special meaning, and a key that contains a NUL also matches. An empty
    /// `prefix` gives all keys of the namespace.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Sql`] if SQLite cannot read the keys.
    pub async fn keys(&self, prefix: &str) -> Result<Vec<String>, StoreError> {
        let namespace = self.name.clone();
        let prefix = prefix.to_string();
        self.sql(move |conn| {
            // Compare the bytes of the prefix. With `LIKE`, `%` and `_` are
            // patterns. With text, `length` and `substr` stop at a NUL, which
            // a key can contain. With a BLOB, they count all bytes. `substr`
            // of an empty BLOB is NULL, so `ifnull` makes the empty key match
            // the empty prefix.
            let mut stmt = conn.prepare(
                "SELECT key FROM _ircbot_kv
                 WHERE namespace = ?1
                   AND ifnull(substr(CAST(key AS BLOB), 1, length(CAST(?2 AS BLOB))), x'')
                       = CAST(?2 AS BLOB)
                 ORDER BY key",
            )?;
            let keys = stmt
                .query_map((&namespace, &prefix), |row| row.get(0))?
                .collect::<rusqlite::Result<Vec<String>>>()?;
            Ok(keys)
        })
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
    steps: &[impl AsRef<str>],
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
            tx.execute_batch(step.as_ref())
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
    // SQLite refuses a table name that starts with `sqlite_`, so the
    // `{namespace}_{table}` names of these namespaces cannot exist.
    let reserved = name == "sqlite" || name.starts_with("sqlite_");
    first.is_ascii_lowercase()
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
        && !reserved
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
            ("sqlite", false),
            ("sqlite_stat", false),
            ("sqlitefoo", true),
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
             letter, use only lowercase ASCII letters, digits and `_`, and do \
             not use `sqlite` or a name that starts with `sqlite_`"
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

    // ── key/value ────────────────────────────────────────────────────────────

    #[derive(Debug, PartialEq, serde::Serialize, serde::Deserialize)]
    struct Quote {
        text: String,
        votes: u32,
    }

    #[tokio::test]
    async fn get_returns_the_value_that_set_stored() {
        let ns = Store::memory().unwrap().namespace("quotes").unwrap();
        let quote = Quote {
            text: "hello".to_string(),
            votes: 3,
        };

        ns.set("q1", &quote).await.unwrap();

        assert_eq!(ns.get::<Quote>("q1").await.unwrap(), Some(quote));
    }

    #[tokio::test]
    async fn get_returns_none_for_a_key_without_a_value() {
        let ns = Store::memory().unwrap().namespace("quotes").unwrap();

        assert_eq!(ns.get::<Quote>("missing").await.unwrap(), None);
    }

    #[tokio::test]
    async fn set_replaces_the_value_of_a_key() {
        let ns = Store::memory().unwrap().namespace("counter").unwrap();
        ns.set("hits", &1_u32).await.unwrap();

        ns.set("hits", &2_u32).await.unwrap();

        assert_eq!(ns.get::<u32>("hits").await.unwrap(), Some(2));
    }

    #[tokio::test]
    async fn namespaces_keep_separate_values_for_the_same_key() {
        let store = Store::memory().unwrap();
        let quotes = store.namespace("quotes").unwrap();
        let seen = store.namespace("seen").unwrap();

        quotes.set("alice", "a quote").await.unwrap();
        seen.set("alice", "#rust").await.unwrap();

        assert_eq!(
            quotes.get::<String>("alice").await.unwrap().as_deref(),
            Some("a quote")
        );
        assert_eq!(
            seen.get::<String>("alice").await.unwrap().as_deref(),
            Some("#rust")
        );
    }

    #[tokio::test]
    async fn delete_removes_the_value_and_tells_if_there_was_one() {
        let ns = Store::memory().unwrap().namespace("quotes").unwrap();
        ns.set("q1", "hello").await.unwrap();

        assert!(ns.delete("q1").await.unwrap());
        assert!(!ns.delete("q1").await.unwrap());
        assert_eq!(ns.get::<String>("q1").await.unwrap(), None);
    }

    #[tokio::test]
    async fn keys_gives_the_keys_with_the_prefix_in_order() {
        let ns = Store::memory().unwrap().namespace("seen").unwrap();
        for key in ["nick:bob", "nick:alice", "chan:rust"] {
            ns.set(key, &true).await.unwrap();
        }

        assert_eq!(
            ns.keys("nick:").await.unwrap(),
            vec!["nick:alice".to_string(), "nick:bob".to_string()]
        );
        assert_eq!(
            ns.keys("").await.unwrap(),
            vec![
                "chan:rust".to_string(),
                "nick:alice".to_string(),
                "nick:bob".to_string()
            ]
        );
    }

    #[tokio::test]
    async fn keys_treats_percent_and_underscore_as_text() {
        let ns = Store::memory().unwrap().namespace("seen").unwrap();
        for key in ["a_b", "axb", "a%c", "azc"] {
            ns.set(key, &true).await.unwrap();
        }

        assert_eq!(ns.keys("a_").await.unwrap(), vec!["a_b".to_string()]);
        assert_eq!(ns.keys("a%").await.unwrap(), vec!["a%c".to_string()]);
    }

    #[tokio::test]
    async fn keys_compares_all_bytes_of_a_key_with_a_nul() {
        let ns = Store::memory().unwrap().namespace("seen").unwrap();
        for key in ["a\0b", "a", ""] {
            ns.set(key, &true).await.unwrap();
        }

        assert_eq!(ns.keys("a\0").await.unwrap(), vec!["a\0b".to_string()]);
        assert_eq!(ns.keys("a\0b").await.unwrap(), vec!["a\0b".to_string()]);
        assert_eq!(
            ns.keys("").await.unwrap(),
            vec![String::new(), "a".to_string(), "a\0b".to_string()]
        );
    }

    #[tokio::test]
    async fn keys_gives_only_the_keys_of_its_namespace() {
        let store = Store::memory().unwrap();
        store
            .namespace("quotes")
            .unwrap()
            .set("k", &1)
            .await
            .unwrap();
        let seen = store.namespace("seen").unwrap();

        assert_eq!(seen.keys("").await.unwrap(), Vec::<String>::new());
    }

    #[tokio::test]
    async fn get_reports_a_value_that_does_not_decode() {
        let ns = Store::memory().unwrap().namespace("quotes").unwrap();
        ns.set("q1", "only a string").await.unwrap();

        let err = ns.get::<Quote>("q1").await.unwrap_err();

        assert!(
            matches!(&err, StoreError::Decode { namespace, key, .. }
                if namespace == "quotes" && key == "q1"),
            "got {err:?}"
        );
    }

    #[tokio::test]
    async fn set_reports_a_value_that_does_not_encode() {
        let ns = Store::memory().unwrap().namespace("quotes").unwrap();
        // JSON refuses a map whose keys are not strings.
        let value = std::collections::BTreeMap::from([((1, 2), "pair")]);

        let err = ns.set("q1", &value).await.unwrap_err();

        assert!(
            matches!(&err, StoreError::Encode { namespace, key, .. }
                if namespace == "quotes" && key == "q1"),
            "got {err:?}"
        );
        assert_eq!(ns.keys("").await.unwrap(), Vec::<String>::new());
    }

    #[test]
    fn store_records_the_version_of_its_own_tables() {
        let store = Store::memory().unwrap();

        assert_eq!(recorded_version(&store, "_ircbot"), Some(1));
        assert!(table_exists(&store, "_ircbot_kv"));
    }

    #[tokio::test]
    async fn open_keeps_the_values_after_the_store_is_dropped() {
        let file = TempFile::new("keeps-values");
        Store::open(&file.0)
            .unwrap()
            .namespace("quotes")
            .unwrap()
            .set("q1", "kept")
            .await
            .unwrap();

        let ns = Store::open(&file.0).unwrap().namespace("quotes").unwrap();

        assert_eq!(
            ns.get::<String>("q1").await.unwrap().as_deref(),
            Some("kept")
        );
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

    /// Given a file that another connection locks for a short time, when the
    /// store opens it, then the store waits for the lock.
    #[test]
    fn open_waits_for_a_lock_of_another_connection() {
        let file = TempFile::new("busy");
        let other = Connection::open(&file.0).expect("open other connection");
        other
            .execute_batch("BEGIN EXCLUSIVE; CREATE TABLE other_t (id INTEGER);")
            .expect("lock the file");
        let holder = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(300));
            other.execute_batch("COMMIT").expect("release the lock");
        });

        let opened = Store::open(&file.0);

        holder.join().expect("lock holder thread");
        assert!(opened.is_ok(), "got {:?}", opened.err());
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
