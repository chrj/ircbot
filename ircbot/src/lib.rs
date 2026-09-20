#![doc = include_str!("../README.md")]
#![warn(missing_docs)]

mod args;
mod auth;
pub mod bot;
pub mod connection;
pub mod context;
pub mod format;
pub mod handler;
pub mod hot_reload;
pub mod irc;
pub mod logging;
pub mod server;
pub mod testing;
mod transport;
pub mod types;

pub use bot::HandlerSet;
pub use connection::{
    State, DEFAULT_FLOOD_BURST, DEFAULT_FLOOD_RATE, DEFAULT_KEEPALIVE_INTERVAL,
    DEFAULT_KEEPALIVE_TIMEOUT, DEFAULT_KEEPNICK_INTERVAL, DEFAULT_MAX_RECONNECT_DELAY,
    DEFAULT_RECONNECT_DELAY, REGISTRATION_TIMEOUT,
};
pub use context::{make_messages, Context, User};
pub use handler::{Bot, BoxFuture, HandlerEntry, HandlerFn, Scope, Trigger};
pub use irc::CtcpMessage;
pub use ircbot_macros::bot;
#[doc = include_str!("../docs/command.md")]
pub use ircbot_macros::command;
#[doc = include_str!("../docs/on.md")]
pub use ircbot_macros::on;
pub use logging::PROTOCOL_LOG_TARGET;
pub use server::Server;
#[cfg(feature = "tls")]
pub use server::TlsServer;
pub use types::{Channel, Nick, Target};

/// The standard error type used throughout the crate.
pub type BoxError = Box<dyn std::error::Error + Send + Sync>;

/// The standard result type returned by handlers.
pub type Result = std::result::Result<(), BoxError>;

