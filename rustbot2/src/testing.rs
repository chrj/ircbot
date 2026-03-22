//! Helpers for unit-testing bot handler methods.
//!
//! The [`TestContext`] type lets you construct a fake [`Context`] backed by an
//! in-process channel so that handler calls can be made without a live IRC
//! connection.  Replies sent through the context are captured and can be
//! inspected with [`TestContext::replies`] and [`TestContext::next_reply`].
//!
//! # Quick start
//!
//! ```rust,no_run
//! # use rustbot2::testing::TestContext;
//! # struct MyBot;
//! # impl MyBot {
//! #     fn default() -> Self { MyBot }
//! #     async fn ping(&self, ctx: rustbot2::Context) -> rustbot2::Result { ctx.reply("pong!") }
//! # }
//! #[tokio::test]
//! async fn test_ping() {
//!     let bot = MyBot::default();
//!     let mut tc = TestContext::channel("#test", "alice", "!ping");
//!     bot.ping(tc.take_ctx()).await.unwrap();
//!     assert_eq!(
//!         tc.next_reply(),
//!         Some("PRIVMSG #test :alice, pong!\r\n".to_string()),
//!     );
//! }
//! ```

use tokio::sync::mpsc;

use crate::context::{Context, User};

// ─── TestContext ──────────────────────────────────────────────────────────────

/// A fake [`Context`] wired to an in-memory channel, for testing handlers.
///
/// Create one with [`TestContext::channel`] or [`TestContext::private`] for the
/// two most common scenarios, or use [`TestContext::builder`] for full control.
///
/// Call [`TestContext::take_ctx`] to extract the [`Context`] to pass to the
/// handler under test, then inspect outgoing messages with
/// [`TestContext::replies`] or [`TestContext::next_reply`].
///
/// # Example
///
/// ```rust,no_run
/// # use rustbot2::testing::TestContext;
/// # struct MyBot;
/// # impl MyBot {
/// #     fn default() -> Self { MyBot }
/// #     async fn echo(&self, ctx: rustbot2::Context, text: String) -> rustbot2::Result { ctx.say(text) }
/// # }
/// #[tokio::test]
/// async fn echo_says_text() {
///     let bot = MyBot::default();
///     let mut tc = TestContext::channel("#test", "alice", "!echo hi");
///     bot.echo(tc.take_ctx(), "hi".to_string()).await.unwrap();
///     assert_eq!(
///         tc.next_reply(),
///         Some("PRIVMSG #test :hi\r\n".to_string()),
///     );
/// }
/// ```
pub struct TestContext {
    ctx: Option<Context>,
    rx: mpsc::UnboundedReceiver<String>,
}

impl TestContext {
    /// Create a `TestContext` simulating a PRIVMSG sent to a channel.
    ///
    /// `target` is the channel name (e.g. `"#test"`), `sender_nick` is the
    /// IRC nick that sent the message, and `text` is the message body.
    /// [`Context::is_channel`] is set to `true`.
    pub fn channel(target: &str, sender_nick: &str, text: &str) -> Self {
        TestContextBuilder::new()
            .target(target)
            .is_channel(true)
            .sender_nick(sender_nick)
            .text(text)
            .build()
    }

    /// Create a `TestContext` simulating a private message sent directly to
    /// the bot.
    ///
    /// `sender_nick` is the IRC nick that sent the message, and `text` is the
    /// message body.  [`Context::is_channel`] is set to `false`.
    pub fn private(sender_nick: &str, text: &str) -> Self {
        TestContextBuilder::new()
            .target(sender_nick)
            .is_channel(false)
            .sender_nick(sender_nick)
            .text(text)
            .build()
    }

    /// Returns a [`TestContextBuilder`] for constructing a fully-customised
    /// test context.
    pub fn builder() -> TestContextBuilder {
        TestContextBuilder::new()
    }

    /// Take the [`Context`] to pass to the handler under test.
    ///
    /// The underlying send channel remains live so that any messages the
    /// handler sends are still captured.  Use [`TestContext::replies`] or
    /// [`TestContext::next_reply`] to inspect them afterwards.
    ///
    /// # Panics
    ///
    /// Panics if called more than once on the same `TestContext`.
    pub fn take_ctx(&mut self) -> Context {
        self.ctx
            .take()
            .expect("take_ctx called twice on the same TestContext")
    }

