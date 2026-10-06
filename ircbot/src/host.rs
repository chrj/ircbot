//! Run many plugins on one IRC connection.
//!
//! A [`Plugin`] is a set of handlers with its own state, made with the
//! `#[plugin]` attribute. A [`Host`] runs plugins together on one connection:
//!
//! ```rust,ignore
//! Host::new("mybot", "irc.example.net:6667", ["rust"])
//!     .with_role("op", ["*!*@trusted.host"])
//!     .plugin(Greeter)
//!     .plugin(Counter::from_state(CounterState::default()))
//!     .connect()
//!     .await?
//!     .main_loop()
//!     .await
//! ```
//!
//! # Checks before the connection
//!
//! [`Host::connect`] checks the plugins before it connects, and refuses:
//!
//! * a plugin name that does not obey the naming rule,
//! * two plugins with the same name,
//! * two plugins with the same command,
//! * a command whose role no [`Host::with_role`] call defines. Without this
//!   check, such a command would never run, and nothing would tell why.
//!
//! # Isolation
//!
//! Each plugin runs in its own Tokio task, with a queue of messages:
//!
//! * A slow plugin does not delay the other plugins.
//! * A plugin that panics does not stop the bot or the other plugins. The host
//!   logs the panic with the plugin name, and the plugin gets the next
//!   message.
//! * One plugin gets its messages in the order that they arrive, and handles
//!   one message at a time. Thus its handlers never run at the same time.
//!   Cron handlers use the same queue.
//! * A queue has a fixed capacity ([`DEFAULT_PLUGIN_QUEUE_CAPACITY`], or
//!   [`Host::with_queue_capacity`]). When the queue of a plugin is full, the
//!   plugin does not get the new message, and the host logs a warning.
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

use crate::connection::Settings;
use crate::handler::{Bot, HandlerEntry, HandlerFn, Trigger};
use crate::{BoxError, Channel, Context, Nick, Server, State};

/// How many messages wait in the queue of one plugin, if
/// [`Host::with_queue_capacity`] does not set another number.
pub const DEFAULT_PLUGIN_QUEUE_CAPACITY: usize = 256;

/// How long [`ConnectedHost::main_loop`] waits for each plugin task to finish
/// its queue after the connection stops, before it stops the task.
const PLUGIN_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);

/// A set of handlers that a [`Host`] runs, with a name.
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

/// An error from [`Host::connect`].
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum HostError {
    /// The name of a plugin does not obey the rule in [`Plugin::NAME`].
    #[error(
        "invalid plugin name {name:?}: start with a lowercase ASCII letter, use only \
         lowercase ASCII letters, digits and `_`, and do not use `sqlite` or a name \
         that starts with `sqlite_`"
    )]
    InvalidName {
        /// The name that was refused.
        name: String,
    },

    /// Two plugins have the same name.
    #[error("two plugins have the name {name:?}: give each plugin its own name")]
    DuplicatePlugin {
        /// The name of the two plugins.
        name: String,
    },

    /// Two plugins have the same command.
    #[error(
        "plugins {first:?} and {second:?} both have the command `{command}`: \
         remove the command from one of them"
    )]
    DuplicateCommand {
        /// The name of the command, without the prefix.
        command: String,
        /// The plugin that was added first.
        first: String,
        /// The plugin that was added second.
        second: String,
    },

    /// A command of a plugin needs a role that no [`Host::with_role`] call
    /// defines. Such a command would never run.
    #[error(
        "the command `{command}` of plugin {plugin:?} needs the role {role:?}, but no \
         role has this name: define it with `Host::with_role`"
    )]
    UnknownRole {
        /// The name of the plugin.
        plugin: String,
        /// The name of the command, without the prefix.
        command: String,
        /// The role that no call defines.
        role: String,
    },

    /// The connection to the IRC server failed.
    #[error("cannot connect to the IRC server: {source}")]
    Connect {
        /// The error from the connection.
        source: BoxError,
    },
}

/// A command of a plugin, for the checks of [`Host::connect`].
struct CommandInfo {
    name: String,
    role: Option<String>,
}

/// A plugin that the host has, with its type erased.
struct Registration {
    name: &'static str,
    commands: Vec<CommandInfo>,
    /// Starts the task of the plugin, and gives the handler entries that put
    /// messages in its queue.
    start: Box<dyn FnOnce(usize) -> Started + Send>,
}

/// A plugin whose task runs.
struct Started {
    entries: Vec<HandlerEntry<()>>,
    task: JoinHandle<()>,
}