/// Errors specific to the bot framework.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Error {
    /// A handler asked for a piece of context that the message did not carry.
    /// The payload names the missing piece, for example `"sender"`.
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

    use crate::connection::Blueprint;
    use crate::{bot::HandlerSet, BoxError, HandlerEntry, Server, State};

    pub use crate::args::Args;

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
    /// The bot retries the reconnect until it is connected again, so this
    /// function does not return while the process runs. It keeps the `Result`
    /// for the generated `main_loop`, which gives it to `main`.
    pub async fn run_bot<T: Send + Sync + 'static>(
        bot: Arc<T>,
        state: State,
        handlers: Vec<HandlerEntry<T>>,
    ) -> std::result::Result<(), BoxError> {
        // Wrap handlers in a HandlerSet so they can be hot-swapped at runtime.
        let handlers = make_handler_set(handlers);

        // Capture how to rebuild this connection before `state` is consumed by
        // the read loop, so every reconnect below restores the caller's
        // configuration in full.
        let blueprint = state.blueprint();
        let server = state.server.clone();

        // Record the flood-control settings so a SIGHUP hot-reload can carry
        // them to the successor process (the `#[bot]` macro doesn't forward
        // them to `exec_reload` itself). Keepalive timings are forwarded by the
        // macro directly; these are not.
        #[cfg(unix)]
        crate::hot_reload::record_flood_settings(
            state.flood_burst(),
            state.flood_rate().as_millis() as u64,
        );

        // The reconnect delays live in the settings, which the read loop
        // consumes with the state. Read them here, while the state is intact.
        let delay = state.reconnect_delay();
        let max_delay = state.max_reconnect_delay();

        let mut current_state = state;

        loop {
            if let Err(e) =
                crate::bot::run_bot_internal(Arc::clone(&bot), current_state, Arc::clone(&handlers))
                    .await
            {
                tracing::error!(%server, error = %e, "connection error");
            } else {
                tracing::warn!(%server, "disconnected");
            }

            current_state = reconnect(&blueprint, &server, delay, max_delay).await;
        }
    }

    /// Attempt to reconnect until a connection is established.
    ///
    /// The first attempt waits `delay`. Each failed attempt doubles the delay,
    /// up to `max_delay`. A lost name server or a server that restarts
    /// therefore costs the bot its uptime, not its process.
    async fn reconnect(
        blueprint: &Blueprint,
        server: &Server,
        delay: Duration,
        max_delay: Duration,
    ) -> State {
        let mut delay = delay;
        let mut attempt: u64 = 1;

        loop {
            tracing::info!(%server, ?delay, attempt, "reconnecting");
            tokio::time::sleep(delay).await;

            match blueprint.connect().await {
                Ok(state) => {
                    tracing::info!(%server, attempt, "reconnected");
                    return state;
                }
                Err(e) => {
                    delay = next_reconnect_delay(delay, max_delay);
                    tracing::error!(
                        %server,
                        error = %e,
                        attempt,
                        next_delay = ?delay,
                        "failed to reconnect",
                    );
                }
            }

            attempt = attempt.saturating_add(1);
        }
    }

    /// The delay for the attempt that comes after one that waited `current`.
    ///
    /// The delay doubles until it reaches `max`. It never decreases, so a `max`
    /// that is shorter than the first delay gives a constant delay.
    fn next_reconnect_delay(current: Duration, max: Duration) -> Duration {
        current.saturating_mul(2).min(max).max(current)
    }

    #[cfg(test)]
    mod tests {
        use tokio::net::TcpListener;
        use tokio::sync::oneshot;

        use super::*;
        use crate::Channel;

        /// Accept and hold connections on `listener` until `stop` is signalled.
        ///
        /// The task owns the listener, so the port refuses connections again
        /// once the task ends.
        fn serve(
            listener: TcpListener,
            stop: oneshot::Receiver<()>,
        ) -> tokio::task::JoinHandle<()> {
            tokio::spawn(async move {
                let mut held = Vec::new();
                tokio::pin!(stop);
                loop {
                    tokio::select! {
                        Ok((sock, _)) = listener.accept() => held.push(sock),
                        _ = &mut stop => return,
                    }
                }
            })
        }

        // ── reconnect ──────────────────────────────────────────────────────────

        #[tokio::test]
        async fn the_reconnect_retries_until_the_server_answers() {
            // Given a bot that connected once, and a server that then stopped.
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap().to_string();
            let (stop, stopped) = oneshot::channel();
            let server_task = serve(listener, stopped);

            let state = State::connect("tester", &addr, vec![Channel::from("general")])
                .await
                .expect("loopback connect failed");
            let blueprint = state.blueprint();
            let server = state.server.clone();
            drop(state);
            stop.send(()).expect("the server task is still running");
            server_task.await.expect("the server task panicked");

            // When the reconnect runs against the port that now refuses.
            let task = tokio::spawn(async move {
                reconnect(
                    &blueprint,
                    &server,
                    Duration::from_millis(5),
                    Duration::from_millis(10),
                )
                .await
            });
            tokio::time::sleep(Duration::from_millis(100)).await;

            // Then it is still trying, and it connects once the server is back.
            assert!(
                !task.is_finished(),
                "the reconnect gave up after a failed attempt"
            );

            let listener = TcpListener::bind(&addr).await.expect("rebind the port");
            let (stop, stopped) = oneshot::channel();
            let server_task = serve(listener, stopped);

            let state = tokio::time::timeout(Duration::from_secs(10), task)
                .await
                .expect("the reconnect did not return after the server came back")
                .expect("the reconnect task panicked");
            assert_eq!(state.server.addr(), addr);

            drop(state);
            stop.send(()).expect("the server task is still running");
            server_task.await.expect("the server task panicked");
        }

        // ── next_reconnect_delay ───────────────────────────────────────────────

        #[test]
        fn the_delay_doubles_until_it_reaches_the_maximum() {
            let max = Duration::from_secs(60);
            let mut delay = Duration::from_secs(5);
            let mut schedule = vec![delay];

            for _ in 0..5 {
                delay = next_reconnect_delay(delay, max);
                schedule.push(delay);
            }

            assert_eq!(
                schedule,
                vec![
                    Duration::from_secs(5),
                    Duration::from_secs(10),
                    Duration::from_secs(20),
                    Duration::from_secs(40),
                    Duration::from_secs(60),
                    Duration::from_secs(60),
                ]
            );
        }

        #[test]
        fn a_maximum_below_the_current_delay_keeps_the_current_delay() {
            let delay = next_reconnect_delay(Duration::from_secs(5), Duration::from_secs(1));

            assert_eq!(delay, Duration::from_secs(5));
        }

        #[test]
        fn a_huge_delay_does_not_overflow() {
            let delay = next_reconnect_delay(Duration::MAX, Duration::MAX);

            assert_eq!(delay, Duration::MAX);
        }
    }
}
