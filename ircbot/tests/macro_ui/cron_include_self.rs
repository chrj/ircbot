use ircbot::{bot, Context, Result};

#[bot]
impl CronBot {
    #[on(cron = "0 0 * * * *", include_self)]
    async fn tick(&self, ctx: Context) -> Result {
        ctx.say("tick")
    }
}

fn main() {}
