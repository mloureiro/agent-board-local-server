# agent-board-local-server

A macOS menu-bar app that runs an [agent-board](https://github.com/mloureiro/agent-board) server on one machine: backend, database, sign-in headers and worker, started and stopped together.

It packages the agent-board core at the commit named in `core.pin.json` and holds no server code of its own. A change to server behaviour is made in the core, and this repository then moves its pin.

Today the app starts the backend (PGlite in process, loopback only) as a supervised sidecar, serves the board through its own proxy and, once enrolled, runs the worker. A crashed process is restarted after 1 s, 5 s and 25 s; a fourth crash without a stable run in between leaves it down, and the count goes back to zero after 10 minutes up. A backend that stops answering its health check three times in a row (checked every 60 s) is treated as crashed. The tray shows the state of each process and stops everything on Quit, on SIGTERM, SIGINT and SIGHUP, and on any other exit of the app (the system ending it at log out, for one) that still finds something running.

The app keeps its files under `~/Library/Application Support/dev.mloureiro.agent-board-local-server/`: `settings.json` (`userName`, `dataDir`, `port`, `startAtLogin`; read at start and never rewritten), `logs/`, `run/`, `worker/` and the data folder. The proxy secret, the machine token and the data key live in the login Keychain and nowhere else. `cargo run --example headless -- --app-root <dir>` runs the same runtime without the tray until SIGTERM.

## On disk, keys and what is stored in plain form

```
~/Library/Application Support/dev.mloureiro.agent-board-local-server/
  settings.json    userName, dataDir, port, startAtLogin (read at start, never rewritten)
  app.lock         one app at a time
  logs/            backend and proxy output
  run/             pid files of the supervised processes
  data/pglite/     the database (the default dataDir)
  worker/          config/, state/, run/, bin/board, worker-credentials
```

Settings keys, all optional: `userName` (default the macOS login name, the board user), `dataDir` (default `data/` under the app folder), `port` (default 4785; it is part of the origin the browser keeps its keys under, so it should not change), `startAtLogin` (default true; the release build registers a login item, a debug build never does). A malformed or out-of-range value stops the app with a message instead of being replaced by a default.

`AGENT_BOARD_LOCAL_SERVER_ROOT` moves the whole app folder, for running a build beside an installed one. Any root other than the default also gets its own Keychain service and leaves the login item alone.

**Keys.** The board's content is sealed in the browser with a data key that the passphrase and the recovery key unlock; the server only stores sealed text and a wrapped copy of the key. The app never holds that key. The worker needs it to seal and open content, so "Connect worker…" takes it, with a machine token, and stores both in the login Keychain; the CLI reads them through the credentials helper and neither reaches a file or a process argument. The proxy secret lives in the Keychain too.

**Stored unsealed.** `at-rest/plain-columns.json` is the authority for what is stored unsealed: it lists every database column that is not sealed, each with its reason, and the smoke suite fails when a column is neither sealed nor listed. In short: ids, timestamps, the login name, workflow states and flags from fixed lists (status, priority, model name, who moved a card), scheduling numbers and cron expressions, worker machine names, the Claude session id (a pointer, not the transcript), the wrapped data key, the machine token hash (not the token), the data key fingerprint, the PR state word (not which PR), transcript event kind and output size, counts and parent sequence numbers, the CLI version and API number, and the time zone name. Titles, prompts, notes, transcripts, directories and watches are sealed. Lane and machine names are chosen by you (a lane when you add it, a machine name when you issue its token on the board) and are stored in plain text by the core's design, so a confidential project name used as either is readable from a copied database. `scripts/acceptance.ts` searches a copy of the data folder for a marker typed into a card as UTF-8 and UTF-16.

**Plain and outside the data folder.** The worker's run logs under `worker/state/` and Claude Code's own session transcripts under `~/.claude/` hold the text of runs in plain form, as they do for any core worker; they are not covered by the data folder's protection. The folder holds nothing a stolen copy of it could read as content, but the Mac's disk encryption is what protects those two.

**Keychain prompt after a rebuild.** The `.app` is ad-hoc signed, so every rebuild has a new code identity and macOS can ask again for permission to read the existing Keychain items. Choose "Always Allow", or the worker stays disconnected until you do.

## The tray

The app has no window and no Dock icon. The menu has "Open board", one line per process, "Connect worker…" and "Quit"; "Open board" and "Connect worker…" are enabled once the proxy runs. "Open board" hands `http://127.0.0.1:<port>/` to the desktop client (bundle identifier `com.mloureiro.agent-board-client`) when macOS knows an application with that identifier, and otherwise opens it in the default browser. The client does not act on the URL yet, so today it only comes forward. The board's keys live where it is opened: the client and each browser unlock once with the passphrase.

## The worker

The worker is the bundled `board` CLI. The app runs `board internal listen` and `board internal watch` itself and a dispatch tick at start and every two minutes; it installs nothing with launchd. Its files sit apart from the core's defaults, under `worker/`:

- `worker/config/` (`AGENT_BOARD_CONFIG_DIR`: `web.json`, `config.json`), `worker/state/` (`AGENT_BOARD_STATE_DIR`: run logs, sessions) and `worker/run/` (`AGENT_BOARD_RUN_DIR`: pid files, the dispatch lock)
- `worker/bin/board`, a link to the bundled CLI that comes first on the worker's `PATH`
- `worker/worker-credentials`, a link to the app's own executable. The CLI runs it as `AGENT_BOARD_CREDENTIALS_COMMAND`; it prints the token and the data key from the Keychain and nothing else, so neither is ever in a file or a process argument.

The `PATH` the worker gets is written out by the app (`worker/bin`, `~/.local/bin`, `/opt/homebrew/bin`, `/usr/local/bin` and the system directories), so `claude` and `gh` are found when the app is started from Finder.

Until the machine is enrolled the tray shows "Worker not connected". "Connect worker…" (later "Reconnect worker…") opens a small window of the app's own, with the machine token and the data key from the board's Settings page in two masked fields, each drawn with one dot per character (a count under each field says how many characters it holds). There is no machine-name field: the board names the machine after the token. "Connect" disables the form and shows "Connecting…"; the window stays open until the board has answered. On success it says "Connected as <machine name>" with a Close button; on a refusal it stays on the form with the reason (token refused, data key not the board's, board unreachable) and nothing is stored. Cancel stores nothing, the window cannot be closed while the board is being asked, and Quit closes it and exits.

The app stores the two values as pending Keychain items, lets `board init` check them against the board (it asks the board before writing any file) and, only when it accepts them, keeps them. Reconnecting stops the worker and asks its running cards to pause first, as Quit does, so they do not pick up the new pair before the board has accepted it; the worker starts again with whichever pair ends up enrolled and the paused cards resume on the next dispatch tick.

The window is a Tauri webview that loads the static page in `ui/`. Only the two commands it needs (`enrol_submit`, `enrol_cancel`) are exposed, to that window only (`src-tauri/capabilities/enrol.json`); the page's content security policy allows nothing but its own files and the app's IPC.

The tray's "Worker:" line starts as "waiting for the first dispatch tick" and then reads "ok" after a good tick. After a failed tick it shows the board's reason, "refused — …" for a 401, a 403 or a token that belongs to another machine and "error — …" for anything else, until the next good tick. The reason is the last line the tick wrote to `logs/dispatch.log`, with anything that looks like a token or a data key withheld.

The headless runner reads the same pair from stdin, so that neither value appears on a command line, and keeps reading further pairs as re-enrolments:

```sh
cargo run --example headless -- --app-root <dir> --port <port> --enrol-from-stdin
```

`--worker` starts an already enrolled worker from that runner's own Keychain items (one Keychain service per app root, so a scratch root never touches the app's items).

On Quit the dispatch tick, the listener and the watch unit get SIGTERM, then the running cards are asked to pause through `board internal shutdown --grace 20`, which leaves them resumable on this machine, then any worker still left is killed, and only then do the proxy and the backend stop. The whole sequence takes about a minute at most.

To use the bundled CLI by hand (to add directories or lanes, for example), give it the worker's directories and the credentials link:

```sh
R="$HOME/Library/Application Support/dev.mloureiro.agent-board-local-server/worker"
AGENT_BOARD_CONFIG_DIR="$R/config" AGENT_BOARD_STATE_DIR="$R/state" AGENT_BOARD_RUN_DIR="$R/run" \
  AGENT_BOARD_CREDENTIALS_COMMAND="$R/worker-credentials" "$R/bin/board" dirs
```

## Requirements

- Node 24 and pnpm
- Rust at the version pinned in `rust-toolchain.toml` (rustup installs it on first use), with `cargo` on `PATH`
- `git` and [Bun](https://bun.sh), for `pnpm core:build`

## Commands

```sh
pnpm install
pnpm format:check   # format:check, lint, typecheck and test run on a fresh checkout, no core:build needed
pnpm lint
pnpm typecheck
pnpm test
pnpm core:build     # build the pinned core and stage it for the app
pnpm dev            # run the app from source (builds the core first)
pnpm build          # build the ad-hoc signed .app into src-tauri/target/release/bundle/macos
pnpm test:smoke     # black-box suite against the built sidecar; run pnpm core:build first
pnpm acceptance     # checks the built .app on a scratch app root; see below
```

The four gates (`format:check`, `lint`, `typecheck`, `test`) also run in CI on every push and pull request. `test:smoke` and `acceptance` stay local: they need the built core. `dev` and `build` run `core:build` themselves; `test:smoke` and `acceptance` need it done first.

## Acceptance

`pnpm acceptance [--app <path>] [--root <dir>|default] [--port <n>] [--skip-marker] [--keep]` starts the built `.app` on a scratch app root (a new folder and port, `startAtLogin` off, its own Keychain service) and prints one PASS, FAIL or INFO line per check: bundle layout and part sizes, the ad-hoc signature, `/healthz`, AC13 (the board page on `localhost`), AC4 (every listener of the app's process tree is loopback), AC8 (the worker folders exist and are not the core's defaults) and idle memory (resident size and physical footprint of the app and `board-server`). Unless `--skip-marker` is given it then prints a random marker, waits for a card carrying it in title and prompt (set up encryption and create it in the browser), quits the app, and checks AC6 (no process from the bundle or the worker folders remains) and AC11 (a copy of the data folder holds the marker in no byte, as UTF-8, UTF-16LE or UTF-16BE). It exits 1 when a check fails and removes the scratch root and its Keychain items unless `--keep` is given.

`pnpm acceptance:binary` compiles the same script into `dist/acceptance` with Bun, for a Mac that has neither Node nor Bun.

## Upgrading the pin

A pin move copies the pinned `apps/frontend/Caddyfile` over `core.Caddyfile` in the same change. `pnpm core:build` fails until the two are equal. Then `pnpm test` runs the header drift test against the new pin.

`pnpm core:build --pin <pin-file> --out <dir>` builds another pin into a staging folder of its own and never touches the app's staging. `cargo run --example headless -- --app-root <dir> --core <dir>` runs a build from such a folder on a data folder, so a database made by an older pin can be opened by the new one.

## The smoke suite

`pnpm test:smoke` builds and starts the headless runner (`cargo build --example headless`) with a scratch app root and drives the real backend through the proxy: health, sign-in, the machine API, event delivery, a foreign Host, loopback-only listeners and the at-rest column inventory (`at-rest/plain-columns.json` lists every stored column that is not sealed, each with a reason). It fails with "run pnpm core:build" when the staged core is not the pin.

The header-parity test runs the Caddy image named by `ARG CADDY_IMAGE` in the pinned `apps/frontend/Dockerfile` in Docker, pulling it on first use, and compares its answers with the proxy's for a fixed request list. Without Docker it fails; set `SKIP_CADDY_PARITY=1` to skip it on purpose.

## What `core:build` produces

`pnpm core:build` fetches the core at the commit in `core.pin.json` into `.core/<commit>/`, installs it with a frozen lockfile, builds the frontend and compiles the backend and the `board` CLI with Bun for `aarch64-apple-darwin`. It stages, all git-ignored:

- `src-tauri/binaries/board-server-aarch64-apple-darwin` and `board-aarch64-apple-darwin`, ad-hoc signed because a fresh Bun executable is killed at launch on macOS until it is signed
- `src-tauri/resources/frontend/`, `drizzle/` and `pglite/` (the three PGlite files the compiled backend cannot find on its own)
- `src-tauri/resources/core-build.json`: the pin commit, the Bun version and the sha256 of every staged file

The build refuses a pin whose `apps/frontend/Caddyfile` differs from `core.Caddyfile`, the committed copy the header drift test reads.

A second run does nothing while that file matches the pin and every staged file.

Until the core is staged, the Rust build leaves the sidecars and resources out of its own config so `cargo check` and `pnpm test` compile on a fresh checkout. The bundle still needs them: `pnpm build` runs `core:build` first and the bundler fails on a missing sidecar.

## Distribution

Not for distribution. The upstream core has no licence file, so no build of it may be published or shared.
