//! The built-in `!help` command.
//!
//! A bot turns it on with its generated `with_help` method. `!help` lists the
//! commands that the sender can run in this place: the commands of the bot
//! itself and of its plugins. `!help <command>` gives the usage of one
//! command, and the first line of the doc comment of its handler.
//!
//! A command is in the list only if one of its handlers would run for this
//! sender, here: its role matches the sender, its target matches the channel,
//! and its scope matches a channel or a private message. Thus `!help` never
//! shows a command that the sender cannot use.

use std::sync::Arc;

use irc_proto::Message;

use crate::bot::{authorized, scope_matches, target_matches, target_param};
use crate::handler::{Bot, CommandHelp, HandlerEntry, Scope, Trigger};
use crate::{Context, Target, User};

/// The name of the built-in help command.
pub(crate) const HELP_COMMAND: &str = "help";

/// The usage of the built-in help command.
const HELP_USAGE: &str = "!help [command]";

/// The summary of the built-in help command.
const HELP_SUMMARY: &str = "List the commands that you can use, or show how to use one.";

/// A command handler that `!help` can show: its trigger and scope decide for
/// whom and where.
#[derive(Clone)]
struct Guard {
    trigger: Trigger,
    scope: Scope,
}

/// One command, with its help text and the handlers that run it.
struct Topic {
    help: CommandHelp,
    guards: Vec<Guard>,
}

/// The help text and the handlers of each command of a bot and its plugins.
#[derive(Default)]
pub(crate) struct HelpIndex {
    topics: Vec<Topic>,
}

/// The help text and command handlers of one bot or plugin type, before they
/// go in a [`HelpIndex`].
#[derive(Default)]
pub(crate) struct HelpSource {
    help: Vec<CommandHelp>,
    guards: Vec<(String, Guard)>,
}

impl HelpSource {
    /// The help text and command handlers of `B`.
    pub(crate) fn of<B: Bot>() -> Self {
        let guards = B::handlers()
            .into_iter()
            .filter_map(|entry| match &entry.trigger {
                Trigger::Command { name, .. } => Some((
                    name.clone(),
                    Guard {
                        trigger: entry.trigger.clone(),
                        scope: entry.scope,
                    },
                )),
                _ => None,
            })
            .collect();
        HelpSource {
            help: B::help(),
            guards,
        }
    }
}

impl HelpIndex {
    /// Add the commands of `source`. A command keeps the help text of its
    /// first handler. The case of a command does not matter.
    pub(crate) fn add(&mut self, source: HelpSource) {
        for help in source.help {
            if self.topic(&help.command).is_none() {
                self.topics.push(Topic {
                    help,
                    guards: Vec::new(),
                });
            }
        }
        for (command, guard) in source.guards {
            if let Some(topic) = self
                .topics
                .iter_mut()
                .find(|t| t.help.command.eq_ignore_ascii_case(&command))
            {
                topic.guards.push(guard);
            }
        }
    }

    /// Add the built-in help command itself.
    fn add_help_command(&mut self) {
        self.topics.push(Topic {
            help: CommandHelp {
                command: HELP_COMMAND.to_string(),
                usage: HELP_USAGE.to_string(),
                summary: Some(HELP_SUMMARY.to_string()),
            },
            guards: vec![Guard {
                trigger: help_trigger(),
                scope: Scope::Any,
            }],
        });
    }

    fn topic(&self, command: &str) -> Option<&Topic> {
        self.topics
            .iter()
            .find(|t| t.help.command.eq_ignore_ascii_case(command))
    }

    /// The reply to `!help`, with `argument` as the text after the command.
    ///
    /// `msg` is the message with the command, `target` is where it arrived,
    /// and `sender` is who sent it.
    #[must_use]
    pub(crate) fn reply(
        &self,
        roles: &[(String, Vec<String>)],
        msg: &Message,
        target: &Target,
        sender: Option<&User>,
        argument: &str,
    ) -> String {
        let visible = |topic: &&Topic| {
            topic.guards.iter().any(|guard| {
                let filter = match &guard.trigger {
                    Trigger::Command { target, .. } => target.as_deref(),
                    _ => None,
                };
                authorized(roles, &guard.trigger, sender)
                    && scope_matches(guard.scope, target)
                    && target_matches(target_param(msg), filter)
            })
        };

        let wanted = argument.split_whitespace().next();
        let Some(wanted) = wanted else {
            let mut commands: Vec<String> = self
                .topics
                .iter()
                .filter(visible)
                .map(|t| format!("!{}", t.help.command))
                .collect();
            commands.sort_unstable();
            commands.dedup();
            return format!(
                "Commands: {}. Use !help <command> for details.",
                commands.join(", ")
            );
        };

        let wanted = wanted.trim_start_matches('!');
        match self.topic(wanted).filter(visible) {
            Some(topic) => match &topic.help.summary {
                Some(summary) => format!("{} — {summary}", topic.help.usage),
                None => topic.help.usage.clone(),
            },
            // A command that the sender cannot use gets the same answer as a
            // command that does not exist, so `!help` does not show it.
            None => format!("No command !{wanted}. Use !help to list the commands."),
        }
    }
}

/// The trigger of the built-in help command.
pub(crate) fn help_trigger() -> Trigger {
    Trigger::Command {
        name: HELP_COMMAND.to_string(),
        target: None,
        role: None,
    }
}

