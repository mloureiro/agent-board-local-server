# agent-board-local-server

A macOS menu-bar app that runs an [agent-board](https://github.com/mloureiro/agent-board) server on one machine: backend, database, sign-in headers and worker, started and stopped together.

It packages the agent-board core at the commit named in `core.pin.json` and holds no server code of its own. A change to server behaviour is made in the core, and this repository then moves its pin.

Today the app is a shell: a tray icon with a menu and no window. It does not start the server yet.

## Requirements

- Node 24 and pnpm
- Rust (stable) with `cargo` on `PATH`

## Commands

```sh
pnpm install
pnpm dev            # run the app from source
pnpm build          # build the unsigned .app into src-tauri/target/release/bundle/macos
pnpm format:check
pnpm lint
pnpm typecheck
pnpm test
```

## Distribution

Not for distribution. The upstream core has no licence file, so no build of it may be published or shared.
