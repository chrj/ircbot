//! The warning when a reconnect loses the capability of an account role, in
//! its own test binary.
//!
//! `tracing` caches whether a log statement is on for all threads. When
//! another test in the same binary runs the statement on another thread, with
//! no subscriber, the cache can turn it off for this test too. Thus this test
//! is alone in its binary.
//!
//! Run with:
//!   cargo test --test role_logs

use std::sync::{Arc, Mutex};
use std::time::Duration;

use ircbot::{bot, Context, Result, Role};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;

/// Keeps what the subscriber writes, so the test can read it.
#[derive(Clone, Default)]
struct Capture(Arc<Mutex<Vec<u8>>>);

impl Capture {
    fn contents(&self) -> String {
        String::from_utf8(self.0.lock().unwrap().clone()).unwrap()
    }
}

impl std::io::Write for Capture {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Capture {
    type Writer = Capture;

    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

#[bot]
impl AdminBot {
    #[command("restart", role = "admin")]
    async fn restart(&self, ctx: Context) -> Result {
        ctx.say("restarting")
    }
}

/// Answer one connection: give `ls` to `CAP LS`, `ack` to `CAP REQ`, and the
/// welcome after `CAP END`, when the capability exchange is over. Close the
/// connection after the welcome when `close`.
async fn serve(listener: &TcpListener, ls: &str, ack: Option<&str>, close: bool) {
    let (sock, _) = listener.accept().await.unwrap();
    let (read, mut write) = sock.into_split();
    let mut lines = BufReader::new(read).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        let reply = if line.starts_with("CAP LS") {
            Some(format!(":srv CAP * LS :{ls}\r\n"))
        } else if line.starts_with("CAP REQ") {
            ack.map(|caps| format!(":srv CAP * ACK :{caps}\r\n"))
        } else if line.starts_with("CAP END") {
            Some(":server 001 testbot :Welcome\r\n".to_string())
        } else {
            None
        };
        if let Some(reply) = reply {
            write.write_all(reply.as_bytes()).await.unwrap();
            if close && line.starts_with("CAP END") {
                tokio::time::sleep(Duration::from_millis(100)).await;
                return;
            }
        }
    }
}

#[tokio::test]
async fn a_reconnect_without_account_tag_warns_about_the_account_role() {
    let capture = Capture::default();
    let subscriber = tracing_subscriber::fmt()
        .with_ansi(false)
        .with_writer(capture.clone())
        .finish();
    let _guard = tracing::subscriber::set_default(subscriber);

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let bot = AdminBot::new("testbot", addr, ["#chan"])
        .with_reconnect(Duration::from_millis(50), Duration::from_millis(50))
        .with_role("admin", Role::account(["alice"]));
    let task = tokio::spawn(async move {
        let _ = bot.main_loop().await;
    });

    // The first connection gives `account-tag`, then closes.
    serve(&listener, "account-tag", Some("account-tag"), true).await;
    // The second connection does not offer it any more.
    let second = tokio::spawn(async move {
        serve(&listener, "server-time", None, false).await;
    });

    let warned = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let logs = capture.contents();
            if logs.contains("WARN") && logs.contains("account-tag") && logs.contains("admin") {
                return logs;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await;

    assert!(
        warned.is_ok(),
        "no warning about the account role; got {:?}",
        capture.contents()
    );
    task.abort();
    second.abort();
}