/// A message for the task of a plugin: which handler runs, with which context.
struct Job {
    handler: usize,
    ctx: Context,
}

/// Runs many plugins on one IRC connection.
///
/// Add plugins with [`Host::plugin`], then call [`Host::connect`] and
/// [`ConnectedHost::main_loop`]. See the [module docs](self) for the checks
/// and the isolation of plugins.
pub struct Host {
    nick: Nick,
    server: Server,
    channels: Vec<Channel>,
    settings: Settings,
    queue_capacity: usize,
    plugins: Vec<Registration>,
}

impl Host {
    /// Make a host for the bot with the nick `nick`, on `server`, in
    /// `channels`. This does not connect yet.
    ///
    /// `server` and `channels` are the same as for the `new` function that
    /// `#[bot]` generates.
    pub fn new(
        nick: impl Into<String>,
        server: impl Into<Server>,
        channels: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        Host {
            nick: Nick::from(nick.into()),
            server: server.into(),
            channels: channels
                .into_iter()
                .map(|c| Channel::from(c.into()))
                .collect(),
            settings: Settings::default(),
            queue_capacity: DEFAULT_PLUGIN_QUEUE_CAPACITY,
            plugins: Vec::new(),
        }
    }

    /// Add `plugin`. The host runs the plugins in the order that you add
    /// them.
    #[must_use]
    pub fn plugin<P: Plugin>(mut self, plugin: P) -> Self {
        let commands = P::handlers()
            .into_iter()
            .filter_map(|entry| match entry.trigger {
                Trigger::Command { name, role, .. } => Some(CommandInfo { name, role }),
                _ => None,
            })
            .collect();
        self.plugins.push(Registration {
            name: P::NAME,
            commands,
            start: Box::new(move |capacity| start_plugin(plugin, capacity)),
        });
        self
    }

    /// Set how many messages can wait in the queue of each plugin. The
    /// default is [`DEFAULT_PLUGIN_QUEUE_CAPACITY`]. A value of 0 is changed
    /// to 1.
    #[must_use]
    pub fn with_queue_capacity(mut self, capacity: usize) -> Self {
        self.queue_capacity = capacity.max(1);
        self
    }

    /// Define a role for commands. See [`State::with_role`].
    #[must_use]
    pub fn with_role(
        mut self,
        name: impl Into<String>,
        masks: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        let patterns: Vec<String> = masks.into_iter().map(Into::into).collect();
        self.settings.roles.push((name.into(), patterns));
        self
    }

    /// Ignore the senders whose hostmask matches one of `masks`. See
    /// [`State::with_ignore`].
    #[must_use]
    pub fn with_ignore(mut self, masks: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.settings
            .ignore
            .extend(masks.into_iter().map(Into::into));
        self
    }

    /// Set the keepalive interval and timeout. See [`State::with_keepalive`].
    #[must_use]
    pub fn with_keepalive(mut self, interval: Duration, timeout: Duration) -> Self {
        self.settings.keepalive_interval = interval;
        self.settings.keepalive_timeout = timeout;
        self
    }

    /// Set the flood control. See [`State::with_flood_control`].
    #[must_use]
    pub fn with_flood_control(mut self, burst: usize, rate: Duration) -> Self {
        self.settings.flood_burst = burst;
        self.settings.flood_rate = rate;
        self
    }

    /// Set the reconnect delays. See [`State::with_reconnect`].
    #[must_use]
    pub fn with_reconnect(mut self, delay: Duration, max_delay: Duration) -> Self {
        self.settings.reconnect_delay = delay;
        self.settings.max_reconnect_delay = max_delay;
        self
    }

    /// Set the reply to CTCP `VERSION`. See [`State::with_ctcp_version`].
    #[must_use]
    pub fn with_ctcp_version(mut self, version: impl Into<String>) -> Self {
        self.settings.ctcp_version = Some(version.into());
        self
    }

    /// Try again to get the configured nick at `interval`. See
    /// [`State::with_keepnick_interval`].
    #[must_use]
    pub fn with_keepnick_interval(mut self, interval: Duration) -> Self {
        self.settings.keepnick_interval = Some(interval);
        self
    }

    /// Try again to get the configured nick at the default interval. See
    /// [`State::with_keepnick`].
    #[must_use]
    pub fn with_keepnick(self) -> Self {
        self.with_keepnick_interval(crate::DEFAULT_KEEPNICK_INTERVAL)
    }

