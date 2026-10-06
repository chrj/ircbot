//! Tests for plugins on a `#[bot]`: the checks before the connection, and the
//! isolation of plugins.
//!
//! Uses an in-process mock IRC server, as the other non-integration tests do,
//! so Docker is not required.
//!
//! Run with:
//!   cargo test --test plugin_host

use std::sync::Arc;
use std::time::Duration;

use ircbot::{bot, plugin, Bot, CommandOwner, Context, Plugin, Result, StartError};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;
use tokio::sync::{mpsc, Notify};

// ─── mock server ─────────────────────────────────────────────────────────────

struct MockServer {
    addr: String,
    to_bot: mpsc::UnboundedSender<String>,
    from_bot: mpsc::UnboundedReceiver<String>,
}

impl MockServer {
    async fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        let (to_bot_tx, mut to_bot_rx) = mpsc::unbounded_channel::<String>();
        let (from_bot_tx, from_bot_rx) = mpsc::unbounded_channel::<String>();

        tokio::spawn(async move {
            let (sock, _) = listener.accept().await.unwrap();
            let (read_half, mut write_half) = sock.into_split();
            tokio::spawn(async move {
                while let Some(line) = to_bot_rx.recv().await {
                    if write_half.write_all(line.as_bytes()).await.is_err() {
                        break;
                    }
                    let _ = write_half.flush().await;
                }
            });
            let mut reader = BufReader::new(read_half).lines();
            while let Ok(Some(line)) = reader.next_line().await {
                if from_bot_tx.send(line).is_err() {
                    break;
                }
            }
        });

        MockServer {
            addr,
            to_bot: to_bot_tx,
            from_bot: from_bot_rx,
        }
    }

    fn send(&self, line: &str) {
        self.to_bot.send(format!("{line}\r\n")).unwrap();
    }

    /// Send a channel message from `alice` to `#chan`.
    fn say(&self, text: &str) {
        self.send(&format!(":alice!a@h PRIVMSG #chan :{text}"));
    }

    /// The next `PRIVMSG` that the bot sends.
    async fn next_privmsg(&mut self) -> String {
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                let line = self.from_bot.recv().await.expect("bot closed");
                if line.starts_with("PRIVMSG") {
                    return line;
                }
            }
        })
        .await
        .expect("timed out waiting for a PRIVMSG")
    }

    /// Fail if the bot sends a `PRIVMSG` within `window`.
    async fn expect_no_privmsg(&mut self, window: Duration) {
        let res = tokio::time::timeout(window, async {
            loop {
                match self.from_bot.recv().await {
                    Some(line) if line.starts_with("PRIVMSG") => return Some(line),
                    Some(_) => continue,
                    None => return None,
                }
            }
        })
        .await;
        if let Ok(Some(line)) = res {
            panic!("unexpected line: {line:?}");
        }
    }
}

/// Start `bot`, and finish the registration with `server`.
fn run(bot: HostBot, server: &MockServer) -> tokio::task::JoinHandle<()> {
    let task = tokio::spawn(async move {
        let _ = bot.main_loop().await;
    });
    server.send(":server 001 testbot :Welcome");
    task
}

/// The error of `main_loop`, as a `StartError`.
async fn start_error(bot: HostBot) -> StartError {
    let err = bot.main_loop().await.expect_err("main_loop must fail");
    *err.downcast::<StartError>().expect("a StartError")
}

// ─── bots under test ─────────────────────────────────────────────────────────

/// A bot whose handlers all come from plugins.
#[bot]
impl HostBot {}

/// A bot with its own `!echo`, and a command that needs a role.
#[bot]
impl OwnBot {
    #[command("echo")]
    async fn echo(&self, ctx: Context, text: String) -> Result {
        ctx.say(text)
    }
}

#[bot]
impl GuardedBot {
    #[command("restart", role = "owner")]
    async fn restart(&self, ctx: Context) -> Result {
        ctx.say("restarting")
    }
}

// ─── plugins under test ──────────────────────────────────────────────────────

#[plugin(name = "echo")]
impl Echo {
    #[command("echo")]
    async fn echo(&self, ctx: Context, text: String) -> Result {
        ctx.say(text)
    }
}

/// A second plugin with the command of `Echo`.
#[plugin(name = "echo_again")]
impl EchoAgain {
    #[command("echo")]
    async fn echo(&self, ctx: Context, text: String) -> Result {
        ctx.say(text)
    }
}

/// A plugin with the command of `Echo`, in upper case.
#[plugin(name = "echo_upper")]
impl EchoUpper {
    #[command("ECHO")]
    async fn echo(&self, ctx: Context, text: String) -> Result {
        ctx.say(text)
    }
}

#[plugin(name = "admin")]
impl Admin {
    #[command("shutdown", role = "admin")]
    async fn shutdown(&self, ctx: Context) -> Result {
        ctx.say("bye")
    }
}

/// A plugin that blocks on `!wait` until the test releases it.
#[plugin(name = "slow", state = Arc<Notify>)]
impl Slow {
    #[command("wait")]
    async fn wait(&self, ctx: Context) -> Result {
        ctx.say("waiting")?;
        self.state.notified().await;
        ctx.say("released")
    }

