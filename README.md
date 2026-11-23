# agent-board-local-server

A macOS menu-bar app that runs an [agent-board](https://github.com/mloureiro/agent-board) server on one machine: backend, database, sign-in headers and worker, started and stopped together.

It packages the agent-board core at the commit named in `core.pin.json` and holds no server code of its own. A change to server behaviour is made in the core, and this repository then moves its pin.

Today the app starts the backend (PGlite in process, loopback only) as a supervised sidecar, serves the board through its own proxy and, once enrolled, runs the worker. A crashed process is restarted after 1 s, 5 s and 25 s; a fourth crash without a stable run in between leaves it down, and the count goes back to zero after 10 minutes up. A backend that stops answering its health check three times in a row (checked every 60 s) is treated as crashed. The tray shows the state of each process and stops everything on Quit, on SIGTERM, SIGINT and SIGHUP, and on any other exit of the app (the system ending it at log out, for one) that still finds something running.

The app keeps its files under `~/Library/Application Support/dev.mloureiro.agent-board-local-server/`: `settings.json` (`userName`, `dataDir`, `backupDir`, `port`, `startAtLogin`, `workerPath`; read at start and never rewritten), `logs/`, `run/`, `worker/` and the data folder. The proxy secret, the machine token and the data key live in the login Keychain and nowhere else. `cargo run --example headless -- --app-root <dir>` runs the same runtime without the tray until SIGTERM.

## On disk, keys and what is stored in plain form

```
~/Library/Application Support/dev.mloureiro.agent-board-local-server/
  settings.json    userName, dataDir, backupDir, port, startAtLogin, workerPath (read at start, never rewritten)
  app.lock         one app at a time
  logs/            app.log, proxy.log, backend.log, worker-listen.log, worker-watch.log and dispatch.log, each with one previous generation (.log.1), and worker-check.log (replaced by each check)
  run/             pid files of the supervised processes
  data/pglite/     the database (the default dataDir)
  data/core-commit the core commit whose migrations last ran on this database
  backups/         manual-<time>/ and pre-upgrade-<commit>-<time>/, each holding a copy of pglite/ (the default backupDir)
  worker/          config/, state/, run/, bin/board, bin/board-local, worker-credentials, worker-credentials-pending
```

Settings keys, all optional: `userName` (default the macOS login name, the board user), `dataDir` (default `data/` under the app folder), `backupDir` (default `backups/` under the app folder; an absolute path, outside an app bundle and not `dataDir` or inside it), `port` (default 4785; it is part of the origin the browser keeps its keys under, so it should not change), `startAtLogin` (default true; the release build registers a login item, a debug build never does), `workerPath` (default empty; a list of absolute directories that go on the worker's `PATH`, see "The worker"). `..` is refused in `dataDir` and `backupDir`, and `backupDir` may not resolve into `dataDir`. A malformed or out-of-range value stops the app with a message instead of being replaced by a default. A key the build does not know stops it too, so an older build refuses a `settings.json` that holds a key added by a newer one. `settings.json` is read once, at start: after a change, Quit and open the app again.

`AGENT_BOARD_LOCAL_SERVER_ROOT` moves the whole app folder, for running a build beside an installed one. Any root other than the default also gets its own Keychain service and leaves the login item alone.

**Keys.** The board's content is sealed in the browser with a data key that the passphrase and the recovery key unlock; the server only stores sealed text and a wrapped copy of the key. The app never holds that key. The worker needs it to seal and open content, so "Connect worker…" takes it, with a machine token, and stores both in the login Keychain; the CLI reads them through the credentials helper and neither reaches a file or a process argument. The proxy secret lives in the Keychain too.

**Stored unsealed.** `at-rest/plain-columns.json` is the authority for what is stored unsealed: it lists every database column that is not sealed, each with its reason, and the smoke suite fails when a column is neither sealed nor listed. In short: ids, timestamps, the login name, workflow states and flags from fixed lists (status, priority, model name, who moved a card), scheduling numbers and cron expressions, worker machine names, the Claude session id (a pointer, not the transcript), the wrapped data key, the machine token hash (not the token), the data key fingerprint, the PR state word (not which PR), transcript event kind and output size, counts and parent sequence numbers, the CLI version and API number, and the time zone name. Titles, prompts, notes, transcripts, directories and watches are sealed. Lane and machine names are chosen by you (a lane when you add it, a machine name when you issue its token on the board) and are stored in plain text by the core's design, so a confidential project name used as either is readable from a copied database. `scripts/acceptance.ts` searches a copy of the data folder for a marker typed into a card as UTF-8 and UTF-16.

**Plain and outside the data folder.** The worker's run logs under `worker/state/` and Claude Code's own session transcripts under `~/.claude/` hold the text of runs in plain form, as they do for any core worker; they are not covered by the data folder's protection. The folder holds nothing a stolen copy of it could read as content, but the Mac's disk encryption is what protects those two.

**Keychain prompt after a rebuild.** An ad-hoc signed `.app` (the default) has a new code identity on every build, so macOS can ask again for permission to read the existing Keychain items; choose "Always Allow", or the worker stays disconnected until you do. A build signed with a fixed identity keeps its code identity across rebuilds: the first signed build prompts once, and a rebuild installed over it reads the same items without a prompt. See "Signing a build".

## Signing a build

Builds are local. `pnpm build` signs the `.app` and both bundled sidecars (`board-server`, `board`) with the identity named in `APPLE_SIGNING_IDENTITY`, and ad-hoc when the variable is not set, so a checkout without an identity builds as before. Create the identity once, in the login Keychain of the Mac that builds:

1. Open Keychain Access, then Keychain Access > Certificate Assistant > Create a Certificate.
2. Name it (for example `Agent Board Local Signing`), set Identity Type to Self Signed Root and Certificate Type to Code Signing, tick "Let me override defaults", and continue to the validity step.
3. Set the validity to a long period (3650 days), accept the other defaults, and keep the login keychain as the location.
4. Build with the certificate's name: `APPLE_SIGNING_IDENTITY="Agent Board Local Signing" pnpm build`.

The first signed build prompts once for each Keychain item; choose "Always Allow". A later build signed with the same identity, installed over the previous one, reads the items without a prompt. Replacing the identity (an expired certificate, a new name) is a new code identity and prompts once again. `pnpm acceptance` reports the signer of the app and of both sidecars and fails when they differ.

## The tray

The app has no window and no Dock icon. The menu has "Open board", one line per process and the "Last backup" line, the version line, then "Restart server", "Back up now", "Restart worker", "Disconnect worker", "Check worker", "Open logs folder", "Open settings file (applies after Quit and reopen)", "Connect worker…" and "Quit". "Open board" and "Connect worker…" are enabled once the proxy runs. "Restart worker", "Disconnect worker" and "Check worker" are enabled once the worker is connected. Every item except Open board, Connect worker… and Quit is disabled while a worker enrolment, a backup, Quit or another of these actions is running, and a click that still arrives is refused and written to `logs/app.log`.

"Back up now" is enabled while the backend runs; see "Backup and restore". "Restart server" restarts the backend only, and starts the proxy again when it is not running; a backend that gave up after repeated crashes tries again. The worker and its running cards are not touched. "Restart worker" restarts the listener, the watcher and the dispatch tick without pausing running cards. "Disconnect worker" stops the worker, asks its running cards to pause, deletes the machine token and the data key from the Keychain and shows "Worker not connected"; `worker/config/` stays. The token stays valid on the board until it is revoked there. No action re-reads `settings.json`; Quit and open the app again for that. "Open board" hands `http://127.0.0.1:<port>/` to the desktop client (bundle identifier `com.mloureiro.agent-board-client`) when macOS knows an application with that identifier, and otherwise opens it in the default browser. The client does not act on the URL yet, so today it only comes forward. The board's keys live where it is opened: the client and each browser unlock once with the passphrase.

While a row is failing, the menu bar icon carries a `!` mark and its tooltip is that row's line. A row is failing when it is not started, when it crashed and no restart is pending, or when the Worker line shows "refused — …" or "error — …". The first such row in menu order is the one shown, and a restart in progress does not count. The mark and the tooltip clear when every row has recovered. A disabled line near the bottom of the menu reads "core `<7 characters of the pinned commit>` · core built `<date>`", or "core unknown" when the build manifest cannot be read.

## Login item and abnormal exits

With `startAtLogin` on, the release build writes its own launch agent, `~/Library/LaunchAgents/dev.mloureiro.agent-board-local-server.plist`, and removes the `Agent Board Server.plist` an earlier build wrote. The agent runs the app at login and has `KeepAlive` on an unsuccessful exit: when the login item started the app and it then crashes or is force quit, launchd starts it again after about ten seconds, and the start sweeps the backend and worker processes the dead app left behind. Quit from the tray, a log-out and a second launch that finds the app running all exit cleanly and are not relaunched. The change takes effect at the next login. Turning `startAtLogin` off removes the file; a debug build and a non-default root never touch it.

An app opened from Finder that crashes is not relaunched. Its leftover processes are stopped at the next launch or login.

## Backup and restore

A backup is a copy of `data/pglite/`. The app stops the backend for the copy and starts it again, so for a few seconds the board answers 502 and a dispatch tick that falls in that window may fail. Backups go to `backups/` under the app folder, or to the folder named by `backupDir` in `settings.json`. A folder on another volume makes the copy a full copy instead of a clone, and the stop lasts longer.

- **Back up now** (tray): copies into `manual-<time>/`. The newest 5 manual backups are kept. The "Last backup" line shows the time of the last one this session, or why it failed. Quit during a backup waits for the copy.
- **Before the first start on a new core**: when `data/core-commit` differs from the commit the app bundles, or is missing while a database exists, the app copies the database into `pre-upgrade-<commit>-<time>/` before the backend starts, then records the new commit. These backups are never deleted by the app. If the copy fails (disk full, folder not writable) or the bundled commit cannot be read, the backend stays stopped and its line reads "not started" with the reason, and the new core never migrates a database that has no backup. Fix the cause and choose "Restart server": it tries the backup again first.
- **A copy is whole or absent**: it is written to `<name>.partial/` and renamed when complete; leftover `.partial` folders are removed at the next manual backup.

**Restore**, with the app stopped (Quit from the tray):

1. Move `<dataDir>/pglite` aside, for example to `pglite.before-restore`.
2. Copy `<backup folder>/<backup>/pglite` to `<dataDir>/pglite`.
3. Open the app.

Leave `data/core-commit` alone: a database older than the record is migrated at start, and the backup it came from still exists. A machine token issued after the backup is not in the restored database, so the board refuses it; issue a new one and use "Connect worker…".

The headless runner takes `SIGUSR1` as "Back up now" and prints `backup written to <path>` or `backup failed: <reason>`.

## The worker

The worker is the bundled `board` CLI. The app runs `board internal listen` and `board internal watch` itself and a dispatch tick at start and every two minutes; it installs nothing with launchd. Its files sit apart from the core's defaults, under `worker/`:

- `worker/config/` (`AGENT_BOARD_CONFIG_DIR`: `web.json`, `config.json`), `worker/state/` (`AGENT_BOARD_STATE_DIR`: run logs, sessions) and `worker/run/` (`AGENT_BOARD_RUN_DIR`: pid files, the dispatch lock)
- `worker/bin/board`, a link to the bundled CLI that comes first on the worker's `PATH`
- `worker/worker-credentials`, a link to the app's own executable. The CLI runs it as `AGENT_BOARD_CREDENTIALS_COMMAND`; it prints the enrolled token and data key from the Keychain and nothing else, so neither is ever in a file or a process argument.
- `worker/worker-credentials-pending`, a second link to the same executable. Only the `board init` that checks a new pair is pointed at it; it prints the pair being checked and fails when there is none, so the running worker can never be served a pair the board has not accepted.

The `PATH` the worker gets is written out by the app: `worker/bin`, then the directories of the `workerPath` setting in the order they are listed, then `~/.local/bin`, `/opt/homebrew/bin`, `/usr/local/bin` and the system directories. `claude` and `gh` are found when the app is started from Finder, and a tool installed elsewhere (a version manager's shims, for example) is reached by listing its directory in `workerPath`:

```json
{ "workerPath": ["/Users/ines/.local/share/mise/shims"] }
```

Until the machine is enrolled the tray shows "Worker not connected". "Connect worker…" (later "Reconnect worker…") opens a small window of the app's own, with the machine token and the data key from the board's Settings page in two masked fields, each drawn with one dot per character (a count under each field says how many characters it holds). There is no machine-name field: the board names the machine after the token. "Connect" disables the form and shows "Connecting…"; the window stays open until the board has answered. On success it says "Connected as <machine name>" with a Close button; on a refusal it stays on the form with the reason (token refused, data key not the board's, board unreachable) and nothing is stored. Cancel stores nothing, the window cannot be closed while the board is being asked, and Quit closes it and exits.

The app stores the two values as pending Keychain items, lets `board init` check them against the board (it asks the board before writing any file) and, only when it accepts them, keeps them. Reconnecting checks the new pair first, while the worker keeps running with the enrolled one: a refused pair stops nothing and pauses nothing. An accepted pair stops the worker, asks its running cards to pause once, as Quit does, and starts it with the new pair; the paused cards resume on the next dispatch tick. A pending pair left behind by a crash is discarded at the next start.

The window is a Tauri webview that loads the static page in `ui/`. Only the two commands it needs (`enrol_submit`, `enrol_cancel`) are exposed, to that window only (`src-tauri/capabilities/enrol.json`); the page's content security policy allows nothing but its own files and the app's IPC.

The tray's "Worker:" line starts as "waiting for the first dispatch tick" and then reads "ok" after a good tick. After a failed tick it shows the board's reason, "refused — …" for a 401, a 403 or a token that belongs to another machine and "error — …" for anything else, until the next good tick. The reason is the last line the tick wrote to `logs/dispatch.log`, with anything that looks like a token or a data key withheld.

The headless runner reads the same pair from stdin, so that neither value appears on a command line, and keeps reading further pairs as re-enrolments:

```sh
cargo run --example headless -- --app-root <dir> --port <port> --enrol-from-stdin
```

`--worker` starts an already enrolled worker from that runner's own Keychain items (the app's own scheme: one Keychain service per app root, so a scratch root never touches the app's items; the runner's proxy secret stays in memory).

On Quit the dispatch tick, the listener and the watch unit get SIGTERM, then the running cards are asked to pause through `board internal shutdown --grace 20`, which leaves them resumable on this machine, then any worker still left is killed, and only then do the proxy and the backend stop. The whole sequence takes about a minute at most.

To use the bundled CLI by hand (to add directories or lanes, for example), run `worker/bin/board-local` by its full path. It gives the CLI the worker's directories and the credentials link, keeps the `PATH` of the terminal it runs in, and refuses `setup`, `self-update`, `restart` and `init`, which would change what the app supervises:

```sh
"$HOME/Library/Application Support/dev.mloureiro.agent-board-local-server/worker/bin/board-local" dirs
```

To type `board-local` alone, link it into a directory on your `PATH`:

```sh
ln -s "$HOME/Library/Application Support/dev.mloureiro.agent-board-local-server/worker/bin/board-local" ~/.local/bin/board-local
```

"Check worker" in the tray runs the bundled `board doctor --json` with the worker's environment, leaves out the checks that do not apply to a worker the app supervises (launchd agents and the core's own lookup of tools), and looks up `claude` and `gh` itself on the worker's `PATH`, `workerPath` included. The result goes to `logs/worker-check.log`, which the app opens, and the Worker line shows "check: N problems" ("check: 1 problem" for one). The item is enabled once the worker is connected; the next dispatch tick replaces the Worker line.

## Uninstall

Quit the app first, then run the executable inside the `.app` (the path below is for an app in `/Applications`):

```
"/Applications/Agent Board Server.app/Contents/MacOS/agent-board-local-server" --uninstall
"/Applications/Agent Board Server.app/Contents/MacOS/agent-board-local-server" --uninstall --delete-data
```

`--uninstall` removes the login item (this build's file and the one an earlier build wrote) and the app's Keychain items for the configured root: the proxy secret, the machine token and the data key, including a pair being checked. The app folder, the data and the backups stay. `--delete-data` also removes the app folder, but only when it holds this app's `settings.json` (a regular file, not a symlink); otherwise it prints that the folder does not look like this app's and leaves it. It also leaves a root that is a symlink, has a `..` component, is the home folder or any folder above it, however the path is spelled, and takes no lock in a root it refuses. A `dataDir` or `backupDir` outside the app folder is printed and left for you to remove. With the app running it prints "quit the app first" and changes nothing. Any other argument prints a usage line and exits 2.

With `AGENT_BOARD_LOCAL_SERVER_ROOT` set it acts on that root and its own Keychain service, and leaves the login item alone. An ad-hoc signed build can raise the Keychain prompt once per item; allow each. The headless runner accepts `--uninstall [--delete-data] --app-root <dir>` for a scratch root and refuses the default root. The `.app` itself is removed by hand.

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
pnpm build          # build the .app (signed with APPLE_SIGNING_IDENTITY, else ad-hoc) into src-tauri/target/release/bundle/macos
pnpm test:smoke     # black-box suite against the built sidecar; run pnpm core:build first
pnpm acceptance     # checks the built .app on a scratch app root; see below
```

The four gates (`format:check`, `lint`, `typecheck`, `test`) also run in CI on every push and pull request. `test:smoke` and `acceptance` stay local: they need the built core. `dev` and `build` run `core:build` themselves; `test:smoke` and `acceptance` need it done first.

## Acceptance

`pnpm acceptance [--app <path>] [--root <dir>|default] [--port <n>] [--skip-marker] [--keep]` starts the built `.app` on a scratch app root (a new folder and port, `startAtLogin` off, its own Keychain service) and prints one PASS, FAIL or INFO line per check: bundle layout and part sizes, the signature (the app and both sidecars must carry the same signer, ad-hoc or the named identity), `/healthz`, AC13 (the board page on `localhost`), AC4 (every listener of the app's process tree is loopback), AC8 (the worker folders exist and are not the core's defaults) and idle memory (resident size and physical footprint of the app and `board-server`). Unless `--skip-marker` is given it then prints a random marker, waits for a card carrying it in title and prompt (set up encryption and create it in the browser), quits the app, and checks AC6 (no process from the bundle or the worker folders remains) and AC11 (a copy of the data folder holds the marker in no byte, as UTF-8, UTF-16LE or UTF-16BE). It exits 1 when a check fails and removes the scratch root and its Keychain items through the app's own `--uninstall --delete-data` unless `--keep` is given.

`pnpm acceptance:binary` compiles the same script into `dist/acceptance` with Bun, for a Mac that has neither Node nor Bun.

## Upgrading the pin

Each branch in `includes` carries a one-line `why`, and `reason` says why the pin names the fork.

1. If an include changes with the new commit, edit `includes` in `core.pin.json` first: `pnpm core:bump <commit>` refuses before writing anything when an existing include is not an ancestor of the new commit.
2. `pnpm core:bump <commit>` takes a full 40-character commit of the fork. It fetches the fork in full into `.core/full.git`, copies the current pin to `core.pin.previous.json`, rewrites `commit` in `core.pin.json` and copies `apps/frontend/Caddyfile` at that commit over `core.Caddyfile`.
3. `pnpm core:bump --verify` checks that every include is an ancestor of the pinned commit, lists each one that is not and exits 1, then prints the change to `packages/shared/src/contract.mts` between `core.pin.previous.json` and the pin.
4. Run the gates and `pnpm test:smoke`.

`pnpm core:build --pin <pin-file> --out <dir>` builds another pin into a staging folder of its own, skipping the Caddyfile check, and never touches the app's staging.

## The smoke suite

`pnpm test:smoke` builds and starts the headless runner (`cargo build --example headless`) with a scratch app root and drives the real backend through the proxy: health, sign-in, the machine API, event delivery, a foreign Host, loopback-only listeners and the at-rest column inventory (`at-rest/plain-columns.json` lists every stored column that is not sealed, each with a reason). It fails with "run pnpm core:build" when the staged core is not the pin.

The upgrade case builds `core.pin.previous.json` into `.core/stage/<commit>/`, creates a card with that core on a scratch app root, then opens the same root with the app's core and checks the card is still listed and exactly one `pre-upgrade-*` backup was taken. The first run builds the previous pin and takes minutes; later runs reuse the stage. The build needs the Bun version the previous pin names and fails with a message naming it. The case fails when the previous pin names the same commit as the current one.

The header-parity test runs the Caddy image named by `ARG CADDY_IMAGE` in the pinned `apps/frontend/Dockerfile` in Docker, pulling it on first use, and compares its answers with the proxy's for a fixed request list. Without Docker it fails; set `SKIP_CADDY_PARITY=1` to skip it on purpose.

## What `core:build` produces

`pnpm core:build` fetches the core at the commit in `core.pin.json` into `.core/<commit>/`, installs it with a frozen lockfile, builds the frontend and compiles the backend and the `board` CLI with Bun for `aarch64-apple-darwin`. It stages, all git-ignored:

- `src-tauri/binaries/board-server-aarch64-apple-darwin` and `board-aarch64-apple-darwin`, ad-hoc signed because a fresh Bun executable is killed at launch on macOS until it is signed
- `src-tauri/resources/frontend/`, `drizzle/` and `pglite/` (the three PGlite files the compiled backend cannot find on its own)
- `src-tauri/resources/core-build.json`: the pin commit, the Bun version, the build time (`builtAt`, UTC) and the sha256 of every staged file

The build refuses a pin whose `apps/frontend/Caddyfile` differs from `core.Caddyfile`, the committed copy the header drift test reads; a build with `--out` skips that check.

A second run does nothing while that file matches the pin and every staged file.

Until the core is staged, the Rust build leaves the sidecars and resources out of its own config so `cargo check` and `pnpm test` compile on a fresh checkout. The bundle still needs them: `pnpm build` runs `core:build` first and the bundler fails on a missing sidecar.

## Distribution

Not for distribution. The upstream core has no licence file, so no build of it may be published or shared.
