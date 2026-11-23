import { spawnSync } from 'node:child_process';
import { mkdirSync, mkdtempSync, readFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { afterAll, beforeAll, describe, expect, it } from 'vitest';
import { repoRoot, requireStagedCore } from './harness.ts';

interface Check {
  name: string;
}

function matches(name: string, entry: string): boolean {
  return entry.endsWith('*') ? name.startsWith(entry.slice(0, -1)) : name === entry;
}

describe('the checks the app drops from board doctor', () => {
  let scratch: string;
  let printed: string[];

  beforeAll(() => {
    requireStagedCore();
    scratch = mkdtempSync(join(tmpdir(), 'worker-check-'));
    const home = join(scratch, 'home');
    mkdirSync(home);
    const result = spawnSync(join(repoRoot, 'src-tauri/binaries/board-aarch64-apple-darwin'), ['doctor', '--json'], {
      encoding: 'utf8',
      env: {
        PATH: process.env['PATH'] ?? '',
        HOME: home,
        AGENT_BOARD_CONFIG_DIR: join(scratch, 'config'),
        AGENT_BOARD_STATE_DIR: join(scratch, 'state'),
        AGENT_BOARD_RUN_DIR: join(scratch, 'run'),
      },
    });
    printed = (JSON.parse(result.stdout) as Check[]).map((check) => check.name);
  });

  afterAll(() => {
    rmSync(scratch, { recursive: true, force: true });
  });

  it('each still match a name the staged core prints on a machine with no worker installed', () => {
    const dropped = JSON.parse(readFileSync(join(repoRoot, 'worker-check/dropped-checks.json'), 'utf8')) as string[];

    const unmatched = dropped.filter((entry) => !printed.some((name) => matches(name, entry)));

    expect(unmatched).toEqual([]);
  });
});
