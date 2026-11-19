# agent-board-local-server

A macOS menu-bar app that runs an [agent-board](https://github.com/mloureiro/agent-board) server on one machine: backend, database, sign-in headers and worker, started and stopped together.

It packages the agent-board core at the commit named in `core.pin.json` and holds no server code of its own. A change to server behaviour is made in the core, and this repository then moves its pin.

Today the app starts the backend (PGlite in process, loopback only) as a supervised sidecar, serves the board through its own proxy and, once enrolled, runs the worker. A crashed process is restarted after 1 s, 5 s and 25 s and then stays down. The tray shows the state of each process and stops everything on Quit, on SIGTERM, SIGINT and SIGHUP, and on any other exit of the app (the system ending it at log out, for one) that still finds something running.

The app keeps its files under `~/Library/Application Support/dev.mloureiro.agent-board-local-server/`: `settings.json` (`userName`, `dataDir`, `port`, `startAtLogin`; read at start and never rewritten), `logs/`, `run/`, `worker/` and the data folder. The proxy secret, the machine token and the data key live in the login Keychain and nowhere else. `cargo run --example headless -- --app-root <dir>` runs the same runtime without the tray until SIGTERM.

## The tray

The app has no window and no Dock icon. The menu has "Open board", one line per process, "Connect worker…" and "Quit"; "Open board" and "Connect worker…" are enabled once the proxy runs. "Open board" hands `http://127.0.0.1:<port>/` to the desktop client (bundle identifier `com.mloureiro.agent-board-client`) when macOS knows an application with that identifier, and otherwise opens it in the default browser. The client does not act on the URL yet, so today it only comes forward. The board's keys live where it is opened: the client and each browser unlock once with the passphrase.

## The worker

The worker is the bundled `board` CLI. The app runs `board internal listen` and `board internal watch` itself and a dispatch tick at start and every two minutes; it installs nothing with launchd. Its files sit apart from the core's defaults, under `worker/`:

- `worker/config/` (`AGENT_BOARD_CONFIG_DIR`: `web.json`, `config.json`), `worker/state/` (`AGENT_BOARD_STATE_DIR`: run logs, sessions) and `worker/run/` (`AGENT_BOARD_RUN_DIR`: pid files, the dispatch lock)
- `worker/bin/board`, a link to the bundled CLI that comes first on the worker's `PATH`
- `worker/worker-credentials`, a link to the app's own executable. The CLI runs it as `AGENT_BOARD_CREDENTIALS_COMMAND`; it prints the token and the data key from the Keychain and nothing else, so neither is ever in a file or a process argument.

The `PATH` the worker gets is written out by the app (`worker/bin`, `~/.local/bin`, `/opt/homebrew/bin`, `/usr/local/bin` and the system directories), so `claude` and `gh` are found when the app is started from Finder.

Until the machine is enrolled the tray shows "Worker not connected". "Connect worker…" (later "Reconnect worker…") shows two native macOS dialogs, one for the machine token and one for the data key, both from the board's Settings page; typing is hidden and Cancel stores nothing. The app stores the two values as pending Keychain items, lets `board init` check them against the board and, only when it accepts them, keeps them. A refusal is shown with `board init`'s own message and a "Try again" button. Reconnecting stops the worker and asks its running cards to pause first, as Quit does, so they do not pick up the new pair before the board has accepted it; the worker starts again with whichever pair ends up enrolled and the paused cards resume on the next dispatch tick. A card whose agent cannot be paused (a known gap in the pinned core) keeps running.

The headless runner reads the same pair from stdin, so that neither value appears on a command line, and keeps reading further pairs as re-enrolments:

```sh
cargo run --example headless -- --app-root <dir> --port <port> --enrol-from-stdin
```

`--worker` starts an already enrolled worker from that runner's own Keychain items (one Keychain service per app root, so a scratch root never touches the app's items).

On Quit the dispatch tick, the listener and the watch unit get SIGTERM, then the running cards are asked to pause through `board internal shutdown --grace 20`, which leaves them resumable on this machine, then any worker still left is killed, and only then do the proxy and the backend stop. The whole sequence takes about 30 s at most.

To use the bundled CLI by hand (to add directories or lanes, for example), give it the worker's directories and the credentials link:

```sh
R="$HOME/Library/Application Support/dev.mloureiro.agent-board-local-server/worker"
AGENT_BOARD_CONFIG_DIR="$R/config" AGENT_BOARD_STATE_DIR="$R/state" AGENT_BOARD_RUN_DIR="$R/run" \
  AGENT_BOARD_CREDENTIALS_COMMAND="$R/worker-credentials" "$R/bin/board" dirs
```

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
pnpm test:smoke     # black-box suite against the built sidecar; run pnpm core:build first
```

## The smoke suite

`pnpm test:smoke` builds and starts the headless runner (`cargo build --example headless`) with a scratch app root and drives the real backend through the proxy: health, sign-in, the machine API, event delivery, a foreign Host, loopback-only listeners and the at-rest column inventory (`at-rest/plain-columns.json` lists every stored column that is not sealed, each with a reason). It fails with "run pnpm core:build" when the staged core is not the pin.

The header-parity test runs the Caddy image named by `ARG CADDY_IMAGE` in the pinned `apps/frontend/Dockerfile` in Docker, pulling it on first use, and compares its answers with the proxy's for a fixed request list. Without Docker it fails; set `SKIP_CADDY_PARITY=1` to skip it on purpose.

## What `core:build` produces

`pnpm core:build` fetches the core at the commit in `core.pin.json` into `.core/<commit>/`, installs it with a frozen lockfile, builds the frontend and compiles the backend and the `board` CLI with Bun for `aarch64-apple-darwin`. It stages, all git-ignored:

- `src-tauri/binaries/board-server-aarch64-apple-darwin` and `board-aarch64-apple-darwin`, ad-hoc signed because a fresh Bun executable is killed at launch on macOS until it is signed
- `src-tauri/resources/frontend/`, `drizzle/` and `pglite/` (the three PGlite files the compiled backend cannot find on its own)
- `src-tauri/resources/Caddyfile`, kept out of the bundle
- `src-tauri/resources/core-build.json`: the pin commit, the Bun version and the sha256 of every staged file

A second run does nothing while that file matches the pin and every staged file.

## Distribution

Not for distribution. The upstream core has no licence file, so no build of it may be published or shared.
