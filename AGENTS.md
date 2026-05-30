# AGENTS.md

Guidance for AI agents and contributors working on the `ircbot` workspace. These
principles are drawn from the conventions actually observed in this repository,
combined with established Rust best practices. When in doubt, **match the
surrounding code** — consistency with what exists beats personal preference.

## Project layout

This is a Cargo workspace (`resolver = "2"`) with two published crates:

- **`ircbot`** — the async IRC bot framework (library + examples + tests).
- **`ircbot-macros`** — the `#[bot]`, `#[command]`, and `#[on]` procedural
  macros. `proc-macro = true`; depends only on `proc-macro2`, `quote`, `syn`,
  plus the validation crates (`cron`, `chrono-tz`) it needs at macro-expansion
  time.

The two crate versions are kept **in lockstep** (both `0.1.6` today). You do not
bump versions by hand — see [Releasing](#releasing).

## Before you finish: CI must pass

Every change must pass the full CI pipeline. Run the non-Docker checks locally
before considering any task complete:

```sh
cargo test --workspace
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
```

CI also enforces:

- **Integration tests** (Docker, ngIRCd): `cargo test --features integration --test integration -- --test-threads=1`
- **Security audit**: `cargo audit`
- **Docs**: `cargo doc --no-deps --workspace` with `RUSTDOCFLAGS="-D warnings"` — broken doc links fail the build.
- **Sync check**: duplicated docs must be byte-identical (see below).

**Clippy is run with `-D warnings`.** Treat every clippy lint as a hard error,
including pedantic ones already in use here (`#[must_use]`, `clippy::too_many_lines`).
Never silence a lint with a blanket `#[allow]` without a clear, local reason.

## Releasing

Releases are automated with [release-plz](https://release-plz.dev) (see
`release-plz.toml` and `.github/workflows/tag.yml`). **Do not bump versions, edit
changelogs, or push tags by hand.**

- Every push to `main` runs release-plz, which opens (or updates) a **release PR**
  that bumps both crate versions in lockstep, updates the `CHANGELOG.md` files, and
  rewrites the `ircbot-macros` dependency requirement in `ircbot/Cargo.toml`.
- Merging that release PR publishes both crates to crates.io in dependency order
  (`ircbot-macros`, then `ircbot`) and creates the `v{version}` git tag and GitHub
  release for `ircbot`. `ircbot-macros` is published silently (no tag/release).
- Lockstep is enforced by a shared `version_group` in `release-plz.toml`.

This requires the `CARGO_REGISTRY_TOKEN` and `RELEASE_PLZ_TOKEN` repository secrets.

## Documentation discipline

- **Prefer rustdoc over the README.** The README is kept short and crisp; detailed
  documentation lives in doc comments. User-facing changes (new features, changed
  behaviour, new defaults, new API surface) must be reflected in `README.md`.
- **Every public item gets a doc comment.** Look at any `pub fn`, `pub struct`, or
  `pub const` in this repo — they all have `///` docs. Match that.
- **Document failure and panics.** Functions returning `Result` carry an
  `# Errors` section; functions that can panic carry a `# Panics` section. This is
  enforced by convention throughout (`connect`, `run_bot`, `take_ctx`, the `#[bot]`
  macro, etc.).
- **Some files are duplicated and must stay identical.** CI diffs them:
  - `README.md` ↔ `ircbot/README.md`
  - `ircbot-macros/docs/command.md` ↔ `ircbot/docs/command.md`
  - `ircbot-macros/docs/on.md` ↔ `ircbot/docs/on.md`

  Edit **all** copies together. These `.md` files are pulled into rustdoc via
  `#[doc = include_str!(...)]`, so they are real API docs, not just notes.

## Error handling

- The crate defines and uses two aliases (see `ircbot/src/lib.rs`):
  - `BoxError = Box<dyn std::error::Error + Send + Sync>`
  - `Result = std::result::Result<(), BoxError>`

  Handlers return `ircbot::Result`. Use these aliases rather than re-spelling the
  boxed error type.
- **Never `unwrap()`/`expect()` on fallible runtime paths.** Lock poisoning is
  handled deliberately with `.unwrap_or_else(|e| e.into_inner())` so a panicked
  handler can't take down the dispatch loop. Background tasks log and continue
  (`eprintln!("[ircbot] ...")`) rather than panicking.
- `unwrap()`/`expect()` are acceptable in **tests**, in **macro code** (compile-time,
  with a helpful message), and for genuinely-impossible invariants — but always
  with an explanatory message (`expect("bot already started")`).
- `?` for propagation; map into `BoxError` at the boundary
  (`.map_err(|e| Box::new(e) as crate::BoxError)`).
- Custom error enums implement `Display` + `std::error::Error` by hand (see
  `Error::MissingContext`) rather than pulling in a derive macro dependency. The
  crate keeps its dependency surface deliberately small — don't add `thiserror`/
  `anyhow` without a strong reason.
- Log lines from the framework are prefixed `[ircbot]` and go to `stderr`.

## Async & concurrency

This is a Tokio program; concurrency correctness is the heart of it. Honour the
patterns already established:

- **Never hold a lock across an `.await` point.** The canonical pattern here is to
  snapshot under a brief lock and release immediately:

  ```rust
  let current: Arc<Vec<HandlerEntry<T>>> = {
      let guard = handlers.read().unwrap_or_else(|e| e.into_inner());
      Arc::clone(&*guard)
  };
  ```

  The `Arc<RwLock<Arc<...>>>` (`HandlerSet`) shape is intentional: the outer `Arc`
  is for cheap cloning, the `RwLock` serialises writes, and the inner `Arc` lets a
  reader take a snapshot with one cheap clone. Preserve this when extending it.
- **Spawned tasks must be cleaned up.** The read loop aborts the keepalive and cron
  tasks and drops the write sender before returning, then awaits the write task. Any
  new long-lived task must be aborted/joined on teardown the same way.
- **Use channels to serialise side effects.** All socket writes funnel through a
  single `mpsc::UnboundedSender<String>` drained by one write task that enforces
  token-bucket flood control. Don't write to the socket from multiple places.
- Use `tokio::select!` for racing the read loop against shutdown signals; use
  `oneshot` for one-time signalling (keepalive failure) and `AtomicBool` with
  `Ordering::Relaxed` for simple cross-task flags.
- Generic task/bot bounds are `T: Send + Sync + 'static`. Share owned state via
  `Arc<T>` and `Arc::clone` before moving into a spawned task.

## API design conventions

- **Builder pattern with `with_*` consuming setters** for optional configuration
  (`State::with_keepalive`, `with_flood_control` take `mut self` and return `Self`).
  A separate `TestContextBuilder` follows the same `self -> Self` style.
- **Accept flexible argument types at boundaries**: `impl Into<String>`,
  `impl AsRef<str>`, `impl IntoIterator<Item = impl Into<String>>`,
  `impl std::fmt::Display`. Convert to owned types once, inside.
- **Named constants for every magic value**, documented and often re-exported:
  `DEFAULT_KEEPALIVE_INTERVAL`, `DEFAULT_FLOOD_BURST`, `CMD_PREFIX`,
  `MAX_IRC_LINE`, `KEEPALIVE_TOKEN`. Don't inline literals that have meaning.
- `#[must_use]` on pure functions whose result must not be discarded
  (`check_trigger`, `glob_match`, `make_handler_set`, `message_text`).
- Keep internal-but-needed-by-macros surface in a `pub mod internal` and document
  it as such. Crate-private fields use `pub(crate)`.
- Re-export the public API flatly from `lib.rs` (`pub use bot::HandlerSet;` etc.)
  so users have one import surface.

## Security & input handling

This crate talks to a hostile network; treat all wire input as untrusted.

- **Sanitise before sending.** All outbound text passes through `sanitize()` to
  strip `\r`, `\n`, and `\0`, preventing IRC command injection. Any new send path
  must do the same. There is a regression test for this — keep it.
- **Respect protocol limits.** IRC lines are capped at 512 bytes; `make_messages`
  splits long output on UTF-8 boundaries (preferring word breaks). Don't emit raw
  unbounded strings.
- **Be careful with `unsafe`.** There is exactly one `unsafe` region (reconstructing
  a `TcpStream` from an inherited fd during hot-reload). It carries a `// Safety:`
  comment explaining the invariant. Any new `unsafe` must be similarly localised and
  justified — and avoided if at all possible.
- Slicing strings by byte offset is only done where an invariant guarantees a char
  boundary, and that invariant is spelled out in a comment (see the ASCII-nick note
  in `check_trigger`).

## Testing

- **Unit tests live in a `#[cfg(test)] mod tests` at the bottom of the file**
  they cover (`context.rs`, `testing.rs`, `irc.rs`). Integration-style tests for the
  public API live under `ircbot/tests/`.
- **One assertion concept per test, with a descriptive snake_case name** that reads
  as a sentence: `say_in_channel_sends_privmsg_to_channel`,
  `take_ctx_panics_on_second_call`. Group related tests with `// ── section ──`
  banner comments.
- **Test handlers without a network** using `ircbot::testing::TestContext`
  (`::channel`, `::private`, or `::builder()`), then assert on captured replies via
  `next_reply()` / `replies()`. New handler features should ship with this style of
  test.
- Use `#[tokio::test]` for async tests; `#[should_panic(expected = "...")]` to pin
  panic messages.
- Integration tests that need a real server are gated behind the `integration`
  feature and run single-threaded against a Dockerised ngIRCd.

## Formatting & style

- **`rustfmt` defaults, no exceptions** — `cargo fmt --all --check` gates CI.
- `edition = "2021"` in both crates.
- Group imports `std` → external crates → `crate::` with blank lines between groups.
- Use the section-banner comment style already pervasive in the codebase to
  structure longer files:

  ```rust
  // ─── dispatch ────────────────────────────────────────────────────────────────
  ```

- Prefer `let ... else { return ...; }` and `if let` over deep nesting; use
  iterator combinators (`filter_map`, `map_or`, `unwrap_or_default`) the way the
  existing code does, but don't sacrifice readability for cleverness.
- Use inline format args (`format!("{nick}")`, `write!(f, "missing context: {ctx}")`)
  rather than positional `{}` with trailing arguments.

## Procedural-macro specifics (`ircbot-macros`)

- **Validate at compile time when you can.** The `#[on(cron = "...")]` arm parses
  the cron expression and timezone during macro expansion and `panic!`s with a
  long, example-rich message on error — failing the user's build instead of their
  running bot. Prefer this kind of early, friendly failure.
- Generated code refers to the user-facing crate by its absolute path
  (`ircbot::Trigger`, `ircbot::HandlerEntry`, `std::boxed::Box`) so it works
  regardless of the caller's imports.
- Keep parsing robust: custom `syn::parse::Parse` impls for attribute args, and
  graceful fallbacks (`.unwrap_or(...)`) rather than hard failures on optional bits.
- Document macro behaviour in the shared `docs/*.md` files (which are doc-included),
  not only in code comments.

## Dependencies

- Keep the dependency tree small and the surface minimal (`irc-proto` is pulled in
  with `default-features = false`; no `anyhow`/`thiserror`). `cargo audit` runs in
  CI, and Dependabot is configured — prefer well-maintained crates and avoid adding
  new dependencies for things the std library or an existing dep already covers.
- Platform-specific deps are gated (`[target.'cfg(unix)'.dependencies] libc`), and
  the corresponding code is behind `#[cfg(unix)]` with a documented non-Unix
  fallback.
</content>
</invoke>
