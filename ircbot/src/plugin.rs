//! Plugins: sets of handlers with their own state, that a bot runs.
//!
//! A [`Plugin`] is made with the `#[plugin]` attribute. A `#[bot]` adds it
//! with its generated `plugin` method:
//!
//! ```rust,ignore
//! MyBot::new("mybot", "irc.example.net:6667", ["rust"])
//!     .with_role("op", ["*!*@trusted.host"])
//!     .plugin(Greeter)
//!     .plugin(Counter::from_state(CounterState::default()))
//!     .main_loop()
//!     .await
//! ```
//!
//! # Checks before the connection
//!
//! `main_loop` checks the bot and its plugins before it connects. It returns
//! a [`StartError`](crate::StartError) for:
//!
//! * a plugin name that does not obey the naming rule,
//! * two plugins with the same name,
//! * a command that the bot and a plugin, or two plugins, both have,
//! * a command whose role no `with_role` call defines. Without this check,
//!   such a command would never run, and nothing would tell why.
//!
//! # Isolation
//!
//! The handlers of the bot itself run in the dispatch loop, one after the
//! other. Each plugin runs in its own Tokio task, with a queue of messages:
//!
//! * A slow plugin does not delay the bot or the other plugins.
//! * A plugin that panics does not stop the bot or the other plugins. The bot
//!   logs the panic with the plugin name, and the plugin gets the next
//!   message.
//! * One plugin gets its messages in the order that they arrive, and handles
//!   one message at a time. Thus its handlers never run at the same time.
//!   Cron handlers use the same queue.
//! * A queue has a fixed capacity ([`DEFAULT_PLUGIN_QUEUE_CAPACITY`], or the
//!   generated `with_queue_capacity` method). When the queue of a plugin is
//!   full, the plugin does not get the new message, and the bot logs a
//!   warning.
//! * The plugin tasks keep running across reconnects, so the state of a
//!   plugin stays.
//!
//! Replies of different plugins can arrive at the server in any order.

use std::any::Any;
use std::panic::AssertUnwindSafe;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use futures::FutureExt;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::handler::{Bot, HandlerEntry, HandlerFn, Scope, Trigger};
use crate::Context;

/// How many messages wait in the queue of one plugin, if the generated
/// `with_queue_capacity` method of the bot does not set another number.
pub const DEFAULT_PLUGIN_QUEUE_CAPACITY: usize = 256;

/// How long a stopped bot waits for each plugin task to finish its queue,
/// before it stops the task.
const PLUGIN_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);

/// A set of handlers that a bot runs in its own task, with a name.
///
/// The `#[plugin]` attribute implements this trait. Implement it yourself only
/// for a plugin that you make without the attribute.
pub trait Plugin: Bot + Send + Sync + 'static {
    /// The name of the plugin. It starts with a lowercase ASCII letter, and
    /// has only lowercase ASCII letters, digits and `_`. It cannot be `sqlite`
    /// or start with `sqlite_`.
    ///
    /// A plugin that keeps data uses this name as its store namespace.
    const NAME: &'static str;
}

/// A command of a plugin, for the checks before the connection.
pub(crate) struct CommandInfo {
    pub(crate) name: String,
    pub(crate) role: Option<String>,
}

/// A plugin that a bot has, with its type erased.
pub(crate) struct Registration {
    pub(crate) name: &'static str,
    pub(crate) commands: Vec<CommandInfo>,
    /// Starts the task of the plugin, and gives the entries that put messages
    /// in its queue.
    start: Box<dyn FnOnce(usize) -> Started + Send + Sync>,
}

impl Registration {
    /// Erase the type of `plugin`, and keep what the checks need.
    pub(crate) fn new<P: Plugin>(plugin: P) -> Self {
        let commands = P::handlers()
            .into_iter()
            .filter_map(|entry| match entry.trigger {
                Trigger::Command { name, role, .. } => Some(CommandInfo { name, role }),
                _ => None,
            })
            .collect();
        Registration {
            name: P::NAME,
            commands,
            start: Box::new(move |capacity| start_plugin(plugin, capacity)),
        }
    }

    /// Start the task of the plugin.
    pub(crate) fn start(self, capacity: usize) -> Started {
        (self.start)(capacity)
    }
}

/// A plugin whose task runs.
pub(crate) struct Started {
    pub(crate) entries: Vec<QueueEntry>,
    pub(crate) task: JoinHandle<()>,
}

/// A handler entry of a plugin whose function puts the message in the queue
/// of the plugin. [`QueueEntry::into_handler_entry`] turns it into an entry
/// for the bot type.
pub(crate) struct QueueEntry {
    trigger: Trigger,
    include_self: bool,
    raw_text: bool,
    scope: Scope,
    enqueue: Arc<dyn Fn(Context) + Send + Sync>,
}

impl QueueEntry {
    /// An entry with the trigger of the plugin handler, for a bot of type
    /// `T`. Its function ignores the bot and returns at once.
    pub(crate) fn into_handler_entry<T: Send + Sync + 'static>(self) -> HandlerEntry<T> {
        let enqueue = self.enqueue;
        HandlerEntry {
            trigger: self.trigger,
            include_self: self.include_self,
            raw_text: self.raw_text,
            scope: self.scope,
            handler: Box::new(move |_bot: Arc<T>, ctx: Context| {
                enqueue(ctx);
                Box::pin(async { Ok(()) })
            }),
        }
    }
}

/// A message for the task of a plugin: which handler runs, with which context.
struct Job {
    handler: usize,
    ctx: Context,
}

