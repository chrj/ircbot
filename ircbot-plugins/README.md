# ircbot-plugins

Standard plugins for the [ircbot](https://crates.io/crates/ircbot) IRC bot
framework.

| Plugin | Feature | Commands |
|---|---|---|
| `Notify` | `notify` | `!notify <nick> <message>`: give a message to a nick when it is next here, in private |
| `Ops` | `ops` | `!op [nick]`, `!deop [nick]`, `!kick <nick> [reason]`, `!ban <nick or mask> [duration]`, `!unban <nick or mask>`: channel operator commands for the role `op` |
| `Seen` | `seen` | `!seen <nick>`: when a nick last spoke in this channel, and what it said |

`ircbot-plugins` has the same version as `ircbot`: use the same version for
both. Each plugin has a feature, and all features are on by default:

```toml
[dependencies]
ircbot = { version = "0.7", features = ["store"] }
ircbot-plugins = "0.7"
```

A bot that wants fewer plugins turns the default features off and names the
plugins it needs, for example
`ircbot-plugins = { version = "0.7", default-features = false, features = ["seen"] }`.

A plugin that keeps data takes an `ircbot::store::Store`, uses the namespace
with its own name, and applies its own schema when it opens:

```rust,ignore
use ircbot::{bot, store::Store};
use ircbot_plugins::{Notify, Ops, Seen};

#[bot]
impl MyBot {}

let store = Store::open("bot.db")?;
MyBot::new("mybot", "irc.example.net:6667", ["rust"])
    .with_help()
    .with_role("op", ["*!*@trusted.host"])
    .plugin(Seen::open(&store).await?)
    .plugin(Notify::open(&store).await?)
    .plugin(Ops::open(&store).await?)
    .main_loop()
    .await
```

See the [docs](https://docs.rs/ircbot-plugins) for each plugin.

## License

MIT