    #[command("count")]
    async fn count(&self, ctx: Context, text: String) -> Result {
        ctx.say(format!("count {text}"))
    }
}

/// A plugin that panics on `!boom`.
#[plugin(name = "panicky")]
impl Panicky {
    #[command("boom")]
    async fn boom(&self, _ctx: Context) -> Result {
        panic!("boom handler failed");
    }

    #[command("alive")]
    async fn alive(&self, ctx: Context) -> Result {
        ctx.say("still alive")
    }
}

/// A plugin whose first handler is slower than its second.
#[plugin(name = "ordered")]
impl Ordered {
    #[command("slowly")]
    async fn slowly(&self, ctx: Context) -> Result {
        tokio::time::sleep(Duration::from_millis(200)).await;
        ctx.say("first")
    }

    #[command("quickly")]
    async fn quickly(&self, ctx: Context) -> Result {
        ctx.say("second")
    }
}

/// A plugin made without the attribute, with an invalid name.
struct BadName;

impl Bot for BadName {
    fn handlers() -> Vec<ircbot::HandlerEntry<Self>> {
        Vec::new()
    }
}

impl Plugin for BadName {
    const NAME: &'static str = "Bad-Name";
}

/// A bot whose server does not exist. A check that passes makes the bot try
/// to connect, which fails with `StartError::Connect`.
fn host() -> HostBot {
    HostBot::new("testbot", "127.0.0.1:1", ["#chan"])
}

// ─── #[plugin] ───────────────────────────────────────────────────────────────

#[test]
fn plugin_attribute_sets_the_name() {
    assert_eq!(Echo::NAME, "echo");
    assert_eq!(Slow::NAME, "slow");
}

#[tokio::test]
async fn plugin_attribute_gives_handlers_that_test_bot_can_run() {
    let bot = ircbot::testing::TestBot::new(Echo);

    let replies = bot
        .deliver(":alice!a@h PRIVMSG #chan :!echo hi")
        .await
        .unwrap();

    assert_eq!(replies, vec!["PRIVMSG #chan :hi\r\n".to_string()]);
}

// ─── checks ──────────────────────────────────────────────────────────────────

#[tokio::test]
async fn main_loop_refuses_an_invalid_plugin_name() {
    let err = start_error(host().plugin(BadName)).await;

    assert!(
        matches!(&err, StartError::InvalidName { name } if name == "Bad-Name"),
        "got {err:?}"
    );
}

#[tokio::test]
async fn main_loop_refuses_two_plugins_with_the_same_name() {
    let err = start_error(host().plugin(Echo).plugin(Echo)).await;

    assert_eq!(
        err.to_string(),
        "two plugins have the name \"echo\": give each plugin its own name"
    );
}

#[tokio::test]
async fn main_loop_refuses_two_plugins_with_the_same_command() {
    let err = start_error(host().plugin(Echo).plugin(EchoAgain)).await;

    assert_eq!(
        err.to_string(),
        "plugin \"echo\" and plugin \"echo_again\" both have the command `echo`: remove \
         it from one of them"
    );
}

#[tokio::test]
async fn main_loop_refuses_the_same_command_in_another_case() {
    // The dispatch ignores the case of a command, so `!echo` would run both.
    let err = start_error(host().plugin(Echo).plugin(EchoUpper)).await;

    assert!(
        matches!(&err, StartError::DuplicateCommand { first, second, .. }
            if *first == CommandOwner::Plugin("echo".to_string())
                && *second == CommandOwner::Plugin("echo_upper".to_string())),
        "got {err:?}"
    );
}

#[tokio::test]
async fn main_loop_refuses_a_command_whose_role_is_not_defined() {
    let err = start_error(host().plugin(Admin)).await;

    assert_eq!(
        err.to_string(),
        "the command `shutdown` of plugin \"admin\" needs the role \"admin\", but no role \
         has this name: define it with `with_role`"
    );
}

#[tokio::test]
async fn main_loop_accepts_a_command_whose_role_is_defined() {
    let err = start_error(
        host()
            .with_role("admin", ["*!*@trusted.host"])
            .plugin(Admin),
    )
    .await;

    // The checks passed, so the bot tried to connect.
    assert!(matches!(err, StartError::Connect { .. }), "got {err:?}");
}

#[tokio::test]
async fn main_loop_refuses_a_plugin_command_that_the_bot_has() {
    let bot = OwnBot::new("testbot", "127.0.0.1:1", ["#chan"]).plugin(Echo);

    let err = bot.main_loop().await.expect_err("main_loop must fail");

    assert_eq!(
        err.to_string(),
        "the bot and plugin \"echo\" both have the command `echo`: remove it from one of \
         them"
    );
}

#[tokio::test]
async fn main_loop_refuses_an_own_command_whose_role_is_not_defined() {
    let bot = GuardedBot::new("testbot", "127.0.0.1:1", ["#chan"]);

    let err = bot.main_loop().await.expect_err("main_loop must fail");

    assert_eq!(
        err.to_string(),
        "the command `restart` of the bot needs the role \"owner\", but no role has this \
         name: define it with `with_role`"
    );
}

