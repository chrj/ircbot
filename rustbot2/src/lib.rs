pub mod bot;
pub mod connection;
pub mod context;
pub mod handler;
pub mod irc;

pub use connection::BotState;
pub use context::{Context, User};
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

/// Internal helpers used by the generated `main_loop` code.
pub mod internal {
    use std::sync::Arc;
    use std::time::Duration;

    use crate::{BotState, BoxError, HandlerEntry};

    /// Delay between successive reconnection attempts.
    const RECONNECT_DELAY: Duration = Duration::from_secs(5);

    /// Run the bot, reconnecting automatically whenever the connection is lost.
    ///
    /// The bot sends a periodic `PING` to verify liveness; if no matching
    /// `PONG` arrives within the configured timeout the connection is dropped
    /// and a reconnect is attempted after [`RECONNECT_DELAY`].
    ///
    /// This function only returns with an `Err` when a reconnection attempt
    /// itself fails (e.g. the server is permanently unreachable).
    ///
    /// # Errors
    ///
    /// Returns an error if a reconnection attempt fails.
    pub async fn run_bot<T: Send + Sync + 'static>(
        bot: Arc<T>,
        state: BotState,
        handlers: Vec<HandlerEntry<T>>,
    ) -> std::result::Result<(), BoxError> {
        // Preserve reconnection parameters before `state` is consumed.
        let server = state.server.clone();
        let nick = state.nick.clone();
        let channels = state.channels.clone();
        let keepalive_interval = state.keepalive_interval;
        let keepalive_timeout = state.keepalive_timeout;

        // Wrap handlers in an Arc so they can be shared across reconnects
        // without requiring `Clone` on `HandlerEntry`.
        let handlers = Arc::new(handlers);
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
