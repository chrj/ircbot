//! Tests for the read-loop dispatch behaviour of `run_bot_internal`:
//! CTCP auto-replies, server PING→PONG, JOIN-on-welcome, keepalive PONG
//! token handling, multiple-handler dispatch, handler error isolation and
//! sender population.
//!
//! These tests use a lightweight in-process mock IRC server (a `TcpListener`
//! bound to a random loopback port) so they do **not** require Docker or the
//! `integration` feature flag.
//!
//! Run with:
//!   cargo test --test dispatch

use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;
use tokio::sync::mpsc;

use ircbot::bot::run_bot_internal;
use ircbot::handler::{HandlerEntry, HandlerFn, Trigger};
use ircbot::{BoxFuture, Context, State};

// ─── mock server harness ──────────────────────────────────────────────────────

/// A handle to an in-process mock IRC server.
///
/// `to_bot` sends raw bytes to the bot (the test is responsible for CRLF
/// termination); `from_bot` yields each CRLF-terminated line the bot writes,
/// already stripped of the trailing newline.
struct MockServer {
    addr: String,
    to_bot: mpsc::UnboundedSender<String>,
    from_bot: mpsc::UnboundedReceiver<String>,
}

impl MockServer {
    /// Bind to a random loopback port and start accepting a single connection.
    async fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap().to_string();

        let (to_bot_tx, mut to_bot_rx) = mpsc::unbounded_channel::<String>();
        let (from_bot_tx, from_bot_rx) = mpsc::unbounded_channel::<String>();

        tokio::spawn(async move {
            let (sock, _) = listener.accept().await.unwrap();
            let (read_half, mut write_half) = sock.into_split();

            // Writer: drain queued server→bot lines onto the socket.
            tokio::spawn(async move {
                while let Some(line) = to_bot_rx.recv().await {
                    if write_half.write_all(line.as_bytes()).await.is_err() {
                        break;
                    }
                    if write_half.flush().await.is_err() {
                        break;
                    }
                }
            });

            // Reader: forward every line the bot writes to the test.
            let mut reader = BufReader::new(read_half).lines();
            while let Ok(Some(line)) = reader.next_line().await {
                if from_bot_tx.send(line).is_err() {
                    break;
                }
            }
        });

        MockServer {
            addr,
            to_bot: to_bot_tx,
            from_bot: from_bot_rx,
        }
    }

    /// Queue a raw line (must include its own `\r\n`) to be sent to the bot.
    fn send(&self, line: &str) {
        self.to_bot.send(line.to_string()).unwrap();
    }

    /// Send the standard `001` welcome so the bot finishes its handshake.
    fn send_welcome(&self) {
        self.send(":server 001 testbot :Welcome to the mock IRC server\r\n");
    }

    /// Wait up to 2 s for a bot-written line matching `predicate`, returning it.
    /// Panics on timeout.
    async fn expect_line(&mut self, predicate: impl Fn(&str) -> bool) -> String {
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let line = self
                    .from_bot
                    .recv()
                    .await
                    .expect("bot connection closed before expected line");
                if predicate(&line) {
                    return line;
                }
            }
        })
        .await
        .expect("timed out waiting for expected line from bot")
    }

    /// Assert that no line matching `predicate` is written within `window`.
    async fn expect_no_line(&mut self, window: Duration, predicate: impl Fn(&str) -> bool) {
        let res = tokio::time::timeout(window, async {
            loop {
                match self.from_bot.recv().await {
                    Some(line) if predicate(&line) => return Some(line),
                    Some(_) => continue,
                    None => return None,
                }
            }
        })
        .await;
        if let Ok(Some(line)) = res {
            panic!("unexpected line written by bot: {line:?}");
        }
    }
}

/// Connect a bot to `addr` and run `run_bot_internal` in a background task,
/// returning the join handle so the caller can abort or inspect it.
async fn spawn_bot<T: Send + Sync + 'static>(
    addr: &str,
    bot: Arc<T>,
    handlers: Vec<HandlerEntry<T>>,
) -> tokio::task::JoinHandle<Result<(), ircbot::BoxError>> {
    let state = State::connect("testbot".to_string(), addr, vec![])
        .await
        .expect("failed to connect to mock server");
    let handler_set = ircbot::internal::make_handler_set(handlers);
    tokio::spawn(run_bot_internal(bot, state, handler_set))
}

