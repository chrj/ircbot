//! Functional integration tests that spin up a real ngircd IRC server in Docker,
//! connect the bot to it, and verify end-to-end behaviour from a test IRC client.
//!
//! Run with:
//!   cargo test --features integration -- --test-threads=1
#![cfg(feature = "integration")]

use std::time::Duration;

use futures::StreamExt;
use irc::client::prelude::{Client, Command, Config};
use testcontainers::{
    core::{ContainerPort, WaitFor},
    runners::AsyncRunner,
    CopyDataSource, GenericImage, ImageExt,
};
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

// ─── ngircd configuration ────────────────────────────────────────────────────

/// A minimal ngircd configuration used for all integration tests.
///
/// `PingTimeout` is set to 5 s (vs. the default 120 s) so that idle-connection
/// PINGs arrive quickly and any PING-handling bugs are caught without a long wait.
const NGIRCD_CONF: &[u8] = b"\
[Global]
ServerGID=irc
ServerUID=irc

[Limits]
PingTimeout = 5

[Options]
Ident=no
PAM=no

[SSL]
CAFile=/etc/ssl/certs/ca-certificates.crt
";

// ─── container helpers ───────────────────────────────────────────────────────

/// Start an ngircd container with a custom config and return `(container, host_port)`.
///
/// The container is automatically stopped and removed when `container` is
/// dropped (i.e. when the test ends).
async fn start_ngircd() -> (testcontainers::ContainerAsync<GenericImage>, u16) {
    let container = GenericImage::new("ghcr.io/ngircd/ngircd", "latest")
        .with_exposed_port(ContainerPort::Tcp(6667))
        // Wait until ngircd logs "ready." to stdout before proceeding.
        .with_wait_for(WaitFor::message_on_stdout("ready."))
        // Inject our custom config to override the default one shipped in the image.
        .with_copy_to(
            "/opt/ngircd/etc/ngircd.conf",
            CopyDataSource::Data(NGIRCD_CONF.to_vec()),
        )
        .start()
        .await
        .expect("failed to start ngircd container");

    let port = container
        .get_host_port_ipv4(ContainerPort::Tcp(6667))
        .await
        .expect("failed to get mapped host port for ngircd");

    (container, port)
}

// ─── test helpers ────────────────────────────────────────────────────────────

/// Build an `irc` crate [`Config`] for connecting to the server at `127.0.0.1:<port>`.
fn irc_config(port: u16, nick: &str, channels: Vec<String>) -> Config {
    Config {
        nickname: Some(nick.to_owned()),
        server: Some("127.0.0.1".to_owned()),
        port: Some(port),
        channels,
        ..Default::default()
    }
}

/// Read messages from `stream` until `predicate` returns `Some(T)` for a
/// message, or until `max_wait` elapses.  Panics on timeout or stream error.
async fn read_until<T>(
    stream: &mut irc::client::ClientStream,
    predicate: impl Fn(&irc::proto::Message) -> Option<T>,
    max_wait: Duration,
) -> T {
    timeout(max_wait, async {
        loop {
            let msg = stream
                .next()
                .await
                .expect("stream ended unexpectedly")
                .expect("stream error");
            if let Some(result) = predicate(&msg) {
                return result;
            }
        }
    })
    .await
    .expect("timed out waiting for expected IRC message")
}

/// Wait until `testbot` is known to be in `#test` on the given stream.
///
/// The server sends a `353 RPL_NAMREPLY` with all current members right after
/// our own `JOIN` is confirmed.  If `testbot` appears in that list, it was
/// already in the channel.  If not, we keep reading until we see its `JOIN`.
async fn wait_for_bot_in_channel(stream: &mut irc::client::ClientStream) {
    read_until(
        stream,
        |msg| {
            match &msg.command {
                // Bot joined after the test client.
                Command::JOIN(chan, _, _)
                    if chan == "#test" && msg.source_nickname() == Some("testbot") =>
                {
                    Some(())
                }
                // NAMES list sent right after our own JOIN: check if testbot is already there.
                Command::Response(irc::proto::Response::RPL_NAMREPLY, args) => {
                    let nicks = args.last().map(String::as_str).unwrap_or("");
                    if nicks
                        .split_whitespace()
                        .any(|n| n.trim_start_matches(['~', '&', '@', '%', '+']) == "testbot")
                    {
                        Some(())
                    } else {
                        None
                    }
                }
                _ => None,
            }
        },
        Duration::from_secs(10),
    )
    .await;
}

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

    // Connect the test client via the `irc` crate.
    let mut client = Client::from_config(irc_config(port, "client", vec!["#test".into()]))
        .await
        .expect("test client: failed to connect");
    client.identify().expect("test client: identify failed");
    let mut stream = client.stream().expect("test client: stream failed");

    // Wait until testbot is confirmed to be in #test.
    wait_for_bot_in_channel(&mut stream).await;

    // Send the command and wait for the bot's reply.
    client
        .send_privmsg("#test", "!ping")
        .expect("test client: send failed");

    let response = read_until(
        &mut stream,
        |msg| {
            if let Command::PRIVMSG(ref target, ref text) = msg.command {
                if target == "#test" && text.contains("pong!") {
                    return Some(text.clone());
                }
            }
            None
        },
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

    let mut client = Client::from_config(irc_config(port, "client", vec!["#test".into()]))
        .await
        .expect("test client: failed to connect");
    client.identify().expect("test client: identify failed");
    let mut stream = client.stream().expect("test client: stream failed");

    // Wait until testbot is confirmed to be in #test.
    wait_for_bot_in_channel(&mut stream).await;

    client
        .send_privmsg("#test", "!echo hello world")
        .expect("test client: send failed");

    let response = read_until(
        &mut stream,
        |msg| {
            if let Command::PRIVMSG(ref target, ref text) = msg.command {
                if target == "#test" && text.contains("hello world") {
                    return Some(text.clone());
                }
            }
            None
        },
        Duration::from_secs(10),
    )
    .await;

    assert!(
        response.contains("hello world"),
        "expected 'hello world' in bot response, got: {response}"
    );

    bot_task.abort();
}
