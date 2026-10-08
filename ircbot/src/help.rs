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

/// One command handler: its help text, and its trigger and scope.
///
/// A command can have more than one handler, for example one for each
/// channel, and each handler can have its own usage and role. Thus each
/// handler keeps its own help text.
struct Entry {
    help: CommandHelp,
    guard: Guard,
}

/// The help text and the trigger of each command handler of a bot and its
/// plugins.
#[derive(Default)]
pub(crate) struct HelpIndex {
    entries: Vec<Entry>,
}

/// The command handlers of one bot or plugin type, with their help text,
/// before they go in a [`HelpIndex`].
#[derive(Default)]
pub(crate) struct HelpSource {
    entries: Vec<Entry>,
}

impl HelpSource {
    /// The command handlers of `B`, with their help text.
    ///
    /// [`Bot::help`] gives one entry for each command handler, in the order of
    /// [`Bot::handlers`]. When the lists do not agree, for example for a `Bot`
    /// written by hand without `help`, each command shows only its name.
    pub(crate) fn of<B: Bot>() -> Self {
        let guards: Vec<(String, Guard)> = B::handlers()
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
        let help = B::help();
        let paired = help.len() == guards.len()
            && help
                .iter()
                .zip(&guards)
                .all(|(help, (name, _))| help.command.eq_ignore_ascii_case(name));

        let entries = if paired {
            help.into_iter()
                .zip(guards)
                .map(|(help, (_, guard))| Entry { help, guard })
                .collect()
        } else {
            guards
                .into_iter()
                .map(|(name, guard)| Entry {
                    help: CommandHelp {
                        usage: format!("!{name}"),
                        command: name,
                        summary: None,
                    },
                    guard,
                })
                .collect()
        };
        HelpSource { entries }
    }
}

impl HelpIndex {
    /// Add the command handlers of `source`.
    pub(crate) fn add(&mut self, source: HelpSource) {
        self.entries.extend(source.entries);
    }

    /// Add the built-in help command itself.
    fn add_help_command(&mut self) {
        self.entries.push(Entry {
            help: CommandHelp {
                command: HELP_COMMAND.to_string(),
                usage: HELP_USAGE.to_string(),
                summary: Some(HELP_SUMMARY.to_string()),
            },
            guard: Guard {
                trigger: help_trigger(),
                scope: Scope::Any,
            },
        });
    }

