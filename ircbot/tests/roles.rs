//! Roles that match the services account of a sender: the IRCv3 capability
//! `account-tag`, from the request to the check of a command.
//!
//! Each test runs a real `main_loop` against a scripted server on loopback.
//!
//! Run with:
//!   cargo test --test roles

use std::time::Duration;

use ircbot::{bot, Context, Result, Role, StartError};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;
use tokio::sync::mpsc;

/// How the server answers: when a line from the bot starts with `.0`, it
/// sends `.1`.
type Script = Vec<(&'static str, Vec<&'static str>)>;

/// A server on loopback that answers the bot from a script, and that a test
/// can also send lines with.
struct ScriptedServer {
    addr: String,
    to_bot: mpsc::UnboundedSender<String>,
    from_bot: mpsc::UnboundedReceiver<String>,
}

impl ScriptedServer {
    async fn start(script: Script) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        let (to_bot, mut to_bot_rx) = mpsc::unbounded_channel::<String>();
        let (from_bot_tx, from_bot) = mpsc::unbounded_channel::<String>();
        let scripted = to_bot.clone();

        tokio::spawn(async move {
            let (sock, _) = listener.accept().await.unwrap();
            let (read, mut write) = sock.into_split();
            tokio::spawn(async move {
                while let Some(line) = to_bot_rx.recv().await {
                    if write.write_all(line.as_bytes()).await.is_err() {
                        break;
                    }
                }
            });
            let mut lines = BufReader::new(read).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                if let Some((_, replies)) = script.iter().find(|(p, _)| line.starts_with(p)) {
                    for reply in replies {
                        let _ = scripted.send(format!("{reply}\r\n"));
                    }
                }
                if from_bot_tx.send(line).is_err() {
                    break;
                }
            }
        });

        ScriptedServer {
            addr,
            to_bot,
            from_bot,
        }
    }

    fn send(&self, line: &str) {
        self.to_bot.send(format!("{line}\r\n")).unwrap();
    }

    /// The next line from the bot that `predicate` accepts.
    async fn expect(&mut self, predicate: impl Fn(&str) -> bool) -> String {
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                let line = self.from_bot.recv().await.expect("bot closed");
                if predicate(&line) {
                    return line;
                }
            }
        })
        .await
        .expect("timed out waiting for a line")
    }

    /// Fail if the bot sends a line that `predicate` accepts within `window`.
    async fn expect_none(&mut self, window: Duration, predicate: impl Fn(&str) -> bool) {
        let res = tokio::time::timeout(window, async {
            loop {
                match self.from_bot.recv().await {
                    Some(line) if predicate(&line) => return Some(line),
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

#[bot]
impl AdminBot {
    #[command("restart", role = "admin")]
    async fn restart(&self, ctx: Context) -> Result {
        ctx.say("restarting")
    }
}

#[tokio::test]
async fn an_account_role_needs_the_account_tag_capability() {
    // The server offers capabilities, but not `account-tag`.
    let server = ScriptedServer::start(vec![("CAP LS", vec![":srv CAP * LS :server-time"])]).await;
    let bot = AdminBot::new("testbot", server.addr.clone(), ["#chan"])
        .with_role("admin", Role::account(["alice"]));

    // Without the check, `main_loop` would run on, so limit the wait.
    let err = tokio::time::timeout(Duration::from_secs(5), bot.main_loop())
        .await
        .expect("main_loop must refuse to start, not run")
        .expect_err("main_loop must refuse");

    let err = err.downcast::<StartError>().expect("a StartError");
    assert_eq!(
        err.to_string(),
        "the role \"admin\" needs the IRCv3 capability account-tag, but the server did not \
         give it: use `Role::hostmask` for this role on this network"
    );
}

#[tokio::test]
async fn an_account_role_runs_the_command_for_the_logged_in_account() {
    let mut server = ScriptedServer::start(vec![
        ("CAP LS", vec![":srv CAP * LS :account-tag server-time"]),
        ("CAP REQ", vec![":srv CAP * ACK :account-tag"]),
    ])
    .await;
    let bot = AdminBot::new("testbot", server.addr.clone(), ["#chan"])
        .with_role("admin", Role::account(["alice"]));
    let task = tokio::spawn(async move {
        let _ = bot.main_loop().await;
    });

    // The bot asks for the capability that the role needs.
    let request = server.expect(|l| l.starts_with("CAP REQ")).await;
    assert_eq!(request, "CAP REQ :account-tag");
    server.send(":server 001 testbot :Welcome");

    // The nick does not matter: the account decides.
    server.send("@account=alice :someone!s@any.host PRIVMSG #chan :!restart");
    let reply = server.expect(|l| l.starts_with("PRIVMSG")).await;
    assert_eq!(reply, "PRIVMSG #chan :restarting");

    // A sender who is not logged in, with the nick of the account.
    server.send(":alice!a@any.host PRIVMSG #chan :!restart");
    // A sender who is logged in to another account.
    server.send("@account=mallory :alice!a@any.host PRIVMSG #chan :!restart");
    server
        .expect_none(Duration::from_millis(400), |l| l.starts_with("PRIVMSG"))
        .await;

    task.abort();
}

#[tokio::test]
async fn hostmask_roles_send_no_capability_request() {
    let mut server = ScriptedServer::start(vec![]).await;
    let bot = AdminBot::new("testbot", server.addr.clone(), ["#chan"])
        .with_role("admin", ["*!*@trusted.host"]);
    let task = tokio::spawn(async move {
        let _ = bot.main_loop().await;
    });

    server.expect(|l| l.starts_with("USER")).await;
    server.send(":server 001 testbot :Welcome");
    server.send(":alice!a@trusted.host PRIVMSG #chan :!restart");

    // Up to the reply, the bot sent no `CAP` line.
    let mut lines = Vec::new();
    loop {
        let line = server.expect(|_| true).await;
        if line.starts_with("PRIVMSG") {
            assert_eq!(line, "PRIVMSG #chan :restarting");
            break;
        }
        lines.push(line);
    }
    assert!(lines.iter().all(|l| !l.starts_with("CAP")), "got {lines:?}");

    task.abort();
}
