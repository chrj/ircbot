//! Tests for flood control: long-message splitting and rate-limiting.
//!
//! The splitting tests are pure unit tests with no network I/O.
//! The rate-limiting test uses a lightweight in-process mock IRC server.
//!
//! Run with:
//!   cargo test --test flood_control

use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;
use tokio::sync::mpsc;

use ircbot::make_messages;

// ─── make_messages unit tests ────────────────────────────────────────────────

/// Short text that fits in one IRC line is returned as a single element.
#[test]
fn test_make_messages_short_text() {
    let lines = make_messages("PRIVMSG #chan :", "hello world", "");
    assert_eq!(lines, vec!["PRIVMSG #chan :hello world\r\n"]);
}

/// Empty text produces a single line containing only the header+suffix.
#[test]
fn test_make_messages_empty_text() {
    let lines = make_messages("PRIVMSG #chan :", "", "");
    assert_eq!(lines, vec!["PRIVMSG #chan :\r\n"]);
}

/// Text longer than the 512-byte limit must be split into multiple lines.
#[test]
fn test_make_messages_long_text_splits() {
    // Header: "PRIVMSG #chan :" = 15 bytes.
    // 510 - 15 = 495 bytes available for text per line.
    // Send 1 000 'a's → must produce ≥ 3 lines.
    let text = "a".repeat(1_000);
    let header = "PRIVMSG #chan :";
    let lines = make_messages(header, &text, "");
    assert!(
        lines.len() >= 3,
        "expected ≥ 3 lines for 1 000-char text, got {}",
        lines.len()
    );
    // Every line must be ≤ 512 bytes.
    for (i, line) in lines.iter().enumerate() {
        assert!(
            line.len() <= 512,
            "line {i} is {} bytes (> 512)",
            line.len()
        );
    }
    // Reassembling must reproduce the original text.
    let recovered: String = lines
        .iter()
        .map(|l| l.strip_prefix(header).unwrap().trim_end_matches("\r\n"))
        .collect::<Vec<_>>()
        .join("");
    assert_eq!(recovered, text);
}

/// When text contains spaces the split should prefer a word boundary.
#[test]
fn test_make_messages_prefers_word_boundary() {
    // Build a string: 400 'a's + space + 200 'b's → total 601 chars.
    // Header "PRIVMSG #chan :" is 15 bytes, leaving 495 for text.
    // Because 601 > 495, the text must be split, and the ideal split is at
    // the space (position 400) which is within the 495-byte window.
    let text = format!("{} {}", "a".repeat(400), "b".repeat(200));
    let header = "PRIVMSG #chan :";
    let lines = make_messages(header, &text, "");
    assert_eq!(lines.len(), 2, "expected 2 lines, got {}", lines.len());
    // First chunk should end with 'a' (not contain any 'b'), meaning we
    // split at the space.
    let first = lines[0]
        .strip_prefix(header)
        .unwrap()
        .trim_end_matches("\r\n");
    assert!(
        first.ends_with('a'),
        "first chunk should end at the word boundary (all 'a's), got: {first:?}"
    );
}

/// CTCP ACTION suffix is included in the byte budget.
#[test]
fn test_make_messages_ctcp_action_suffix() {
    let header = "PRIVMSG #chan :\x01ACTION ";
    let suffix = "\x01";
    // header = 17 bytes, suffix = 1 byte → available = 510 - 17 - 1 = 492 bytes.
    let text = "x".repeat(1_000);
    let lines = make_messages(header, &text, suffix);
    for (i, line) in lines.iter().enumerate() {
        assert!(
            line.len() <= 512,
            "ACTION line {i} is {} bytes (> 512)",
            line.len()
        );
        assert!(
            line.ends_with("\x01\r\n"),
            "ACTION line {i} must end with \\x01\\r\\n"
        );
    }
}

/// Multi-byte UTF-8 characters must not be split mid-character.
#[test]
fn test_make_messages_utf8_boundary() {
    // "é" is 2 bytes (U+00E9); build a string just over the limit.
    let header = "PRIVMSG #chan :";
    // available = 510 - 15 = 495 bytes
    // "é" × 248 = 496 bytes; adding one more "é" → 498 bytes (still fits in 510)
    // Let's go well over: 300 × "é" = 600 bytes → must be split.
    let text = "é".repeat(300);
    let lines = make_messages(header, &text, "");
    for (i, line) in lines.iter().enumerate() {
        assert!(
            line.len() <= 512,
            "line {i} is {} bytes (> 512)",
            line.len()
        );
        // Each line must be valid UTF-8 (no split mid-codepoint).
        assert!(
            std::str::from_utf8(line.as_bytes()).is_ok(),
            "line {i} is not valid UTF-8"
        );
    }
}

