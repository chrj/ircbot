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
//! # use ircbot::testing::TestContext;
//! # struct MyBot;
//! # impl MyBot {
//! #     fn default() -> Self { MyBot }
//! #     async fn ping(&self, ctx: ircbot::Context) -> ircbot::Result { ctx.reply("pong!") }
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
//!
//! # Constructing the bot under test
//!
//! A handler test needs a bot instance, but never a live connection — replies
//! are captured by the [`TestContext`], not sent over a socket. The `#[bot]`
//! macro gives you two connection-free constructors:
//!
//! * [`Default::default`] — for a stateless bot, or one whose state's `Default`
//!   is cheap and side-effect-free.
//! * `from_state(state)` — generated only when the bot declares
//!   `#[bot(state = T)]`. Use it whenever `T::default()` does real work
//!   (opens a database, reads config from the environment, dials a service).
//!   Building such a bot with `Default::default` would run that work — and
//!   often panic — inside your unit test. `from_state` lets you inject a
//!   purpose-built state instead.
//!
//! ```rust,no_run
//! # use ircbot::{bot, Context, Result};
//! # use ircbot::testing::TestContext;
//! #[derive(Default)]
//! struct State { greeting: String }
//!
//! #[bot(state = State)]
//! impl Greeter {
//!     #[on(mention)]
//!     async fn hello(&self, ctx: Context, _text: String) -> Result {
//!         ctx.reply(self.state.greeting.clone())
//!     }
//! }
//!
//! #[tokio::test]
//! async fn greets_with_configured_text() {
//!     // Inject a known state rather than going through `Default`.
//!     let bot = Greeter::from_state(State { greeting: "hi!".into() });
//!     let mut tc = TestContext::channel("#test", "alice", "greeter: yo");
//!     bot.hello(tc.take_ctx(), "yo".into()).await.unwrap();
//!     // `reply` prefixes the sender's nick in a channel.
//!     assert_eq!(tc.next_reply().as_deref(), Some("PRIVMSG #test :alice, hi!\r\n"));
//! }
//! ```
//!
//! # Best practices
//!
//! * **Test handlers, not the framework.** Call the handler method directly
//!   with a [`TestContext`]-built [`Context`]; the macro's dispatch, matching,
//!   and connection handling are covered by the crate's own tests.
//! * **Build real, isolated state.** Prefer a genuine state value over mocks —
//!   an in-memory store, or a temp-dir fixture (e.g. via the `tempfile` crate)
//!   for file-backed state, created fresh per test so cases don't interleave.
//! * **Assert on the wire bytes.** [`TestContext::next_reply`] and
//!   [`TestContext::replies`] return complete IRC lines including the trailing
//!   `\r\n`; assert against those exact strings to catch formatting changes.
//! * **Drive one handler per test.** Each `#[on(...)]` method is independent;
//!   testing them in isolation keeps failures pinpointed. Use
//!   [`TestContext::channel`], [`TestContext::private`], or
//!   [`TestContext::builder`] to reproduce the scenario each handler expects.
//! * **Cover the silent paths.** A handler that filters or ignores some input
//!   should produce no reply — assert that `next_reply()` returns `None`, not
//!   just that the happy path works.

use tokio::sync::mpsc;

