# Repository Guidelines

## Project Structure & Module Organization
- Core Rust code lives in `src/`.
- Entry points: `src/main.rs` (daemon binary) and `src/lib.rs` (library surface).
- Provider pipeline modules are in `src/providers/` (`history`, `context`, `spec`, `ai`).
- Integration tests live in `tests/` (for example `tests/spec_tests.rs`, `tests/security_tests.rs`).
- Built-in command specs are in `specs/builtin/*.toml`.
- Shell integration is in `plugin/synapse.zsh`; architecture notes are in `docs/design-doc.md`.

## Build, Test, and Development Commands
- `cargo build` builds debug artifacts.
- `cargo build --release` builds optimized binaries.
- `cargo test` runs the full test suite.
- `cargo test --test spec_tests` runs one integration test file.
- `cargo clippy -- -D warnings` enforces lint rules used by CI.
- `cargo fmt --check` verifies formatting.
- `cargo run -- daemon start --foreground -vv` runs the daemon in the foreground for local debugging.
- `./scripts/setup-hooks.sh` installs pre-commit checks.

## Coding Style & Naming Conventions
- Use Rust 2021 idioms and keep code `rustfmt` clean (`cargo fmt`).
- Treat all Clippy warnings as errors (`-D warnings`).
- Follow existing naming patterns: `snake_case` for functions/modules/files, `PascalCase` for types, `SCREAMING_SNAKE_CASE` for constants.
- Keep modules focused; prefer small, composable functions over large handlers.

## Testing Guidelines
- Add integration tests under `tests/` for cross-module behavior and protocol flows.
- Use descriptive test names such as `test_spec_provider_completes_subcommands`.
- For async behavior, use Tokio test patterns already present in the repository.
- Run `cargo test`, `cargo fmt --check`, and `cargo clippy -- -D warnings` before opening a PR.

## Commit & Pull Request Guidelines
- Match existing commit style: imperative summary with optional issue/PR reference, e.g. `Add pre-commit hooks and GitHub Actions CI (#5)`.
- Keep commits focused and logically grouped.
- PRs should include:
  - Clear problem/solution summary.
  - Linked issue(s) when relevant.
  - Test evidence (commands run and results).
  - Screenshots or terminal snippets for plugin UX changes.

## Security & Configuration Tips
- Do not commit secrets or local config overrides.
- Use `config.example.toml` as the reference for configuration fields.
- Validate security-sensitive changes against `tests/security_tests.rs` and `src/security.rs`.
