pub mod bot;
pub mod connection;
pub mod context;
pub mod handler;
pub mod irc;

pub use connection::BotState;
pub use context::{Context, User};
pub use handler::{BoxFuture, HandlerEntry, HandlerFn, Trigger};
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
            BotError::MissingContext(ctx) => write!(f, "missing context: {}", ctx),
        }
    }
}

impl std::error::Error for BotError {}

/// Internal helpers used by the generated `main_loop` code.
pub mod internal {
    use std::sync::Arc;

    use crate::{BotState, BoxError, HandlerEntry};

    pub async fn run_bot<T: Send + Sync + 'static>(
        bot: Arc<T>,
        state: BotState,
        handlers: Vec<HandlerEntry<T>>,
    ) -> std::result::Result<(), BoxError> {
        crate::bot::run_bot_internal(bot, state, handlers).await
    }
}
