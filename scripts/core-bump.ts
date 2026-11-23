import { execFileSync, spawnSync } from 'node:child_process';
import { copyFileSync, existsSync, mkdirSync, readFileSync, writeFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { failedIncludesSummary, parseBumpArgs, rewritePinCommit } from './core-bump-plan.ts';
import { CADDYFILE_COPY } from './core-build-plan.ts';
import { parseCorePin, type CorePin } from './core-pin.ts';

const root = join(dirname(fileURLToPath(import.meta.url)), '..');
const pinPath = join(root, 'core.pin.json');
const previousPath = join(root, 'core.pin.previous.json');
const fullClone = join(root, '.core', 'full.git');
const CONTRACT = 'packages/shared/src/contract.mts';

const readPin = (path: string): CorePin => parseCorePin(JSON.parse(readFileSync(path, 'utf8')));

const git = (args: string[], stdio: 'pipe' | 'inherit' = 'pipe'): string =>
  execFileSync('git', ['--git-dir', fullClone, ...args], { encoding: 'utf8', stdio: ['ignore', stdio, 'inherit'] });

const hasObject = (commit: string): boolean =>
  spawnSync('git', ['--git-dir', fullClone, 'cat-file', '-e', `${commit}^{commit}`], { stdio: 'ignore' }).status === 0;

function fetchFork(repository: string, commits: string[]): void {
  mkdirSync(dirname(fullClone), { recursive: true });
  if (!existsSync(fullClone)) execFileSync('git', ['init', '--quiet', '--bare', fullClone], { stdio: 'inherit' });
  const url = `https://github.com/${repository}.git`;
  console.log(`Fetching every branch of ${repository}`);
  git(['fetch', '--quiet', url, '+refs/heads/*:refs/remotes/fork/*'], 'inherit');
  for (const commit of commits) if (!hasObject(commit)) git(['fetch', '--quiet', url, commit], 'inherit');
}

function verifyIncludes(pin: CorePin): void {
  const failed = (pin.includes ?? []).filter(
    (include) =>
      spawnSync('git', ['--git-dir', fullClone, 'merge-base', '--is-ancestor', include.commit, pin.commit], {
        stdio: 'ignore',
      }).status !== 0,
  );
  if (failed.length > 0) {
    console.error(failedIncludesSummary(failed, pin.commit));
    process.exit(1);
  }
  console.log(`All ${pin.includes?.length ?? 0} included branches are ancestors of ${pin.commit}`);
}

function printContractDiff(from: string, to: string): void {
  console.log(`\nChanges to ${CONTRACT} from ${from} to ${to}:`);
  const diff = git(['diff', `${from}..${to}`, '--', CONTRACT]);
  console.log(diff === '' ? '(none)' : diff);
}

function main(): void {
  const request = parseBumpArgs(process.argv.slice(2));
  const current = readPin(pinPath);

  if (request.kind === 'verify') {
    const previous = readPin(previousPath);
    fetchFork(current.repository, [current.commit, previous.commit]);
    verifyIncludes(current);
    printContractDiff(previous.commit, current.commit);
    return;
  }

  if (request.commit === current.commit) throw new Error(`core.pin.json already names ${request.commit}`);
  fetchFork(current.repository, [current.commit, request.commit]);
  copyFileSync(pinPath, previousPath);
  writeFileSync(pinPath, rewritePinCommit(readFileSync(pinPath, 'utf8'), request.commit));
  writeFileSync(join(root, CADDYFILE_COPY), git(['show', `${request.commit}:apps/frontend/Caddyfile`]));
  console.log(`core.pin.json now names ${request.commit}; the old pin is in core.pin.previous.json`);
  const next = readPin(pinPath);
  verifyIncludes(next);
  printContractDiff(current.commit, next.commit);
}

main();
