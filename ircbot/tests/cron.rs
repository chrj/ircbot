//! Tests for the cron-like periodic handler.
//!
//! These tests use a lightweight in-process mock IRC server (a `TcpListener`
//! bound to a random loopback port) so they do **not** require Docker or the
//! `integration` feature flag.
//!
//! Run with:
//!   cargo test --test cron

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use tokio::io::AsyncWriteExt;
use tokio::net::TcpListener;

use ircbot::bot::run_bot_internal;
use ircbot::handler::{HandlerEntry, HandlerFn, Trigger};
use ircbot::{BoxFuture, Context, State};

// ─── helpers ─────────────────────────────────────────────────────────────────

/// Minimal IRC handshake: sends RPL_WELCOME so the bot considers itself
/// connected and begins processing.
const WELCOME: &[u8] = b":server 001 testbot :Welcome to the mock IRC server\r\n";

// ─── tests ───────────────────────────────────────────────────────────────────

/// A cron handler must fire at least twice within a reasonable time when a
/// short interval is configured.
#[tokio::test]
async fn test_cron_handler_fires_periodically() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();

    // Accept one connection and keep it open for the duration of the test.
    tokio::spawn(async move {
        let (mut sock, _) = listener.accept().await.unwrap();
        let _ = sock.write_all(WELCOME).await;
        // Hold the connection open so the bot's read loop doesn't exit.
        tokio::time::sleep(Duration::from_secs(60)).await;
    });

    let state = State::connect("testbot".to_string(), &addr, vec![])
        .await
        .expect("failed to connect to mock server");

    // Counter incremented each time the cron handler fires.
    let fire_count = Arc::new(AtomicUsize::new(0));
    let fire_count_handler = Arc::clone(&fire_count);

    let handler: HandlerFn<()> = Box::new(
        move |_bot: Arc<()>, _ctx: Context| -> BoxFuture<ircbot::Result> {
            let fc = Arc::clone(&fire_count_handler);
            Box::pin(async move {
                fc.fetch_add(1, Ordering::Relaxed);
                Ok(())
            })
        },
    );

    let handlers = vec![HandlerEntry {
        trigger: Trigger::Cron {
            interval: Duration::from_millis(50),
            target: None,
        },
        handler,
    }];

    let bot = Arc::new(());
    let handler_set = ircbot::internal::make_handler_set(handlers);
    let bot_task = tokio::spawn(run_bot_internal(bot, state, handler_set));

    // The handler fires every 50 ms; wait up to 2 s for at least 2 firings.
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if fire_count.load(Ordering::Relaxed) >= 2 {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("cron handler did not fire at least twice within 2 s");

    bot_task.abort();
}

/// A cron handler with `target = "#chan"` must build a context whose `target`
/// field is `"#chan"` and `is_channel` is `true`.
#[tokio::test]
async fn test_cron_handler_context_target() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();

    tokio::spawn(async move {
        let (mut sock, _) = listener.accept().await.unwrap();
        let _ = sock.write_all(WELCOME).await;
        tokio::time::sleep(Duration::from_secs(60)).await;
    });

    let state = State::connect("testbot".to_string(), &addr, vec![])
        .await
        .expect("failed to connect to mock server");

    // Capture the target and is_channel values seen by the handler.
    let seen_target: Arc<std::sync::Mutex<Option<(String, bool)>>> =
        Arc::new(std::sync::Mutex::new(None));
    let seen_clone = Arc::clone(&seen_target);

    let handler: HandlerFn<()> = Box::new(
        move |_bot: Arc<()>, ctx: Context| -> BoxFuture<ircbot::Result> {
            let sc = Arc::clone(&seen_clone);
            Box::pin(async move {
                let mut guard = sc.lock().unwrap();
                if guard.is_none() {
                    *guard = Some((ctx.target.clone(), ctx.is_channel));
                }
                Ok(())
            })
        },
    );

    let handlers = vec![HandlerEntry {
        trigger: Trigger::Cron {
            interval: Duration::from_millis(50),
            target: Some("#chan".to_string()),
        },
        handler,
    }];

    let bot = Arc::new(());
    let handler_set = ircbot::internal::make_handler_set(handlers);
    let bot_task = tokio::spawn(run_bot_internal(bot, state, handler_set));

    // Wait until the handler has fired at least once.
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if seen_target.lock().unwrap().is_some() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("cron handler did not fire within 2 s");

    let (target, is_channel) = seen_target.lock().unwrap().clone().unwrap();
    assert_eq!(target, "#chan");
    assert!(is_channel);

    bot_task.abort();
}