    /// Drain and return all IRC protocol lines sent through this context so far.
    ///
    /// Each entry is a complete IRC line ending with `\r\n`, for example
    /// `"PRIVMSG #chan :hello\r\n"`.
    pub fn replies(&mut self) -> Vec<String> {
        let mut out = Vec::new();
        while let Ok(msg) = self.rx.try_recv() {
            out.push(msg);
        }
        out
    }

    /// Return the next IRC protocol line sent through this context, if any.
    pub fn next_reply(&mut self) -> Option<String> {
        self.rx.try_recv().ok()
    }
}

// ─── TestContextBuilder ───────────────────────────────────────────────────────

/// Builder for constructing a [`TestContext`] with custom parameters.
///
/// Obtain one from [`TestContext::builder`].
#[derive(Debug)]
pub struct TestContextBuilder {
    target: String,
    is_channel: bool,
    sender_nick: String,
    sender_user: String,
    sender_host: String,
    bot_nick: String,
    text: String,
    captures: Vec<String>,
}

impl Default for TestContextBuilder {
    fn default() -> Self {
        TestContextBuilder {
            target: "#test".to_string(),
            is_channel: true,
            sender_nick: "tester".to_string(),
            sender_user: "tester".to_string(),
            sender_host: "test.host".to_string(),
            bot_nick: "testbot".to_string(),
            text: String::new(),
            captures: Vec::new(),
        }
    }
}

impl TestContextBuilder {
    /// Create a new builder with sensible defaults.
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the target channel or nick (default: `"#test"`).
    pub fn target(mut self, t: impl Into<String>) -> Self {
        self.target = t.into();
        self
    }

    /// Set whether this is a channel context (default: `true`).
    pub fn is_channel(mut self, v: bool) -> Self {
        self.is_channel = v;
        self
    }

    /// Set the sending user's nick (default: `"tester"`).
    pub fn sender_nick(mut self, n: impl Into<String>) -> Self {
        self.sender_nick = n.into();
        self
    }

    /// Set the sending user's ident (default: `"tester"`).
    pub fn sender_user(mut self, u: impl Into<String>) -> Self {
        self.sender_user = u.into();
        self
    }

    /// Set the sending user's hostname (default: `"test.host"`).
    pub fn sender_host(mut self, h: impl Into<String>) -> Self {
        self.sender_host = h.into();
        self
    }

    /// Set the bot's own nick (default: `"testbot"`).
    pub fn bot_nick(mut self, n: impl Into<String>) -> Self {
        self.bot_nick = n.into();
        self
    }

    /// Set the message text body (default: `""`).
    pub fn text(mut self, t: impl Into<String>) -> Self {
        self.text = t.into();
        self
    }

    /// Pre-populate the [`Context::captures`] list.
    ///
    /// Use this when testing handlers that receive regex or glob captures
    /// directly, rather than relying on the framework to extract them.
    pub fn captures(mut self, caps: Vec<String>) -> Self {
        self.captures = caps;
        self
    }

    /// Build the [`TestContext`].
    pub fn build(self) -> TestContext {
        let (tx, rx) = mpsc::unbounded_channel();
        let raw_line = format!(
            ":{}!{}@{} PRIVMSG {} :{}",
            self.sender_nick, self.sender_user, self.sender_host, self.target, self.text,
        );
        let raw = raw_line
            .parse::<irc_proto::Message>()
            .unwrap_or_else(|_| ":test!t@h PRIVMSG #test :test".parse().unwrap());
        let ctx = Context {
            tx,
            target: self.target,
            is_channel: self.is_channel,
            sender: Some(User {
                nick: self.sender_nick,
                user: self.sender_user,
                host: self.sender_host,
            }),
            raw,
            bot_nick: self.bot_nick,
            captures: self.captures,
        };
        TestContext { ctx: Some(ctx), rx }
    }
}