use crate::context::{Context, User};
use crate::types::{Channel, Nick, Target};

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
/// # use ircbot::testing::TestContext;
/// # struct MyBot;
/// # impl MyBot {
/// #     fn default() -> Self { MyBot }
/// #     async fn echo(&self, ctx: ircbot::Context, text: String) -> ircbot::Result { ctx.say(text) }
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
    /// [`Context::is_channel`] returns `true`.
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
    /// message body.  [`Context::is_channel`] returns `false`.
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
        let target = if self.is_channel {
            Target::Channel(Channel::from(self.target))
        } else {
            Target::User(Nick::from(self.target))
        };
        let ctx = Context {
            tx,
            target,
            sender: Some(User {
                nick: Nick::from(self.sender_nick),
                user: self.sender_user,
                host: self.sender_host,
            }),
            raw,
            bot_nick: Nick::from(self.bot_nick),
            captures: self.captures,
        };
        TestContext { ctx: Some(ctx), rx }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── TestContext::channel ──────────────────────────────────────────────────

    #[test]
    fn channel_sets_is_channel_true() {
        let mut tc = TestContext::channel("#rust", "alice", "hello");
        assert!(tc.take_ctx().is_channel());
    }

    #[test]
    fn channel_sets_target() {
        let mut tc = TestContext::channel("#rust", "alice", "hello");
        assert_eq!(tc.take_ctx().target.as_str(), "#rust");
    }

    #[test]
    fn channel_sets_sender_nick() {
        let mut tc = TestContext::channel("#rust", "alice", "hello");
        let ctx = tc.take_ctx();
        assert_eq!(ctx.sender.as_ref().unwrap().nick, "alice");
    }

    #[test]
    fn channel_sets_message_text() {
        let mut tc = TestContext::channel("#rust", "alice", "hello world");
        assert_eq!(tc.take_ctx().message_text(), "hello world");
    }

    // ── TestContext::private ──────────────────────────────────────────────────

    #[test]
    fn private_sets_is_channel_false() {
        let mut tc = TestContext::private("alice", "hey bot");
        assert!(!tc.take_ctx().is_channel());
    }

    #[test]
    fn private_sets_target_to_sender_nick() {
        let mut tc = TestContext::private("alice", "hey bot");
        assert_eq!(tc.take_ctx().target.as_str(), "alice");
    }

    #[test]
    fn private_sets_sender_nick() {
        let mut tc = TestContext::private("alice", "hey bot");
        let ctx = tc.take_ctx();
        assert_eq!(ctx.sender.as_ref().unwrap().nick, "alice");
    }

    // ── take_ctx ─────────────────────────────────────────────────────────────

    #[test]
    fn take_ctx_returns_context() {
        let mut tc = TestContext::channel("#test", "nick", "msg");
        let ctx = tc.take_ctx();
        assert_eq!(ctx.target.as_str(), "#test");
    }

    #[test]
    #[should_panic(expected = "take_ctx called twice")]
    fn take_ctx_panics_on_second_call() {
        let mut tc = TestContext::channel("#test", "nick", "msg");
        let _ = tc.take_ctx();
        let _ = tc.take_ctx(); // must panic
    }

    // ── next_reply ────────────────────────────────────────────────────────────

    #[test]
    fn next_reply_returns_none_when_no_messages_sent() {
        let mut tc = TestContext::channel("#test", "nick", "msg");
        // Context not even consumed — nothing sent.
        assert_eq!(tc.next_reply(), None);
    }

    #[test]
    fn next_reply_captures_say() {
        let mut tc = TestContext::channel("#test", "nick", "msg");
        tc.take_ctx().say("hello").unwrap();
        assert_eq!(
            tc.next_reply(),
            Some("PRIVMSG #test :hello\r\n".to_string()),
        );
    }

    #[test]
    fn next_reply_returns_messages_in_order() {
        let mut tc = TestContext::channel("#test", "nick", "msg");
        let ctx = tc.take_ctx();
        ctx.say("first").unwrap();
        ctx.say("second").unwrap();
        assert_eq!(
            tc.next_reply(),
            Some("PRIVMSG #test :first\r\n".to_string()),
        );
        assert_eq!(
            tc.next_reply(),
            Some("PRIVMSG #test :second\r\n".to_string()),
        );
        assert_eq!(tc.next_reply(), None);
    }

    // ── replies ───────────────────────────────────────────────────────────────

    #[test]
    fn replies_returns_empty_vec_when_nothing_sent() {
        let mut tc = TestContext::channel("#test", "nick", "msg");
        assert!(tc.replies().is_empty());
    }

    #[test]
    fn replies_drains_all_messages_at_once() {
        let mut tc = TestContext::channel("#test", "nick", "msg");
        let ctx = tc.take_ctx();
        ctx.say("one").unwrap();
        ctx.say("two").unwrap();
        ctx.say("three").unwrap();
        let msgs = tc.replies();
        assert_eq!(msgs.len(), 3);
        assert_eq!(msgs[0], "PRIVMSG #test :one\r\n");
        assert_eq!(msgs[1], "PRIVMSG #test :two\r\n");
        assert_eq!(msgs[2], "PRIVMSG #test :three\r\n");
    }

    #[test]
    fn replies_is_empty_after_being_drained() {
        let mut tc = TestContext::channel("#test", "nick", "msg");
        tc.take_ctx().say("hi").unwrap();
        let _ = tc.replies();
        assert!(tc.replies().is_empty());
    }

    // ── TestContextBuilder defaults ───────────────────────────────────────────

    #[test]
    fn builder_default_target_is_test_channel() {
        let mut tc = TestContextBuilder::new().build();
        assert_eq!(tc.take_ctx().target.as_str(), "#test");
    }

    #[test]
    fn builder_default_is_channel_true() {
        let mut tc = TestContextBuilder::new().build();
        assert!(tc.take_ctx().is_channel());
    }

    #[test]
    fn builder_default_sender_nick_is_tester() {
        let mut tc = TestContextBuilder::new().build();
        let ctx = tc.take_ctx();
        assert_eq!(ctx.sender.as_ref().unwrap().nick, "tester");
    }

    #[test]
    fn builder_default_bot_nick_is_testbot() {
        let mut tc = TestContextBuilder::new().build();
        assert_eq!(tc.take_ctx().bot_nick, "testbot");
    }

    #[test]
    fn builder_default_captures_empty() {
        let mut tc = TestContextBuilder::new().build();
        assert!(tc.take_ctx().captures.is_empty());
    }

    // ── TestContextBuilder setters ────────────────────────────────────────────

    #[test]
    fn builder_target_overrides_default() {
        let mut tc = TestContextBuilder::new().target("#general").build();
        assert_eq!(tc.take_ctx().target.as_str(), "#general");
    }

    #[test]
    fn builder_is_channel_false_overrides_default() {
        let mut tc = TestContextBuilder::new().is_channel(false).build();
        assert!(!tc.take_ctx().is_channel());
    }

    #[test]
    fn builder_sender_nick_overrides_default() {
        let mut tc = TestContextBuilder::new().sender_nick("bob").build();
        let ctx = tc.take_ctx();
        assert_eq!(ctx.sender.as_ref().unwrap().nick, "bob");
    }

    #[test]
    fn builder_sender_user_overrides_default() {
        let mut tc = TestContextBuilder::new().sender_user("bobident").build();
        let ctx = tc.take_ctx();
        assert_eq!(ctx.sender.as_ref().unwrap().user, "bobident");
    }

    #[test]
    fn builder_sender_host_overrides_default() {
        let mut tc = TestContextBuilder::new().sender_host("example.com").build();
        let ctx = tc.take_ctx();
        assert_eq!(ctx.sender.as_ref().unwrap().host, "example.com");
    }

    #[test]
    fn builder_bot_nick_overrides_default() {
        let mut tc = TestContextBuilder::new().bot_nick("mybot").build();
        assert_eq!(tc.take_ctx().bot_nick, "mybot");
    }

    #[test]
    fn builder_text_sets_message_text() {
        let mut tc = TestContextBuilder::new().text("hello there").build();
        assert_eq!(tc.take_ctx().message_text(), "hello there");
    }

    #[test]
    fn builder_captures_sets_captures_list() {
        let caps = vec!["foo".to_string(), "bar".to_string()];
        let mut tc = TestContextBuilder::new().captures(caps.clone()).build();
        assert_eq!(tc.take_ctx().captures, caps);
    }

    // ── reply / say helpers via TestContext ───────────────────────────────────

    #[test]
    fn reply_in_channel_prefixes_nick() {
        let mut tc = TestContext::channel("#test", "alice", "msg");
        tc.take_ctx().reply("hi").unwrap();
        assert_eq!(
            tc.next_reply(),
            Some("PRIVMSG #test :alice, hi\r\n".to_string()),
        );
    }

    #[test]
    fn reply_in_query_sends_to_sender() {
        let mut tc = TestContext::private("alice", "msg");
        tc.take_ctx().reply("hi").unwrap();
        assert_eq!(tc.next_reply(), Some("PRIVMSG alice :hi\r\n".to_string()),);
    }
}
