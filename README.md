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
        ctx.reply("Pong!")
    }

    /// Respond to any message that looks like "you are …".
    #[on(message = "you are *")]
    async fn praise_me(&self, ctx: Context) -> Result {
        ctx.say("Correct.")
    }

    /// Welcome users who join a channel.
    #[on(event = "JOIN")]
    async fn welcome(&self, ctx: Context, user: User) -> Result {
        ctx.say(format!("Welcome to the void, {}!", user.nick))
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
        ctx.say(message)
    }

    /// Respond to `!dance` with a /me action, but only in #general.
    #[on(command = "dance", target = "#general")]
    async fn dance(&self, ctx: Context) -> Result {
        ctx.action("Dancing!")
    }

    /// Respond when the bot is addressed by name in any channel.
    #[on(mention)]
    async fn on_mention(&self, ctx: Context, text: String) -> Result {
        ctx.reply(format!("You said: {}", text))
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
- **Active keepalive** — the bot sends a periodic `PING` to the server (default every 30 s) and reconnects automatically if no `PONG` arrives within the timeout (default 10 s).  Interval and timeout are configurable via `State::with_keepalive()`.
- **Automatic reconnection** — on TCP drop or keepalive timeout the bot re-dials and re-joins all configured channels, preserving all handler registrations.
- **Hot reload** — replace the running bot binary without dropping the IRC connection.  On Unix, sending `SIGHUP` execs the new binary with the live TCP socket inherited; no reconnect, no missed messages. See [Hot reload](#hot-reload).
- **Concurrent write loop** — outgoing messages are serialised through an in-process channel so handlers can send replies without blocking each other.
- **Flood protection** — a token-bucket rate limiter in the write loop ensures the bot cannot send messages faster than the server allows (default: burst of 4, then 1 message per 500 ms).  Configurable via `State::with_flood_control()`.
- **Automatic message splitting** — any outgoing message that would exceed the IRC 512-byte line limit is automatically split across multiple lines, with word-boundary awareness and UTF-8 safety.
- **Output sanitization** — `\r`, `\n`, and `\0` are stripped from every outgoing message, preventing IRC injection attacks.

---

## Workspace layout

```
rustbot2/               ← library crate (public API)
  src/
    lib.rs              ← re-exports, type aliases, and internal::run_bot reconnection loop
    irc.rs              ← RFC 1459 IRC line parser
    connection.rs       ← TCP connect + NICK/USER/JOIN, State, with_keepalive
    context.rs          ← Context, User
    handler.rs          ← Trigger, HandlerEntry type aliases
    bot.rs              ← run_bot_internal, trigger matching, glob, keepalive ping
  tests/
    irc_parsing.rs      ← unit tests (IRC parsing)
    trigger_matching.rs ← unit tests (trigger dispatch)
    keepalive.rs        ← unit tests (keepalive timeout, automatic reconnection)
    flood_control.rs    ← unit + integration tests (message splitting, rate limiting)
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
- `YourBot::new(nick, server, channels)` — connects to the server, identifies, and joins the given channels.  On Unix, if this process was started via `SIGHUP` hot-reload, the live TCP socket is inherited from the previous binary instead.
- `YourBot::main_loop(self)` — runs the event loop, reconnecting automatically on TCP drops or keepalive timeouts.  On Unix, also listens for `SIGHUP` and performs a zero-disconnect binary exec-reload.

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
    ctx.reply("Pong!")
}

// The rest of the line after `!echo` is captured as `text`.
#[command("echo")]
async fn echo(&self, ctx: Context, text: String) -> Result {
    ctx.say(text)
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
    ctx.say(format!("Indeed, I am {}!", praise))
}
```

**`event`** — any raw IRC command.  Use this for protocol-level events:

```rust
// Fires whenever any user joins any channel.
#[on(event = "JOIN")]
async fn on_join(&self, ctx: Context, user: User) -> Result {
    ctx.say(format!("Welcome, {}!", user.nick))
}
```

**`event` + `target`** — restrict to a specific channel:

```rust
// Fires only when someone joins #rust.
#[on(event = "JOIN", target = "#rust")]
async fn welcome_rust(&self, ctx: Context, user: User) -> Result {
    ctx.say(format!("Welcome to #rust, {}!", user.nick))
}
```

**`event` + `regex`** — filter by a regex applied to the message text.  Capture groups become `String` parameters in the order they appear:

```rust
// Fires on PRIVMSG lines that match `!op <nick> <reason>`.
#[on(event = "PRIVMSG", regex = r"^!op (\S+) (.+)$")]
async fn op_request(&self, ctx: Context, target_nick: String, reason: String) -> Result {
    ctx.say(format!("Granting op to {} (reason: {})", target_nick, reason))
}
```

**`command`** — command-style shorthand inside `#[on]`, useful when you also need `target`:

```rust
#[on(command = "dance", target = "#general")]
async fn dance(&self, ctx: Context) -> Result {
    ctx.action("dances!")
}
```

**`mention`** — fires when a PRIVMSG directly addresses the bot by name (`"botname: …"` or `"botname, …"`).  The text that follows the prefix is passed as the first `String` parameter:

```rust
// Fires when a user writes "mybot: hello there" in a channel.
#[on(mention)]
async fn on_mention(&self, ctx: Context, text: String) -> Result {
    ctx.reply(format!("You said: {}", text))
}

// Restrict to a specific channel.
#[on(mention, target = "#rust")]
async fn on_mention_rust(&self, ctx: Context) -> Result {
    ctx.notice("I heard you!").await
}
```

---

## Keepalive & reconnection

The bot actively monitors its connection by sending a `PING rustbot2-keepalive` to the server at a regular interval. If no matching `PONG` is received within the timeout window, the connection is treated as dead and a new TCP connection is established.

**Defaults:**

| Setting | Value |
|---------|-------|
| Keepalive interval | 30 s |
| PONG response timeout | 10 s |
| Reconnect delay | 5 s |

`main_loop()` never returns normally — it reconnects automatically whenever the connection is lost (TCP close or keepalive timeout), re-sends `NICK`/`USER`, and re-joins all configured channels.

**Custom intervals** — configure keepalive before starting the bot by calling `State::with_keepalive()`.  When using the `#[bot]` macro, `new()` manages the `State` internally, so custom keepalive settings require the lower-level API:

```rust
use std::sync::Arc;
use std::time::Duration;
use rustbot2::{State, HandlerEntry, internal};

let state = State::connect("mybot", "irc.libera.chat:6667", vec!["#rust".into()])
    .await?
    .with_keepalive(Duration::from_secs(60), Duration::from_secs(15));

let handlers: Vec<HandlerEntry<()>> = vec![/* your HandlerEntry values */];
internal::run_bot(Arc::new(()), state, handlers).await?;
```

---

## Hot reload

Hot reload lets you replace the running bot **binary** without ever dropping the IRC connection — no reconnect, no missed messages, no re-authentication.

### How it works

On Unix, a TCP socket is just a file descriptor.  When a process calls `exec()` the new process image inherits every file descriptor that does **not** have `FD_CLOEXEC` set.  The hot-reload path exploits this:

1. **`SIGHUP` received** — `main_loop()` catches the signal.
2. **FD prepared** — `FD_CLOEXEC` is cleared on the live TCP socket so it survives `exec`.
3. **State encoded** — the fd number, nick, server, channels, and keepalive settings are written into environment variables.
4. **`exec` called** — the current process image is replaced with the new binary at the same path.  The PID is unchanged; the TCP connection is never closed.
5. **New binary starts** — `new()` detects the env vars, calls `State::try_inherit_from_env()`, and wraps the inherited fd in a Tokio `TcpStream`.  No `NICK`/`USER`/`JOIN` is sent; the IRC session continues seamlessly.

### Using SIGHUP (zero configuration)

When using the `#[bot]` macro, `main_loop()` installs the SIGHUP handler automatically.  The full workflow is:

```sh
# 1. Build the updated binary.
cargo build --release

# 2. Send SIGHUP to the running bot.
kill -HUP $(pidof my_bot)

# 3. The old process execs the new binary.
#    The IRC connection is never interrupted.
```

### Lower-level API

For programmatic control call `hot_reload::exec_reload` directly — for example from an IRC admin command:

```rust
use rustbot2::hot_reload::exec_reload;

// Inside a handler:
#[command("reload")]
async fn do_reload(&self, ctx: Context) -> Result {
    ctx.say("Reloading…")?;
    // exec_reload only returns if exec itself failed.
    let err = exec_reload(
        ctx.raw_fd,          // inherited TCP socket fd
        &ctx.bot_nick,
        "irc.libera.chat:6667",
        &["#rust".to_string()],
        30_000,              // keepalive interval ms
        10_000,              // keepalive timeout ms
    );
    ctx.say(format!("Reload failed: {err}"))
}
```

---

## Flood protection

The bot's write loop enforces a **token-bucket rate limiter** to prevent it from
overwhelming the IRC server with outgoing messages.

**How it works:**

1. The bucket starts full with `burst` tokens.
2. Each outgoing message consumes one token.
3. While at least one token is available the message is sent immediately.
4. Once the bucket is empty the write loop waits until enough time has elapsed
   for a new token to be added (one token per `rate` interval) before sending
   the next message.

**Defaults:**

| Setting | Value |
|---------|-------|
| Burst (initial token supply) | 4 messages |
| Rate (token refill interval) | 500 ms |
| Steady-state throughput | ≈ 2 messages / second |

**Custom flood-control settings** — call `State::with_flood_control()` before
starting the bot.  When using the `#[bot]` macro, use the lower-level API:

```rust
use std::sync::Arc;
use std::time::Duration;
use rustbot2::{State, HandlerEntry, internal};

let state = State::connect("mybot", "irc.libera.chat:6667", vec!["#rust".into()])
    .await?
    .with_flood_control(8, Duration::from_millis(250)); // burst of 8, ≈ 4 msg/s

let handlers: Vec<HandlerEntry<()>> = vec![/* your HandlerEntry values */];
internal::run_bot(Arc::new(()), state, handlers).await?;
```

---

## Automatic message splitting

IRC limits each protocol line to **512 bytes** (including the trailing `\r\n`).
Every `Context` reply method (`reply`, `say`, `action`, `notice`, `whisper`)
automatically splits text that would exceed this limit into multiple messages.
The splitter:

- Prefers to break at an **ASCII space** (word-wrapping), falling back to a
  hard byte-limit split when no space is available.
- Always splits on a valid **UTF-8 character boundary** so multi-byte characters
  are never corrupted.
- Accounts for the fixed overhead of the IRC command prefix (e.g.
  `PRIVMSG #channel :`) and any CTCP suffix when computing the available space.

Splitting happens transparently — your handler code does not need to do
anything special.

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
    ctx.say(format!("Kicking {} ({})", target_nick, reason))
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
| `ctx.reply(msg)` | In a channel: `nick, msg`. In a query: `msg` to the sender. Synchronous. |
| `ctx.say(msg)` | Send `msg` to the current channel or query target, without a nick prefix. Synchronous. |
| `ctx.action(msg)` | Send a CTCP ACTION (`/me msg`) to the current target. Synchronous. |
| `ctx.notice(msg)` | Send a `NOTICE` to the current target. NOTICEs must never be replied to automatically (by convention), making them suitable for status messages and one-shot notifications. **Async** — use `.await`. |
| `ctx.whisper(msg)` | Send a private message directly to the sender's nick, regardless of whether the original message arrived in a channel or a query. **Async** — use `.await`. |
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

Unit tests covering IRC parsing, all trigger types, keepalive timeouts, automatic reconnection, message splitting, and rate-limiting.

Integration tests (require Docker):

```sh
cargo test --features integration -- --test-threads=1
```

---

## License

MIT
