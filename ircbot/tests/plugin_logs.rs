//! The log of a plugin panic, in its own test binary.
//!
//! `tracing` caches whether a log statement is on for all threads. When
//! another test in the same binary runs the statement on another thread, with
//! no subscriber, the cache can turn it off for this test too. Thus this test
//! is alone in its binary.
//!
//! Run with:
//!   cargo test --test plugin_logs

use std::sync::{Arc, Mutex};
use std::time::Duration;

use ircbot::{bot, plugin, Context, Result};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;
use tokio::sync::mpsc;

// ─── mock server ─────────────────────────────────────────────────────────────

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
        self.to_bot.send(format!("{line}\r\n")).unwrap();
    }

    /// Send a channel message from `alice` to `#chan`.
    fn say(&self, text: &str) {
        self.send(&format!(":alice!a@h PRIVMSG #chan :{text}"));
    }

    /// The next `PRIVMSG` that the bot sends.
    async fn next_privmsg(&mut self) -> String {
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                let line = self.from_bot.recv().await.expect("bot closed");
                if line.starts_with("PRIVMSG") {
                    return line;
                }
            }
        })
        .await
        .expect("timed out waiting for a PRIVMSG")
    }
}

/// A bot whose handlers all come from plugins.
#[bot]
impl HostBot {}

/// Start `bot`, and finish the registration with `server`.
fn run(bot: HostBot, server: &MockServer) -> tokio::task::JoinHandle<()> {
    let task = tokio::spawn(async move {
        let _ = bot.main_loop().await;
    });
    server.send(":server 001 testbot :Welcome");
    task
}

/// A plugin that panics on `!boom`.
#[plugin(name = "panicky")]
impl Panicky {
    #[command("boom")]
    async fn boom(&self, _ctx: Context) -> Result {
        panic!("boom handler failed");
    }

    #[command("alive")]
    async fn alive(&self, ctx: Context) -> Result {
        ctx.say("still alive")
    }
}

// ─── logs ────────────────────────────────────────────────────────────────────

/// Keeps what the subscriber writes, so the test can read it.
#[derive(Clone, Default)]
struct Capture(Arc<Mutex<Vec<u8>>>);

impl Capture {
    fn contents(&self) -> String {
        String::from_utf8(self.0.lock().unwrap().clone()).unwrap()
    }
}

impl std::io::Write for Capture {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Capture {
    type Writer = Capture;

    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

#[tokio::test]
async fn a_panic_is_logged_with_the_plugin_name_and_message() {
    let capture = Capture::default();
    let subscriber = tracing_subscriber::fmt()
        .with_ansi(false)
        .with_writer(capture.clone())
        .finish();
    // `#[tokio::test]` uses a current-thread runtime, so the plugin tasks use
    // this subscriber too.
    let _guard = tracing::subscriber::set_default(subscriber);

    let mut server = MockServer::start().await;
    let bot = HostBot::new("testbot", server.addr.clone(), ["#chan"]).plugin(Panicky);
    let task = run(bot, &server);

    server.say("!boom");
    server.say("!alive");
    assert_eq!(server.next_privmsg().await, "PRIVMSG #chan :still alive");

    let logs = capture.contents();
    assert!(
        logs.contains("plugin handler panicked")
            && logs.contains("panicky")
            && logs.contains("boom handler failed"),
        "got {logs:?}"
    );

    task.abort();
}
