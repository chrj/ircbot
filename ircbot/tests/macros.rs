//! Runtime tests for the `#[bot]` / `#[command]` / `#[on(...)]` procedural
//! macros: the `Trigger` variants they generate, trigger precedence, the
//! handler count/shape, and the argument-extraction wrapper.
//!
//! Compile-time behaviour (invalid cron / timezone / non-simple type) is
//! covered separately by the trybuild UI tests in `tests/macro_ui.rs`.
//!
//! These tests inspect the handler list that `#[bot]` gives through its
//! implementation of the `ircbot::Bot` trait.
//!
//! Run with:
//!   cargo test --test macros

use ircbot::handler::HandlerEntry;
use ircbot::testing::TestContext;
use ircbot::{bot, Bot, Context, Result, Scope, Trigger, User};

// ─── bot under test ──────────────────────────────────────────────────────────

#[bot]
impl MacroBot {
    // [0]
    #[command("ping")]
    async fn ping(&self, ctx: Context) -> Result {
        ctx.say("pong")
    }

    // [1] — target propagation
    #[command("hi", target = "#rust")]
    async fn hi_rust(&self, ctx: Context) -> Result {
        ctx.say("hi")
    }

    // [2] — Message trigger with a single String capture arg
    #[on(message = "hello *")]
    async fn greet(&self, ctx: Context, who: String) -> Result {
        ctx.say(format!("hi {who}"))
    }

    // [3] — Event trigger with a regex
    #[on(event = "JOIN", regex = "(.+)")]
    async fn on_join(&self, ctx: Context) -> Result {
        ctx.say("joined")
    }

    // [4] — Mention trigger
    #[on(mention)]
    async fn on_mention(&self, ctx: Context) -> Result {
        ctx.say("mentioned")
    }

    // [5] — Cron trigger
    #[on(cron = "0 0 * * * *", tz = "UTC")]
    async fn on_cron(&self, ctx: Context) -> Result {
        ctx.say("cron")
    }

    // [6] — precedence: message wins over command/event/mention/cron
    #[on(
        message = "winner",
        command = "loser",
        event = "ALSO_LOSER",
        mention,
        cron = "0 0 * * * *"
    )]
    async fn precedence(&self, ctx: Context) -> Result {
        ctx.say("p")
    }

    // [7] — two String capture args
    #[on(event = "PRIVMSG", regex = r"(\w+) (\w+)")]
    async fn two_args(&self, ctx: Context, a: String, b: String) -> Result {
        ctx.say(format!("{a}-{b}"))
    }

    // [8] — User arg extraction
    #[command("whoami")]
    async fn whoami(&self, ctx: Context, user: User) -> Result {
        ctx.say(user.nick)
    }

    // [9] — typed scalar command args parsed from the tail
    #[command("add")]
    async fn add(&self, ctx: Context, a: i64, b: i64) -> Result {
        ctx.say(format!("{}", a + b))
    }

    // [10] — scalar arg followed by a trailing rest-of-line String
    #[command("repeat")]
    async fn repeat(&self, ctx: Context, n: u32, text: String) -> Result {
        ctx.say(text.repeat(n as usize))
    }

    // [11] — optional trailing scalar arg
    #[command("maybe")]
    async fn maybe(&self, ctx: Context, n: Option<u32>) -> Result {
        ctx.say(format!("{n:?}"))
    }

    // [12] — variadic trailing Vec<String>
    #[command("tags")]
    async fn tags(&self, ctx: Context, items: Vec<String>) -> Result {
        ctx.say(items.join(","))
    }

    // [13] — role-gated command
    #[command("ban", role = "admin")]
    async fn ban(&self, ctx: Context) -> Result {
        ctx.say("banned")
    }

    // [14] — Action trigger with a String capture arg
    #[on(action = "slaps * around a bit with a large trout")]
    async fn trout(&self, ctx: Context, victim: String) -> Result {
        ctx.say(format!("poor {victim}"))
    }

    // [15] — Ctcp trigger with a target
    #[on(ctcp = "TIME", target = "#rust")]
    async fn time(&self, ctx: Context) -> Result {
        ctx.say("time")
    }

    // [16] — include_self on an `#[on(...)]` trigger
    #[on(event = "PART", include_self)]
    async fn own_part(&self, ctx: Context) -> Result {
        ctx.say("parted")
    }

    // [17] — include_self on a command
    #[command("selfcount", include_self)]
    async fn selfcount(&self, ctx: Context) -> Result {
        ctx.say("counted")
    }

    // [18] — raw on an `#[on(...)]` trigger
    #[on(message = "raw *", raw)]
    async fn raw_message(&self, ctx: Context) -> Result {
        ctx.say("raw")
    }

    // [19] — raw on a command
    #[command("rawcount", raw)]
    async fn rawcount(&self, ctx: Context) -> Result {
        ctx.say("counted")
    }

    // [20] — scope on an `#[on(...)]` trigger
    #[on(message = "scoped *", scope = "channel")]
    async fn scoped_message(&self, ctx: Context) -> Result {
        ctx.say("scoped")
    }

    // [21] — scope on a command
    #[command("scopedcmd", scope = "private")]
    async fn scoped_command(&self, ctx: Context) -> Result {
        ctx.say("scoped")
    }

    // Plain (non-annotated) method — must remain callable and produce NO entry.
    fn helper(&self) -> u32 {
        42
    }
}

