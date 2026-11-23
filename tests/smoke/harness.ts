import { spawn, spawnSync, type ChildProcess } from 'node:child_process';
import { existsSync, mkdtempSync, readFileSync, rmSync } from 'node:fs';
import { createServer } from 'node:net';
import { tmpdir } from 'node:os';
import { basename, join, resolve } from 'node:path';

export const repoRoot = resolve(import.meta.dirname, '../..');
const tauriDir = join(repoRoot, 'src-tauri');
const runnerBinary = join(tauriDir, 'target/debug/examples/headless');

export const pinnedCaddyfile = join(repoRoot, 'core.Caddyfile');
export const stagedFrontend = join(tauriDir, 'resources/frontend');

function readJson(path: string): Record<string, unknown> {
  return JSON.parse(readFileSync(path, 'utf8')) as Record<string, unknown>;
}

export function pinnedCommit(): string {
  const commit = readJson(join(repoRoot, 'core.pin.json'))['commit'];
  if (typeof commit !== 'string') throw new Error('core.pin.json names no commit');
  return commit;
}

export function requireStagedCore(): string {
  const commit = pinnedCommit();
  let staged: unknown;
  try {
    staged = readJson(join(tauriDir, 'resources/core-build.json'))['commit'];
  } catch {
    throw new Error('The core is not staged: run pnpm core:build');
  }
  if (staged !== commit)
    throw new Error(`The staged core is ${String(staged)}, the pin is ${commit}: run pnpm core:build`);
  const checkout = join(repoRoot, '.core', commit);
  if (!existsSync(join(checkout, 'node_modules')))
    throw new Error(`The core checkout ${checkout} is missing or not installed: run pnpm core:build`);
  return commit;
}

export async function freePort(): Promise<number> {
  const server = createServer();
  await new Promise<void>((done) => server.listen(0, '127.0.0.1', done));
  const address = server.address();
  await new Promise((done) => server.close(done));
  if (address === null || typeof address === 'string') throw new Error('no port');
  return address.port;
}

let built = false;

function buildRunner(): void {
  if (built) return;
  const result = spawnSync(
    'cargo',
    ['build', '--manifest-path', join(tauriDir, 'Cargo.toml'), '--example', 'headless'],
    {
      stdio: 'inherit',
    },
  );
  if (result.status !== 0) throw new Error('cargo build --example headless failed');
  built = true;
}

function processTable(): Map<number, number[]> {
  const listing = spawnSync('ps', ['-axo', 'pid=,ppid='], { encoding: 'utf8' }).stdout;
  const children = new Map<number, number[]>();
  for (const line of listing.split('\n')) {
    const [pid, ppid] = line.trim().split(/\s+/).map(Number);
    if (pid === undefined || ppid === undefined || Number.isNaN(pid) || Number.isNaN(ppid)) continue;
    children.set(ppid, [...(children.get(ppid) ?? []), pid]);
  }
  return children;
}

export function processTree(root: number): number[] {
  const children = processTable();
  const found = [root];
  for (let index = 0; index < found.length; index += 1) found.push(...(children.get(found[index] as number) ?? []));
  return found;
}

export function findByName(pids: number[], name: string): number[] {
  const listing = spawnSync('ps', ['-o', 'pid=,comm=', '-p', pids.join(',')], { encoding: 'utf8' }).stdout;
  return listing
    .split('\n')
    .map((line) => line.trim().match(/^(\d+)\s+(.+)$/))
    .filter((match): match is RegExpMatchArray => match !== null)
    .filter((match) => basename(match[2] as string).startsWith(name))
    .map((match) => Number(match[1]));
}

function alive(pid: number): boolean {
  try {
    process.kill(pid, 0);
    return true;
  } catch {
    return false;
  }
}

export interface Runner {
  /** Writes one line to the runner's stdin; the headless runner reads the machine token and the data key from it. */
  writeLine: (line: string) => void;
  /** Resolves with the first match in what the runner printed so far or prints next; fails after the timeout. */
  waitFor: (pattern: RegExp, timeoutMs: number) => Promise<RegExpMatchArray>;
  waitForNew: (pattern: RegExp, timeoutMs: number) => Promise<RegExpMatchArray>;
  appRoot: string;
  proxyPort: number;
  backendPort: number | null;
  userName: string;
  output: () => string;
  tree: () => number[];
  stop: () => Promise<void>;
}