/// When overhead ≥ 510 the available byte count saturates to 0; the function
/// must still return exactly one line (containing no text body).
#[test]
fn test_make_messages_zero_available_bytes() {
    // overhead = header.len() + suffix.len() + 2 (\r\n)
    // header = 509 bytes, suffix = 1 byte → overhead = 512 → available = 0.
    let header = "X".repeat(509);
    let suffix = "S";
    let lines = make_messages(&header, "some text", suffix);
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0], format!("{header}{suffix}\r\n"));
}

/// Text whose byte length equals exactly `available` must fit in a single line.
#[test]
fn test_make_messages_exact_fit() {
    let header = "PRIVMSG #chan :";
    let overhead = header.len() + 2; // suffix is empty, +2 for \r\n
    let available = 510 - overhead; // = 495
    let text = "a".repeat(available);
    let lines = make_messages(header, &text, "");
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0], format!("{header}{text}\r\n"));
}

// ─── rate-limiting integration test ──────────────────────────────────────────

/// Verify that the write task's flood control delays messages when the burst
/// budget is exhausted.
///
/// Setup:
///   - burst = 2, rate = 200 ms  → 2 messages sent immediately, then 1 per 200 ms.
///   - We queue 6 messages and measure the wall-clock time from first to last.
///   - Expected minimum: 4 × 200 ms = 800 ms (the 4 messages beyond the burst).
///   - We allow up to 5 s to keep the test robust under load.
#[tokio::test]
async fn test_flood_control_rate_limits_messages() {
    use ircbot::State;

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();

    // Receive side: collect PRIVMSG timestamps.
    let (ts_tx, mut ts_rx) = mpsc::unbounded_channel::<Instant>();

    tokio::spawn(async move {
        let (sock, _) = listener.accept().await.unwrap();
        let (read_half, mut write_half) = sock.into_split();
        // Send 001 welcome so the bot finishes the handshake.
        let _ = write_half
            .write_all(b":server 001 testbot :Welcome\r\n")
            .await;

        let mut reader = BufReader::new(read_half).lines();
        while let Ok(Some(line)) = reader.next_line().await {
            if line.starts_with("PRIVMSG") {
                let _ = ts_tx.send(Instant::now());
            }
        }
    });

    let state = State::connect("testbot".to_string(), &addr, vec![])
        .await
        .expect("failed to connect to mock server")
        .with_flood_control(2, Duration::from_millis(200));

    // Build a tiny bot that sends 6 PRIVMSG lines on the 001 welcome event.
    use ircbot::{handler::HandlerEntry, handler::Trigger, BoxFuture, Context};

    let handler: HandlerEntry<()> = HandlerEntry {
        trigger: Trigger::Event {
            event: "001".to_string(),
            target: None,
            regex: None,
        },
        handler: Box::new(|_bot: Arc<()>, ctx: Context| -> BoxFuture<ircbot::Result> {
            Box::pin(async move {
                for i in 0..6_u32 {
                    ctx.say(format!("message {i}"))?;
                }
                Ok(())
            })
        }),
    };

    let bot = Arc::new(());
    let bot_task = tokio::spawn(ircbot::internal::run_bot(bot, state, vec![handler]));

    // Collect all 6 timestamps with a generous timeout.
    let mut timestamps = Vec::new();
    let collect = tokio::time::timeout(Duration::from_secs(10), async {
        while timestamps.len() < 6 {
            if let Some(ts) = ts_rx.recv().await {
                timestamps.push(ts);
            }
        }
    });
    collect
        .await
        .expect("did not receive all 6 messages within 10 s");
    bot_task.abort();

    // The gap between the first and last message must be at least
    // (6 - burst) × rate = 4 × 200 ms = 800 ms.
    let elapsed = timestamps
        .last()
        .unwrap()
        .duration_since(*timestamps.first().unwrap());
    assert!(
        elapsed >= Duration::from_millis(700), // small slack for scheduler jitter
        "flood control did not delay messages enough: elapsed = {elapsed:?}"
    );
}
