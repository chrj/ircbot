Registers the annotated method as a command handler inside a [`#[bot]`](macro@bot) impl block.

Fires when a user sends `!name` (case-insensitive) to any channel the bot
has joined, or as a private message.  The text that follows `!name` on the
same line is parsed into the method's parameters (see
[Typed arguments](#typed-arguments)).

# Arguments

- `"name"` — *(required, positional)* the command keyword, without the
  leading `!`.  Matching is case-insensitive.
- `target = "#channel"` — *(optional)* restrict the command to a specific
  channel.  When omitted, the command responds everywhere.

# Typed arguments

Parameters after `ctx` are filled from the words following the command, in
order.  The declared type decides how each word is consumed:

- **A plain `FromStr` type** (`i64`, `u32`, `f64`, `bool`, a custom type, …)
  consumes one whitespace-delimited token and parses it.
- **A trailing `String`** captures the rest of the line verbatim (it may be
  empty).  A non-final `String` consumes a single token.
- **`Option<T>`** (as the last parameter) is optional: `None` when no word is
  left, otherwise the parsed value.
- **`Vec<T>`** (as the last parameter) collects every remaining word.
- **`User`** is filled with the message sender (it is not taken from the text).

If a required argument is missing or fails to parse, the bot replies with a
generated usage string (e.g. `usage: !add <a> <b>`) and the handler body does
**not** run.  `Option<T>` and `Vec<T>` are only supported as the **last**
parameter.

# Usage

```rust,ignore
#[bot]
impl MyBot {
    // Responds to `!ping` from anywhere.
    #[command("ping")]
    async fn ping(&self, ctx: Context) -> Result {
        ctx.reply("Pong!")
    }

    // Captures everything after `!echo` as `text`.
    #[command("echo")]
    async fn echo(&self, ctx: Context, text: String) -> Result {
        ctx.say(text)
    }

    // Typed arguments: `!add 2 3` replies "5"; `!add x 3` replies the usage string.
    #[command("add")]
    async fn add(&self, ctx: Context, a: i64, b: i64) -> Result {
        ctx.reply(a + b)
    }

    // Only responds in #dice.
    #[command("roll", target = "#dice")]
    async fn roll(&self, ctx: Context) -> Result {
        ctx.say("🎲 You rolled a 4!")
    }
}
```

# Note

`#[command]` is meaningful **only** when placed on a method inside an
`#[bot]` impl block.  Outside that context it is a no-op marker that leaves
the item unchanged.
