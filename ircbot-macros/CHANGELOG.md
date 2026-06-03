# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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
