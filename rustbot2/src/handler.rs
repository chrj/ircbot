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
}

/// Associates a [`Trigger`] with a handler function for a bot of type `T`.
pub struct HandlerEntry<T> {
    pub trigger: Trigger,
    pub handler: HandlerFn<T>,
}
