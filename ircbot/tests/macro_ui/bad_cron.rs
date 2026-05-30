use ircbot::{bot, Context, Result};

#[bot]
impl BadCronBot {
    #[on(cron = "not a valid cron expression")]
    async fn tick(&self, ctx: Context) -> Result {
        ctx.say("tick")
    }
}

fn main() {}