/// The handler entry of the built-in help command, for a bot of type `T`.
pub(crate) fn help_entry<T: Send + Sync + 'static>(
    mut index: HelpIndex,
    roles: Vec<(String, Vec<String>)>,
) -> HandlerEntry<T> {
    index.add_help_command();
    let index = Arc::new(index);
    let roles = Arc::new(roles);
    HandlerEntry {
        trigger: help_trigger(),
        include_self: false,
        raw_text: false,
        scope: Scope::Any,
        handler: Box::new(move |_bot: Arc<T>, ctx: Context| {
            let index = Arc::clone(&index);
            let roles = Arc::clone(&roles);
            Box::pin(async move {
                let argument = ctx.captures.first().cloned().unwrap_or_default();
                let reply = index.reply(
                    &roles,
                    &ctx.raw,
                    &ctx.target,
                    ctx.sender.as_ref(),
                    &argument,
                );
                ctx.reply(reply)
            })
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A command handler for the index: `name`, with an optional role and
    /// target, and a scope.
    fn guard(
        name: &str,
        role: Option<&str>,
        target: Option<&str>,
        scope: Scope,
    ) -> (String, Guard) {
        (
            name.to_string(),
            Guard {
                trigger: Trigger::Command {
                    name: name.to_string(),
                    target: target.map(str::to_string),
                    role: role.map(str::to_string),
                },
                scope,
            },
        )
    }

    fn help(command: &str, usage: &str, summary: Option<&str>) -> CommandHelp {
        CommandHelp {
            command: command.to_string(),
            usage: usage.to_string(),
            summary: summary.map(str::to_string),
        }
    }

    /// An index with `!echo`, `!seen` (in #rust only), `!kick` (role `op`), and
    /// `!secret` (private messages only).
    fn index() -> HelpIndex {
        let mut index = HelpIndex::default();
        index.add(HelpSource {
            help: vec![
                help("echo", "!echo <text>", Some("Say the text again.")),
                help("seen", "!seen <nick>", None),
                help("kick", "!kick <nick>", Some("Kick a user.")),
                help("secret", "!secret", None),
            ],
            guards: vec![
                guard("echo", None, None, Scope::Any),
                guard("seen", None, Some("#rust"), Scope::Any),
                guard("kick", Some("op"), None, Scope::Any),
                guard("secret", None, None, Scope::Private),
            ],
        });
        index.add_help_command();
        index
    }

    fn roles() -> Vec<(String, Vec<String>)> {
        vec![("op".to_string(), vec!["*!*@ops.host".to_string()])]
    }

    fn user(host: &str) -> User {
        User {
            nick: "alice".into(),
            user: "a".to_string(),
            host: host.to_string(),
        }
    }

    /// The reply to `text` from `sender` in `target`.
    fn ask(target: &str, sender: &User, argument: &str) -> String {
        let msg: Message = format!(":{} PRIVMSG {target} :!help {argument}", sender.hostmask())
            .parse()
            .expect("valid message");
        index().reply(
            &roles(),
            &msg,
            &Target::from_raw(target),
            Some(sender),
            argument,
        )
    }

    #[test]
    fn help_lists_the_commands_that_the_sender_can_use_here() {
        let reply = ask("#rust", &user("home.host"), "");

        assert_eq!(
            reply,
            "Commands: !echo, !help, !seen. Use !help <command> for details."
        );
    }

    #[test]
    fn help_lists_a_role_command_for_a_sender_with_the_role() {
        let reply = ask("#rust", &user("ops.host"), "");

        assert_eq!(
            reply,
            "Commands: !echo, !help, !kick, !seen. Use !help <command> for details."
        );
    }

    #[test]
    fn help_leaves_out_a_command_of_another_channel() {
        let reply = ask("#other", &user("home.host"), "");

        assert_eq!(
            reply,
            "Commands: !echo, !help. Use !help <command> for details."
        );
    }

    #[test]
    fn help_in_a_private_message_lists_the_private_commands() {
        let reply = ask("bot", &user("home.host"), "");

        assert_eq!(
            reply,
            "Commands: !echo, !help, !secret. Use !help <command> for details."
        );
    }

    #[test]
    fn help_for_a_command_gives_its_usage_and_summary() {
        assert_eq!(
            ask("#rust", &user("home.host"), "echo"),
            "!echo <text> — Say the text again."
        );
    }

    #[test]
    fn help_for_a_command_without_a_summary_gives_its_usage() {
        assert_eq!(ask("#rust", &user("home.host"), "seen"), "!seen <nick>");
    }

    #[test]
    fn help_for_a_command_accepts_the_prefix_and_another_case() {
        assert_eq!(
            ask("#rust", &user("home.host"), "!ECHO"),
            "!echo <text> — Say the text again."
        );
    }

    #[test]
    fn help_hides_a_role_command_from_a_sender_without_the_role() {
        assert_eq!(
            ask("#rust", &user("home.host"), "kick"),
            "No command !kick. Use !help to list the commands."
        );
    }

    #[test]
    fn help_for_an_unknown_command_says_so() {
        assert_eq!(
            ask("#rust", &user("home.host"), "dance"),
            "No command !dance. Use !help to list the commands."
        );
    }

    #[test]
    fn help_describes_itself() {
        assert_eq!(
            ask("#rust", &user("home.host"), "help"),
            "!help [command] — List the commands that you can use, or show how to use one."
        );
    }

    #[test]
    fn a_command_with_two_handlers_shows_one_time() {
        let mut index = HelpIndex::default();
        index.add(HelpSource {
            help: vec![
                help("seen", "!seen <nick>", None),
                help("seen", "!seen <nick>", None),
            ],
            guards: vec![
                guard("seen", None, Some("#rust"), Scope::Any),
                guard("seen", None, Some("#tokio"), Scope::Any),
            ],
        });
        let msg: Message = ":alice!a@h PRIVMSG #tokio :!help".parse().unwrap();

        let reply = index.reply(&[], &msg, &Target::from_raw("#tokio"), None, "");

        assert_eq!(reply, "Commands: !seen. Use !help <command> for details.");
    }
}
