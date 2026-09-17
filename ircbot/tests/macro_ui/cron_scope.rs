use ircbot::{bot, Context, Result};

#[bot]
impl CronBot {
    #[on(cron = "0 0 * * * *", scope = "channel")]
    async fn tick(&self, ctx: Context) -> Result {
        ctx.say("tick")
    }
}

fn main() {}
