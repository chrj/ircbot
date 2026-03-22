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

// ─── hot-reload helper ───────────────────────────────────────────────────────

/// Read one CRLF-terminated IRC line byte-by-byte from a blocking `TcpStream`.
///
/// Reading one byte at a time avoids introducing a user-space buffer that
/// could swallow server messages meant for the inherited connection in
/// [`test_hot_reload_inherit`].
#[cfg(unix)]
fn irc_read_line(stream: &mut std::net::TcpStream) -> String {
    use std::io::Read;
    let mut line = Vec::new();
    let mut byte = [0u8; 1];
    loop {
        stream
            .read_exact(&mut byte)
            .expect("irc_read_line: read_exact failed");
        line.push(byte[0]);
        if line.ends_with(b"\r\n") {
            break;
        }
    }
    String::from_utf8_lossy(&line)
        .trim_end_matches(['\r', '\n'])
        .to_owned()
}

// ─── hot-reload integration test ─────────────────────────────────────────────

/// Verify that a bot reconstructed from environment variables — the path taken
/// by `State::try_inherit_from_env` after `exec_reload` — can continue to
/// respond to commands on the same IRC connection without any disconnection.
///
/// The test simulates the "old bot" phase by opening a raw blocking TCP
/// connection to ngircd and performing the IRC handshake manually.  It then
/// transfers the fd via the env vars that `try_inherit_from_env` reads and
/// constructs a new `TestBot` — which, on Unix, calls `try_inherit_from_env`
/// automatically inside `new()`, skipping the TCP dial and reusing the live
/// socket.
///
/// This test is Unix-only because the hot-reload mechanism (fd inheritance
/// across `exec`) is a Unix-specific feature.
#[tokio::test]
#[cfg(unix)]
async fn test_hot_reload_inherit() {
    use std::io::Write;
    use std::os::unix::io::IntoRawFd;

    use rustbot2::hot_reload::{
        ENV_CHANNELS, ENV_FD, ENV_KA_INTERVAL, ENV_KA_TIMEOUT, ENV_NICK, ENV_SERVER,
    };

    let (_container, port) = start_ngircd().await;
    let addr = format!("127.0.0.1:{port}");

    // ── Phase 1: "Old bot" — raw blocking IRC session ─────────────────────
    //
    // We use a plain std::net::TcpStream (not tokio) so that we can transfer
    // the raw fd cleanly to try_inherit_from_env without tokio fd-registration
    // complications.

    let mut raw_stream = std::net::TcpStream::connect(&addr).expect("failed to connect to ngircd");
    raw_stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .expect("set_read_timeout failed");

    // Send NICK / USER to begin the IRC handshake.
    raw_stream
        .write_all(b"NICK testbot\r\nUSER testbot 0 * :testbot\r\n")
        .expect("NICK/USER write failed");

    // Read until RPL_WELCOME (001), responding to any server PINGs.
    loop {
        let line = irc_read_line(&mut raw_stream);
        if line.starts_with("PING ") {
            let token = line.strip_prefix("PING ").unwrap_or("");
            raw_stream
                .write_all(format!("PONG {token}\r\n").as_bytes())
                .expect("PONG write failed");
        }
        if line.contains(" 001 ") {
            break;
        }
    }

    // Join #test and wait for the server to echo the JOIN back.
    raw_stream
        .write_all(b"JOIN #test\r\n")
        .expect("JOIN write failed");

    loop {
        let line = irc_read_line(&mut raw_stream);
        if line.starts_with("PING ") {
            let token = line.strip_prefix("PING ").unwrap_or("");
            raw_stream
                .write_all(format!("PONG {token}\r\n").as_bytes())
                .expect("PONG write failed");
        }
        // ngircd echoes: ":testbot!user@host JOIN :#test"
        if line.contains("JOIN") && line.contains("#test") {
            break;
        }
    }

    // ── Phase 2: Transfer the fd (simulating exec_reload) ────────────────
    //
    // `into_raw_fd` consumes the TcpStream and returns the raw fd without
    // closing it — exactly what exec_reload does before calling exec().
    // Any server data that arrives between now and phase 3 accumulates in the
    // kernel TCP receive buffer and will be read by the new tokio BufReader.

    let raw_fd = raw_stream.into_raw_fd();

    let ka_interval_ms = rustbot2::DEFAULT_KEEPALIVE_INTERVAL.as_millis() as u64;
    let ka_timeout_ms = rustbot2::DEFAULT_KEEPALIVE_TIMEOUT.as_millis() as u64;

    // Populate the same env vars that exec_reload would have written.
    std::env::set_var(ENV_FD, raw_fd.to_string());
    std::env::set_var(ENV_NICK, "testbot");
    std::env::set_var(ENV_SERVER, &addr);
    std::env::set_var(ENV_CHANNELS, "#test");
    std::env::set_var(ENV_KA_INTERVAL, ka_interval_ms.to_string());
    std::env::set_var(ENV_KA_TIMEOUT, ka_timeout_ms.to_string());

    // ── Phase 3: "New bot" — pick up the inherited connection ─────────────
    //
    // TestBot::new() calls State::try_inherit_from_env() on Unix.  Because
    // ENV_FD is set it reconstructs a State from the raw fd instead of
    // dialling a new TCP connection.  The env vars are erased inside
    // try_inherit_from_env once consumed.

    let bot = TestBot::new("testbot", &addr, ["#test"])
        .await
        .expect("hot-reload bot failed to start");
    let bot_task = tokio::spawn(bot.main_loop());

    // ── Phase 4: Verify the inherited connection still works ──────────────

    let mut client = Client::from_config(irc_config(port, "client", vec!["#test".into()]))
        .await
        .expect("test client: failed to connect");
    client.identify().expect("test client: identify failed");
    let mut stream = client.stream().expect("test client: stream failed");

    // testbot was already in #test before the reload; it should appear in the
    // NAMES reply (RPL_NAMREPLY) rather than arriving via a fresh JOIN.
    wait_for_bot_in_channel(&mut stream).await;

    // A successful !ping → pong! exchange confirms the inherited connection
    // is live and the bot is processing messages correctly.
    client
        .send_privmsg("#test", "!ping")
        .expect("test client: send_privmsg failed");

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
        "expected 'pong!' in bot response after hot reload, got: {response}"
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