// ─── helpers ─────────────────────────────────────────────────────────────────

fn handlers() -> Vec<HandlerEntry<MacroBot>> {
    MacroBot::handlers()
}

/// Invoke a handler entry with a context taken from `tc`, returning the first
/// reply line written.
async fn invoke(entry: &HandlerEntry<MacroBot>, mut tc: TestContext) -> Option<String> {
    let bot = std::sync::Arc::new(MacroBot::default());
    (entry.handler)(bot, tc.take_ctx()).await.unwrap();
    tc.next_reply()
}

// ─── trigger variants ────────────────────────────────────────────────────────

#[test]
fn command_attr_yields_command_trigger() {
    match &handlers()[0].trigger {
        Trigger::Command { name, target, .. } => {
            assert_eq!(name, "ping");
            assert_eq!(target.as_deref(), None);
        }
        other => panic!("expected Command, got {other:?}"),
    }
}

#[test]
fn command_target_propagates() {
    match &handlers()[1].trigger {
        Trigger::Command { name, target, .. } => {
            assert_eq!(name, "hi");
            assert_eq!(target.as_deref(), Some("#rust"));
        }
        other => panic!("expected Command, got {other:?}"),
    }
}

#[test]
fn on_message_yields_message_trigger() {
    match &handlers()[2].trigger {
        Trigger::Message { pattern, .. } => assert_eq!(pattern, "hello *"),
        other => panic!("expected Message, got {other:?}"),
    }
}

#[test]
fn on_event_with_regex_yields_event_trigger() {
    match &handlers()[3].trigger {
        Trigger::Event {
            event,
            target,
            regex,
        } => {
            assert_eq!(event, "JOIN");
            assert_eq!(target.as_deref(), None);
            assert_eq!(regex.as_deref(), Some("(.+)"));
        }
        other => panic!("expected Event, got {other:?}"),
    }
}

#[test]
fn on_mention_yields_mention_trigger() {
    assert!(matches!(
        &handlers()[4].trigger,
        Trigger::Mention { target: None }
    ));
}

#[test]
fn on_cron_yields_cron_trigger() {
    match &handlers()[5].trigger {
        Trigger::Cron { schedule, tz, .. } => {
            assert_eq!(schedule, "0 0 * * * *");
            assert_eq!(tz, "UTC");
        }
        other => panic!("expected Cron, got {other:?}"),
    }
}