/// Connect a bot with explicit channels and keepalive settings.
async fn spawn_bot_with<T: Send + Sync + 'static>(
    addr: &str,
    channels: Vec<String>,
    keepalive: Option<(Duration, Duration)>,
    bot: Arc<T>,
    handlers: Vec<HandlerEntry<T>>,
) -> tokio::task::JoinHandle<Result<(), ircbot::BoxError>> {
    let mut state = State::connect(
        "testbot".to_string(),
        addr,
        channels.into_iter().map(ircbot::Channel::from).collect(),
    )
    .await
    .expect("failed to connect to mock server");
    if let Some((interval, timeout)) = keepalive {
        state = state.with_keepalive(interval, timeout);
    }
    let handler_set = ircbot::internal::make_handler_set(handlers);
    tokio::spawn(run_bot_internal(bot, state, handler_set))
}

/// A no-op handler set.
fn no_handlers() -> Vec<HandlerEntry<()>> {
    vec![]
}

// ─── A1–A3: CTCP auto-replies ────────────────────────────────────────────────

#[tokio::test]
async fn ctcp_ping_with_arg_gets_notice_reply() {
    let mut server = MockServer::start().await;
    let bot_task = spawn_bot(&server.addr, Arc::new(()), no_handlers()).await;
    server.send_welcome();

    server.send(":alice!a@h PRIVMSG testbot :\x01PING 12345\x01\r\n");
    let line = server.expect_line(|l| l.starts_with("NOTICE")).await;
    assert_eq!(line, "NOTICE alice :\x01PING 12345\x01");

    bot_task.abort();
}

#[tokio::test]
async fn ctcp_ping_without_arg_gets_notice_reply() {
    let mut server = MockServer::start().await;
    let bot_task = spawn_bot(&server.addr, Arc::new(()), no_handlers()).await;
    server.send_welcome();

    // No trailing space / arg.
    server.send(":alice!a@h PRIVMSG testbot :\x01PING\x01\r\n");
    let line = server.expect_line(|l| l.starts_with("NOTICE")).await;
    assert_eq!(line, "NOTICE alice :\x01PING\x01");

    bot_task.abort();
}

#[tokio::test]
async fn ctcp_version_gets_notice_reply() {
    let mut server = MockServer::start().await;
    let bot_task = spawn_bot(&server.addr, Arc::new(()), no_handlers()).await;
    server.send_welcome();

    server.send(":alice!a@h PRIVMSG testbot :\x01VERSION\x01\r\n");
    let line = server.expect_line(|l| l.starts_with("NOTICE")).await;
    // Assert the routing and CTCP framing, not the exact version string (which
    // would just re-read CARGO_PKG_VERSION the same way the implementation does).
    assert!(
        line.starts_with("NOTICE alice :\x01VERSION ircbot "),
        "unexpected VERSION reply: {line:?}"
    );
    assert!(
        line.ends_with('\x01'),
        "VERSION reply must be CTCP-terminated: {line:?}"
    );

    bot_task.abort();
}

#[tokio::test]
async fn ctcp_ping_without_sender_produces_no_reply() {
    let mut server = MockServer::start().await;
    let bot_task = spawn_bot(&server.addr, Arc::new(()), no_handlers()).await;
    server.send_welcome();

    // A CTCP PING with a server-name prefix (no nick!user@host) has no
    // `source_nickname`, so the bot cannot — and must not — reply.
    server.send(":irc.server PRIVMSG testbot :\x01PING 999\x01\r\n");
    server
        .expect_no_line(Duration::from_millis(400), |l| l.starts_with("NOTICE"))
        .await;

    bot_task.abort();
}

// ─── A4: server PING → PONG ──────────────────────────────────────────────────

#[tokio::test]
async fn server_ping_gets_pong_reply() {
    let mut server = MockServer::start().await;
    let bot_task = spawn_bot(&server.addr, Arc::new(()), no_handlers()).await;
    server.send_welcome();

    server.send("PING :server.test\r\n");
    let line = server.expect_line(|l| l.starts_with("PONG")).await;
    assert_eq!(line, "PONG :server.test");

    bot_task.abort();
}

// ─── A5: JOIN-on-welcome and join-once guard ─────────────────────────────────

#[tokio::test]
async fn joins_all_channels_on_welcome_once() {
    let mut server = MockServer::start().await;
    let bot_task = spawn_bot_with(
        &server.addr,
        vec!["#a".to_string(), "#b".to_string()],
        None,
        Arc::new(()),
        no_handlers(),
    )
    .await;

    server.send_welcome();

    let first = server.expect_line(|l| l.starts_with("JOIN")).await;
    let second = server.expect_line(|l| l.starts_with("JOIN")).await;
    let mut joins = [first, second];
    joins.sort();
    assert_eq!(joins, ["JOIN #a".to_string(), "JOIN #b".to_string()]);

    // A second welcome must NOT trigger duplicate JOINs (the `joined` guard).
    server.send_welcome();
    server
        .expect_no_line(Duration::from_millis(400), |l| l.starts_with("JOIN"))
        .await;

    bot_task.abort();
}

