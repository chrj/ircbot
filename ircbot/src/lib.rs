#![doc = include_str!("../README.md")]
#![warn(missing_docs)]
// The crate had one `unsafe` region, for the socket that a hot-reload handed
// to the next binary. Both are gone, and this keeps them gone.
#![forbid(unsafe_code)]

mod args;
mod auth;
pub mod bot;
pub mod connection;
pub mod context;
pub mod format;
pub mod handler;
pub mod irc;
pub mod logging;
pub mod server;
#[cfg(test)]
mod test_capture;
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
/// Obtain one via [`internal::make_handler_set`] + [`ReloadHandle::new`]. It
/// swaps the handlers of a running bot in process; the connection and the
/// binary stay as they are.
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

    use crate::bot::Session;
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

        // The reconnect delays live in the settings, which the read loop
        // consumes with the state. Read them here, while the state is intact.
        let mut backoff = Backoff::new(state.reconnect_delay(), state.max_reconnect_delay());

        let mut current_state = state;

        loop {
            let (session, result) =
                crate::bot::run_session(Arc::clone(&bot), current_state, Arc::clone(&handlers))
                    .await;
            if let Err(e) = result {
                tracing::error!(%server, error = %e, "connection error");
            } else {
                tracing::warn!(%server, "disconnected");
            }

            // A connection that never reached `RPL_WELCOME` was not a working
            // connection, whatever the TCP layer says: a reconnect throttle, a
            // `K-line`, and a wrong server password all accept the TCP
            // connection and then close it. Such a session counts as a failed
            // attempt, so the delay grows instead of the bot knocking every
            // five seconds for as long as the server refuses it.
            match session {
                Session::Registered => backoff.reset(),
                Session::Unregistered => {
                    let attempt = backoff.attempt;
                    backoff.fail();
                    tracing::error!(
                        %server,
                        attempt,
                        next_delay = ?backoff.delay,
                        "the server closed the connection before registration",
                    );
                }
            }

            current_state = reconnect(&blueprint, &server, &mut backoff).await;
        }
    }

    /// The reconnect delay schedule.
    ///
    /// The first attempt waits `first`. Each failed attempt doubles the delay,
    /// up to `max`. A lost name server or a server that restarts therefore
    /// costs the bot its uptime, not its process.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    struct Backoff {
        /// The delay before the next attempt.
        delay: Duration,
        /// The delay to return to once a connection works.
        first: Duration,
        /// The longest delay the doubling reaches.
        max: Duration,
        /// The number of the next attempt, for the log.
        attempt: u64,
    }

    impl Backoff {
        fn new(first: Duration, max: Duration) -> Self {
            Backoff {
                delay: first,
                first,
                max,
                attempt: 1,
            }
        }

        /// Count a failed attempt and double the delay, up to `max`.
        ///
        /// The delay never decreases, so a `max` that is shorter than `first`
        /// gives a constant delay.
        fn fail(&mut self) {
            self.delay = self.delay.saturating_mul(2).min(self.max).max(self.delay);
            self.attempt = self.attempt.saturating_add(1);
        }

        /// Return to the first delay, after a connection that worked.
        fn reset(&mut self) {
            self.delay = self.first;
            self.attempt = 1;
        }
    }

    /// Attempt to reconnect until a connection is established.
    async fn reconnect(blueprint: &Blueprint, server: &Server, backoff: &mut Backoff) -> State {
        loop {
            tracing::info!(%server, delay = ?backoff.delay, attempt = backoff.attempt, "reconnecting");
            tokio::time::sleep(backoff.delay).await;

            match blueprint.connect().await {
                Ok(state) => {
                    tracing::info!(%server, attempt = backoff.attempt, "reconnected");
                    return state;
                }
                Err(e) => {
                    let attempt = backoff.attempt;
                    backoff.fail();
                    tracing::error!(
                        %server,
                        error = %e,
                        attempt,
                        next_delay = ?backoff.delay,
                        "failed to reconnect",
                    );
                }
            }
        }
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
                let mut backoff = Backoff::new(Duration::from_millis(5), Duration::from_millis(10));
                reconnect(&blueprint, &server, &mut backoff).await
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

        #[tokio::test]
        async fn a_server_that_never_welcomes_the_bot_gets_a_growing_delay() {
            // Given a server that accepts the connection and then closes it,
            // as a reconnect throttle and a refused password both do. The
            // bot never sees RPL_WELCOME.
            let capture = crate::test_capture::CaptureWriter::default();
            let subscriber = tracing_subscriber::fmt()
                .with_ansi(false)
                .with_writer(capture.clone())
                .with_env_filter("ircbot::internal=error")
                .finish();
            let _guard = tracing::subscriber::set_default(subscriber);

            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap().to_string();
            let accepts: Arc<std::sync::Mutex<Vec<tokio::time::Instant>>> =
                Arc::new(std::sync::Mutex::new(Vec::new()));
            let server_task = {
                let accepts = Arc::clone(&accepts);
                tokio::spawn(async move {
                    while let Ok((sock, _)) = listener.accept().await {
                        accepts.lock().unwrap().push(tokio::time::Instant::now());
                        // Long enough for the read loop to start, short enough
                        // to keep the test quick.
                        tokio::time::sleep(Duration::from_millis(20)).await;
                        drop(sock);
                    }
                })
            };

            let state = State::connect("tester", &addr, vec![Channel::from("general")])
                .await
                .expect("loopback connect failed")
                .with_reconnect(Duration::from_millis(50), Duration::from_millis(400));

            // When the bot runs for a while against that server.
            let bot = tokio::spawn(run_bot(Arc::new(()), state, Vec::<HandlerEntry<()>>::new()));
            tokio::time::sleep(Duration::from_millis(1200)).await;
            bot.abort();
            server_task.abort();

            // Then the delay grew with each refused session. At the first
            // delay of 50 ms the bot would reach the server about 17 times in
            // that window; the doubling delay holds it to a handful.
            let accepts = accepts.lock().unwrap().clone();
            assert!(
                accepts.len() <= 8,
                "the delay did not grow: {} connections in 1200 ms",
                accepts.len()
            );
            let last = accepts
                .windows(2)
                .last()
                .map(|w| w[1] - w[0])
                .expect("the bot connected at least twice");
            assert!(
                last >= Duration::from_millis(300),
                "the last delay was {last:?}, so the backoff started again"
            );

            // And each refused session said so, with the attempt number and
            // the delay before the next one.
            let logged = capture.contents();
            assert!(
                logged.contains("the server closed the connection before registration"),
                "the refused session was not logged; got:\n{logged}"
            );
            assert!(
                logged.contains("attempt=1") && logged.contains("next_delay=100ms"),
                "the log gives neither the attempt nor the next delay; got:\n{logged}"
            );
        }

        // ── Backoff ────────────────────────────────────────────────────────────

        #[test]
        fn the_delay_doubles_until_it_reaches_the_maximum() {
            let mut backoff = Backoff::new(Duration::from_secs(5), Duration::from_secs(60));
            let mut schedule = vec![backoff.delay];

            for _ in 0..5 {
                backoff.fail();
                schedule.push(backoff.delay);
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
            assert_eq!(backoff.attempt, 6);
        }

        #[test]
        fn a_reset_returns_to_the_first_delay_and_the_first_attempt() {
            let mut backoff = Backoff::new(Duration::from_secs(5), Duration::from_secs(60));
            backoff.fail();
            backoff.fail();

            backoff.reset();

            assert_eq!(
                backoff,
                Backoff::new(Duration::from_secs(5), Duration::from_secs(60))
            );
        }

        #[test]
        fn a_maximum_below_the_first_delay_keeps_the_first_delay() {
            let mut backoff = Backoff::new(Duration::from_secs(5), Duration::from_secs(1));

            backoff.fail();

            assert_eq!(backoff.delay, Duration::from_secs(5));
        }

        #[test]
        fn a_huge_delay_does_not_overflow() {
            let mut backoff = Backoff::new(Duration::MAX, Duration::MAX);

            backoff.fail();

            assert_eq!(backoff.delay, Duration::MAX);
        }
    }
}
