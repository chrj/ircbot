//! The setup of a bot before it starts: its server, settings and plugins.
//!
//! The `#[bot]` macro keeps a [`BotSetup`] in each bot, and its generated
//! methods call this type. `main_loop` calls [`BotSetup::run`], which checks
//! the setup, connects, and runs the bot.

use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use crate::connection::Settings;
use crate::handler::{Bot, HandlerEntry, Trigger};
use crate::help::{help_entry, HelpIndex, HelpSource, HELP_COMMAND};
use crate::plugin::{Plugin, PluginTasks, Registration, DEFAULT_PLUGIN_QUEUE_CAPACITY};
use crate::role::ACCOUNT_TAG;
use crate::{BoxError, Channel, Nick, Server, State};

/// Who has a command: the bot itself, a plugin, or the built-in `!help`.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum CommandOwner {
    /// A handler in the `#[bot]` impl block.
    Bot,
    /// A plugin, with its name.
    Plugin(String),
    /// The built-in `!help` of the generated `with_help` method.
    Help,
}

impl fmt::Display for CommandOwner {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CommandOwner::Bot => write!(f, "the bot"),
            CommandOwner::Plugin(name) => write!(f, "plugin {name:?}"),
            CommandOwner::Help => write!(f, "the built-in help"),
        }
    }
}

/// Why a bot cannot start. `main_loop` returns this error, in a `BoxError`.
///
/// Each check comes before the bot connects.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum StartError {
    /// The bot has no server to connect to. A bot made with `Default` or
    /// `from_state` is for tests. Make a bot that runs with `new` or
    /// `new_with_state`.
    #[error(
        "the bot has no server to connect to: make it with `new` or `new_with_state`, \
         not with `default` or `from_state`"
    )]
    NoServer,

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

    /// The bot and a plugin, or two plugins, have the same command. The case
    /// of a command does not matter, as in the dispatch.
    #[error("{first} and {second} both have the command `{command}`: remove it from one of them")]
    DuplicateCommand {
        /// The name of the command, without the prefix.
        command: String,
        /// The first owner of the command: the bot, or the plugin that was
        /// added first.
        first: CommandOwner,
        /// The second owner of the command.
        second: CommandOwner,
    },

    /// A command needs a role that no `with_role` call defines. Such a
    /// command would never run.
    #[error(
        "the command `{command}` of {owner} needs the role {role:?}, but no role has \
         this name: define it with `with_role`"
    )]
    UnknownRole {
        /// The bot or the plugin with the command.
        owner: CommandOwner,
        /// The name of the command, without the prefix.
        command: String,
        /// The role that no call defines.
        role: String,
    },

    /// A role needs an IRCv3 capability that the server did not give. An
    /// account role needs `account-tag`, and could never match without it.
    #[error(
        "the role {role:?} needs the IRCv3 capability {capability}, but the server did not \
         give it: use `Role::hostmask` for this role on this network"
    )]
    MissingCapability {
        /// The capability that the server did not give.
        capability: String,
        /// The first role that needs it.
        role: String,
    },

    /// The connection to the IRC server failed.
    #[error("cannot connect to the IRC server: {source}")]
    Connect {
        /// The error from the connection.
        source: BoxError,
    },
}

/// Where a bot connects to.
struct Target {
    nick: Nick,
    server: Server,
    channels: Vec<Channel>,
}

/// The server, settings and plugins of a bot that has not started.
///
/// This is part of the internal API that the `#[bot]` macro uses. Use the
/// methods that the macro generates on the bot instead.
#[derive(Default)]
pub struct BotSetup {
    target: Option<Target>,
    settings: Settings,
    queue_capacity: Option<usize>,
    plugins: Vec<Registration>,
    help: bool,
}

