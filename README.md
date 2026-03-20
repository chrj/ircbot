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

    /// Respond when the bot is addressed by name in any channel.
    #[on(mention)]
    async fn on_mention(&self, ctx: Context, text: String) -> Result {
        ctx.reply(format!("You said: {}", text)).await
    }

    /// Send a private message directly to the caller, regardless of channel.
    #[command("secret")]
    async fn secret(&self, ctx: Context) -> Result {
        ctx.whisper("This is just between us.").await
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
- **Flexible triggers** — commands (`!ping`), glob message patterns (`"you are *"`), raw IRC events (`JOIN`, `PRIVMSG`, …), and bot-mention detection (`"botname: …"`), all with optional target-channel and regex filters.
- **Context helpers** — `ctx.reply()`, `ctx.say()`, `ctx.action()`, `ctx.notice()`, and `ctx.whisper()` cover the most common reply patterns.
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

```rust
// Generated signatures (simplified):
impl YourBot {
    pub async fn new(
        nick: impl Into<String>,
        server: impl AsRef<str>,
        channels: impl IntoIterator<Item = impl Into<String>>,
    ) -> Result<Self, Box<dyn std::error::Error + Send + Sync>>;

    pub async fn main_loop(self)
        -> Result<(), Box<dyn std::error::Error + Send + Sync>>;
}
```

Channel names in `channels` are automatically prefixed with `#` if they do not already start with a channel sigil (`#`, `&`, `+`, `!`).

#### `#[command("name")]`

Fires when a user sends `!name` (case-insensitive) in any channel or as a private message.  The text that follows `!name` on the same line is available as the first `String` parameter.

```rust
#[command("ping")]
async fn ping(&self, ctx: Context) -> Result {
    ctx.reply("Pong!").await
}

// The rest of the line after `!echo` is captured as `text`.
#[command("echo")]
async fn echo(&self, ctx: Context, text: String) -> Result {
    ctx.say(text).await
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
| `mention` | Fires when a PRIVMSG addresses the bot by name (`"botname: …"` or `"botname, …"`) |
| `target = "#channel"` | Optional channel filter (for any trigger type) |
| `regex = "…"` | Optional regex on the message text; capture groups become `String` args |

Exactly one of `command`, `message`, `event`, or `mention` must be present. `target` and `regex` are optional modifiers. Trigger precedence when multiple keys are given: `message` > `command` > `event` > `mention`.

**`message`** — glob pattern on PRIVMSG text.  Each `*` captures the corresponding portion of the text as a `String` parameter:

```rust
// Fires on any PRIVMSG that looks like "you are <something>".
// The captured text is available as `praise`.
#[on(message = "you are *")]
async fn praise_me(&self, ctx: Context, praise: String) -> Result {
    ctx.say(format!("Indeed, I am {}!", praise)).await
}
```

**`event`** — any raw IRC command.  Use this for protocol-level events:

```rust
// Fires whenever any user joins any channel.
#[on(event = "JOIN")]
async fn on_join(&self, ctx: Context, user: User) -> Result {
    ctx.say(format!("Welcome, {}!", user.nick)).await
}
```

**`event` + `target`** — restrict to a specific channel:

```rust
// Fires only when someone joins #rust.
#[on(event = "JOIN", target = "#rust")]
async fn welcome_rust(&self, ctx: Context, user: User) -> Result {
    ctx.say(format!("Welcome to #rust, {}!", user.nick)).await
}
```

**`event` + `regex`** — filter by a regex applied to the message text.  Capture groups become `String` parameters in the order they appear:

```rust
// Fires on PRIVMSG lines that match `!op <nick> <reason>`.
#[on(event = "PRIVMSG", regex = r"^!op (\S+) (.+)$")]
async fn op_request(&self, ctx: Context, target_nick: String, reason: String) -> Result {
    ctx.say(format!("Granting op to {} (reason: {})", target_nick, reason)).await
}
```

**`command`** — command-style shorthand inside `#[on]`, useful when you also need `target`:

```rust
#[on(command = "dance", target = "#general")]
async fn dance(&self, ctx: Context) -> Result {
    ctx.action("dances!").await
}
```

**`mention`** — fires when a PRIVMSG directly addresses the bot by name (`"botname: …"` or `"botname, …"`).  The text that follows the prefix is passed as the first `String` parameter:

```rust
// Fires when a user writes "mybot: hello there" in a channel.
#[on(mention)]
async fn on_mention(&self, ctx: Context, text: String) -> Result {
    ctx.reply(format!("You said: {}", text)).await
}

// Restrict to a specific channel.
#[on(mention, target = "#rust")]
async fn on_mention_rust(&self, ctx: Context) -> Result {
    ctx.notice("I heard you!").await
}
```

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

### Multiple capture groups

When a `regex` (or a `message` glob with multiple `*`) produces more than one
capture, each extra `String` parameter receives the next capture in order:

```rust
// regex with two capture groups → two String parameters
#[on(event = "PRIVMSG", regex = r"^!kick (\S+) (.*)$")]
async fn kick(&self, ctx: Context, target_nick: String, reason: String) -> Result {
    ctx.say(format!("Kicking {} ({})", target_nick, reason)).await
}
```

If `captures` is empty the first `String` parameter falls back to the full
message text (`ctx.message_text()`).

---

## Context

`Context` is passed to every handler and provides both metadata about the
incoming message and helper methods for sending replies.

### Fields

| Field | Type | Description |
|-------|------|-------------|
| `ctx.target` | `String` | Channel or nick the message was directed to |
| `ctx.is_channel` | `bool` | `true` when `target` is a channel, `false` for private messages |
| `ctx.sender` | `Option<User>` | The user who sent the message |
| `ctx.bot_nick` | `String` | The bot's own IRC nick (useful for self-detection) |
| `ctx.captures` | `Vec<String>` | Regex or glob capture groups from the matched trigger |
| `ctx.raw` | `IrcMessage` | The underlying parsed IRC message |

### Methods

| Method | Behaviour |
|--------|-----------|
| `ctx.reply(msg)` | In a channel: `<target> nick, msg`. In a query: `<nick> msg`. |
| `ctx.say(msg)` | Send `msg` to the current channel or query target, without a nick prefix. |
| `ctx.action(msg)` | Send a CTCP ACTION (`/me msg`) to the current target. |
| `ctx.notice(msg)` | Send a `NOTICE` to the current target. NOTICEs must never be replied to automatically (by convention), making them suitable for status messages and one-shot notifications. |
| `ctx.whisper(msg)` | Send a private message directly to the sender's nick, regardless of whether the original message arrived in a channel or a query. |
| `ctx.message_text()` | The raw trailing text of the underlying IRC message. |

## User

`User` represents the nick!user@host prefix on an IRC message.

| Field | Type | Description |
|-------|------|-------------|
| `user.nick` | `String` | IRC nickname |
| `user.user` | `String` | IRC username (ident) |
| `user.host` | `String` | Hostname or IP |

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

39 tests covering IRC parsing and all trigger types.

---

## License

MIT
