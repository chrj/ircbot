//! The registration handshake: `PASS`, IRCv3 capability negotiation, and SASL.
//!
//! Every test drives the real [`State::connect`] against a scripted server on
//! loopback, so what is asserted is the bytes that go on the wire.

use std::time::Duration;

use ircbot::{Server, State};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;
use tokio::sync::oneshot;

/// How the fake server answers: when a client line starts with `.0`, it sends
/// back `.1`. The first matching rule wins, so put the more specific one first.
type Script = Vec<(&'static str, Vec<&'static str>)>;

/// How long the fake server waits for another client line before deciding the
/// exchange is over. Only reached once the client is done talking, so it costs
/// this much per test and nothing more.
const IDLE: Duration = Duration::from_millis(300);

/// Start a scripted IRC server on loopback.
///
/// Returns its address and a receiver yielding every line the client sent, once
/// the client has gone quiet or disconnected.
async fn scripted_server(script: Script) -> (String, oneshot::Receiver<Vec<String>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind failed");
    let addr = listener
        .local_addr()
        .expect("local_addr failed")
        .to_string();
    let (tx, rx) = oneshot::channel();

    tokio::spawn(async move {
        let (sock, _) = listener.accept().await.expect("accept failed");
        let (read, mut write) = sock.into_split();
        let mut lines = BufReader::new(read).lines();
        let mut received: Vec<String> = Vec::new();

        loop {
            let line = match tokio::time::timeout(IDLE, lines.next_line()).await {
                Ok(Ok(Some(line))) => line,
                // Idle, disconnected, or errored — the exchange is over.
                _ => break,
            };
            if let Some((_, replies)) = script.iter().find(|(p, _)| line.starts_with(p)) {
                for reply in replies {
                    write
                        .write_all(format!("{reply}\r\n").as_bytes())
                        .await
                        .expect("write failed");
                }
            }
            received.push(line);
        }

        let _ = tx.send(received);
        // Hold the socket open so a still-connected client never sees an EOF
        // it did not ask for. Longer than the registration timeout, so the
        // timeout test observes a silent server rather than a closed one.
        tokio::time::sleep(Duration::from_secs(120)).await;
    });

    (addr, rx)
}

/// The rules for a server that offers SASL and accepts whatever is sent.
///
/// `advertisement` is the `CAP LS` line it answers with, which is what decides
/// the mechanisms on offer.
fn accepting_sasl_script(advertisement: &'static str) -> Script {
    vec![
        ("CAP LS", vec![advertisement]),
        ("CAP REQ", vec![":srv CAP * ACK :sasl"]),
        ("AUTHENTICATE PLAIN", vec!["AUTHENTICATE +"]),
        ("AUTHENTICATE EXTERNAL", vec!["AUTHENTICATE +"]),
        (
            "AUTHENTICATE",
            vec![
                ":srv 900 bot bot!bot@host bot :You are now logged in as bot",
                ":srv 903 bot :SASL authentication successful",
            ],
        ),
    ]
}

/// Connect expecting failure, and return the error message.
///
/// `State` is not `Debug`, so `unwrap_err` is unavailable.
async fn connect_error(server: Server) -> String {
    match State::connect("bot", server, vec![]).await {
        Ok(_) => panic!("expected the connection to be rejected"),
        Err(e) => e.to_string(),
    }
}

// ── no credentials ───────────────────────────────────────────────────────────

/// Given a server with no credentials configured, when the bot connects, then
/// the handshake is the bare `NICK`/`USER` pair it has always been — no `CAP`
/// line is sent to a network that may not understand one.
#[tokio::test]
async fn without_credentials_the_handshake_skips_cap_entirely() {
    let (addr, rx) = scripted_server(vec![]).await;

    let _state = State::connect("bot", Server::plain(&addr), vec![])
        .await
        .expect("connect failed");

    assert_eq!(
        rx.await.expect("server reported nothing"),
        vec!["NICK bot", "USER bot 0 * :bot"]
    );
}

// ── server password ──────────────────────────────────────────────────────────

/// `PASS` must precede `NICK`: a server reads it as part of registration and
/// rejects it afterwards.
#[tokio::test]
async fn a_server_password_is_sent_before_the_nick() {
    let (addr, rx) = scripted_server(vec![]).await;

    let _state = State::connect("bot", Server::plain(&addr).with_password("s3cret"), vec![])
        .await
        .expect("connect failed");

    assert_eq!(
        rx.await.expect("server reported nothing"),
        vec!["PASS :s3cret", "NICK bot", "USER bot 0 * :bot"]
    );
}

