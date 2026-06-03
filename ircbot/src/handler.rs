use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use crate::context::Context;

/// A boxed, heap-allocated future that is `Send + 'static`.
pub type BoxFuture<T> = Pin<Box<dyn Future<Output = T> + Send + 'static>>;

/// The type-erased handler function stored in [`HandlerEntry`].
pub type HandlerFn<T> = Box<dyn Fn(Arc<T>, Context) -> BoxFuture<crate::Result> + Send + Sync>;

/// What causes a handler to fire.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Trigger {
    /// Fires when the user sends `!<name>` (optionally in a specific channel).
    Command {
        name: String,
        target: Option<String>,
    },
    /// Fires when an incoming PRIVMSG matches a glob pattern (`*` as wildcard).
    Message {
        pattern: String,
        target: Option<String>,
    },
    /// Fires on a specific IRC event (e.g. "JOIN"), with optional target/regex filter.
    Event {
        event: String,
        target: Option<String>,
        regex: Option<String>,
    },
    /// Fires when a PRIVMSG addresses the bot by name at the start of the
    /// message (e.g. `"botname: hello"` or `"botname, ping"`).
    /// The text following the address prefix is provided as a capture.
    Mention { target: Option<String> },
    /// Fires on a schedule described by a cron expression.  The expression uses
    /// the 6-field Quartz format: `sec min hour day-of-month month day-of-week`
    /// with an optional 7th `year` field.  Times are evaluated in `tz`, which
    /// must be a valid IANA timezone name (e.g. `"America/New_York"`); defaults
    /// to `"UTC"` when not specified.
    ///
    /// When `target` is `None` the handler's [`Context::target`] is a
    /// [`Target::User`](crate::Target::User) with an empty name (so its
    /// `as_str()` is empty) and [`Context::is_channel`] returns `false`.
    /// Handlers that need to send a message should either specify a `target` or
    /// store the destination in their bot state.
    ///
    /// Example — top of every hour on weekday afternoons (Eastern time):
    /// `"0 0 8-16 * * MON-FRI"` with `tz = "America/New_York"`
    Cron {
        schedule: String,
        tz: String,
        target: Option<String>,
    },
}

/// Associates a [`Trigger`] with a handler function for a bot of type `T`.
pub struct HandlerEntry<T> {
    pub trigger: Trigger,
    pub handler: HandlerFn<T>,
}
