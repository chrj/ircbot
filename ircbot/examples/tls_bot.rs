//! Connecting over TLS.
//!
//! Run with the `tls` feature:
//!
//!     cargo run --example tls_bot --features tls

use ircbot::{bot, Context, Result, Server};

#[bot]
impl TlsBot {
    #[command("ping")]
    async fn ping(&self, ctx: Context) -> Result {
        ctx.reply("pong!")
    }
}

#[tokio::main]
async fn main() -> std::result::Result<(), Box<dyn std::error::Error + Send + Sync>> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    // The server value decides the transport. A bare "host:port" string would
    // connect in plaintext; `Server::tls` negotiates TLS and verifies the
    // certificate against the platform's root store. `new` takes anything that
    // converts into a `Server`, so the builder chain can be passed straight in;
    // it is bound to a `Server` here only so it can be printed below.
    let server: Server = Server::tls("irc.libera.chat:6697").into();

    // A network with a private CA or a self-signed certificate is handled by
    // trusting that certificate specifically — verification stays on:
    //
    //     Server::tls("irc.internal.example:6697")
    //         .with_extra_root_pem(std::fs::read("ca.pem")?)
    //
    // When connecting to an IP address whose certificate names a hostname,
    // `with_sni("irc.example.net")` sets the name to verify against.
    //
    // Authentication happens during registration, so the credentials go on the
    // server. SASL EXTERNAL proves the bot's identity with a client
    // certificate, and sends no password at all — register the certificate's
    // fingerprint with the network first:
    //
    //     Server::tls("irc.libera.chat:6697")
    //         .with_client_cert_pem(std::fs::read("bot.pem")?)
    //         .with_sasl_external()
    //
    // SASL PLAIN uses an account name and a password instead. Read it from the
    // environment rather than writing it into the source:
    //
    //     Server::tls("irc.libera.chat:6697")
    //         .with_sasl_plain("mybot", std::env::var("IRC_PASSWORD")?)
    //
    // Either way, a network that refuses the login fails the connection rather
    // than letting the bot arrive unauthenticated.

    println!("tls_bot example compiled successfully.");
    println!("Connecting for real is two lines:");
    println!("  let bot = TlsBot::new(\"ircbot\", {server}, [\"#rust\"]).await?;");
    println!("  bot.main_loop().await?;");

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ircbot::testing::TestContext;

    #[tokio::test]
    async fn ping_replies_with_pong() {
        let bot = TlsBot::default();
        let mut tc = TestContext::channel("#test", "alice", "!ping");
        bot.ping(tc.take_ctx()).await.unwrap();
        assert_eq!(
            tc.next_reply(),
            Some("PRIVMSG #test :alice, pong!\r\n".to_string()),
        );
    }
}