/// A password carrying a line terminator must not be able to inject a second
/// command into the stream.
#[tokio::test]
async fn a_server_password_cannot_inject_a_second_command() {
    let (addr, rx) = scripted_server(vec![]).await;

    let _state = State::connect(
        "bot",
        Server::plain(&addr).with_password("s3cret\r\nJOIN #evil"),
        vec![],
    )
    .await
    .expect("connect failed");

    assert_eq!(
        rx.await.expect("server reported nothing"),
        vec!["PASS :s3cretJOIN #evil", "NICK bot", "USER bot 0 * :bot"]
    );
}

// ── SASL PLAIN ───────────────────────────────────────────────────────────────

/// The full happy path. `AGJvdABodW50ZXIy` is base64 of `\0bot\0hunter2`, the
/// `authzid \0 authcid \0 passwd` form RFC 4616 specifies.
#[tokio::test]
async fn sasl_plain_authenticates_and_ends_the_capability_exchange() {
    let (addr, rx) = scripted_server(accepting_sasl_script(
        ":srv CAP * LS :sasl=PLAIN,EXTERNAL multi-prefix",
    ))
    .await;

    let _state = State::connect(
        "bot",
        Server::plain(&addr).with_sasl_plain("bot", "hunter2"),
        vec![],
    )
    .await
    .expect("connect failed");

    assert_eq!(
        rx.await.expect("server reported nothing"),
        vec![
            "CAP LS 302",
            "NICK bot",
            "USER bot 0 * :bot",
            "CAP REQ :sasl",
            "AUTHENTICATE PLAIN",
            "AUTHENTICATE AGJvdABodW50ZXIy",
            "CAP END",
        ]
    );
}

/// A `CAP LS` batch split across lines is one advertisement, not two: the `sasl`
/// arriving in the second half must still be seen.
#[tokio::test]
async fn a_multi_line_capability_advertisement_is_reassembled() {
    let mut script = accepting_sasl_script(":srv CAP * LS :sasl=PLAIN multi-prefix");
    script[0] = (
        "CAP LS",
        vec![
            ":srv CAP * LS * :multi-prefix away-notify",
            ":srv CAP * LS :sasl=PLAIN",
        ],
    );
    let (addr, rx) = scripted_server(script).await;

    let _state = State::connect(
        "bot",
        Server::plain(&addr).with_sasl_plain("bot", "hunter2"),
        vec![],
    )
    .await
    .expect("connect failed");

    let sent = rx.await.expect("server reported nothing");
    assert!(
        sent.contains(&"CAP REQ :sasl".to_string()),
        "sasl was not requested: {sent:?}"
    );
}

// ── SASL EXTERNAL ────────────────────────────────────────────────────────────

/// `EXTERNAL` proves identity with the TLS client certificate, so the response
/// is empty — which the protocol spells `+`. No credential may appear.
#[tokio::test]
async fn sasl_external_sends_an_empty_response() {
    let (addr, rx) = scripted_server(accepting_sasl_script(
        ":srv CAP * LS :sasl=PLAIN,EXTERNAL multi-prefix",
    ))
    .await;

    let _state = State::connect("bot", Server::plain(&addr).with_sasl_external(), vec![])
        .await
        .expect("connect failed");

    assert_eq!(
        rx.await.expect("server reported nothing"),
        vec![
            "CAP LS 302",
            "NICK bot",
            "USER bot 0 * :bot",
            "CAP REQ :sasl",
            "AUTHENTICATE EXTERNAL",
            "AUTHENTICATE +",
            "CAP END",
        ]
    );
}

// ── extra capabilities ───────────────────────────────────────────────────────

/// Capabilities the server never advertised must be dropped from the request:
/// asking for one is how a server is entitled to `NAK` the whole batch.
#[tokio::test]
async fn unadvertised_capabilities_are_not_requested() {
    let (addr, rx) = scripted_server(vec![
        ("CAP LS", vec![":srv CAP * LS :server-time multi-prefix"]),
        ("CAP REQ", vec![":srv CAP * ACK :server-time"]),
    ])
    .await;

    let _state = State::connect(
        "bot",
        Server::plain(&addr).with_capabilities(["server-time", "away-notify"]),
        vec![],
    )
    .await
    .expect("connect failed");

    assert_eq!(
        rx.await.expect("server reported nothing"),
        vec![
            "CAP LS 302",
            "NICK bot",
            "USER bot 0 * :bot",
            "CAP REQ :server-time",
            "CAP END",
        ]
    );
}

