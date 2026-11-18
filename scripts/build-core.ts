import { execFileSync, spawnSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { cpSync, existsSync, mkdirSync, readFileSync, readdirSync, rmSync, writeFileSync } from 'node:fs';
import { dirname, join, relative } from 'node:path';
import { fileURLToPath } from 'node:url';
import {
  BUILD_MANIFEST,
  BUN_TARGET,
  CLI_TARGET,
  REQUIRED_TOOLS,
  isBuildCurrent,
  missingTools,
  parseCoreBuild,
  stagedBinaries,
  stagingPlan,
  type CoreBuild,
} from './core-build-plan.ts';
import { parseCorePin } from './core-pin.ts';

const root = join(dirname(fileURLToPath(import.meta.url)), '..');
const tauriDir = join(root, 'src-tauri');

const run = (command: string, args: string[], cwd: string, env: NodeJS.ProcessEnv = process.env): void => {
  console.log(`$ ${command} ${args.join(' ')}`);
  execFileSync(command, args, { cwd, env, stdio: 'inherit' });
};

const output = (command: string, args: string[], cwd = root): string =>
  execFileSync(command, args, { cwd, encoding: 'utf8' }).trim();

const hasTool = (tool: string): boolean => spawnSync(tool, ['--version'], { stdio: 'ignore' }).status === 0;

function listFiles(dir: string): string[] {
  if (!existsSync(dir)) return [];
  return readdirSync(dir, { withFileTypes: true }).flatMap((entry) =>
    entry.isDirectory() ? listFiles(join(dir, entry.name)) : [join(dir, entry.name)],
  );
}

function hashStaged(): Record<string, string> {
  const manifestPath = join(tauriDir, BUILD_MANIFEST);
  const files = [...listFiles(join(tauriDir, 'binaries')), ...listFiles(join(tauriDir, 'resources'))]
    .filter((file) => file !== manifestPath)
    .sort();
  return Object.fromEntries(
    files.map((file) => [relative(tauriDir, file), createHash('sha256').update(readFileSync(file)).digest('hex')]),
  );
}

function readRecordedBuild(): CoreBuild | null {
  try {
    return parseCoreBuild(JSON.parse(readFileSync(join(tauriDir, BUILD_MANIFEST), 'utf8')));
  } catch {
    return null;
  }
}

function materialise(repository: string, commit: string): string {
  const dir = join(root, '.core', commit);
  const head = (): string | null => {
    try {
      return output('git', ['rev-parse', 'HEAD'], dir);
    } catch {
      return null;
    }
  };
  if (head() !== commit) {
    mkdirSync(dir, { recursive: true });
    if (!existsSync(join(dir, '.git'))) run('git', ['init', '--quiet'], dir);
    run('git', ['fetch', '--quiet', '--depth', '1', `https://github.com/${repository}.git`, commit], dir);
    run('git', ['checkout', '--quiet', '--detach', commit], dir);
  }
  const actual = head();
  if (actual !== commit) throw new Error(`The core checkout is at ${actual ?? 'nothing'}, not at the pinned ${commit}`);
  return dir;
}

function stage(coreDir: string, serverBinary: string, cliBinary: string): void {
  rmSync(join(tauriDir, 'binaries'), { recursive: true, force: true });
  rmSync(join(tauriDir, 'resources'), { recursive: true, force: true });
  mkdirSync(join(tauriDir, 'binaries'), { recursive: true });
  for (const step of stagingPlan()) {
    const source = join(coreDir, step.from);
    if (!existsSync(source)) throw new Error(`The core build did not produce ${step.from}`);
    const target = join(tauriDir, step.to);
    mkdirSync(dirname(target), { recursive: true });
    cpSync(source, target, { recursive: true });
  }
  cpSync(serverBinary, join(tauriDir, stagedBinaries.server));
  cpSync(cliBinary, join(tauriDir, stagedBinaries.cli));
  for (const binary of Object.values(stagedBinaries))
    run('codesign', ['--force', '--sign', '-', join(tauriDir, binary)], root);
}

function main(): void {
  const missing = missingTools(Object.fromEntries(REQUIRED_TOOLS.map((tool) => [tool, hasTool(tool)])) as never);
  if (missing.length > 0) throw new Error(`core:build needs ${missing.join(', ')} on PATH`);

  const pin = parseCorePin(JSON.parse(readFileSync(join(root, 'core.pin.json'), 'utf8')));
  const bun = output('bun', ['--version']);

  if (isBuildCurrent(readRecordedBuild(), { commit: pin.commit, bun }, hashStaged())) {
    console.log(`The core at ${pin.commit} is already built and staged`);
    return;
  }

  const coreDir = materialise(pin.repository, pin.commit);
  run('pnpm', ['install', '--frozen-lockfile'], coreDir);
  run('pnpm', ['--filter', '@agent-board/frontend', 'build'], coreDir);

  const out = join(coreDir, '.build');
  rmSync(out, { recursive: true, force: true });
  mkdirSync(out, { recursive: true });
  const cliOut = join(out, 'cli');
  const serverBinary = join(out, 'board-server');
  run(
    'bun',
    [
      'build',
      'apps/backend/src/index.ts',
      '--compile',
      `--target=${BUN_TARGET}`,
      '--no-compile-autoload-dotenv',
      '--no-compile-autoload-bunfig',
      '--outfile',
      serverBinary,
    ],
    coreDir,
  );
  run('./build.sh', [CLI_TARGET], join(coreDir, 'apps/cli'), { ...process.env, DIST_DIR: cliOut });

  stage(coreDir, serverBinary, join(cliOut, `board-${CLI_TARGET}`));

  const build: CoreBuild = { commit: pin.commit, bun, files: hashStaged() };
  writeFileSync(join(tauriDir, BUILD_MANIFEST), `${JSON.stringify(build, null, 2)}\n`);
  console.log(`Staged the core at ${pin.commit}`);
}

main();