    /// The reply to `!help`, with `argument` as the text after the command.
    ///
    /// `msg` is the message with the command, `target` is where it arrived,
    /// and `sender` is who sent it.
    #[must_use]
    pub(crate) fn reply(
        &self,
        roles: &[(String, crate::Role)],
        msg: &Message,
        target: &Target,
        sender: Option<&User>,
        argument: &str,
    ) -> String {
        // The handlers that would run for this sender, here.
        let visible: Vec<&Entry> = self
            .entries
            .iter()
            .filter(|entry| {
                let guard = &entry.guard;
                let filter = match &guard.trigger {
                    Trigger::Command { target, .. } => target.as_deref(),
                    _ => None,
                };
                authorized(roles, &guard.trigger, sender, crate::role::account_tag(msg))
                    && scope_matches(guard.scope, target)
                    && target_matches(target_param(msg), filter)
            })
            .collect();

        let Some(wanted) = argument.split_whitespace().next() else {
            let mut commands: Vec<String> = visible
                .iter()
                .map(|entry| format!("!{}", entry.help.command.to_ascii_lowercase()))
                .collect();
            commands.sort_unstable();
            commands.dedup();
            return format!(
                "Commands: {}. Use !help <command> for details.",
                commands.join(", ")
            );
        };

        let wanted = wanted.trim_start_matches('!');
        let mut texts: Vec<String> = Vec::new();
        for entry in visible
            .iter()
            .filter(|entry| entry.help.command.eq_ignore_ascii_case(wanted))
        {
            let text = match &entry.help.summary {
                Some(summary) => format!("{} — {summary}", entry.help.usage),
                None => entry.help.usage.clone(),
            };
            if !texts.contains(&text) {
                texts.push(text);
            }
        }
        if texts.is_empty() {
            // A command that the sender cannot use gets the same answer as a
            // command that does not exist, so `!help` does not show it.
            return format!("No command !{wanted}. Use !help to list the commands.");
        }
        texts.join(" | ")
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
    roles: Vec<(String, crate::Role)>,
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

    /// A command handler with its help text: `name`, with an optional role
    /// and target, and a scope.
    fn entry(
        usage: &str,
        summary: Option<&str>,
        role: Option<&str>,
        target: Option<&str>,
        scope: Scope,
    ) -> Entry {
        let name = usage
            .trim_start_matches('!')
            .split_whitespace()
            .next()
            .unwrap()
            .to_string();
        Entry {
            help: CommandHelp {
                command: name.clone(),
                usage: usage.to_string(),
                summary: summary.map(str::to_string),
            },
            guard: Guard {
                trigger: Trigger::Command {
                    name,
                    target: target.map(str::to_string),
                    role: role.map(str::to_string),
                },
                scope,
            },
        }
    }

    fn index_of(entries: Vec<Entry>) -> HelpIndex {
        let mut index = HelpIndex::default();
        index.add(HelpSource { entries });
        index.add_help_command();
        index
    }

    /// An index with `!echo`, `!seen` (in #rust only), `!kick` (role `op`), and
    /// `!secret` (private messages only).
    fn index() -> HelpIndex {
        index_of(vec![
            entry(
                "!echo <text>",
                Some("Say the text again."),
                None,
                None,
                Scope::Any,
            ),
            entry("!seen <nick>", None, None, Some("#rust"), Scope::Any),
            entry(
                "!kick <nick>",
                Some("Kick a user."),
                Some("op"),
                None,
                Scope::Any,
            ),
            entry("!secret", None, None, None, Scope::Private),
        ])
    }

    fn roles() -> Vec<(String, crate::Role)> {
        vec![("op".to_string(), crate::Role::hostmask(["*!*@ops.host"]))]
    }

    fn user(host: &str) -> User {
        User {
            nick: "alice".into(),
            user: "a".to_string(),
            host: host.to_string(),
        }
    }

    /// The reply of `index` to `!help <argument>` from `sender` in `target`.
    fn ask_index(index: &HelpIndex, target: &str, sender: &User, argument: &str) -> String {
        let msg: Message = format!(":{} PRIVMSG {target} :!help {argument}", sender.hostmask())
            .parse()
            .expect("valid message");
        index.reply(
            &roles(),
            &msg,
            &Target::from_raw(target),
            Some(sender),
            argument,
        )
    }

    fn ask(target: &str, sender: &User, argument: &str) -> String {
        ask_index(&index(), target, sender, argument)
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
    fn help_lists_an_account_role_command_for_the_logged_in_account() {
        let index = index_of(vec![entry(
            "!restart",
            Some("Restart the bot."),
            Some("admin"),
            None,
            Scope::Any,
        )]);
        let roles = vec![("admin".to_string(), crate::Role::account(["alice"]))];
        let ask = |line: &str| {
            let msg: Message = line.parse().expect("valid message");
            let sender = User {
                nick: "someone".into(),
                user: "s".to_string(),
                host: "any.host".to_string(),
            };
            index.reply(&roles, &msg, &Target::from_raw("#rust"), Some(&sender), "")
        };

        let logged_in = ask("@account=alice :someone!s@any.host PRIVMSG #rust :!help");
        let other = ask("@account=mallory :someone!s@any.host PRIVMSG #rust :!help");
        let not_logged_in = ask(":someone!s@any.host PRIVMSG #rust :!help");

        assert_eq!(
            logged_in,
            "Commands: !help, !restart. Use !help <command> for details."
        );
        assert_eq!(other, "Commands: !help. Use !help <command> for details.");
        assert_eq!(
            not_logged_in,
            "Commands: !help. Use !help <command> for details."
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
    fn a_command_with_two_handlers_shows_one_time_in_the_list() {
        let index = index_of(vec![
            entry("!seen <nick>", None, None, Some("#rust"), Scope::Any),
            entry("!seen <nick>", None, None, Some("#tokio"), Scope::Any),
            entry("!seen <nick>", None, None, None, Scope::Any),
        ]);

        let reply = ask_index(&index, "#tokio", &user("home.host"), "");

        assert_eq!(
            reply,
            "Commands: !help, !seen. Use !help <command> for details."
        );
    }

    #[test]
    fn help_for_a_command_gives_the_handler_of_this_channel() {
        let index = index_of(vec![
            entry(
                "!lookup <nick>",
                Some("Find a person."),
                None,
                Some("#people"),
                Scope::Any,
            ),
            entry(
                "!lookup <id>",
                Some("Find a ticket."),
                None,
                Some("#tickets"),
                Scope::Any,
            ),
        ]);

        let reply = ask_index(&index, "#tickets", &user("home.host"), "lookup");

        assert_eq!(reply, "!lookup <id> — Find a ticket.");
    }

    #[test]
    fn help_does_not_show_the_text_of_a_role_handler_without_the_role() {
        let index = index_of(vec![
            entry("!user <nick>", Some("Show a user."), None, None, Scope::Any),
            entry(
                "!user <nick> <ban>",
                Some("Ban a user."),
                Some("op"),
                None,
                Scope::Any,
            ),
        ]);

        let without = ask_index(&index, "#rust", &user("home.host"), "user");
        let with = ask_index(&index, "#rust", &user("ops.host"), "user");

        assert_eq!(without, "!user <nick> — Show a user.");
        assert_eq!(
            with,
            "!user <nick> — Show a user. | !user <nick> <ban> — Ban a user."
        );
    }
}
