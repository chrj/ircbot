//! What fires a handler, and the shape of the handler itself.
//!
//! A [`Trigger`] describes a condition on an incoming message, or a schedule.
//! A [`HandlerEntry`] pairs one trigger with the function to call. The bot holds
//! a list of these entries and tests each incoming message against all of them.
//!
//! The `#[command]` and `#[on]` macros build these values for you. Construct
//! them by hand only when you assemble a handler list without the macros.

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
    ///
    /// When `role` is `Some`, the command only fires for senders whose
    /// `nick!user@host` matches one of the hostmask patterns configured for that
    /// role (see [`State::with_role`](crate::State::with_role)); unauthorized
    /// senders are silently ignored.
    Command {
        /// The command word that follows the `!` prefix.
        name: String,
        /// When set, the command fires only in this channel or query.
        target: Option<String>,
        /// When set, the command fires only for senders that hold this role.
        role: Option<String>,
    },
    /// Fires when an incoming PRIVMSG matches a glob pattern (`*` as wildcard).
    Message {
        /// The glob pattern matched against the message text.
        pattern: String,
        /// When set, the pattern applies only to messages sent to this target.
        target: Option<String>,
    },
    /// Fires on a specific IRC event (e.g. "JOIN"), with optional target/regex filter.
    Event {
        /// The IRC command or numeric that fires the handler, compared
        /// without case sensitivity.
        event: String,
        /// When set, the handler fires only for events on this target.
        target: Option<String>,
        /// When set, the trailing parameter of the message must also match this
        /// regular expression. Its capture groups become the handler's captures.
        regex: Option<String>,
    },
    /// Fires when a PRIVMSG addresses the bot by name at the start of the
    /// message (e.g. `"botname: hello"` or `"botname, ping"`).
    /// The text following the address prefix is provided as a capture.
    Mention {
        /// When set, the handler fires only for messages sent to this target.
        target: Option<String>,
    },
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
        /// The cron expression, in 6-field Quartz format with an optional
        /// 7th `year` field.
        schedule: String,
        /// The IANA timezone name that the schedule is evaluated in.
        tz: String,
        /// When set, the handler's [`Context::target`] is this channel or user.
        target: Option<String>,
    },
}

/// Associates a [`Trigger`] with a handler function for a bot of type `T`.
pub struct HandlerEntry<T> {
    /// What causes the handler to fire.
    pub trigger: Trigger,
    /// The function called when the trigger matches.
    pub handler: HandlerFn<T>,
}
