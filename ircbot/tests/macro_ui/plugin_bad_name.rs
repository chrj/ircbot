use ircbot::{plugin, Context, Result};

#[plugin(name = "Bad-Name")]
impl BadName {
    #[command("hi")]
    async fn hi(&self, ctx: Context) -> Result {
        ctx.say("hi")
    }
}

fn main() {}