/// When nothing we asked for exists, the exchange still has to be closed —
/// otherwise the server holds registration open until it times us out.
#[tokio::test]
async fn a_capability_exchange_with_nothing_to_request_still_ends() {
    let (addr, rx) = scripted_server(vec![("CAP LS", vec![":srv CAP * LS :multi-prefix"])]).await;

    let _state = State::connect(
        "bot",
        Server::plain(&addr).with_capabilities(["server-time"]),
        vec![],
    )
    .await
    .expect("connect failed");

    assert_eq!(
        rx.await.expect("server reported nothing"),
        vec!["CAP LS 302", "NICK bot", "USER bot 0 * :bot", "CAP END"]
    );
}

// ── failure modes ────────────────────────────────────────────────────────────

/// Connecting unauthenticated when authentication was asked for is the failure
/// SASL exists to prevent, so it must be an error rather than a warning.
#[tokio::test]
async fn a_server_without_sasl_fails_the_connection() {
    let (addr, _rx) = scripted_server(vec![("CAP LS", vec![":srv CAP * LS :multi-prefix"])]).await;

    let err = connect_error(Server::plain(&addr).with_sasl_plain("bot", "hunter2")).await;

    assert!(
        err.contains("does not offer the sasl capability"),
        "unhelpful error: {err}"
    );
}

#[tokio::test]
async fn a_server_lacking_our_mechanism_fails_the_connection() {
    let (addr, _rx) = scripted_server(vec![(
        "CAP LS",
        vec![":srv CAP * LS :sasl=EXTERNAL multi-prefix"],
    )])
    .await;

    let err = connect_error(Server::plain(&addr).with_sasl_plain("bot", "hunter2")).await;

    assert!(
        err.contains("does not support SASL PLAIN"),
        "unhelpful error: {err}"
    );
    assert!(
        err.contains("EXTERNAL"),
        "error omits what is on offer: {err}"
    );
}

#[tokio::test]
async fn rejected_credentials_fail_the_connection() {
    let (addr, _rx) = scripted_server(vec![
        ("CAP LS", vec![":srv CAP * LS :sasl=PLAIN"]),
        ("CAP REQ", vec![":srv CAP * ACK :sasl"]),
        ("AUTHENTICATE PLAIN", vec!["AUTHENTICATE +"]),
        // Answers the credentials themselves, not the mechanism line above.
        (
            "AUTHENTICATE",
            vec![":srv 904 bot :SASL authentication failed"],
        ),
    ])
    .await;

    let err = connect_error(Server::plain(&addr).with_sasl_plain("bot", "wrong")).await;

    assert!(
        err.contains("SASL authentication failed"),
        "unhelpful error: {err}"
    );
    assert!(
        err.contains("with_sasl_plain"),
        "error does not say what to fix: {err}"
    );
}

#[tokio::test]
async fn a_refused_sasl_capability_fails_the_connection() {
    let (addr, _rx) = scripted_server(vec![
        ("CAP LS", vec![":srv CAP * LS :sasl=PLAIN"]),
        ("CAP REQ", vec![":srv CAP * NAK :sasl"]),
    ])
    .await;

    let err = connect_error(Server::plain(&addr).with_sasl_plain("bot", "hunter2")).await;

    assert!(err.contains("refused the sasl capability"), "{err}");
}

/// A pre-IRCv3 server answers `CAP` with `ERR_UNKNOWNCOMMAND`. With no SASL
/// configured that is survivable, and the bot should register anyway.
#[tokio::test]
async fn a_server_without_cap_support_still_registers() {
    let (addr, rx) =
        scripted_server(vec![("CAP LS", vec![":srv 421 bot CAP :Unknown command"])]).await;

    let _state = State::connect(
        "bot",
        Server::plain(&addr).with_capabilities(["server-time"]),
        vec![],
    )
    .await
    .expect("connect failed");

    assert_eq!(
        rx.await.expect("server reported nothing"),
        vec!["CAP LS 302", "NICK bot", "USER bot 0 * :bot"]
    );
}

