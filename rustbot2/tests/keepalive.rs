//! Tests for the keepalive (self-ping) and automatic reconnection behaviour.
//!
//! These tests use a lightweight in-process mock IRC server (a `TcpListener`
//! bound to a random loopback port) so they do **not** require Docker or the
//! `integration` feature flag.
//!
//! Run with:
//!   cargo test --test keepalive

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;

use rustbot2::BotState;

// ─── helpers ─────────────────────────────────────────────────────────────────

/// Minimal IRC handshake that makes the bot believe it has successfully
/// connected: just a `001` RPL_WELCOME line.
const WELCOME: &[u8] = b":server 001 testbot :Welcome to the mock IRC server\r\n";

// ─── tests ───────────────────────────────────────────────────────────────────

/// The bot must reconnect automatically when the server closes the TCP
/// connection.
#[tokio::test]
async fn test_reconnects_after_tcp_disconnect() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();

    let conn_count = Arc::new(AtomicUsize::new(0));
    let cc = Arc::clone(&conn_count);

    tokio::spawn(async move {
        // First connection — send welcome, then close immediately.
        let (mut sock1, _) = listener.accept().await.unwrap();
        cc.fetch_add(1, Ordering::Relaxed);
        let _ = sock1.write_all(WELCOME).await;
        drop(sock1);

        // Second connection (the reconnect) — keep it alive long enough for
        // the assertion below to complete.
        let (mut sock2, _) = listener.accept().await.unwrap();
        cc.fetch_add(1, Ordering::Relaxed);
        let _ = sock2.write_all(WELCOME).await;
        tokio::time::sleep(Duration::from_secs(60)).await;
    });

    let state = BotState::connect("testbot".to_string(), &addr, vec![])
        .await
        .expect("failed to connect to mock server");

    let bot = Arc::new(());
    let bot_task = tokio::spawn(rustbot2::internal::run_bot(bot, state, vec![]));

    // The reconnect loop waits RECONNECT_DELAY (5 s) before dialling again;
    // allow up to 10 s total.
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if conn_count.load(Ordering::Relaxed) >= 2 {
                return;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("bot did not reconnect within 10 s after TCP disconnect");

    bot_task.abort();
}

/// When the server stops responding to keepalive `PING`s the bot must
/// disconnect and reconnect.  We use a very short keepalive interval/timeout
/// (1 s each) so the test completes quickly.
#[tokio::test]
async fn test_keepalive_timeout_triggers_reconnect() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();

    let conn_count = Arc::new(AtomicUsize::new(0));
    let cc = Arc::clone(&conn_count);
    let ping_seen = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let ps = Arc::clone(&ping_seen);

    tokio::spawn(async move {
        loop {
            let (sock, _) = listener.accept().await.unwrap();
            cc.fetch_add(1, Ordering::Relaxed);

            let (read_half, mut write_half) = sock.into_split();
            let _ = write_half.write_all(WELCOME).await;

            // Read lines the bot sends, note any keepalive PING but never
            // reply — this starves the bot of PONG responses.
            let mut reader = BufReader::new(read_half);
            let mut line = String::new();
            loop {
                line.clear();
                match tokio::time::timeout(Duration::from_secs(10), reader.read_line(&mut line))
                    .await
                {
                    Ok(Ok(0)) | Err(_) => break, // EOF or timed out
                    Ok(Ok(_)) => {
                        if line.contains("PING") && line.contains("rustbot2-keepalive") {
                            ps.store(true, std::sync::atomic::Ordering::Relaxed);
                        }
                        // Intentionally do NOT send a PONG.
                    }
                    Ok(Err(_)) => break,
                }
            }
        }
    });

    let state = BotState::connect("testbot".to_string(), &addr, vec![])
        .await
        .expect("failed to connect to mock server")
        // Short intervals so the test finishes in a few seconds.
        .with_keepalive(Duration::from_secs(1), Duration::from_secs(1));

    let bot = Arc::new(());
    let bot_task = tokio::spawn(rustbot2::internal::run_bot(bot, state, vec![]));

    // 1 s interval + 1 s timeout + 5 s reconnect delay = ~7 s; allow 15 s.
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            if conn_count.load(Ordering::Relaxed) >= 2 {
                return;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("bot did not reconnect within 15 s after keepalive timeout");

    assert!(
        ping_seen.load(std::sync::atomic::Ordering::Relaxed),
        "bot never sent PING rustbot2-keepalive"
    );

    bot_task.abort();
}
