Registers the annotated method as a command handler inside a [`#[bot]`](macro@bot) impl block.

Fires when a user sends `!name` (case-insensitive) to any channel the bot
has joined, or as a private message.  The text that follows `!name` on the
same line is available as the first `String` parameter.

# Arguments

- `"name"` — *(required, positional)* the command keyword, without the
  leading `!`.  Matching is case-insensitive.
- `target = "#channel"` — *(optional)* restrict the command to a specific
  channel.  When omitted, the command responds everywhere.

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
