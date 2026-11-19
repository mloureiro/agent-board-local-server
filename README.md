# agent-board-local-server

A macOS menu-bar app that runs an [agent-board](https://github.com/mloureiro/agent-board) server on one machine: backend, database, sign-in headers and worker, started and stopped together.

It packages the agent-board core at the commit named in `core.pin.json` and holds no server code of its own. A change to server behaviour is made in the core, and this repository then moves its pin.

Today the app starts the backend (PGlite in process, loopback only) as a supervised sidecar, restarts it after a crash (1 s, 5 s, 25 s, then it stays down), shows its state in the tray and stops it on Quit. The proxy and the worker are not wired yet.

The app keeps its files under `~/Library/Application Support/dev.mloureiro.agent-board-local-server/`: `settings.json` (`userName`, `dataDir`, `port`, `startAtLogin`; read at start and never rewritten), `logs/`, `run/` and the data folder. The proxy secret lives in the login Keychain. `cargo run --example headless -- --app-root <dir>` runs the same runtime without the tray until SIGTERM.

## Requirements

- Node 24 and pnpm
- Rust (stable) with `cargo` on `PATH`
- `git` and [Bun](https://bun.sh), for `pnpm core:build`

## Commands

```sh
pnpm install
pnpm core:build     # build the pinned core and stage it for the app
pnpm dev            # run the app from source (builds the core first)
pnpm build          # build the unsigned .app into src-tauri/target/release/bundle/macos
pnpm format:check
pnpm lint
pnpm typecheck
pnpm test
```

## What `core:build` produces

`pnpm core:build` fetches the core at the commit in `core.pin.json` into `.core/<commit>/`, installs it with a frozen lockfile, builds the frontend and compiles the backend and the `board` CLI with Bun for `aarch64-apple-darwin`. It stages, all git-ignored:

- `src-tauri/binaries/board-server-aarch64-apple-darwin` and `board-aarch64-apple-darwin`, ad-hoc signed because a fresh Bun executable is killed at launch on macOS until it is signed
- `src-tauri/resources/frontend/`, `drizzle/` and `pglite/` (the three PGlite files the compiled backend cannot find on its own)
- `src-tauri/resources/Caddyfile`, kept out of the bundle
- `src-tauri/resources/core-build.json`: the pin commit, the Bun version and the sha256 of every staged file

A second run does nothing while that file matches the pin and every staged file.

## Distribution

Not for distribution. The upstream core has no licence file, so no build of it may be published or shared.