#[tokio::test]
async fn main_loop_refuses_a_bot_without_a_server() {
    let err = start_error(HostBot::default()).await;

    assert!(matches!(err, StartError::NoServer), "got {err:?}");
}

// ─── running plugins ─────────────────────────────────────────────────────────

#[tokio::test]
async fn each_plugin_gets_its_own_commands() {
    let mut server = MockServer::start().await;
    let bot = HostBot::new("testbot", server.addr.clone(), ["#chan"])
        .plugin(Echo)
        .plugin(Ordered);
    let task = run(bot, &server);

    server.say("!echo hello");
    assert_eq!(server.next_privmsg().await, "PRIVMSG #chan :hello");
    server.say("!quickly");
    assert_eq!(server.next_privmsg().await, "PRIVMSG #chan :second");

    task.abort();
}

#[tokio::test]
async fn a_blocked_plugin_does_not_delay_another_plugin() {
    let mut server = MockServer::start().await;
    let release = Arc::new(Notify::new());
    let bot = HostBot::new("testbot", server.addr.clone(), ["#chan"])
        .plugin(Slow::from_state(Arc::clone(&release)))
        .plugin(Echo);
    let task = run(bot, &server);

    server.say("!wait");
    assert_eq!(server.next_privmsg().await, "PRIVMSG #chan :waiting");

    server.say("!echo meanwhile");
    assert_eq!(server.next_privmsg().await, "PRIVMSG #chan :meanwhile");

    release.notify_one();
    assert_eq!(server.next_privmsg().await, "PRIVMSG #chan :released");

    task.abort();
}

#[tokio::test]
async fn a_plugin_that_panics_gets_the_next_message() {
    let mut server = MockServer::start().await;
    let bot = HostBot::new("testbot", server.addr.clone(), ["#chan"])
        .plugin(Panicky)
        .plugin(Echo);
    let task = run(bot, &server);

    server.say("!boom");
    server.say("!alive");
    assert_eq!(server.next_privmsg().await, "PRIVMSG #chan :still alive");
    server.say("!echo ok");
    assert_eq!(server.next_privmsg().await, "PRIVMSG #chan :ok");

    task.abort();
}

#[tokio::test]
async fn a_plugin_gets_its_messages_in_order() {
    let mut server = MockServer::start().await;
    let bot = HostBot::new("testbot", server.addr.clone(), ["#chan"]).plugin(Ordered);
    let task = run(bot, &server);

    // `!slowly` takes longer, but its reply still comes first.
    server.say("!slowly");
    server.say("!quickly");

    assert_eq!(server.next_privmsg().await, "PRIVMSG #chan :first");
    assert_eq!(server.next_privmsg().await, "PRIVMSG #chan :second");

    task.abort();
}

#[tokio::test]
async fn a_full_queue_drops_new_messages_for_that_plugin() {
    let mut server = MockServer::start().await;
    let release = Arc::new(Notify::new());
    let bot = HostBot::new("testbot", server.addr.clone(), ["#chan"])
        .with_queue_capacity(1)
        .plugin(Slow::from_state(Arc::clone(&release)));
    let task = run(bot, &server);

    // The task of the plugin holds `!wait`, so its queue is empty.
    server.say("!wait");
    assert_eq!(server.next_privmsg().await, "PRIVMSG #chan :waiting");

    // The queue has space for one message, so `b` and `c` are dropped.
    server.say("!count a");
    server.say("!count b");
    server.say("!count c");
    // Let the dispatch handle the three messages before the release.
    tokio::time::sleep(Duration::from_millis(200)).await;
    release.notify_one();

    assert_eq!(server.next_privmsg().await, "PRIVMSG #chan :released");
    assert_eq!(server.next_privmsg().await, "PRIVMSG #chan :count a");
    server.expect_no_privmsg(Duration::from_millis(300)).await;

    task.abort();
}

#[tokio::test]
async fn dropping_the_main_loop_stops_a_blocked_plugin() {
    let mut server = MockServer::start().await;
    let release = Arc::new(Notify::new());
    let bot = HostBot::new("testbot", server.addr.clone(), ["#chan"])
        .plugin(Slow::from_state(Arc::clone(&release)));
    let task = run(bot, &server);

    // The plugin blocks, and the test never releases it.
    server.say("!wait");
    assert_eq!(server.next_privmsg().await, "PRIVMSG #chan :waiting");
    assert_eq!(Arc::strong_count(&release), 2);

    task.abort();
    let _ = task.await;

    // The task of the plugin held the plugin and its state. When it stops,
    // only the test holds the state.
    let stopped = tokio::time::timeout(Duration::from_secs(2), async {
        while Arc::strong_count(&release) > 1 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
    assert!(stopped.is_ok(), "the plugin task still runs");
}

// ─── #[bot] still works next to #[plugin] ────────────────────────────────────

#[bot]
impl PlainBot {
    #[command("ping")]
    async fn ping(&self, ctx: Context) -> Result {
        ctx.say("pong")
    }
}

#[test]
fn bot_attribute_is_not_a_plugin() {
    assert_eq!(PlainBot::handlers().len(), 1);
}
