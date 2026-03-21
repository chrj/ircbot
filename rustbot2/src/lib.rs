pub mod bot;
pub mod connection;
pub mod context;
pub mod handler;
pub mod hot_reload;
pub mod irc;

pub use bot::HandlerSet;
pub use connection::{
    BotState, DEFAULT_FLOOD_BURST, DEFAULT_FLOOD_RATE, DEFAULT_KEEPALIVE_INTERVAL,
    DEFAULT_KEEPALIVE_TIMEOUT,
};
pub use context::{make_messages, Context, User};
pub use handler::{BoxFuture, HandlerEntry, HandlerFn, Trigger};
pub use irc::CtcpMessage;
pub use rustbot2_macros::{bot, command, on};

/// The standard error type used throughout the crate.
pub type BoxError = Box<dyn std::error::Error + Send + Sync>;

/// The standard result type returned by handlers.
pub type Result = std::result::Result<(), BoxError>;

/// Errors specific to the bot framework.
#[derive(Debug)]
pub enum BotError {
    MissingContext(&'static str),
}

impl std::fmt::Display for BotError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BotError::MissingContext(ctx) => write!(f, "missing context: {ctx}"),
        }
    }
}

impl std::error::Error for BotError {}

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

    use crate::{bot::HandlerSet, BotState, BoxError, HandlerEntry};

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
    /// The `handlers` [`HandlerSet`] can be swapped at any time without
    /// interrupting the IRC connection.
    ///
    /// # Errors
    ///
    /// Returns an error if a reconnection attempt fails.
    pub async fn run_bot<T: Send + Sync + 'static>(
        bot: Arc<T>,
        state: BotState,
        handlers: HandlerSet<T>,
    ) -> std::result::Result<(), BoxError> {
        // Preserve reconnection parameters before `state` is consumed.
        let server = state.server.clone();
        let nick = state.nick.clone();
        let channels = state.channels.clone();
        let keepalive_interval = state.keepalive_interval;
        let keepalive_timeout = state.keepalive_timeout;
        let flood_burst = state.flood_burst;
        let flood_rate = state.flood_rate;

        let mut current_state = state;

        loop {
            if let Err(e) =
                crate::bot::run_bot_internal(Arc::clone(&bot), current_state, Arc::clone(&handlers))
                    .await
            {
                eprintln!("[rustbot2] connection error: {e}");
            } else {
                eprintln!("[rustbot2] disconnected from {server}");
            }

            eprintln!(
                "[rustbot2] reconnecting to {server} in {:.0?}…",
                RECONNECT_DELAY
            );
            tokio::time::sleep(RECONNECT_DELAY).await;

            match BotState::connect(nick.clone(), &server, channels.clone()).await {
                Ok(mut new_state) => {
                    new_state.keepalive_interval = keepalive_interval;
                    new_state.keepalive_timeout = keepalive_timeout;
                    new_state.flood_burst = flood_burst;
                    new_state.flood_rate = flood_rate;
                    current_state = new_state;
                }
                Err(e) => {
                    eprintln!("[rustbot2] failed to reconnect to {server}: {e}");
                    return Err(e);
                }
            }
        }
    }
}
