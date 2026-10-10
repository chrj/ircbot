# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.7.0](https://github.com/chrj/ircbot/compare/ircbot-macros-v0.6.0...ircbot-macros-v0.7.0) - 2026-10-10

### Added

- add the ops plugin ([#201](https://github.com/chrj/ircbot/pull/201))
- [**breaking**] let a role match the hostmask, the services account, or either ([#196](https://github.com/chrj/ircbot/pull/196))

  `with_role` takes `impl Into<Role>` instead of a list of patterns, so a call with an iterator that is not an array or a `Vec` must use `Role::hostmask`, and the public `authorized` function has a new `account` argument.

- add the notify plugin ([#194](https://github.com/chrj/ircbot/pull/194))
- add the ircbot-plugins crate with the seen plugin ([#192](https://github.com/chrj/ircbot/pull/192))
- [**breaking**] add a built-in !help command ([#190](https://github.com/chrj/ircbot/pull/190))

  `CommandOwner` is now `#[non_exhaustive]` and has a new variant `Help`, so a `match` on it needs a wildcard arm.

- [**breaking**] run plugins on #[bot], and connect in main_loop ([#189](https://github.com/chrj/ircbot/pull/189))

  The `new` and `new_with_state` constructors of a `#[bot]` no longer connect and are no longer async, so remove the `.await?` after them; `main_loop` now connects and returns the connection error, and it refuses to start, with a `StartError`, when a command needs a role that no `with_role` defines.

- add typed key/value data to the store ([#186](https://github.com/chrj/ircbot/pull/186))
- add a SQLite store for bot data ([#185](https://github.com/chrj/ircbot/pull/185))
- add no_default flag for bot state without Default ([#181](https://github.com/chrj/ircbot/pull/181))
- add new_with_state constructor for bots with state ([#179](https://github.com/chrj/ircbot/pull/179))

### Fixed

- keep !seen of the sqlite_bot example inside the channel that asks ([#193](https://github.com/chrj/ircbot/pull/193))
- let an Option<String> argument come before other arguments ([#191](https://github.com/chrj/ircbot/pull/191))

### Other

- use the store in the SQLite example ([#187](https://github.com/chrj/ircbot/pull/187))
- add a SQLite example with no_default state ([#182](https://github.com/chrj/ircbot/pull/182))

## [0.6.0](https://github.com/chrj/ircbot/compare/ircbot-macros-v0.5.0...ircbot-macros-v0.6.0) - 2026-09-22

### Added

- [**breaking**] [171] Remove hot-reload: high complexity, low value, no TLS support ([#172](https://github.com/chrj/ircbot/pull/172))

  The hot-reload feature is removed. It was a high-complexity feature that brought little value, and it never worked for a TLS connection, because a TLS session cannot survive an `exec`. The `hot_reload` module, `State::try_inherit_from_env` and `State::raw_fd` go with it, and a bot takes a restart to run a new binary now.

- [165] Retry the reconnect with a growing delay ([#166](https://github.com/chrj/ircbot/pull/166))

## [0.5.0](https://github.com/chrj/ircbot/compare/ircbot-macros-v0.4.3...ircbot-macros-v0.5.0) - 2026-09-17

### Added

- [153] Ignore senders by hostmask before the dispatch ([#163](https://github.com/chrj/ircbot/pull/163))
- [**breaking**] [156] Add a scope option for channel or private handlers ([#162](https://github.com/chrj/ircbot/pull/162))
- [**breaking**] match triggers on the text without IRC formatting codes ([#161](https://github.com/chrj/ircbot/pull/161))
- [155] Add a helper that removes the IRC formatting codes ([#160](https://github.com/chrj/ircbot/pull/160))
- [**breaking**] [154] Do not give the messages of the bot to handlers ([#159](https://github.com/chrj/ircbot/pull/159))
- [152] Add TestBot to send a raw IRC line through the dispatch ([#157](https://github.com/chrj/ircbot/pull/157))
- [**breaking**] add action and ctcp triggers, keep CTCP out of text triggers ([#150](https://github.com/chrj/ircbot/pull/150))

## [0.4.3](https://github.com/chrj/ircbot/compare/ircbot-macros-v0.4.2...ircbot-macros-v0.4.3) - 2026-08-17

### Added

- SASL authentication and IRCv3 capability negotiation ([#138](https://github.com/chrj/ircbot/pull/138))

### Other

- warn on missing documentation ([#131](https://github.com/chrj/ircbot/pull/131))

## [0.4.2](https://github.com/chrj/ircbot/compare/ircbot-macros-v0.4.1...ircbot-macros-v0.4.2) - 2026-08-04

### Other

- *(deps)* bump syn from 2.0.118 to 3.0.3 ([#121](https://github.com/chrj/ircbot/pull/121))

## [0.4.1](https://github.com/chrj/ircbot/compare/ircbot-macros-v0.4.0...ircbot-macros-v0.4.1) - 2026-07-26

### Other

- bump the documented dependency version to 0.4 ([#116](https://github.com/chrj/ircbot/pull/116))

## [0.4.0](https://github.com/chrj/ircbot/compare/ircbot-macros-v0.3.0...ircbot-macros-v0.4.0) - 2026-07-26

### Added

- [**breaking**] TLS support via rustls ([#112](https://github.com/chrj/ircbot/pull/112))

## [0.3.0](https://github.com/chrj/ircbot/compare/ircbot-macros-v0.2.1...ircbot-macros-v0.3.0) - 2026-07-25

### Added

- role-based command access control ([#102](https://github.com/chrj/ircbot/pull/102))
- typed positional command arguments ([#101](https://github.com/chrj/ircbot/pull/101))
- opt-in keepnick to reclaim the desired nick ([#97](https://github.com/chrj/ircbot/pull/97))

### Other

- *(deps)* bump cron from 0.15.0 to 0.17.0 ([#108](https://github.com/chrj/ircbot/pull/108))
- bump README dependency version to 0.3 ([#103](https://github.com/chrj/ircbot/pull/103))

## [0.2.1](https://github.com/chrj/ircbot/compare/ircbot-macros-v0.2.0...ircbot-macros-v0.2.1) - 2026-06-04

### Added

- structured logging via tracing with opt-in protocol logs ([#96](https://github.com/chrj/ircbot/pull/96))

### Other

- Bump crate version in README.md, add agent instructions. ([#94](https://github.com/chrj/ircbot/pull/94))

## [0.2.0](https://github.com/chrj/ircbot/compare/ircbot-macros-v0.1.9...ircbot-macros-v0.2.0) - 2026-06-03

### Other

- align doc comments with the Target enum and is_channel method ([#93](https://github.com/chrj/ircbot/pull/93))
- *(types)* [**breaking**] introduce Nick and Channel newtypes ([#90](https://github.com/chrj/ircbot/pull/90))
- *(context)* [**breaking**] make notice and whisper synchronous ([#89](https://github.com/chrj/ircbot/pull/89))

## [0.1.9](https://github.com/chrj/ircbot/compare/ircbot-macros-v0.1.8...ircbot-macros-v0.1.9) - 2026-06-02

### Added

- *(macro)* generate `from_state` constructor for unit-testing handlers ([#84](https://github.com/chrj/ircbot/pull/84))
- configurable CTCP VERSION reply ([#82](https://github.com/chrj/ircbot/pull/82))

## [0.1.8](https://github.com/chrj/ircbot/compare/ircbot-macros-v0.1.7...ircbot-macros-v0.1.8) - 2026-06-01

### Added

- *(bot)* support custom bot state via #[bot(state = MyState)] ([#81](https://github.com/chrj/ircbot/pull/81))
- *(bot)* retry with an alternate nick on ERR_NICKNAMEINUSE ([#79](https://github.com/chrj/ircbot/pull/79))
- *(context)* add nick, is_from_self and mentions_me accessors ([#73](https://github.com/chrj/ircbot/pull/73))
- *(context)* add set_topic and kick moderation helpers ([#72](https://github.com/chrj/ircbot/pull/72))
- *(context)* add raw escape hatch ([#71](https://github.com/chrj/ircbot/pull/71))
- *(context)* add join and part helpers ([#70](https://github.com/chrj/ircbot/pull/70))