// ─── A5b: nick-in-use negotiation (433 / 437) ────────────────────────────────

#[tokio::test]
async fn nick_in_use_retries_with_underscore_suffixes() {
    let mut server = MockServer::start().await;
    let bot_task = spawn_bot(&server.addr, Arc::new(()), no_handlers()).await;

    // The bot sends its initial `NICK testbot` on connect; the server rejects
    // it as already in use. The bot should retry with an underscore suffix.
    server.send(":server 433 * testbot :Nickname is already in use\r\n");
    let first = server.expect_line(|l| l == "NICK testbot_").await;
    assert_eq!(first, "NICK testbot_");

    // Reject the fallback too → a second underscore (the base nick, not the
    // last candidate, drives the suffix count).
    server.send(":server 433 * testbot_ :Nickname is already in use\r\n");
    let second = server.expect_line(|l| l == "NICK testbot__").await;
    assert_eq!(second, "NICK testbot__");

    bot_task.abort();
}

#[tokio::test]
async fn unavailresource_437_also_triggers_nick_retry() {
    let mut server = MockServer::start().await;
    let bot_task = spawn_bot(&server.addr, Arc::new(()), no_handlers()).await;

    // 437 (ERR_UNAVAILRESOURCE) during registration is treated like 433.
    server.send(":server 437 * testbot :Nick/channel is temporarily unavailable\r\n");
    let line = server.expect_line(|l| l == "NICK testbot_").await;
    assert_eq!(line, "NICK testbot_");

    bot_task.abort();
}

#[tokio::test]
async fn nick_in_use_after_welcome_does_not_renegotiate() {
    let mut server = MockServer::start().await;
    let bot_task = spawn_bot(&server.addr, Arc::new(()), no_handlers()).await;

    server.send_welcome();
    // A 433 arriving after registration completes (e.g. a later voluntary NICK
    // change) must NOT trigger the registration-time fallback. The initial
    // `NICK testbot` has no underscore suffix, so matching `NICK testbot_`
    // isolates any (incorrect) fallback attempt.
    server.send(":server 433 * testbot :Nickname is already in use\r\n");
    server
        .expect_no_line(Duration::from_millis(400), |l| {
            l.starts_with("NICK testbot_")
        })
        .await;

    bot_task.abort();
}

// ─── A6: keepalive PONG token positions ──────────────────────────────────────

/// Drive one keepalive cycle: wait for the bot's keepalive PING, reply with
/// `pong_line`, and report whether the bot task is still running afterwards.
///
/// `run_bot_internal` returns (task finishes) when the keepalive times out, so
/// a still-running task means the PONG token was accepted.
async fn keepalive_survives(pong_line: &str) -> bool {
    let mut server = MockServer::start().await;
    let bot_task = spawn_bot_with(
        &server.addr,
        vec![],
        Some((Duration::from_millis(300), Duration::from_millis(300))),
        Arc::new(()),
        no_handlers(),
    )
    .await;
    server.send_welcome();

    // Wait for the bot's keepalive PING and answer it.
    server.expect_line(|l| l.contains("ircbot-keepalive")).await;
    server.send(pong_line);

    // Give the keepalive cycle (interval + timeout) time to evaluate.
    tokio::time::sleep(Duration::from_millis(800)).await;
    let finished = bot_task.is_finished();
    bot_task.abort();
    !finished
}

#[tokio::test]
async fn keepalive_pong_token_in_trailing_position_is_accepted() {
    // "PONG server :token" — token in the `b` field.
    assert!(keepalive_survives("PONG irc.server :ircbot-keepalive\r\n").await);
}

#[tokio::test]
async fn keepalive_pong_token_in_first_position_is_accepted() {
    // "PONG :token" — token in the `a` field.
    assert!(keepalive_survives("PONG :ircbot-keepalive\r\n").await);
}

#[tokio::test]
async fn keepalive_wrong_pong_token_triggers_timeout() {
    // A non-matching token does not reset the flag, so the bot times out and
    // the read loop exits (task finishes).
    assert!(!keepalive_survives("PONG irc.server :not-the-token\r\n").await);
}

// ─── B7: multiple matching handlers fire in order ────────────────────────────

