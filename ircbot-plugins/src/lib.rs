//! Standard plugins for the [`ircbot`](https://docs.rs/ircbot) framework.
//!
//! Each plugin is a `#[plugin]` that a `#[bot]` adds with its `plugin`
//! method, and each has a feature of this crate. All features are on by
//! default. A plugin whose feature is off is not in the crate, so this table
//! names each plugin as text, not as a link.
//!
//! | Plugin | Feature | Commands |
//! |---|---|---|
//! | `Notify` | `notify` | `!notify <nick> <message>` |
//! | `Ops` | `ops` | `!op [nick]`, `!deop [nick]`, `!kick <nick> [reason]`, `!ban <nick or mask> [duration]`, `!unban <nick or mask>` |
//! | `Seen` | `seen` | `!seen <nick>` |
//!
//! A plugin that keeps data takes an [`ircbot::Store`]. It uses the namespace
//! with its own name, and applies its own schema when it opens:
//!
//! ```rust,ignore
//! use ircbot::{bot, store::Store};
//! use ircbot_plugins::{Notify, Ops, Seen};
//!
//! #[bot]
//! impl MyBot {}
//!
//! let store = Store::open("bot.db")?;
//! MyBot::new("mybot", "irc.example.net:6667", ["rust"])
//!     .with_help()
//!     .with_role("op", ["*!*@trusted.host"])
//!     .plugin(Seen::open(&store).await?)
//!     .plugin(Notify::open(&store).await?)
//!     .plugin(Ops::open(&store).await?)
//!     .main_loop()
//!     .await
//! ```
#![warn(missing_docs)]
#![forbid(unsafe_code)]

#[cfg(feature = "notify")]
pub mod notify;
#[cfg(feature = "ops")]
pub mod ops;
#[cfg(feature = "seen")]
pub mod seen;

#[cfg(feature = "notify")]
pub use notify::Notify;
#[cfg(feature = "ops")]
pub use ops::Ops;
#[cfg(feature = "seen")]
pub use seen::Seen;
