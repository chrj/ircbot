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
- **Flexible triggers** — commands (`!ping`), glob patterns (`"you are *"`), raw IRC events, mention detection, cron schedules — all with optional target-channel and regex filters.
- **Reply helpers** — `ctx.reply()`, `ctx.say()`, `ctx.action()`, `ctx.notice()`, `ctx.whisper()`.
- **Keepalive & auto-reconnect** — periodic `PING`/`PONG` monitoring; reconnects and re-joins on drop.
- **Hot reload** (Unix) — `SIGHUP` execs the new binary with the live TCP socket inherited; no reconnect, no missed messages.
- **Flood protection** — token-bucket rate limiter (default: burst 4, 1 msg / 500 ms).
- **Auto message splitting** — long messages are word-wrapped and split within the 512-byte IRC limit.
- **Output sanitization** — `\r`, `\n`, `\0` stripped from every outgoing message.
- **Unit-testable** — `ircbot::testing::TestContext` lets you test handlers without a live server.

Full API reference: **[docs.rs/ircbot](https://docs.rs/ircbot)**

## Getting started

```toml
[dependencies]
ircbot = "0.1"
tokio  = { version = "1", features = ["full"] }
```

See the [`basic_bot` example](ircbot/examples/basic_bot.rs) and the [docs](https://docs.rs/ircbot) for the complete API, hot-reload guide, testing helpers, and lower-level `State` / `internal` APIs.

## License

MIT
