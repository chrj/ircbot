# Copilot Instructions

## CI Checks

All CI checks must pass before completing any task. The CI pipeline runs the following checks:

- **Tests**: `cargo test --workspace`
- **Formatting**: `cargo fmt --all --check`
- **Clippy**: `cargo clippy --workspace --all-targets -- -D warnings`
- **Integration tests**: `cargo test --features integration --test integration -- --test-threads=1` (requires Docker with `ghcr.io/ngircd/ngircd:latest`)
- **Security audit**: `cargo audit`

Before submitting changes, run the non-Docker checks locally to verify they pass:

```sh
cargo test --workspace
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
```

## README

Any user-facing changes (new features, changed behaviour, updated defaults, new API surface) must be reflected in `README.md`.

The README must be distributed to README.me, ircbot/README.md

## Documentation

Prefer writing documentation over putting things in the README. The README should be short and crisp.