/// The same server, but with SASL configured, cannot authenticate at all.
#[tokio::test]
async fn a_server_without_cap_support_fails_a_sasl_connection() {
    let (addr, _rx) =
        scripted_server(vec![("CAP LS", vec![":srv 421 bot CAP :Unknown command"])]).await;

    let err = connect_error(Server::plain(&addr).with_sasl_plain("bot", "hunter2")).await;

    assert!(err.contains("does not implement CAP"), "{err}");
}

/// A server that goes silent mid-exchange must not hang the bot forever: the
/// connection has to fail so the caller can reconnect.
#[tokio::test(start_paused = true)]
async fn a_silent_server_times_the_exchange_out() {
    // No rules: the server accepts the connection and then says nothing.
    let (addr, _rx) = scripted_server(vec![]).await;

    let err = connect_error(Server::plain(&addr).with_sasl_plain("bot", "hunter2")).await;

    assert!(err.contains("stopped responding"), "unhelpful error: {err}");
}

// ── the PING that arrives mid-handshake ──────────────────────────────────────

/// Some servers ping during registration; an unanswered ping gets the bot
/// disconnected before it ever finishes.
#[tokio::test]
async fn a_ping_during_negotiation_is_answered() {
    let mut script = accepting_sasl_script(":srv CAP * LS :sasl=PLAIN multi-prefix");
    script[0] = (
        "CAP LS",
        vec!["PING :handshake", ":srv CAP * LS :sasl=PLAIN"],
    );
    let (addr, rx) = scripted_server(script).await;

    let _state = State::connect(
        "bot",
        Server::plain(&addr).with_sasl_plain("bot", "hunter2"),
        vec![],
    )
    .await
    .expect("connect failed");

    let sent = rx.await.expect("server reported nothing");
    assert!(
        sent.contains(&"PONG :handshake".to_string()),
        "the ping went unanswered: {sent:?}"
    );
}

// ── lines that arrive during the exchange ────────────────────────────────────

/// The server answers `NICK` as soon as it reads it, which is in the middle of
/// the capability exchange. That `ERR_NICKNAMEINUSE` is not part of the
/// exchange, so it must reach the read loop rather than being swallowed —
/// otherwise a bot whose nick is taken keeps the fallback for ever.
#[tokio::test]
async fn a_nick_collision_during_negotiation_still_reaches_the_read_loop() {
    let (addr, rx) = scripted_server(vec![
        (
            "CAP LS",
            vec![
                ":srv CAP * LS :sasl=PLAIN",
                ":srv 433 * bot :Nickname is already in use",
            ],
        ),
        ("CAP REQ", vec![":srv CAP * ACK :sasl"]),
        ("AUTHENTICATE PLAIN", vec!["AUTHENTICATE +"]),
        (
            "AUTHENTICATE",
            vec![":srv 903 bot :SASL authentication successful"],
        ),
    ])
    .await;

    let state = State::connect(
        "bot",
        Server::plain(&addr).with_sasl_plain("bot", "hunter2"),
        vec![],
    )
    .await
    .expect("connect failed");

    let _bot = tokio::spawn(ircbot::internal::run_bot(
        std::sync::Arc::new(()),
        state,
        vec![],
    ));

    let sent = rx.await.expect("server reported nothing");
    assert!(
        sent.contains(&"NICK bot_".to_string()),
        "the nick collision was dropped: {sent:?}"
    );
}

/// A success numeric that arrives before the bot has sent anything to succeed
/// at proves nothing, so the exchange must carry on rather than treat the
/// connection as authenticated.
#[tokio::test]
async fn an_unprompted_success_numeric_does_not_end_the_exchange() {
    let (addr, rx) = scripted_server(vec![
        (
            "CAP LS",
            vec![
                ":srv CAP * LS :sasl=PLAIN",
                ":srv 903 bot :SASL authentication successful",
            ],
        ),
        ("CAP REQ", vec![":srv CAP * ACK :sasl"]),
        ("AUTHENTICATE PLAIN", vec!["AUTHENTICATE +"]),
        (
            "AUTHENTICATE",
            vec![":srv 903 bot :SASL authentication successful"],
        ),
    ])
    .await;

    let _state = State::connect(
        "bot",
        Server::plain(&addr).with_sasl_plain("bot", "hunter2"),
        vec![],
    )
    .await
    .expect("connect failed");

    let sent = rx.await.expect("server reported nothing");
    assert!(
        sent.contains(&"AUTHENTICATE AGJvdABodW50ZXIy".to_string()),
        "the exchange ended before authenticating: {sent:?}"
    );
}