impl BotSetup {
    /// A setup that connects as `nick` to `server`, and joins `channels`.
    pub fn new(
        nick: impl Into<String>,
        server: impl Into<Server>,
        channels: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        BotSetup {
            target: Some(Target {
                nick: Nick::from(nick.into()),
                server: server.into(),
                channels: channels
                    .into_iter()
                    .map(|c| Channel::from(c.into()))
                    .collect(),
            }),
            ..BotSetup::default()
        }
    }

    /// Add `plugin`.
    pub fn add_plugin<P: Plugin>(&mut self, plugin: P) {
        self.plugins.push(Registration::new(plugin));
    }

    /// Turn on the built-in `!help` command. See the `help` module.
    pub fn enable_help(&mut self) {
        self.help = true;
    }

    /// Set the capacity of the queue of each plugin. A value of 0 is changed
    /// to 1. Tokio refuses a queue larger than
    /// [`Semaphore::MAX_PERMITS`](tokio::sync::Semaphore::MAX_PERMITS), so a
    /// larger value is changed to that number.
    pub fn set_queue_capacity(&mut self, capacity: usize) {
        self.queue_capacity = Some(capacity.clamp(1, tokio::sync::Semaphore::MAX_PERMITS));
    }

    /// Define a role. See [`State::with_role`].
    pub fn add_role(&mut self, name: impl Into<String>, role: impl Into<crate::Role>) {
        self.settings.roles.push((name.into(), role.into()));
    }

    /// Ignore senders. See [`State::with_ignore`].
    pub fn add_ignore(&mut self, masks: impl IntoIterator<Item = impl Into<String>>) {
        self.settings
            .ignore
            .extend(masks.into_iter().map(Into::into));
    }

    /// Set the keepalive. See [`State::with_keepalive`].
    pub fn set_keepalive(&mut self, interval: Duration, timeout: Duration) {
        self.settings.keepalive_interval = interval;
        self.settings.keepalive_timeout = timeout;
    }

    /// Set the flood control. See [`State::with_flood_control`].
    pub fn set_flood_control(&mut self, burst: usize, rate: Duration) {
        self.settings.flood_burst = burst;
        self.settings.flood_rate = rate;
    }

    /// Set the reconnect delays. See [`State::with_reconnect`].
    pub fn set_reconnect(&mut self, delay: Duration, max_delay: Duration) {
        self.settings.reconnect_delay = delay;
        self.settings.max_reconnect_delay = max_delay;
    }

    /// Set the reply to CTCP `VERSION`. See [`State::with_ctcp_version`].
    pub fn set_ctcp_version(&mut self, version: impl Into<String>) {
        self.settings.ctcp_version = Some(version.into());
    }

    /// Set the keepnick interval. See [`State::with_keepnick_interval`].
    pub fn set_keepnick_interval(&mut self, interval: Duration) {
        self.settings.keepnick_interval = Some(interval);
    }

    /// Check the setup, connect, and run `bot` with its plugins.
    ///
    /// # Errors
    ///
    /// Returns a [`StartError`] if a check or the connection fails, or if an
    /// account role needs `account-tag` and the server does not give it.
    /// Returns the error of the main event loop if it stops with one.
    pub async fn run<T: Bot + Send + Sync + 'static>(self, bot: T) -> Result<(), BoxError> {
        let Some(mut target) = self.target else {
            return Err(StartError::NoServer.into());
        };
        check(
            &own_commands::<T>(),
            &self.plugins,
            self.help,
            &self.settings.roles,
        )?;

        // An account role matches the `account` tag of a message, which the
        // server only sends with the capability `account-tag`.
        let account_role = self
            .settings
            .roles
            .iter()
            .find(|(_, role)| role.needs_account())
            .map(|(name, _)| name.clone());
        if account_role.is_some() {
            target.server.request_capability(ACCOUNT_TAG);
        }

        let mut state = State::connect(target.nick, target.server, target.channels)
            .await
            .map_err(|source| StartError::Connect { source })?;
        if let Some(role) = account_role {
            if !state.capabilities().iter().any(|cap| cap == ACCOUNT_TAG) {
                return Err(StartError::MissingCapability {
                    capability: ACCOUNT_TAG.to_string(),
                    role,
                }
                .into());
            }
        }
        state.settings = self.settings;

