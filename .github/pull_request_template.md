<!-- Title the PR as a Conventional Commit: `feat: ...`, `fix(deploy): ...`. See CONTRIBUTING.md. -->

## What and why

<!-- What changes, and the problem it solves. Link the issue, if there is one. -->

## How you checked it

<!-- The commands you ran, or what you saw in the dashboard. -->

- [ ] `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings` and `(cd ui && pnpm check)` pass
- [ ] `cargo test --workspace` passes, and the API types it writes to `ui/src/api` are committed
- [ ] The README says what changed, if users or self-hosters see it
- [ ] A changelog entry in `content/changelog/`, if users see or feel the change (see its README)