#[tokio::test]
async fn all_matching_handlers_fire_in_registration_order() {
    let mut server = MockServer::start().await;

    fn say_handler(text: &'static str) -> HandlerFn<()> {
        Box::new(
            move |_bot: Arc<()>, ctx: Context| -> BoxFuture<ircbot::Result> {
                Box::pin(async move { ctx.say(text) })
            },
        )
    }

    let handlers = vec![
        HandlerEntry {
            trigger: Trigger::Message {
                pattern: "go".to_string(),
                target: None,
            },
            handler: say_handler("first"),
        },
        HandlerEntry {
            trigger: Trigger::Message {
                pattern: "go".to_string(),
                target: None,
            },
            handler: say_handler("second"),
        },
    ];

    let bot_task = spawn_bot(&server.addr, Arc::new(()), handlers).await;
    server.send_welcome();

    server.send(":alice!a@h PRIVMSG #chan :go\r\n");
    let first = server.expect_line(|l| l.starts_with("PRIVMSG")).await;
    let second = server.expect_line(|l| l.starts_with("PRIVMSG")).await;
    assert_eq!(first, "PRIVMSG #chan :first");
    assert_eq!(second, "PRIVMSG #chan :second");

    bot_task.abort();
}

// ─── B8: handler error isolation ─────────────────────────────────────────────

#[tokio::test]
async fn handler_error_does_not_prevent_later_handlers() {
    let mut server = MockServer::start().await;

    let erroring: HandlerFn<()> = Box::new(
        |_bot: Arc<()>, _ctx: Context| -> BoxFuture<ircbot::Result> {
            Box::pin(async move { Err("boom".into()) })
        },
    );
    let healthy: HandlerFn<()> =
        Box::new(|_bot: Arc<()>, ctx: Context| -> BoxFuture<ircbot::Result> {
            Box::pin(async move { ctx.say("still here") })
        });

    let handlers = vec![
        HandlerEntry {
            trigger: Trigger::Message {
                pattern: "go".to_string(),
                target: None,
            },
            handler: erroring,
        },
        HandlerEntry {
            trigger: Trigger::Message {
                pattern: "go".to_string(),
                target: None,
            },
            handler: healthy,
        },
    ];

    let bot_task = spawn_bot(&server.addr, Arc::new(()), handlers).await;
    server.send_welcome();

    server.send(":alice!a@h PRIVMSG #chan :go\r\n");
    let line = server.expect_line(|l| l.starts_with("PRIVMSG")).await;
    assert_eq!(line, "PRIVMSG #chan :still here");

    bot_task.abort();
}

// ─── B9: sender population ───────────────────────────────────────────────────

/// Capture whether `ctx.sender` was populated for the first dispatched message.
fn sender_capturing_handler(slot: Arc<Mutex<Option<Option<String>>>>) -> HandlerEntry<()> {
    HandlerEntry {
        trigger: Trigger::Event {
            event: "PRIVMSG".to_string(),
            target: None,
            regex: None,
        },
        handler: Box::new(
            move |_bot: Arc<()>, ctx: Context| -> BoxFuture<ircbot::Result> {
                let slot = Arc::clone(&slot);
                Box::pin(async move {
                    let mut guard = slot.lock().unwrap();
                    if guard.is_none() {
                        *guard = Some(ctx.sender.as_ref().map(|u| u.nick.to_string()));
                    }
                    Ok(())
                })
            },
        ),
    }
}

#[tokio::test]
async fn sender_populated_for_nick_user_host_prefix() {
    let server = MockServer::start().await;
    let slot: Arc<Mutex<Option<Option<String>>>> = Arc::new(Mutex::new(None));
    let bot_task = spawn_bot(
        &server.addr,
        Arc::new(()),
        vec![sender_capturing_handler(Arc::clone(&slot))],
    )
    .await;
    server.send_welcome();

    server.send(":alice!user@host PRIVMSG #chan :hi\r\n");
    wait_for_some(&slot).await;
    assert_eq!(
        slot.lock().unwrap().clone().unwrap(),
        Some("alice".to_string())
    );

    bot_task.abort();
}

#[tokio::test]
async fn sender_none_for_server_prefix() {
    let server = MockServer::start().await;
    let slot: Arc<Mutex<Option<Option<String>>>> = Arc::new(Mutex::new(None));
    let bot_task = spawn_bot(
        &server.addr,
        Arc::new(()),
        vec![sender_capturing_handler(Arc::clone(&slot))],
    )
    .await;
    server.send_welcome();

    // Server-name prefix → no nick!user@host → sender is None.
    server.send(":irc.server PRIVMSG #chan :hi\r\n");
    wait_for_some(&slot).await;
    assert_eq!(slot.lock().unwrap().clone().unwrap(), None);

    bot_task.abort();
}

/// Spin until the shared slot has been written by a handler.
async fn wait_for_some<V>(slot: &Arc<Mutex<Option<V>>>) {
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if slot.lock().unwrap().is_some() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("handler did not fire within 2 s");
}
