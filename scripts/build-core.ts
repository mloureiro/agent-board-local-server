import { execFileSync, spawnSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { cpSync, existsSync, mkdirSync, mkdtempSync, readFileSync, readdirSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join, relative, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import {
  BUILD_MANIFEST,
  BUN_TARGET,
  CADDYFILE_COPY,
  caddyfileCopyError,
  caddyfileRules,
  CLI_TARGET,
  isBuildCurrent,
  bunVersionError,
  missingTools,
  needsCheckoutRestore,
  parseBuildArgs,
  parseCoreBuild,
  parsePinnedBun,
  type ToolAvailability,
  stagedBinaries,
  stagingPlan,
  type CoreBuild,
} from './core-build-plan.ts';
import { parseCorePin } from './core-pin.ts';
import { parseKnownUnits, workerUnitsError } from './core-worker-units.ts';

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

function hashStaged(stageDir: string): Record<string, string> {
  const manifestPath = join(stageDir, BUILD_MANIFEST);
  const files = [...listFiles(join(stageDir, 'binaries')), ...listFiles(join(stageDir, 'resources'))]
    .filter((file) => file !== manifestPath)
    .sort();
  return Object.fromEntries(
    files.map((file) => [relative(stageDir, file), createHash('sha256').update(readFileSync(file)).digest('hex')]),
  );
}

const recipeFingerprint = (fingerprintCaddyfile: boolean): string => {
  const hash = createHash('sha256');
  for (const file of ['build-core.ts', 'core-build-plan.ts']) hash.update(readFileSync(join(root, 'scripts', file)));
  if (fingerprintCaddyfile) hash.update(readFileSync(join(root, CADDYFILE_COPY)));
  return hash.digest('hex');
};

function readRecordedBuild(stageDir: string): CoreBuild | null {
  try {
    return parseCoreBuild(JSON.parse(readFileSync(join(stageDir, BUILD_MANIFEST), 'utf8')));
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
  if (spawnSync('git', ['diff', '--quiet', 'HEAD'], { cwd: dir }).status !== 0)
    throw new Error(`The core checkout in ${dir} has changes that are not in the pinned commit; delete it and rebuild`);
  return dir;
}

function stage(stageDir: string, coreDir: string, serverBinary: string, cliBinary: string): void {
  rmSync(join(stageDir, 'binaries'), { recursive: true, force: true });
  rmSync(join(stageDir, 'resources'), { recursive: true, force: true });
  mkdirSync(join(stageDir, 'binaries'), { recursive: true });
  for (const step of stagingPlan()) {
    const source = join(coreDir, step.from);
    if (!existsSync(source)) throw new Error(`The core build did not produce ${step.from}`);
    const target = join(stageDir, step.to);
    mkdirSync(dirname(target), { recursive: true });
    cpSync(source, target, { recursive: true });
  }
  cpSync(serverBinary, join(stageDir, stagedBinaries.server));
  cpSync(cliBinary, join(stageDir, stagedBinaries.cli));
  for (const binary of Object.values(stagedBinaries))
    run('codesign', ['--force', '--sign', '-', join(stageDir, binary)], root);
}

// A build of another pin (--pin) stages an older core for the upgrade smoke test, which predates `internal units`.
function checkWorkerUnits(stageDir: string): void {
  const scratch = mkdtempSync(join(tmpdir(), 'worker-units-'));
  try {
    const result = spawnSync(join(stageDir, stagedBinaries.cli), ['internal', 'units'], {
      encoding: 'utf8',
      timeout: 30_000,
      env: {
        HOME: scratch,
        AGENT_BOARD_CONFIG_DIR: join(scratch, 'config'),
        AGENT_BOARD_STATE_DIR: join(scratch, 'state'),
        AGENT_BOARD_RUN_DIR: join(scratch, 'run'),
        PATH: '/usr/bin:/bin:/usr/sbin:/sbin',
      },
    });
    const excerpt = (result.stderr || result.stdout || String(result.error ?? '')).trim().slice(0, 500);
    if (result.status !== 0) throw new Error(`board internal units did not run (exit ${result.status}): ${excerpt}`);
    let printed: unknown;
    try {
      printed = JSON.parse(result.stdout);
    } catch {
      throw new Error(`board internal units did not print JSON: ${result.stdout.trim().slice(0, 500)}`);
    }
    const known = parseKnownUnits(JSON.parse(readFileSync(join(root, 'core.worker-units.json'), 'utf8')));
    const problem = workerUnitsError(printed, known);
    if (problem !== null) throw new Error(problem);
    console.log(`Checked the worker units: ${known.units.join(', ')} (schema ${known.schemas.join(', ')})`);
  } finally {
    rmSync(scratch, { recursive: true, force: true });
  }
}

function main(): void {
  const args = parseBuildArgs(process.argv.slice(2));
  const pinFile = args.pin === null ? join(root, 'core.pin.json') : resolve(args.pin);
  const stageDir = args.out === null ? tauriDir : resolve(args.out);
  const found: ToolAvailability = { git: hasTool('git'), pnpm: hasTool('pnpm'), bun: hasTool('bun') };
  const missing = missingTools(found);
  if (missing.length > 0) throw new Error(`core:build needs ${missing.join(', ')} on PATH`);

  const pin = parseCorePin(JSON.parse(readFileSync(pinFile, 'utf8')));
  const bun = output('bun', ['--version']);

  const rules = caddyfileRules(args.out);
  const recipe = recipeFingerprint(rules.fingerprint);

  const stageCurrent = isBuildCurrent(
    readRecordedBuild(stageDir),
    { commit: pin.commit, bun, recipe },
    hashStaged(stageDir),
  );
  if (stageCurrent) {
    const checkout = join(root, '.core', pin.commit);
    const restore =
      args.out === null &&
      needsCheckoutRestore(stageCurrent, {
        present: existsSync(join(checkout, '.git')),
        hasNodeModules: existsSync(join(checkout, 'node_modules')),
      });
    if (restore) {
      run('pnpm', ['install', '--frozen-lockfile'], materialise(pin.repository, pin.commit));
      console.log(`Restored the core checkout at ${pin.commit}; the staged build was already current`);
    } else {
      console.log(`The core at ${pin.commit} is already built and staged`);
    }
    if (args.pin === null) checkWorkerUnits(stageDir);
    return;
  }

  const coreDir = materialise(pin.repository, pin.commit);
  if (rules.check) {
    const copyError = caddyfileCopyError(
      readFileSync(join(coreDir, 'apps/frontend/Caddyfile'), 'utf8'),
      readFileSync(join(root, CADDYFILE_COPY), 'utf8'),
    );
    if (copyError !== null) throw new Error(copyError);
  }
  const bunError = bunVersionError(parsePinnedBun(readFileSync(join(coreDir, 'mise.toml'), 'utf8')), bun);
  if (bunError !== null) throw new Error(bunError);
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

  stage(stageDir, coreDir, serverBinary, join(cliOut, `board-${CLI_TARGET}`));

  const build: CoreBuild = {
    commit: pin.commit,
    bun,
    recipe,
    builtAt: new Date().toISOString(),
    files: hashStaged(stageDir),
  };
  writeFileSync(join(stageDir, BUILD_MANIFEST), `${JSON.stringify(build, null, 2)}\n`);
  console.log(`Staged the core at ${pin.commit}`);
  if (args.pin === null) checkWorkerUnits(stageDir);
}

main();
