//! Tests for runtime handler hot-swapping via [`ircbot::ReloadHandle`].
//!
//! Uses the same lightweight in-process mock IRC server pattern as the other
//! non-integration tests, so Docker is not required.
//!
//! Run with:
//!   cargo test --test reload

use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;
use tokio::sync::mpsc;

use ircbot::bot::run_bot_internal;
use ircbot::handler::{HandlerEntry, HandlerFn, Trigger};
use ircbot::{BoxFuture, Context, ReloadHandle, State};

// ─── minimal mock server ──────────────────────────────────────────────────────

struct MockServer {
    addr: String,
    to_bot: mpsc::UnboundedSender<String>,
    from_bot: mpsc::UnboundedReceiver<String>,
}

impl MockServer {
    async fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        let (to_bot_tx, mut to_bot_rx) = mpsc::unbounded_channel::<String>();
        let (from_bot_tx, from_bot_rx) = mpsc::unbounded_channel::<String>();

        tokio::spawn(async move {
            let (sock, _) = listener.accept().await.unwrap();
            let (read_half, mut write_half) = sock.into_split();
            tokio::spawn(async move {
                while let Some(line) = to_bot_rx.recv().await {
                    if write_half.write_all(line.as_bytes()).await.is_err() {
                        break;
                    }
                    let _ = write_half.flush().await;
                }
            });
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

    fn send(&self, line: &str) {
        self.to_bot.send(line.to_string()).unwrap();
    }

    fn send_welcome(&self) {
        self.send(":server 001 testbot :Welcome\r\n");
    }

    async fn expect_line(&mut self, predicate: impl Fn(&str) -> bool) -> String {
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let line = self.from_bot.recv().await.expect("bot closed");
                if predicate(&line) {
                    return line;
                }
            }
        })
        .await
        .expect("timed out waiting for line")
    }

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
            panic!("unexpected line: {line:?}");
        }
    }
}

fn say_handler(pattern: &str, text: &'static str) -> HandlerEntry<()> {
    HandlerEntry {
        trigger: Trigger::Message {
            pattern: pattern.to_string(),
            target: None,
        },
        handler: Box::new(
            move |_bot: Arc<()>, ctx: Context| -> BoxFuture<ircbot::Result> {
                Box::pin(async move { ctx.say(text) })
            },
        ) as HandlerFn<()>,
    }
}

/// A cron handler firing every second that says `text` to `target`.
fn cron_say(target: &str, text: &'static str) -> HandlerEntry<()> {
    HandlerEntry {
        trigger: Trigger::Cron {
            schedule: "* * * * * *".to_string(),
            tz: "UTC".to_string(),
            target: Some(target.to_string()),
        },
        handler: Box::new(
            move |_bot: Arc<()>, ctx: Context| -> BoxFuture<ircbot::Result> {
                Box::pin(async move { ctx.say(text) })
            },
        ) as HandlerFn<()>,
    }
}

// ─── C10: reload swaps the live handler set ──────────────────────────────────

#[tokio::test]
async fn reload_swaps_live_handler_set() {
    let mut server = MockServer::start().await;

    // Initial handler only matches "nope" — "go" is ignored.
    let set = ircbot::internal::make_handler_set(vec![say_handler("nope", "old")]);
    let handle = ReloadHandle::new(Arc::clone(&set));

    let state = State::connect("testbot".to_string(), &server.addr, vec![])
        .await
        .expect("connect failed");
    let bot_task = tokio::spawn(run_bot_internal(Arc::new(()), state, Arc::clone(&set)));
    server.send_welcome();

    // Before reload: "go" produces no reply.
    server.send(":alice!a@h PRIVMSG #chan :go\r\n");
    server
        .expect_no_line(Duration::from_millis(400), |l| l.starts_with("PRIVMSG"))
        .await;

    // Hot-swap in a handler that matches "go".
    handle.reload(vec![say_handler("go", "reloaded")]);

    // After reload: "go" is now answered.
    server.send(":alice!a@h PRIVMSG #chan :go\r\n");
    let line = server.expect_line(|l| l.starts_with("PRIVMSG")).await;
    assert_eq!(line, "PRIVMSG #chan :reloaded");

    bot_task.abort();
}

// ─── C10b: reload is honoured by the cron supervisor ─────────────────────────

#[tokio::test]
async fn reload_swaps_live_cron_handler_body() {
    let mut server = MockServer::start().await;

    // Initial per-second cron says "old".
    let set = ircbot::internal::make_handler_set(vec![cron_say("#chan", "old")]);
    let handle = ReloadHandle::new(Arc::clone(&set));

    let state = State::connect("testbot".to_string(), &server.addr, vec![])
        .await
        .expect("connect failed");
    let bot_task = tokio::spawn(run_bot_internal(Arc::new(()), state, Arc::clone(&set)));
    server.send_welcome();

    // The original cron body fires.
    server.expect_line(|l| l == "PRIVMSG #chan :old").await;

    // Hot-swap the cron handler's body without reconnecting.
    handle.reload(vec![cron_say("#chan", "new")]);

    // The *reloaded* body fires on a subsequent tick (skipping any in-flight
    // "old" line that may already have been queued).
    let line = server.expect_line(|l| l == "PRIVMSG #chan :new").await;
    assert_eq!(line, "PRIVMSG #chan :new");

    bot_task.abort();
}

#[tokio::test]
async fn reload_adds_cron_handler_while_running() {
    let mut server = MockServer::start().await;

    // Start with a single per-second cron so the supervisor is already active
    // (and thus re-scans every tick).
    let set = ircbot::internal::make_handler_set(vec![cron_say("#chan", "tick")]);
    let handle = ReloadHandle::new(Arc::clone(&set));

    let state = State::connect("testbot".to_string(), &server.addr, vec![])
        .await
        .expect("connect failed");
    let bot_task = tokio::spawn(run_bot_internal(Arc::new(()), state, Arc::clone(&set)));
    server.send_welcome();

    server.expect_line(|l| l == "PRIVMSG #chan :tick").await;

    // Add a second cron handler targeting a different channel.
    handle.reload(vec![cron_say("#chan", "tick"), cron_say("#other", "added")]);

    // The newly added handler fires without a reconnect.
    let line = server.expect_line(|l| l == "PRIVMSG #other :added").await;
    assert_eq!(line, "PRIVMSG #other :added");

    bot_task.abort();
}

// ─── C11: Clone shares the same underlying set ───────────────────────────────

#[tokio::test]
async fn clone_shares_underlying_handler_set() {
    let set = ircbot::internal::make_handler_set::<()>(vec![]);
    assert_eq!(set.read().unwrap().len(), 0);

    let handle = ReloadHandle::new(Arc::clone(&set));
    let cloned = handle.clone();

    // Reloading through the clone must be visible through the original set.
    cloned.reload(vec![say_handler("x", "y")]);
    assert_eq!(set.read().unwrap().len(), 1);
}