        let capacity = self.queue_capacity.unwrap_or(DEFAULT_PLUGIN_QUEUE_CAPACITY);
        let mut entries: Vec<HandlerEntry<T>> = T::handlers();
        let mut plugins = self.plugins;
        if self.help {
            let mut index = HelpIndex::default();
            index.add(HelpSource::of::<T>());
            for registration in &mut plugins {
                index.add(std::mem::take(&mut registration.help));
            }
            entries.push(help_entry(index, state.settings.roles.clone()));
        }
        let mut tasks = PluginTasks::new();
        for registration in plugins {
            let name = registration.name;
            let started = registration.start(capacity);
            entries.extend(
                started
                    .entries
                    .into_iter()
                    .map(crate::plugin::QueueEntry::into_handler_entry),
            );
            tasks.push(name, started.task);
        }

        let result = crate::internal::run_bot(Arc::new(bot), state, entries).await;
        // `run_bot` dropped the handler entries, and with them each sender of
        // a plugin queue.
        tasks.stop().await;
        result
    }
}

/// The commands of the bot itself, with their roles.
fn own_commands<T: Bot>() -> Vec<(String, Option<String>)> {
    T::handlers()
        .into_iter()
        .filter_map(|entry| match entry.trigger {
            Trigger::Command { name, role, .. } => Some((name, role)),
            _ => None,
        })
        .collect()
}

/// Refuse invalid plugin names, duplicate plugins, duplicate commands, and
/// commands whose role nobody defines.
fn check(
    own: &[(String, Option<String>)],
    plugins: &[Registration],
    help: bool,
    roles: &[(String, crate::Role)],
) -> Result<(), StartError> {
    let mut owners: Vec<(String, CommandOwner)> = Vec::new();
    if help {
        claim(&mut owners, roles, HELP_COMMAND, None, CommandOwner::Help)?;
    }
    for (command, role) in own {
        claim(
            &mut owners,
            roles,
            command,
            role.as_ref(),
            CommandOwner::Bot,
        )?;
    }

    let mut names: Vec<&str> = Vec::new();
    for plugin in plugins {
        if !crate::name::is_valid_name(plugin.name) {
            return Err(StartError::InvalidName {
                name: plugin.name.to_string(),
            });
        }
        if names.contains(&plugin.name) {
            return Err(StartError::DuplicatePlugin {
                name: plugin.name.to_string(),
            });
        }
        names.push(plugin.name);
        for command in &plugin.commands {
            claim(
                &mut owners,
                roles,
                &command.name,
                command.role.as_ref(),
                CommandOwner::Plugin(plugin.name.to_string()),
            )?;
        }
    }
    Ok(())
}

/// Record that `owner` has `command`, and check its role.
///
/// The dispatch ignores the case of a command, so this check does too: `echo`
/// and `ECHO` are the same command. One owner can have more than one handler
/// for its own command, for example one for each channel.
fn claim(
    owners: &mut Vec<(String, CommandOwner)>,
    roles: &[(String, crate::Role)],
    command: &str,
    role: Option<&String>,
    owner: CommandOwner,
) -> Result<(), StartError> {
    let first = owners
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case(command))
        .map(|(_, first)| first.clone());
    match first {
        Some(first) if first != owner => {
            return Err(StartError::DuplicateCommand {
                command: command.to_string(),
                first,
                second: owner,
            });
        }
        Some(_) => {}
        None => owners.push((command.to_string(), owner.clone())),
    }
    if let Some(role) = role {
        if !roles.iter().any(|(name, _)| name == role) {
            return Err(StartError::UnknownRole {
                owner,
                command: command.to_string(),
                role: role.clone(),
            });
        }
    }
    Ok(())
}
