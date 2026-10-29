# Mergepatrol menu bar app

Mergepatrol's merge queues in the macOS menu bar. The menu bar shows `3▸5 ⚠1`: PRs in flight,
PRs waiting, and PRs that need a human. The panel lists what is merging now and how far along
it is, what is on hold, what is next, and what left the queue today. Click a row to open its PR.

macOS 13 or later, Apple silicon only. Pure Rust: [GPUI](https://www.gpui.rs) draws the panel,
on the shell from [slopify](https://github.com/jnsdls/slopify).

It is its own Cargo workspace, so the server's CI, on Linux, never builds GPUI.

## Run it

```bash
cargo run --manifest-path apps/tray/Cargo.toml                     # against https://mergepatrol.ai
MERGEPATROL_URL=http://localhost:8080 cargo run --manifest-path apps/tray/Cargo.toml
apps/tray/package.sh                                               # dist/Mergepatrol.app
```

## Signing in

**Sign in with GitHub** opens the browser at the server's `/auth/login`, with a loopback port, a
random `state`, and a PKCE challenge (RFC 8252). After GitHub, the server sends the browser to
`http://127.0.0.1:<port>/callback` with a one-time code. The app trades the code and its
verifier at `/auth/tray/token` for a session token, then sends the browser on to the
dashboard's `/tray` page, which says how it went.

The app keeps the token in the login Keychain (`mergepatrol-tray-session-token`, one item per
server) and sends it as `Authorization: Bearer`. The session lasts 30 days from its last use,
like a browser's.

## What it reads

Every 10 seconds, for each workspace the user can access: `/lanes`, the events of each lane's
running PR (for its step, as the dashboard's Activity derives it), and the day's `/history`.
Every 5 minutes, `/history/stats` for the typical time of each step, which the progress bars
measure against.