#[test]
fn on_action_yields_action_trigger() {
    match &handlers()[14].trigger {
        Trigger::Action { pattern, target } => {
            assert_eq!(pattern, "slaps * around a bit with a large trout");
            assert_eq!(target.as_deref(), None);
        }
        other => panic!("expected Action, got {other:?}"),
    }
}

#[test]
fn on_ctcp_yields_ctcp_trigger() {
    match &handlers()[15].trigger {
        Trigger::Ctcp { command, target } => {
            assert_eq!(command, "TIME");
            assert_eq!(target.as_deref(), Some("#rust"));
        }
        other => panic!("expected Ctcp, got {other:?}"),
    }
}

#[test]
fn message_wins_trigger_precedence() {
    // message > command > event > mention > cron — only the message survives.
    match &handlers()[6].trigger {
        Trigger::Message { pattern, .. } => assert_eq!(pattern, "winner"),
        other => panic!("expected Message (precedence), got {other:?}"),
    }
}

// ─── handler count / shape ───────────────────────────────────────────────────

#[test]
fn only_annotated_methods_produce_handler_entries() {
    // 22 annotated methods; the plain `helper` produces no entry.
    assert_eq!(handlers().len(), 22);
}

#[test]
fn command_role_propagates() {
    match &handlers()[13].trigger {
        Trigger::Command { name, role, .. } => {
            assert_eq!(name, "ban");
            assert_eq!(role.as_deref(), Some("admin"));
        }
        other => panic!("expected Command, got {other:?}"),
    }
}

#[test]
fn command_without_role_has_none() {
    match &handlers()[0].trigger {
        Trigger::Command { name, role, .. } => {
            assert_eq!(name, "ping");
            assert_eq!(role.as_deref(), None);
        }
        other => panic!("expected Command, got {other:?}"),
    }
}

#[test]
fn plain_method_remains_callable() {
    assert_eq!(MacroBot::default().helper(), 42);
}

// ─── argument extraction (build_wrapper) ─────────────────────────────────────

#[tokio::test]
async fn string_arg_filled_from_captures_when_present() {
    let entry = &handlers()[2]; // greet(who: String)
    let tc = TestContext::builder()
        .target("#test")
        .captures(vec!["world".to_string()])
        .build();
    assert_eq!(
        invoke(entry, tc).await,
        Some("PRIVMSG #test :hi world\r\n".to_string())
    );
}

#[tokio::test]
async fn string_arg_falls_back_to_message_text_when_no_captures() {
    let entry = &handlers()[2]; // greet(who: String)
    let tc = TestContext::builder()
        .target("#test")
        .text("raw body")
        .build();
    assert_eq!(
        invoke(entry, tc).await,
        Some("PRIVMSG #test :hi raw body\r\n".to_string())
    );
}

#[tokio::test]
async fn two_string_args_pull_successive_captures() {
    let entry = &handlers()[7]; // two_args(a, b: String)
    let tc = TestContext::builder()
        .target("#test")
        .captures(vec!["foo".to_string(), "bar".to_string()])
        .build();
    assert_eq!(
        invoke(entry, tc).await,
        Some("PRIVMSG #test :foo-bar\r\n".to_string())
    );
}

#[tokio::test]
async fn action_string_arg_filled_from_captures() {
    let entry = &handlers()[14]; // trout(victim: String)
    let tc = TestContext::builder()
        .target("#test")
        .captures(vec!["bob".to_string()])
        .build();
    assert_eq!(
        invoke(entry, tc).await,
        Some("PRIVMSG #test :poor bob\r\n".to_string())
    );
}

#[tokio::test]
async fn user_arg_filled_from_sender() {
    let entry = &handlers()[8]; // whoami(user: User)
    let tc = TestContext::builder()
        .target("#test")
        .sender_nick("zaphod")
        .build();
    assert_eq!(
        invoke(entry, tc).await,
        Some("PRIVMSG #test :zaphod\r\n".to_string())
    );
}

