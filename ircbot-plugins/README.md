# ircbot-plugins

Standard plugins for the [ircbot](https://crates.io/crates/ircbot) IRC bot
framework.

| Plugin | Feature | Commands |
|---|---|---|
| `Seen` | `seen` | `!seen <nick>`: when a nick last spoke in this channel, and what it said |

The plugins need the plugin support of `ircbot` 0.7 or later. Each plugin has a
feature, and all features are on by default. A bot that wants
fewer plugins turns the default features off and names the plugins it needs:

```toml
[dependencies]
ircbot = { version = "0.7", features = ["store"] }
ircbot-plugins = { version = "0.1", default-features = false, features = ["seen"] }
```

A plugin that keeps data takes an `ircbot::store::Store`, uses the namespace
with its own name, and applies its own schema when it opens:

```rust,ignore
use ircbot::{bot, store::Store};
use ircbot_plugins::Seen;

#[bot]
impl MyBot {}

let store = Store::open("bot.db")?;
MyBot::new("mybot", "irc.example.net:6667", ["rust"])
    .with_help()
    .plugin(Seen::open(&store).await?)
    .main_loop()
    .await
```

See the [docs](https://docs.rs/ircbot-plugins) for each plugin.

## License

MIT
