# rustbot2

An async IRC bot framework for Rust powered by [Tokio](https://tokio.rs/) and procedural macros.

Write clean, declarative bots without boilerplate:

```rust
use rustbot2::{bot, Context, User, Result};

#[bot]
impl MyBot {
    /// Respond to `!ping` from anywhere.
    #[command("ping")]
    async fn ping(&self, ctx: Context) -> Result {
        ctx.reply("Pong!").await
    }

    /// Respond to any message that looks like "you are …".
    #[on(message = "you are *")]
    async fn praise_me(&self, ctx: Context) -> Result {
        ctx.say("Correct.").await
    }

    /// Welcome users who join a channel.
    #[on(event = "JOIN")]
    async fn welcome(&self, ctx: Context, user: User) -> Result {
        ctx.say(format!("Welcome to the void, {}!", user.nick)).await
    }

    /// Log every message posted to #general.
    #[on(event = "PRIVMSG", target = "#general")]
    async fn general_chat(&self, ctx: Context, message: String) -> Result {
        println!("Message in #general: {}", message);
        Ok(())
    }

    /// Echo messages matching the regex back to the channel.
    #[on(event = "PRIVMSG", target = "#general", regex = r"^!echo (.+)$")]
    async fn echo(&self, ctx: Context, message: String) -> Result {
        ctx.say(message).await
    }

    /// Respond to `!dance` with a /me action, but only in #general.
    #[on(command = "dance", target = "#general")]
    async fn dance(&self, ctx: Context) -> Result {
        ctx.action("Dancing!").await
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let bot = MyBot::new("mybot", "localhost:6667", ["general"])
        .await
        .expect("Failed to create bot");

    bot.main_loop().await.expect("Bot encountered an error");
    Ok(())
}
```

---

## Features

- **Proc-macro API** — annotate handler methods with `#[command]` or `#[on]` and let the `#[bot]` macro wire everything up.
- **Flexible triggers** — commands (`!ping`), glob message patterns (`"you are *"`), raw IRC events (`JOIN`, `PRIVMSG`, …) with optional target-channel and regex filters.
- **Context helpers** — `ctx.reply()`, `ctx.say()`, and `ctx.action()` cover the most common reply patterns.
- **Async / non-blocking** — built on Tokio; every handler is an `async fn`.
- **Automatic PING/PONG** — the framework handles keepalives transparently.
- **Concurrent write loop** — outgoing messages are serialised through an in-process channel so handlers can send replies without blocking each other.

---

## Workspace layout

```
rustbot2/               ← library crate (public API)
  src/
    lib.rs              ← re-exports and type aliases
    irc.rs              ← RFC 1459 IRC line parser
    connection.rs       ← TCP connect + NICK/USER/JOIN
    context.rs          ← Context, User
    handler.rs          ← Trigger, HandlerEntry type aliases
    bot.rs              ← run_bot_internal, trigger matching, glob
  tests/
    irc_parsing.rs      ← unit tests (IRC parsing)
    trigger_matching.rs ← unit tests (trigger dispatch)
  examples/
    basic_bot.rs        ← minimal demo

rustbot2-macros/        ← proc-macro crate
  src/
    lib.rs              ← #[bot], #[command], #[on]
```

---

## Getting started

Add `rustbot2` to your `Cargo.toml`:

```toml
[dependencies]
rustbot2 = { path = "path/to/rustbot2" }
tokio    = { version = "1", features = ["full"] }
```

### Macros

#### `#[bot]`

Placed on an `impl` block.  The macro generates:

- A `struct` definition for the named type with internal connection state.
- `YourBot::new(nick, server, channels)` — connects to the server, identifies, and joins the given channels.
- `YourBot::main_loop(self)` — runs the event loop until the connection closes.

#### `#[command("name")]`

Fires when a user sends `!name` (case-insensitive) in any channel or as a private message.

```rust
#[command("ping")]
async fn ping(&self, ctx: Context) -> Result {
    ctx.reply("Pong!").await
}
```

Optional `target` filter:

```rust
#[command("roll", target = "#dice")]
async fn roll(&self, ctx: Context) -> Result { … }
```

#### `#[on(…)]`

The general-purpose trigger attribute.  Accepts the following named keys:

| Key | Description |
|-----|-------------|
| `command = "name"` | Same as `#[command("name")]` |
| `message = "pattern"` | Glob pattern on PRIVMSG text; `*` is a capturing wildcard |
| `event = "IRC_CMD"` | Any IRC command (e.g. `"JOIN"`, `"PRIVMSG"`, `"PART"`) |
| `target = "#channel"` | Optional channel filter (for any trigger type) |
| `regex = "…"` | Optional regex on the message text; capture groups become `String` args |

---

## Handler signatures

Handlers always start with `&self` and `ctx: Context`.  Additional parameters
are extracted automatically from the matched message:

```rust
// No extra args — most handlers look like this.
async fn handler(&self, ctx: Context) -> Result

// User — populated from the IRC prefix (JOIN, PART, etc.)
async fn handler(&self, ctx: Context, user: User) -> Result

// String — message body, or the first regex/glob capture group.
async fn handler(&self, ctx: Context, message: String) -> Result
```

---

## Context methods

| Method | Behaviour |
|--------|-----------|
| `ctx.reply(msg)` | In a channel: `<target> nick, msg`. In a query: `<nick> msg`. |
| `ctx.say(msg)` | Send `msg` to the current channel or query target, without a nick prefix. |
| `ctx.action(msg)` | Send a CTCP ACTION (`/me msg`) to the current target. |
| `ctx.message_text()` | The raw trailing text of the underlying IRC message. |
| `ctx.sender` | `Option<User>` — the user who sent the message. |
| `ctx.captures` | `Vec<String>` — regex or glob capture groups from the trigger. |

---

## Running the example

```sh
cargo run --example basic_bot
```

The example prints the API usage and exits cleanly; point it at a real server
by editing the `main` function.

---

## Running the tests

```sh
cargo test
```

31 tests covering IRC parsing and all trigger types.

---

## License

MIT