// ─── typed command arguments ─────────────────────────────────────────────────

#[tokio::test]
async fn typed_scalar_args_parsed_from_command_tail() {
    let entry = &handlers()[9]; // add(a: i64, b: i64)
    let tc = TestContext::builder()
        .target("#test")
        .captures(vec!["3 4".to_string()])
        .build();
    assert_eq!(
        invoke(entry, tc).await,
        Some("PRIVMSG #test :7\r\n".to_string())
    );
}

#[tokio::test]
async fn typed_scalar_parse_failure_replies_usage() {
    let entry = &handlers()[9]; // add(a: i64, b: i64)
    let tc = TestContext::builder()
        .target("#test")
        .captures(vec!["foo 4".to_string()])
        .build();
    assert_eq!(
        invoke(entry, tc).await,
        Some("PRIVMSG #test :tester, usage: !add <a> <b>\r\n".to_string())
    );
}

#[tokio::test]
async fn missing_required_arg_replies_usage() {
    let entry = &handlers()[9]; // add(a: i64, b: i64)
    let tc = TestContext::builder()
        .target("#test")
        .captures(vec!["3".to_string()])
        .build();
    assert_eq!(
        invoke(entry, tc).await,
        Some("PRIVMSG #test :tester, usage: !add <a> <b>\r\n".to_string())
    );
}

#[tokio::test]
async fn trailing_string_captures_rest_of_line() {
    let entry = &handlers()[10]; // repeat(n: u32, text: String)
    let tc = TestContext::builder()
        .target("#test")
        .captures(vec!["2 ab cd".to_string()])
        .build();
    // n = 2, text = "ab cd" (rest, verbatim) -> repeated twice.
    assert_eq!(
        invoke(entry, tc).await,
        Some("PRIVMSG #test :ab cdab cd\r\n".to_string())
    );
}

#[tokio::test]
async fn optional_arg_is_some_when_present() {
    let entry = &handlers()[11]; // maybe(n: Option<u32>)
    let tc = TestContext::builder()
        .target("#test")
        .captures(vec!["42".to_string()])
        .build();
    assert_eq!(
        invoke(entry, tc).await,
        Some("PRIVMSG #test :Some(42)\r\n".to_string())
    );
}

#[tokio::test]
async fn optional_arg_is_none_when_absent() {
    let entry = &handlers()[11]; // maybe(n: Option<u32>)
    let tc = TestContext::builder().target("#test").build();
    assert_eq!(
        invoke(entry, tc).await,
        Some("PRIVMSG #test :None\r\n".to_string())
    );
}

#[tokio::test]
async fn optional_arg_present_but_unparseable_replies_usage() {
    let entry = &handlers()[11]; // maybe(n: Option<u32>)
    let tc = TestContext::builder()
        .target("#test")
        .captures(vec!["notanumber".to_string()])
        .build();
    assert_eq!(
        invoke(entry, tc).await,
        Some("PRIVMSG #test :tester, usage: !maybe [n]\r\n".to_string())
    );
}

#[tokio::test]
async fn variadic_vec_collects_remaining_tokens() {
    let entry = &handlers()[12]; // tags(items: Vec<String>)
    let tc = TestContext::builder()
        .target("#test")
        .captures(vec!["red green blue".to_string()])
        .build();
    assert_eq!(
        invoke(entry, tc).await,
        Some("PRIVMSG #test :red,green,blue\r\n".to_string())
    );
}

// ─── from_state constructor ────────────────────────────────────────────────────

/// State whose `Default` is *not* test-safe: it records whether it was built
/// the easy way, so a test can prove `from_state` skipped `Default`.
struct StateBotState {
    greeting: String,
    from_default: bool,
}

impl Default for StateBotState {
    fn default() -> Self {
        // Stand-in for real work (opening a DB, reading the environment, …)
        // that a unit test must not trigger.
        StateBotState {
            greeting: "default".to_string(),
            from_default: true,
        }
    }
}

