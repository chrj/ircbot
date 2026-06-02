# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.9](https://github.com/chrj/ircbot/compare/v0.1.8...v0.1.9) - 2026-06-02

### Added

- *(macro)* generate `from_state` constructor for unit-testing handlers ([#84](https://github.com/chrj/ircbot/pull/84))
- configurable CTCP VERSION reply ([#82](https://github.com/chrj/ircbot/pull/82))

## [0.1.8](https://github.com/chrj/ircbot/compare/v0.1.7...v0.1.8) - 2026-06-01

### Added

- *(bot)* support custom bot state via #[bot(state = MyState)] ([#81](https://github.com/chrj/ircbot/pull/81))
- *(bot)* retry with an alternate nick on ERR_NICKNAMEINUSE ([#79](https://github.com/chrj/ircbot/pull/79))
- *(context)* add nick, is_from_self and mentions_me accessors ([#73](https://github.com/chrj/ircbot/pull/73))
- *(context)* add set_topic and kick moderation helpers ([#72](https://github.com/chrj/ircbot/pull/72))
- *(context)* add raw escape hatch ([#71](https://github.com/chrj/ircbot/pull/71))
- *(context)* add join and part helpers ([#70](https://github.com/chrj/ircbot/pull/70))

### Fixed

- *(hot-reload)* honour reloads in cron supervisor and preserve flood settings ([#80](https://github.com/chrj/ircbot/pull/80))
- *(context)* avoid panic in make_messages on multibyte char wider than budget ([#78](https://github.com/chrj/ircbot/pull/78))

## [0.1.7](https://github.com/chrj/ircbot/compare/v0.1.6...v0.1.7) - 2026-05-30

### Other

- Improve test coverage ([#64](https://github.com/chrj/ircbot/pull/64))