const running = new Set<ChildProcess>();
const scratchRoots = new Set<string>();

export function killStrays(): void {
  for (const child of running) child.kill('SIGKILL');
  running.clear();
  for (const root of scratchRoots) rmSync(root, { recursive: true, force: true });
  scratchRoots.clear();
}

export async function tearDown(...steps: Array<() => unknown>): Promise<void> {
  const failures: unknown[] = [];
  for (const step of steps) {
    try {
      await step();
    } catch (error) {
      failures.push(error);
    }
  }
  if (failures.length === 1) throw failures[0];
  if (failures.length > 1) throw new AggregateError(failures, 'Several teardown steps failed');
}

export function scratchDir(prefix: string): string {
  const dir = mkdtempSync(join(tmpdir(), prefix));
  scratchRoots.add(dir);
  return dir;
}

export async function startRunner(
  options: { appRoot?: string; upstreamPort?: number; enrolFromStdin?: boolean } = {},
): Promise<Runner> {
  buildRunner();
  const appRoot = options.appRoot ?? scratchDir('abls-smoke-');
  const port = await freePort();
  const args = ['--app-root', appRoot, '--port', String(port)];
  if (options.upstreamPort !== undefined) args.push('--upstream', `http://127.0.0.1:${options.upstreamPort}`);

  if (options.enrolFromStdin === true) args.push('--enrol-from-stdin');
  const child = spawn(runnerBinary, args, { stdio: ['pipe', 'pipe', 'pipe'] });
  if (options.enrolFromStdin !== true) child.stdin.end();
  running.add(child);
  let output = '';
  const exited = new Promise<number | null>((done) => child.once('exit', (code) => done(code)));
  const collect = (chunk: Buffer) => {
    output += chunk.toString();
  };
  child.stdout.on('data', collect);
  child.stderr.on('data', collect);

  const waitFrom = async (from: number, pattern: RegExp, timeoutMs: number): Promise<RegExpMatchArray> => {
    const deadline = Date.now() + timeoutMs;
    while (Date.now() < deadline) {
      const match = output.slice(from).match(pattern);
      if (match) return match;
      if (child.exitCode !== null) throw new Error(`The runner exited early:\n${output}`);
      await new Promise((done) => setTimeout(done, 100));
    }
    throw new Error(`Timed out waiting for ${String(pattern)}:\n${output}`);
  };

  const wait = (pattern: RegExp, timeoutMs: number) => waitFrom(0, pattern, timeoutMs);

  let backendPort: number | null = null;
  try {
    if (options.upstreamPort === undefined) {
      backendPort = Number((await wait(/Backend running on 127\.0\.0\.1:(\d+)/, 180_000))[1]);
    }
    await wait(/Proxy running on 127\.0\.0\.1:(\d+)/, 30_000);
  } catch (error) {
    child.kill('SIGKILL');
    running.delete(child);
    throw error;
  }

  const settings = readJson(join(appRoot, 'settings.json'));
  const userName = typeof settings['userName'] === 'string' ? settings['userName'] : '';

  return {
    writeLine: (line) => {
      child.stdin.write(`${line}\n`);
    },
    waitFor: wait,
    waitForNew: (pattern, timeoutMs) => waitFrom(output.length, pattern, timeoutMs),
    appRoot,
    proxyPort: port,
    backendPort,
    userName,
    output: () => output,
    tree: () => processTree(child.pid as number),
    stop: async () => {
      const members = processTree(child.pid as number);
      child.kill('SIGTERM');
      const code = await Promise.race([
        exited,
        new Promise<'timeout'>((done) => setTimeout(() => done('timeout'), 45_000)),
      ]);
      running.delete(child);
      if (code === 'timeout') {
        child.kill('SIGKILL');
        throw new Error('The runner did not stop within 45 s of SIGTERM');
      }
      const left = members.filter(alive);
      if (left.length > 0) throw new Error(`Processes left after the runner stopped: ${left.join(', ')}`);
    },
  };
}