/// The tasks of the plugins of a running bot.
///
/// The main loop usually runs until the program stops, so a program often
/// stops it by dropping its future, for example in `tokio::select!`. Then the
/// code after the main loop does not run. Dropping a `JoinHandle` does not
/// stop its task, so this guard aborts each task that is still in it.
pub(crate) struct PluginTasks(Vec<(&'static str, JoinHandle<()>)>);

impl PluginTasks {
    pub(crate) fn new() -> Self {
        PluginTasks(Vec::new())
    }

    pub(crate) fn push(&mut self, name: &'static str, task: JoinHandle<()>) {
        self.0.push((name, task));
    }

    /// Give each task up to `PLUGIN_SHUTDOWN_TIMEOUT` to finish its queue,
    /// then stop it.
    ///
    /// Call this after the handler entries are dropped. The entries hold the
    /// senders of the queues, so each task then stops after its last message.
    pub(crate) async fn stop(mut self) {
        for (name, task) in std::mem::take(&mut self.0) {
            stop_plugin(name, task).await;
        }
    }
}

impl Drop for PluginTasks {
    fn drop(&mut self) {
        for (_, task) in &self.0 {
            task.abort();
        }
    }
}

/// Start the task of `plugin`, and give the entries that put messages in its
/// queue.
fn start_plugin<P: Plugin>(plugin: P, capacity: usize) -> Started {
    let (tx, rx) = mpsc::channel::<Job>(capacity);
    let dropped = Arc::new(AtomicU64::new(0));
    let mut handlers: Vec<HandlerFn<P>> = Vec::new();
    let mut triggers: Vec<Trigger> = Vec::new();
    let mut entries: Vec<QueueEntry> = Vec::new();

    for (index, entry) in P::handlers().into_iter().enumerate() {
        let HandlerEntry {
            trigger,
            include_self,
            raw_text,
            scope,
            handler,
        } = entry;
        handlers.push(handler);
        triggers.push(trigger.clone());

        let tx = tx.clone();
        let dropped = Arc::clone(&dropped);
        entries.push(QueueEntry {
            trigger,
            include_self,
            raw_text,
            scope,
            enqueue: Arc::new(move |ctx| {
                enqueue(
                    &tx,
                    P::NAME,
                    Job {
                        handler: index,
                        ctx,
                    },
                    &dropped,
                );
            }),
        });
    }

    let task = tokio::spawn(run_plugin(
        P::NAME,
        Arc::new(plugin),
        handlers,
        triggers,
        rx,
    ));
    Started { entries, task }
}

/// Put `job` in the queue of `plugin`, or log why the plugin does not get it.
fn enqueue(tx: &mpsc::Sender<Job>, plugin: &'static str, job: Job, dropped: &AtomicU64) {
    match tx.try_send(job) {
        Ok(()) => {}
        Err(mpsc::error::TrySendError::Full(_)) => {
            let total = dropped.fetch_add(1, Ordering::Relaxed) + 1;
            tracing::warn!(
                plugin,
                capacity = tx.max_capacity(),
                dropped_total = total,
                "the queue of the plugin is full, so the plugin does not get this message"
            );
        }
        Err(mpsc::error::TrySendError::Closed(_)) => {
            tracing::error!(
                plugin,
                "the task of the plugin has stopped, so the plugin does not get this message"
            );
        }
    }
}

/// The task of one plugin: run each message of the queue, one at a time.
async fn run_plugin<P: Plugin>(
    name: &'static str,
    plugin: Arc<P>,
    handlers: Vec<HandlerFn<P>>,
    triggers: Vec<Trigger>,
    mut rx: mpsc::Receiver<Job>,
) {
    while let Some(Job { handler, ctx }) = rx.recv().await {
        let (Some(run), Some(trigger)) = (handlers.get(handler), triggers.get(handler)) else {
            // `start_plugin` makes each index from the same list, so this
            // cannot happen. Log it, and do not stop the task.
            tracing::error!(
                plugin = name,
                handler,
                "the plugin has no handler with this index"
            );
            continue;
        };
        // The call is in the async block, so a panic while the handler makes
        // its future is also caught.
        let call = AssertUnwindSafe(async { run(Arc::clone(&plugin), ctx).await });
        match call.catch_unwind().await {
            Ok(Ok(())) => {}
            Ok(Err(error)) => {
                tracing::error!(plugin = name, ?trigger, %error, "plugin handler error");
            }
            Err(panic) => {
                tracing::error!(
                    plugin = name,
                    ?trigger,
                    panic = panic_message(panic.as_ref()),
                    "plugin handler panicked; the plugin gets the next message"
                );
            }
        }
    }
}

/// Wait for the task of a plugin to finish its queue, for a limited time.
async fn stop_plugin(name: &'static str, mut task: JoinHandle<()>) {
    match tokio::time::timeout(PLUGIN_SHUTDOWN_TIMEOUT, &mut task).await {
        Ok(Ok(())) => {}
        Ok(Err(error)) => {
            tracing::error!(plugin = name, %error, "the task of the plugin failed");
        }
        Err(_) => {
            task.abort();
            tracing::warn!(
                plugin = name,
                timeout = ?PLUGIN_SHUTDOWN_TIMEOUT,
                "the plugin did not finish its queue in time, so the bot stopped it"
            );
        }
    }
}

/// The message of a panic, if it has one.
fn panic_message(panic: &(dyn Any + Send)) -> &str {
    if let Some(message) = panic.downcast_ref::<&str>() {
        message
    } else if let Some(message) = panic.downcast_ref::<String>() {
        message
    } else {
        "a panic without a text message"
    }
}
