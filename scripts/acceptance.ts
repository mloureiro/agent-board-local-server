import { spawn, spawnSync, type ChildProcess } from 'node:child_process';
import { randomBytes } from 'node:crypto';
import {
  cpSync,
  existsSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  readdirSync,
  realpathSync,
  rmSync,
  statSync,
  writeFileSync,
} from 'node:fs';
import { createServer } from 'node:net';
import { homedir, tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import {
  containsMarker,
  descendantsOf,
  isInside,
  isLoopbackAddress,
  listenAddresses,
  parseProcessRows,
  survivors,
  type ProcessRow,
} from './acceptance-checks.ts';

const APP_IDENTIFIER = 'dev.mloureiro.agent-board-local-server';
const EXECUTABLE = 'agent-board-local-server';
const START_TIMEOUT_MS = 180_000;
const QUIT_TIMEOUT_MS = 60_000;
const CARD_TIMEOUT_MS = 20 * 60_000;

interface Options {
  app: string;
  root: string | null;
  port: number | null;
  skipMarker: boolean;
  keep: boolean;
}

function parseOptions(argv: string[]): Options {
  const defaultApp = resolve(
    dirname(import.meta.dirname ?? '.'),
    'src-tauri/target/release/bundle/macos/Agent Board Server.app',
  );
  const options: Options = { app: defaultApp, root: null, port: null, skipMarker: false, keep: false };
  for (let index = 0; index < argv.length; index += 1) {
    const flag = argv[index];
    const value = (): string => {
      const next = argv[(index += 1)];
      if (next === undefined) throw new Error(`${flag} needs a value`);
      return next;
    };
    if (flag === '--app') options.app = resolve(value());
    else if (flag === '--root') {
      const given = value();
      options.root = given === 'default' ? given : resolve(given);
    } else if (flag === '--port') options.port = Number(value());
    else if (flag === '--skip-marker') options.skipMarker = true;
    else if (flag === '--keep') options.keep = true;
    else throw new Error(`Unknown option ${flag}`);
  }
  return options;
}

let failures = 0;

function report(id: string, ok: boolean | null, evidence: string): void {
  const label = ok === null ? 'INFO' : ok ? 'PASS' : 'FAIL';
  if (ok === false) failures += 1;
  console.log(`${label}  ${id.padEnd(8)} ${evidence}`);
}

const sleep = (ms: number): Promise<void> => new Promise((done) => setTimeout(done, ms));

const run = (command: string, args: string[]): string =>
  spawnSync(command, args, { encoding: 'utf8', maxBuffer: 64 * 1024 * 1024 }).stdout;

const processes = (): ProcessRow[] => parseProcessRows(run('ps', ['-axo', 'pid=,ppid=,command=']));

async function freePort(): Promise<number> {
  const server = createServer();
  await new Promise<void>((done) => server.listen(0, '127.0.0.1', done));
  const address = server.address();
  await new Promise((done) => server.close(done));
  if (address === null || typeof address === 'string') throw new Error('No free port');
  return address.port;
}

async function until<T>(what: string, ms: number, probe: () => Promise<T | null>): Promise<T> {
  const deadline = Date.now() + ms;
  while (Date.now() < deadline) {
    const found = await probe().catch(() => null);
    if (found !== null) return found;
    await sleep(1_000);
  }
  throw new Error(`Timed out waiting for ${what}`);
}

const sizeKb = (path: string): number => Number(run('du', ['-sk', path]).split(/\s/)[0] ?? 0);
const mb = (kb: number): string => `${(kb / 1024).toFixed(1)} MB`;

function checkBundle(app: string): void {
  const macos = join(app, 'Contents/MacOS');
  const resources = join(app, 'Contents/Resources/resources');
  const parts = [
    ['board-server', join(macos, 'board-server')],
    ['board', join(macos, 'board')],
    ['app executable', join(macos, EXECUTABLE)],
    ['frontend', join(resources, 'frontend')],
    ['drizzle', join(resources, 'drizzle')],
    ['pglite', join(resources, 'pglite')],
  ] as const;
  const missing = parts.filter(([, path]) => !existsSync(path)).map(([name]) => name);
  const sizes = parts.map(([name, path]) => `${name} ${existsSync(path) ? mb(sizeKb(path)) : 'missing'}`).join(', ');
  report('bundle', missing.length === 0, `${mb(sizeKb(app))} in total: ${sizes}`);
  const signature = spawnSync('codesign', ['--verify', '--deep', '--strict', app], { encoding: 'utf8' });
  report('signature', signature.status === 0, signature.status === 0 ? 'ad-hoc signature verifies' : signature.stderr);
}

function walk(dir: string): string[] {
  return readdirSync(dir, { withFileTypes: true }).flatMap((entry) =>
    entry.isDirectory() ? walk(join(dir, entry.name)) : [join(dir, entry.name)],
  );
}

function searchDataFolder(dataDir: string, marker: string): { files: number; bytes: number; hits: string[] } {
  const copy = mkdtempSync(join(tmpdir(), 'abls-marker-'));
  try {
    cpSync(dataDir, copy, { recursive: true });
    const files = walk(copy);
    let bytes = 0;
    const hits: string[] = [];
    for (const file of files) {
      const content = readFileSync(file);
      bytes += content.length;
      if (containsMarker(content, marker)) hits.push(file.slice(copy.length + 1));
    }
    return { files: files.length, bytes, hits };
  } finally {
    rmSync(copy, { recursive: true, force: true });
  }
}

async function main(): Promise<void> {
  const options = parseOptions(process.argv.slice(2));
  if (!existsSync(join(options.app, 'Contents/MacOS', EXECUTABLE)))
    throw new Error(`${options.app} is not the built app (no ${EXECUTABLE}); build it or pass --app`);
  const app = realpathSync(options.app);

  checkBundle(app);

  const defaultRoot = join(homedir(), 'Library/Application Support', APP_IDENTIFIER);
  const scratch = options.root === null;
  const root = options.root === 'default' ? defaultRoot : (options.root ?? mkdtempSync(join(tmpdir(), 'abls-accept-')));
  const env: NodeJS.ProcessEnv = { ...process.env };
  delete env['AGENT_BOARD_LOCAL_SERVER_ROOT'];
  let port: number;
  let dataDir: string;
  if (root === defaultRoot) {
    if (existsSync(join(root, 'app.lock')) && processes().some((row) => row.command.includes(`${EXECUTABLE}`)))
      throw new Error('The app already runs on its default root; quit it first');
    const settingsFile = join(root, 'settings.json');
    const settings = existsSync(settingsFile)
      ? (JSON.parse(readFileSync(settingsFile, 'utf8')) as { port?: number; dataDir?: string })
      : {};
    port = settings.port ?? 4785;
    dataDir = settings.dataDir ?? join(root, 'data');
  } else {
    mkdirSync(root, { recursive: true });
    port = options.port ?? (await freePort());
    dataDir = join(root, 'data');
    writeFileSync(
      join(root, 'settings.json'),
      `${JSON.stringify({ userName: 'acceptance', dataDir, port, startAtLogin: false }, null, 2)}\n`,
    );
    env['AGENT_BOARD_LOCAL_SERVER_ROOT'] = root;
  }
  const base = `http://127.0.0.1:${port}`;
  console.log(`app root ${root}\nboard    ${base}/\n`);

  const child: ChildProcess = spawn(join(app, 'Contents/MacOS', EXECUTABLE), [], { env, stdio: 'ignore' });
  const appPid = child.pid;
  if (appPid === undefined) throw new Error('The app did not start');
  const exited = new Promise<number | null>((done) => child.on('exit', (code) => done(code)));
  let quit = false;

  try {
    const startedAt = Date.now();
    await until('the board to answer /healthz', START_TIMEOUT_MS, async () =>
      (await fetch(`${base}/healthz`)).ok ? true : null,
    );
    report('start', true, `/healthz answered ${((Date.now() - startedAt) / 1000).toFixed(1)} s after launch`);

    const home = await fetch(`http://localhost:${port}/`);
    const html = await home.text();
    report(
      'AC13',
      home.ok && html.includes('<div id="root">'),
      `GET http://localhost:${port}/ -> ${home.status}, the board page`,
    );

    const tree = descendantsOf(processes(), appPid);
    const lsof = run('lsof', ['-nP', '-a', '-p', tree.join(','), '-iTCP', '-sTCP:LISTEN']).concat(
      run('lsof', ['-nP', '-a', '-p', tree.join(','), '-iUDP']),
    );
    const addresses = listenAddresses(lsof);
    const open = addresses.filter((address) => !isLoopbackAddress(address));
    report(
      'AC4',
      addresses.length > 0 && open.length === 0,
      `${addresses.length} listeners in ${tree.length} processes: ${addresses.join(', ')}${open.length > 0 ? `; not loopback: ${open.join(', ')}` : ''}`,
    );

    const worker = join(root, 'worker');
    const coreDefaults = [join(homedir(), '.config/agent-board'), join(homedir(), '.local/state/agent-board')];
    const workerDirs = ['config', 'state', 'run'].map((name) => join(worker, name));
    const separate = workerDirs.every(
      (dir) => existsSync(dir) && statSync(dir).isDirectory() && !coreDefaults.some((core) => isInside(dir, core)),
    );
    report('AC8', separate, `worker directories ${workerDirs.join(', ')} are not under ${coreDefaults.join(' or ')}`);

    await sleep(10_000);
    const rows = processes();
    const rss = (pid: number | undefined): string => {
      if (pid === undefined) return 'not running';
      const resident = mb(Number(run('ps', ['-o', 'rss=', '-p', String(pid)]).trim()));
      const footprint = /phys_footprint: (\S+ \S+)/.exec(run('footprint', ['-p', String(pid)]))?.[1];
      return `${resident} (physical footprint ${footprint ?? 'unknown'})`;
    };
    const server = rows.find(
      (row) => descendantsOf(rows, appPid).includes(row.pid) && row.command.includes('/board-server'),
    );
    report('memory', null, `resident size 10 s after start: app ${rss(appPid)}, board-server ${rss(server?.pid)}`);

    if (!options.skipMarker) {
      const marker = `ABLS-${randomBytes(8).toString('hex')}`;
      console.log(
        `\nOpen ${base}/ in a browser, set up encryption if the board asks, and create a card whose title and\nprompt both contain\n\n    ${marker}\n\nWaiting up to ${CARD_TIMEOUT_MS / 60_000} minutes for the card to appear.\n`,
      );
      await until('a card on the board', CARD_TIMEOUT_MS, async () => {
        const response = await fetch(`${base}/trpc/tasks.list`);
        const body = (await response.json()) as { result?: { data?: unknown[] } };
        return (body.result?.data?.length ?? 0) > 0 ? true : null;
      });
      console.log('A card is on the board; quitting the app.\n');
      quit = true;
      const quitAt = Date.now();
      child.kill('SIGTERM');
      const code = await Promise.race([exited, sleep(QUIT_TIMEOUT_MS).then(() => 'timeout' as const)]);
      report(
        'quit',
        code === 0,
        `the app exited with ${String(code)} after ${((Date.now() - quitAt) / 1000).toFixed(1)} s`,
      );
      const left = survivors(processes(), [`${app}/`, `${worker}/`, `${join(root, 'data')}/`]);
      report(
        'AC6',
        left.length === 0,
        left.length === 0
          ? 'no process from the bundle or the worker directories remains'
          : left.map((row) => `${row.pid} ${row.command}`).join('; '),
      );

      const found = searchDataFolder(dataDir, marker);
      report(
        'AC11',
        found.hits.length === 0 && found.files > 0,
        `${found.files} files, ${(found.bytes / 1024 / 1024).toFixed(1)} MB searched for ${marker} as UTF-8, UTF-16LE and UTF-16BE; ${found.hits.length} hits${found.hits.length > 0 ? `: ${found.hits.join(', ')}` : ''}`,
      );
    }
  } finally {
    if (!quit) {
      child.kill('SIGTERM');
      await Promise.race([exited, sleep(QUIT_TIMEOUT_MS)]);
      if (options.skipMarker) {
        const left = survivors(processes(), [`${app}/`, `${join(root, 'worker')}/`]);
        report(
          'AC6',
          left.length === 0,
          left.length === 0
            ? 'no process from the bundle or the worker directories remains after quit'
            : left.map((row) => `${row.pid} ${row.command}`).join('; '),
        );
      }
    }
    if (scratch && !options.keep) {
      rmSync(root, { recursive: true, force: true });
      spawnSync('security', ['delete-generic-password', '-s', `${APP_IDENTIFIER}.root:${root}`, '-a', 'proxy-secret'], {
        stdio: 'ignore',
      });
    }
  }
}

main()
  .then(() => {
    console.log(failures === 0 ? '\nAll checks passed' : `\n${failures} check(s) failed`);
    process.exit(failures === 0 ? 0 : 1);
  })
  .catch((error: unknown) => {
    console.error(error instanceof Error ? error.message : String(error));
    process.exit(1);
  });
