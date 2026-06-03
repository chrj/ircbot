#![doc = include_str!("../README.md")]

pub mod bot;
pub mod connection;
pub mod context;
pub mod handler;
pub mod hot_reload;
pub mod irc;
pub mod testing;
pub mod types;

pub use bot::HandlerSet;
pub use connection::{
    State, DEFAULT_FLOOD_BURST, DEFAULT_FLOOD_RATE, DEFAULT_KEEPALIVE_INTERVAL,
    DEFAULT_KEEPALIVE_TIMEOUT,
};
pub use context::{make_messages, Context, User};
pub use handler::{BoxFuture, HandlerEntry, HandlerFn, Trigger};
pub use irc::CtcpMessage;
pub use ircbot_macros::bot;
#[doc = include_str!("../docs/command.md")]
pub use ircbot_macros::command;
#[doc = include_str!("../docs/on.md")]
pub use ircbot_macros::on;
pub use types::{Channel, Nick, Target};

/// The standard error type used throughout the crate.
pub type BoxError = Box<dyn std::error::Error + Send + Sync>;

/// The standard result type returned by handlers.
pub type Result = std::result::Result<(), BoxError>;

/// Errors specific to the bot framework.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("missing context: {0}")]
    MissingContext(&'static str),
}

// ─── ReloadHandle ─────────────────────────────────────────────────────────────

/// A handle for replacing the bot's handler list at runtime without
/// disconnecting from IRC.
///
/// Obtain one via [`internal::make_handler_set`] + [`ReloadHandle::new`] when
/// using the lower-level API, or use the generated `main_loop()` which wires
/// up `SIGHUP` automatically.
///
/// `ReloadHandle` is `Clone` — clones share the same underlying [`HandlerSet`].
pub struct ReloadHandle<T> {
    handlers: HandlerSet<T>,
}

impl<T> ReloadHandle<T> {
    /// Create a [`ReloadHandle`] from a [`HandlerSet`].
    pub fn new(handlers: HandlerSet<T>) -> Self {
        ReloadHandle { handlers }
    }

    /// Atomically replace the running bot's handler list.
    ///
    /// Takes effect on the next incoming IRC message; the connection is not
    /// interrupted.
    ///
    /// Cron handlers are honoured too: the cron supervisor re-reads the live
    /// set every cycle, so replaced or removed `#[on(cron = …)]` handlers take
    /// effect at their next scheduled tick. The one exception is a cron handler
    /// **added** when the previous set had no cron handlers at all — the idle
    /// supervisor only re-scans periodically, so it can take up to a minute to
    /// fire for the first time. Body swaps and additions alongside an existing
    /// cron handler are picked up promptly.
    pub fn reload(&self, new_handlers: Vec<HandlerEntry<T>>) {
        if let Ok(mut guard) = self.handlers.write() {
            *guard = std::sync::Arc::new(new_handlers);
        }
    }
}

impl<T> Clone for ReloadHandle<T> {
    fn clone(&self) -> Self {
        ReloadHandle {
            handlers: std::sync::Arc::clone(&self.handlers),
        }
    }
}

/// Internal helpers used by the generated `main_loop` code.
pub mod internal {
    use std::sync::{Arc, RwLock};
    use std::time::Duration;

    use crate::{bot::HandlerSet, BoxError, HandlerEntry, State};

    /// Delay between successive reconnection attempts.
    const RECONNECT_DELAY: Duration = Duration::from_secs(5);

    /// Wrap a `Vec<HandlerEntry<T>>` in a [`HandlerSet`].
    ///
    /// Convenience used by generated `main_loop` code and tests.
    #[must_use]
    pub fn make_handler_set<T>(handlers: Vec<HandlerEntry<T>>) -> HandlerSet<T> {
        Arc::new(RwLock::new(Arc::new(handlers)))
    }

    /// Run the bot, reconnecting automatically whenever the connection is lost.
    ///
    /// # Errors
    ///
    /// Returns an error if a reconnection attempt fails.
    pub async fn run_bot<T: Send + Sync + 'static>(
        bot: Arc<T>,
        state: State,
        handlers: Vec<HandlerEntry<T>>,
    ) -> std::result::Result<(), BoxError> {
        // Wrap handlers in a HandlerSet so they can be hot-swapped at runtime.
        let handlers = make_handler_set(handlers);

        // Preserve reconnection parameters before `state` is consumed.
        let server = state.server.clone();
        let nick = state.nick.clone();
        let channels = state.channels.clone();
        let keepalive_interval = state.keepalive_interval;
        let keepalive_timeout = state.keepalive_timeout;
        let flood_burst = state.flood_burst;
        let flood_rate = state.flood_rate;

        // Record the flood-control settings so a SIGHUP hot-reload can carry
        // them to the successor process (the `#[bot]` macro doesn't forward
        // them to `exec_reload` itself). Keepalive timings are forwarded by the
        // macro directly; these are not.
        #[cfg(unix)]
        crate::hot_reload::record_flood_settings(flood_burst, flood_rate.as_millis() as u64);

        let mut current_state = state;

        loop {
            if let Err(e) =
                crate::bot::run_bot_internal(Arc::clone(&bot), current_state, Arc::clone(&handlers))
                    .await
            {
                eprintln!("[ircbot] connection error: {e}");
            } else {
                eprintln!("[ircbot] disconnected from {server}");
            }

            eprintln!(
                "[ircbot] reconnecting to {server} in {:.0?}…",
                RECONNECT_DELAY
            );
            tokio::time::sleep(RECONNECT_DELAY).await;

            match State::connect(nick.clone(), &server, channels.clone()).await {
                Ok(mut new_state) => {
                    new_state.keepalive_interval = keepalive_interval;
                    new_state.keepalive_timeout = keepalive_timeout;
                    new_state.flood_burst = flood_burst;
                    new_state.flood_rate = flood_rate;
                    current_state = new_state;
                }
                Err(e) => {
                    eprintln!("[ircbot] failed to reconnect to {server}: {e}");
                    return Err(e);
                }
            }
        }
    }
}
