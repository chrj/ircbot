# ircbot

[![ircbot on crates.io](https://img.shields.io/crates/v/ircbot.svg)](https://crates.io/crates/ircbot)
[![ircbot-macros on crates.io](https://img.shields.io/crates/v/ircbot-macros.svg)](https://crates.io/crates/ircbot-macros)
[![docs.rs](https://docs.rs/ircbot/badge.svg)](https://docs.rs/ircbot)

An async IRC bot framework for Rust powered by [Tokio](https://tokio.rs/) and procedural macros.

```rust,ignore
use ircbot::{bot, Context, User, Result};

#[bot]
impl MyBot {
    #[command("ping")]
    async fn ping(&self, ctx: Context) -> Result {
        ctx.reply("Pong!")
    }

    // Typed args: the words after `!add` are parsed into `a` and `b`.
    #[command("add")]
    async fn add(&self, ctx: Context, a: i64, b: i64) -> Result {
        ctx.reply(a + b)
    }

    #[on(message = "you are *")]
    async fn praise_me(&self, ctx: Context) -> Result {
        ctx.say("Correct.")
    }

    #[on(event = "JOIN")]
    async fn welcome(&self, ctx: Context, user: User) -> Result {
        ctx.say(format!("Welcome, {}!", user.nick))
    }

    #[on(cron = "0 0 9 * * MON-FRI", target = "#general")]
    async fn morning(&self, ctx: Context) -> Result {
        ctx.say("Good morning!")
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    MyBot::new("mybot", "localhost:6667", ["general"])
        .await?
        .main_loop()
        .await
}
```

## Highlights

- **Proc-macro API** — annotate methods with `#[command]` or `#[on]`; `#[bot]` wires everything up.
- **Typed state** — `#[bot(state = MyState)]` adds a `pub state` field your handlers can read; mutate it through interior mutability (`Mutex`/atomics). See `examples/stateful_bot.rs`.
- **Flexible triggers** — commands (`!ping`), glob patterns (`"you are *"`), raw IRC events, mention detection, cron schedules — all with optional target-channel and regex filters.
- **Typed command arguments** — declare `async fn add(&self, ctx: Context, a: i64, b: i64)` and the words after `!add` are parsed into the parameters (`FromStr` types, a trailing `String`/`Vec`, `Option<T>`); on bad input the bot replies with a generated usage string.
- **Reply helpers** — `ctx.reply()`, `ctx.say()`, `ctx.action()`, `ctx.notice()`, `ctx.whisper()`.
- **Channel control** — `ctx.join()` and `ctx.part()` to make the bot enter or leave channels from a handler.
- **Raw escape hatch** — `ctx.raw()` sends any IRC line the helpers don't wrap (`MODE`, `INVITE`, …), still sanitized.
- **Moderation** — `ctx.set_topic()` and `ctx.kick()` act on the channel the message arrived in.
- **Access control** — define hostmask-based roles with `.with_role("admin", ["*!*@trusted.host"])` and gate commands with `#[command("op", role = "admin")]`; unauthorized senders are silently ignored.
- **Message accessors** — `ctx.nick()`, `ctx.is_from_self()`, `ctx.mentions_me()` to inspect who sent a message and what it says.
- **Keepalive & auto-reconnect** — periodic `PING`/`PONG` monitoring; reconnects and re-joins on drop. If the configured nick is already in use, the bot automatically retries with a suffixed alternative (`bot`, `bot_`, …).
- **Authentication** — SASL `PLAIN` and `EXTERNAL` (CertFP) during registration, a `PASS` server password, and IRCv3 capability negotiation. A rejected login fails the connection instead of continuing unauthenticated.
- **TLS** (optional) — `Server::tls("irc.libera.chat:6697")` behind the `tls` feature, with certificate verification against the platform root store, private-CA and self-signed support, and client certificates for CertFP.
- **Hot reload** (Unix) — `SIGHUP` execs the new binary with the live TCP socket inherited; no reconnect, no missed messages.
- **Flood protection** — token-bucket rate limiter (default: burst 4, 1 msg / 500 ms).
- **Auto message splitting** — long messages are word-wrapped and split within the 512-byte IRC limit.
- **Output sanitization** — `\r`, `\n`, `\0` stripped from every outgoing message.
- **Unit-testable** — `ircbot::testing::TestContext` lets you test handlers without a live server.
- **Structured logging** — diagnostics are emitted through [`tracing`](https://docs.rs/tracing); you pick the subscriber, level, and format. Raw IRC traffic is available opt-in on the `ircbot::protocol` target.

Full API reference: **[docs.rs/ircbot](https://docs.rs/ircbot)**

## Getting started

```toml
[dependencies]
ircbot = "0.4"
tokio  = { version = "1", features = ["full"] }
```

See the [`basic_bot` example](ircbot/examples/basic_bot.rs) and the [docs](https://docs.rs/ircbot) for the complete API, hot-reload guide, testing helpers, and lower-level `State` / `internal` APIs.

## Authentication

Most networks want a bot to identify itself. The credentials live on `Server`,
because the handshake needs them before the bot exists:

```rust,ignore
use ircbot::Server;

MyBot::new(
    "mybot",
    Server::tls("irc.libera.chat:6697").with_sasl_plain("mybot", &password),
    ["rust"],
)
```

The bot authenticates during registration, before it joins a channel. It is
therefore already identified when it arrives, which is what `+r` channels
require and what earns the account its cloak.

Three methods are available:

- `with_sasl_plain(account, password)` — an account name and a password. The
  password travels in a reversible encoding, so use it only over TLS.
- `with_sasl_external()` — the server reads the account from the TLS client
  certificate (CertFP), and no password is sent. Set the certificate with
  `Server::tls(..).with_client_cert_pem(..)`, and register its fingerprint with
  the network first.
- `with_password(password)` — a server password (`PASS`). This authenticates to
  the server itself, not to its services.

**A failed SASL exchange fails the connection.** If the network offers no SASL,
or rejects the credentials, `connect` returns an error. The bot does not
continue unauthenticated: a bot that loses its identity without saying so is the
fault SASL exists to prevent.

`with_capabilities` asks for further IRCv3 capabilities, for example
`server-time` or `multi-prefix`. The framework requests the ones the server
advertises and skips the rest. It does not interpret them itself — what they
change reaches handlers on the raw message.

A bot with no credentials sends the same bare `NICK`/`USER` handshake as before,
and no `CAP` line at all.

## TLS

TLS is behind the optional `tls` feature, which pulls in
[rustls](https://github.com/rustls/rustls):

```toml
[dependencies]
ircbot = { version = "0.4", features = ["tls"] }
```

The second argument to `new` is the server, and it decides the transport. A bare
`"host:port"` string connects in plaintext; `Server::tls` connects over TLS,
verifying the certificate against the platform's root store:

```rust,ignore
use ircbot::Server;

MyBot::new("mybot", Server::tls("irc.libera.chat:6697"), ["rust"])
    .await?
    .main_loop()
    .await
```

The transport is never inferred from the port number, so a misconfigured port
cannot silently downgrade the connection to plaintext.

For a network with a private CA or a self-signed certificate, trust that
certificate specifically rather than turning verification off:

```rust,ignore
Server::tls("irc.internal.example:6697")
    .with_extra_root_pem(std::fs::read("ca.pem")?)
```

`with_client_cert_pem` presents a client certificate for CertFP, which
`with_sasl_external` then authenticates with. `with_sni` overrides the verified
hostname when connecting by IP.
`danger_accept_invalid_certs` disables verification entirely — it is meant for a
development server on `localhost`, leaves the connection unauthenticated, and
logs a warning on every connect.

**Hot reload and TLS are mutually exclusive.** `SIGHUP` still swaps the binary,
but a TLS session cannot be handed to the new process: the socket survives
`exec`, while the session keys, record sequence numbers, and partially-read
records that make it decryptable do not. The successor reconnects and rejoins,
logging a warning, so a TLS bot trades zero-disconnect reloads for a few seconds
of downtime.

## Logging

The framework emits structured [`tracing`](https://docs.rs/tracing) events and
installs no subscriber of its own, so verbosity, format, and destination are
yours to configure. Raw IRC traffic is available opt-in on the `ircbot::protocol`
target.

See the [`logging` module docs](https://docs.rs/ircbot/latest/ircbot/logging/)
for subscriber setup and the raw-protocol opt-in.

## License

MIT

## AI Disclaimer

This project was written primarily by AI, orchestrated, supervised and reviewed by a human (me).
Feel free to use any AI tool for contributions to this project.