#[bot(state = StateBotState)]
impl StateBot {
    #[on(mention)]
    async fn hello(&self, ctx: Context, _text: String) -> Result {
        ctx.reply(self.state.greeting.clone())
    }
}

#[tokio::test]
async fn from_state_injects_given_state_and_skips_default() {
    let bot = StateBot::from_state(StateBotState {
        greeting: "hi!".to_string(),
        from_default: false,
    });
    // The injected state is used verbatim — `Default` never ran.
    assert!(!bot.state.from_default);

    let mut tc = TestContext::channel("#test", "alice", "statebot: yo");
    bot.hello(tc.take_ctx(), "yo".to_string()).await.unwrap();
    assert_eq!(
        tc.next_reply(),
        Some("PRIVMSG #test :alice, hi!\r\n".to_string())
    );
}

// ─── include_self ────────────────────────────────────────────────────────────

/// The entry whose trigger is the `event` with this name.
fn event_entry(event: &str) -> HandlerEntry<MacroBot> {
    handlers()
        .into_iter()
        .find(|e| matches!(&e.trigger, Trigger::Event { event: e, .. } if e == event))
        .unwrap_or_else(|| panic!("no handler for event {event}"))
}

/// The entry whose trigger is the command with this name.
fn command_entry(name: &str) -> HandlerEntry<MacroBot> {
    handlers()
        .into_iter()
        .find(|e| matches!(&e.trigger, Trigger::Command { name: n, .. } if n == name))
        .unwrap_or_else(|| panic!("no handler for command {name}"))
}

#[test]
fn handlers_do_not_get_own_messages_by_default() {
    assert!(!event_entry("JOIN").include_self);
    assert!(!command_entry("ping").include_self);
}

#[test]
fn include_self_on_an_on_trigger_sets_the_flag() {
    assert!(event_entry("PART").include_self);
}

#[test]
fn include_self_on_a_command_sets_the_flag() {
    assert!(command_entry("selfcount").include_self);
}

#[test]
fn include_self_keeps_the_other_command_arguments() {
    match &command_entry("selfcount").trigger {
        Trigger::Command { name, target, role } => {
            assert_eq!(name, "selfcount");
            assert_eq!(target.as_deref(), None);
            assert_eq!(role.as_deref(), None);
        }
        other => panic!("expected Command, got {other:?}"),
    }
}

// ─── raw ─────────────────────────────────────────────────────────────────────

/// The entry whose trigger is the message glob with this pattern.
fn message_entry(pattern: &str) -> HandlerEntry<MacroBot> {
    handlers()
        .into_iter()
        .find(|e| matches!(&e.trigger, Trigger::Message { pattern: p, .. } if p == pattern))
        .unwrap_or_else(|| panic!("no handler for message {pattern}"))
}

#[test]
fn handlers_match_without_the_formatting_codes_by_default() {
    assert!(!message_entry("hello *").raw_text);
    assert!(!command_entry("ping").raw_text);
}

#[test]
fn raw_on_an_on_trigger_sets_the_flag() {
    assert!(message_entry("raw *").raw_text);
}

#[test]
fn raw_on_a_command_sets_the_flag() {
    assert!(command_entry("rawcount").raw_text);
}

// ─── scope ───────────────────────────────────────────────────────────────────

#[test]
fn handlers_answer_every_target_by_default() {
    assert_eq!(message_entry("hello *").scope, Scope::Any);
    assert_eq!(command_entry("ping").scope, Scope::Any);
}

#[test]
fn scope_channel_on_an_on_trigger_sets_the_field() {
    assert_eq!(message_entry("scoped *").scope, Scope::Channel);
}

#[test]
fn scope_private_on_a_command_sets_the_field() {
    assert_eq!(command_entry("scopedcmd").scope, Scope::Private);
}
