use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use crate::context::Context;

/// A boxed, heap-allocated future that is `Send + 'static`.
pub type BoxFuture<T> = Pin<Box<dyn Future<Output = T> + Send + 'static>>;

/// The type-erased handler function stored in [`HandlerEntry`].
pub type HandlerFn<T> = Box<dyn Fn(Arc<T>, Context) -> BoxFuture<crate::Result> + Send + Sync>;

/// What causes a handler to fire.
#[derive(Debug, Clone)]
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
    /// Fires periodically at the given `interval`, independent of any incoming
    /// IRC message.  The handler receives a synthetic [`Context`] whose
    /// `target` and `is_channel` fields are derived from `target`; when
    /// `target` is `None` the target string is empty.
    Cron {
        interval: std::time::Duration,
        target: Option<String>,
    },
}

/// Associates a [`Trigger`] with a handler function for a bot of type `T`.
pub struct HandlerEntry<T> {
    pub trigger: Trigger,
    pub handler: HandlerFn<T>,
}