    /// Check the plugins, then connect to the server.
    ///
    /// # Errors
    ///
    /// Returns [`HostError::InvalidName`], [`HostError::DuplicatePlugin`],
    /// [`HostError::DuplicateCommand`] or [`HostError::UnknownRole`] if a check
    /// fails. These errors come before the host connects. Returns
    /// [`HostError::Connect`] if the connection or the registration with the
    /// server fails.
    pub async fn connect(self) -> Result<ConnectedHost, HostError> {
        check(&self.plugins, &self.settings.roles)?;
        let mut state = State::connect(self.nick, self.server, self.channels)
            .await
            .map_err(|source| HostError::Connect { source })?;
        state.settings = self.settings;
        Ok(ConnectedHost {
            state,
            queue_capacity: self.queue_capacity,
            plugins: self.plugins,
        })
    }
}

/// A [`Host`] that is connected and ready to run.
pub struct ConnectedHost {
    state: State,
    queue_capacity: usize,
    plugins: Vec<Registration>,
}

impl ConnectedHost {
    /// Start the task of each plugin, and run the main event loop.
    ///
    /// The host reconnects on its own when the connection is lost, as a
    /// `#[bot]` does. The plugin tasks keep running across reconnects, so the
    /// state of a plugin stays.
    ///
    /// # Errors
    ///
    /// Returns an error if the main event loop stops with an error. Before it
    /// returns, each plugin task gets up to 5 seconds to finish the messages
    /// in its queue.
    pub async fn main_loop(self) -> Result<(), BoxError> {
        let mut entries = Vec::new();
        let mut tasks = Vec::new();
        for registration in self.plugins {
            let started = (registration.start)(self.queue_capacity);
            entries.extend(started.entries);
            tasks.push((registration.name, started.task));
        }

        let result = crate::internal::run_bot(Arc::new(()), self.state, entries).await;

        // `run_bot` dropped the handler entries, and with them each sender of
        // a plugin queue. Thus each plugin task stops after its last message.
        for (name, task) in tasks {
            stop_plugin(name, task).await;
        }
        result
    }
}

/// Refuse invalid plugin names, duplicate plugins, duplicate commands, and
/// commands whose role nobody defines.
fn check(plugins: &[Registration], roles: &[(String, Vec<String>)]) -> Result<(), HostError> {
    let mut names: Vec<&str> = Vec::new();
    let mut commands: Vec<(&str, &str)> = Vec::new();
    for plugin in plugins {
        if !crate::name::is_valid_name(plugin.name) {
            return Err(HostError::InvalidName {
                name: plugin.name.to_string(),
            });
        }
        if names.contains(&plugin.name) {
            return Err(HostError::DuplicatePlugin {
                name: plugin.name.to_string(),
            });
        }
        names.push(plugin.name);

        for command in &plugin.commands {
            // A plugin can have more than one handler for its own command,
            // for example one for each channel. Only a second plugin clashes.
            let owner = commands
                .iter()
                .find(|(name, _)| *name == command.name)
                .map(|(_, owner)| *owner);
            match owner {
                Some(first) if first != plugin.name => {
                    return Err(HostError::DuplicateCommand {
                        command: command.name.clone(),
                        first: first.to_string(),
                        second: plugin.name.to_string(),
                    });
                }
                Some(_) => {}
                None => commands.push((&command.name, plugin.name)),
            }

            if let Some(role) = &command.role {
                if !roles.iter().any(|(name, _)| name == role) {
                    return Err(HostError::UnknownRole {
                        plugin: plugin.name.to_string(),
                        command: command.name.clone(),
                        role: role.clone(),
                    });
                }
            }
        }
    }
    Ok(())
}

/// Start the task of `plugin`, and give the handler entries that put messages
/// in its queue.
fn start_plugin<P: Plugin>(plugin: P, capacity: usize) -> Started {
    let (tx, rx) = mpsc::channel::<Job>(capacity);
    let dropped = Arc::new(AtomicU64::new(0));
    let mut handlers: Vec<HandlerFn<P>> = Vec::new();
    let mut triggers: Vec<Trigger> = Vec::new();
    let mut entries: Vec<HandlerEntry<()>> = Vec::new();

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
        entries.push(HandlerEntry {
            trigger,
            include_self,
            raw_text,
            scope,
            handler: Box::new(move |_host: Arc<()>, ctx: Context| {
                enqueue(
                    &tx,
                    P::NAME,
                    Job {
                        handler: index,
                        ctx,
                    },
                    &dropped,
                );
                Box::pin(async { Ok(()) })
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
                "the plugin did not finish its queue in time, so the host stopped it"
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
