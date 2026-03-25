Registers the annotated method as an event handler inside a [`#[bot]`](macro@bot) impl block.

The general-purpose trigger attribute.  Exactly one of `command`,
`message`, `event`, `mention`, or `cron` must be present.  `target`,
`regex`, and `tz` are optional modifiers.

# Keys

| Key | Description |
|-----|-------------|
| `command = "name"` | Fires on `!name`; equivalent to [`#[command("name")]`](macro@command) but also accepts `target` |
| `message = "pattern"` | Glob pattern on PRIVMSG text; each `*` captures the matched portion as a `String` parameter |
| `event = "IRC_CMD"` | Any raw IRC command (e.g. `"JOIN"`, `"PRIVMSG"`, `"PART"`, `"NICK"`) |
| `mention` | Fires when a PRIVMSG addresses the bot by name (`"botname: …"` or `"botname, …"`); matched text is passed as first `String` parameter |
| `cron = "expr"` | Fires on a cron schedule independent of any IRC message; uses the 6-field Quartz format (`sec min hour dom month dow`) with an optional 7th year field, validated at compile time |
| `target = "#channel"` | *(optional)* Restrict the trigger to a specific channel |
| `regex = "pattern"` | *(optional, with `event`)* Further filter by a regex on the message text; capture groups become `String` parameters |
| `tz = "Timezone"` | *(optional, with `cron`)* IANA timezone for evaluating the schedule (default: `"UTC"`), validated at compile time |

When multiple trigger keys are present, the first one in this precedence
order wins: `message` › `command` › `event` › `mention` › `cron`.

# Examples

**`message`** — glob pattern on PRIVMSG text.  Each `*` captures the
corresponding portion of the text as a `String` parameter:

```rust,ignore
// Fires on any PRIVMSG that looks like "you are <something>".
#[on(message = "you are *")]
async fn praise_me(&self, ctx: Context, praise: String) -> Result {
    ctx.say(format!("Indeed, I am {}!", praise))
}
```

**`event`** — any raw IRC command.  Use this for protocol-level events:

```rust,ignore
// Fires whenever any user joins any channel.
#[on(event = "JOIN")]
async fn on_join(&self, ctx: Context, user: User) -> Result {
    ctx.say(format!("Welcome, {}!", user.nick))
}
```

**`event` + `target`** — restrict to a specific channel:

```rust,ignore
// Fires only when someone joins #rust.
#[on(event = "JOIN", target = "#rust")]
async fn welcome_rust(&self, ctx: Context, user: User) -> Result {
    ctx.say(format!("Welcome to #rust, {}!", user.nick))
}
```

**`event` + `regex`** — filter by a regex applied to the message text.
Capture groups become `String` parameters in the order they appear:

```rust,ignore
// Fires on PRIVMSG lines that match `!op <nick> <reason>`.
#[on(event = "PRIVMSG", regex = r"^!op (\S+) (.+)$")]
async fn op_request(&self, ctx: Context, target_nick: String, reason: String) -> Result {
    ctx.say(format!("Granting op to {} (reason: {})", target_nick, reason))
}
```

**`command`** — command-style shorthand inside `#[on]`, useful when you
also need `target`:

```rust,ignore
#[on(command = "dance", target = "#general")]
async fn dance(&self, ctx: Context) -> Result {
    ctx.action("dances!")
}
```

**`mention`** — fires when a PRIVMSG directly addresses the bot by name
(`"botname: …"` or `"botname, …"`).  The text that follows the prefix is
passed as the first `String` parameter:

```rust,ignore
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

**`cron`** — fires the handler according to a cron schedule, independent
of any IRC message.  The expression uses the **6-field Quartz format**
backed by the [`cron`](https://crates.io/crates/cron) crate:

```text
sec  min  hour  day-of-month  month  day-of-week  [year]
```

Times are evaluated in UTC by default.  Use `tz` to specify any IANA
timezone (backed by [`chrono-tz`](https://crates.io/crates/chrono-tz)).
Both the cron expression and the timezone are **validated at compile
time** — a typo is a compile error, not a runtime panic.

```rust,ignore
// Top of every hour, 8 a.m.–4 p.m. Eastern time, Monday–Friday.
#[on(cron = "0 0 8-16 * * MON-FRI", tz = "America/New_York", target = "#work")]
async fn work_hours_reminder(&self, ctx: Context) -> Result {
    ctx.say("Heads up: stand-up in 5 minutes!")
}

// Every 15 minutes, UTC (default when `tz` is omitted).
#[on(cron = "0 */15 * * * *", target = "#general")]
async fn quarter_hour(&self, ctx: Context) -> Result {
    ctx.say("15-minute check-in!")
}

// Every Monday at 9 a.m. Tokyo time.
#[on(cron = "0 0 9 * * MON", tz = "Asia/Tokyo")]
async fn weekly_report(&self, ctx: Context) -> Result {
    // ctx.target is empty when no target is specified;
    // use ctx.tx directly or store the channel name in bot state.
    Ok(())
}
```

**Quick reference — common expressions:**

| Expression | Meaning |
|---|---|
| `"0 0 * * * *"` | Every hour (on the minute) |
| `"0 0 8-16 * * MON-FRI"` | Top of each hour, 8 a.m.–4 p.m., weekdays |
| `"0 */15 * * * *"` | Every 15 minutes |
| `"0 30 9 * * *"` | Every day at 09:30 |
| `"0 0 9 * * MON"` | Every Monday at 9 a.m. |
| `"* * * * * *"` | Every second (useful in tests) |

The handler fires for the first time after the next scheduled time is
reached (never at bot startup).  On reconnect, the schedule is evaluated
fresh from the current time.

Cron handlers receive a synthetic `Context` whose `sender` is `None` and
`captures` is empty.  Use `ctx.say()` to post to the configured `target`.

# Note

`#[on]` is meaningful **only** when placed on a method inside an `#[bot]`
impl block.  Outside that context it is a no-op marker that leaves the
item unchanged.
