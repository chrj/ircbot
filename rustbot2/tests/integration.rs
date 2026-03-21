//! Functional integration tests that spin up a real ngircd IRC server in Docker,
//! connect the bot to it, and verify end-to-end behaviour from a test IRC client.
//!
//! Run with:
//!   cargo test --features integration -- --test-threads=1
#![cfg(feature = "integration")]

use std::time::Duration;

use testcontainers::{
    core::{ContainerPort, WaitFor},
    runners::AsyncRunner,
    GenericImage,
};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, BufWriter};
use tokio::net::TcpStream;
use tokio::time::timeout;

use rustbot2::{bot, Context, Result};

// ─── bot under test ──────────────────────────────────────────────────────────

/// A minimal bot used exclusively by the integration tests.
#[bot]
impl TestBot {
    #[command("ping")]
    async fn ping(&self, ctx: Context) -> Result {
        ctx.reply("pong!")
    }

    #[command("echo")]
    async fn echo(&self, ctx: Context, text: String) -> Result {
        ctx.say(text)
    }
}

// ─── constants ───────────────────────────────────────────────────────────────

/// How long to wait after the bot connects for it to finish joining the channel.
const BOT_JOIN_DELAY: Duration = Duration::from_millis(500);

// ─── test IRC client ─────────────────────────────────────────────────────────

/// A minimal synchronous-style IRC client used to drive the tests.
struct IrcClient {
    reader: BufReader<tokio::net::tcp::OwnedReadHalf>,
    writer: BufWriter<tokio::net::tcp::OwnedWriteHalf>,
}

impl IrcClient {
    /// Connect to `addr` and register with the given `nick`.
    async fn connect(addr: &str, nick: &str) -> Self {
        let stream = TcpStream::connect(addr)
            .await
            .expect("test client: failed to connect to IRC server");
        let (r, w) = stream.into_split();
        let mut client = IrcClient {
            reader: BufReader::new(r),
            writer: BufWriter::new(w),
        };
        client.send_raw(&format!("NICK {nick}")).await;
        client
            .send_raw(&format!("USER {nick} 0 * :Integration Test"))
            .await;
        client
    }

    /// Send a raw IRC line (a `\r\n` terminator is appended automatically).
    async fn send_raw(&mut self, line: &str) {
        self.writer
            .write_all(format!("{line}\r\n").as_bytes())
            .await
            .expect("test client: write failed");
        self.writer
            .flush()
            .await
            .expect("test client: flush failed");
    }

    /// Send a `JOIN` command for `channel`.
    async fn join(&mut self, channel: &str) {
        self.send_raw(&format!("JOIN {channel}")).await;
    }

    /// Send a `PRIVMSG` to `target` with body `text`.
    async fn privmsg(&mut self, target: &str, text: &str) {
        self.send_raw(&format!("PRIVMSG {target} :{text}")).await;
    }

    /// Read IRC lines until `predicate` returns `true` for a line, or until
    /// `max_wait` elapses.  Automatically replies to `PING` challenges.
    /// Returns the first matching line on success; panics on timeout or I/O error.
    async fn read_until(&mut self, predicate: impl Fn(&str) -> bool, max_wait: Duration) -> String {
        let deadline = tokio::time::Instant::now() + max_wait;
        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            assert!(
                !remaining.is_zero(),
                "test client: timed out waiting for expected IRC message"
            );

            let mut line = String::new();
            match timeout(remaining, self.reader.read_line(&mut line)).await {
                Ok(Ok(0)) => panic!("test client: connection closed unexpectedly"),
                Ok(Ok(_)) => {}
                Ok(Err(e)) => panic!("test client: I/O error reading from server: {e}"),
                Err(_) => panic!("test client: timed out waiting for expected IRC message"),
            }

            let trimmed = line.trim_end_matches(['\r', '\n']);

            // Respond to server PINGs so the connection stays alive.
            if let Some(rest) = trimmed.strip_prefix("PING ") {
                self.send_raw(&format!("PONG {rest}")).await;
                continue;
            }

            if predicate(trimmed) {
                return trimmed.to_string();
            }
        }
    }

    /// Block until the server's numeric `001` welcome message arrives.
    async fn wait_for_welcome(&mut self) {
        self.read_until(|l| l.contains(" 001 "), Duration::from_secs(15))
            .await;
    }

    /// Block until the server echoes our own `JOIN` back for `channel`.
    async fn wait_for_join(&mut self, channel: &str) {
        self.read_until(
            |l| {
                l.contains("JOIN")
                    && l.to_ascii_lowercase()
                        .contains(&channel.to_ascii_lowercase())
            },
            Duration::from_secs(10),
        )
        .await;
    }
}

// ─── container helpers ───────────────────────────────────────────────────────

/// Start an ngircd container and return `(container, host_port)`.
///
/// The container is automatically stopped and removed when `container` is
/// dropped (i.e. when the test ends).
async fn start_ngircd() -> (testcontainers::ContainerAsync<GenericImage>, u16) {
    let container = GenericImage::new("ghcr.io/ngircd/ngircd", "latest")
        .with_exposed_port(ContainerPort::Tcp(6667))
        // Wait until ngircd logs "ready." to stdout before proceeding.
        .with_wait_for(WaitFor::message_on_stdout("ready."))
        .start()
        .await
        .expect("failed to start ngircd container");

    let port = container
        .get_host_port_ipv4(ContainerPort::Tcp(6667))
        .await
        .expect("failed to get mapped host port for ngircd");

    (container, port)
}

// ─── tests ───────────────────────────────────────────────────────────────────

/// Verify that the bot responds to `!ping` with `pong!`.
#[tokio::test]
async fn test_ping_command() {
    let (_container, port) = start_ngircd().await;
    let addr = format!("127.0.0.1:{port}");

    // Start the bot and let it run in a background task.
    let bot = TestBot::new("testbot", &addr, ["#test"])
        .await
        .expect("bot failed to connect");
    let bot_task = tokio::spawn(bot.main_loop());

    // Connect the test client.
    let mut client = IrcClient::connect(&addr, "client").await;
    client.wait_for_welcome().await;
    client.join("#test").await;
    client.wait_for_join("#test").await;

    // Give the bot time to join the channel.
    tokio::time::sleep(BOT_JOIN_DELAY).await;

    // Send the command and wait for the bot's reply.
    client.privmsg("#test", "!ping").await;
    let response = client
        .read_until(
            |l| l.contains("PRIVMSG") && l.contains("pong!"),
            Duration::from_secs(10),
        )
        .await;

    assert!(
        response.contains("pong!"),
        "expected 'pong!' in bot response, got: {response}"
    );

    bot_task.abort();
}

/// Verify that the bot echoes the text argument back when given `!echo <text>`.
#[tokio::test]
async fn test_echo_command() {
    let (_container, port) = start_ngircd().await;
    let addr = format!("127.0.0.1:{port}");

    let bot = TestBot::new("testbot", &addr, ["#test"])
        .await
        .expect("bot failed to connect");
    let bot_task = tokio::spawn(bot.main_loop());

    let mut client = IrcClient::connect(&addr, "client").await;
    client.wait_for_welcome().await;
    client.join("#test").await;
    client.wait_for_join("#test").await;

    tokio::time::sleep(BOT_JOIN_DELAY).await;

    client.privmsg("#test", "!echo hello world").await;
    let response = client
        .read_until(
            |l| l.contains("PRIVMSG") && l.contains("hello world"),
            Duration::from_secs(10),
        )
        .await;

    assert!(
        response.contains("hello world"),
        "expected 'hello world' in bot response, got: {response}"
    );

    bot_task.abort();
}
