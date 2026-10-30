# Contributing

Mergepatrol is early, and its design still moves. Before a large change, open an issue so we can agree on the shape first.

## Develop

You need Rust (`rust-toolchain.toml` picks the version, and rustup installs it), Node 24 with pnpm, and Docker.

```bash
docker compose -f compose.dev.yml up -d        # Postgres for the tests
(cd ui && pnpm install && pnpm build)          # the dashboard, which the binary embeds
cargo test --workspace
```

`cargo test` also writes the API types to `ui/src/api`. Commit them with the Rust change.

Before you open a PR, run what CI runs:

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
(cd ui && pnpm check)
```

A dashboard feature is a route in `crates/web/src/routes.rs`, with the MCP tool that calls it, so agents can do what the dashboard does. `AGENTS.md` says how.

The tests marked `#[ignore]` need Docker images, or a real Anthropic API key. Each one says how to run it.

## Pull requests

Title PRs as [Conventional Commits](https://www.conventionalcommits.org/): `feat: ...`, `fix(deploy): ...`. A maintainer merges them through Mergepatrol's own queue.
