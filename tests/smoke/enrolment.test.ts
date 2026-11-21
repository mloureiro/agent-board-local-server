import { randomBytes } from 'node:crypto';
import { spawnSync } from 'node:child_process';
import { readFileSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';
import { pathToFileURL } from 'node:url';
import { afterAll, beforeAll, describe, expect, it } from 'vitest';
import { killStrays, repoRoot, requireStagedCore, startRunner, type Runner } from './harness.ts';

interface Shared {
  generateDataKey: () => Uint8Array;
  encodeDataKey: (key: Uint8Array) => string;
  wrapDataKey: (wrappingKey: Uint8Array, dataKey: Uint8Array) => Promise<string>;
  deriveKeys: (dataKey: Uint8Array, owner: string) => Promise<{ keyId: string }>;
}

describe('worker enrolment against the built board', () => {
  let runner: Runner;
  let base: string;
  let dataKeyLine: string;

  const trpc = async (path: string, input: unknown): Promise<unknown> => {
    const response = await fetch(`${base}/trpc/${path}`, {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify(input),
    });
    expect(response.status, `${path} answered ${response.status}`).toBe(200);
    return ((await response.json()) as { result: { data: unknown } }).result.data;
  };

  const issue = async (name: string): Promise<string> =>
    ((await trpc('machines.issue', { name })) as { token: string }).token;

  const machineInConfig = (): unknown =>
    (JSON.parse(readFileSync(join(runner.appRoot, 'worker/config/config.json'), 'utf8')) as { machine: unknown })
      .machine;

  beforeAll(async () => {
    const commit = requireStagedCore();
    const shared = (await import(
      pathToFileURL(join(repoRoot, '.core', commit, 'packages/shared/src/index.mts')).href
    )) as Shared;
    runner = await startRunner({ enrolFromStdin: true });
    base = `http://127.0.0.1:${runner.proxyPort}`;

    const dataKey = shared.generateDataKey();
    const wrapped = await shared.wrapDataKey(randomBytes(32), dataKey);
    const { keyId } = await shared.deriveKeys(dataKey, runner.userName);
    await trpc('keys.setup', {
      keyId,
      slots: [
        {
          kind: 'passphrase',
          kdf: 'argon2id',
          m: 8192,
          t: 1,
          p: 1,
          salt: randomBytes(16).toString('base64url'),
          wrapped,
        },
        { kind: 'recovery', wrapped },
      ],
    });
    dataKeyLine = `AGENT_BOARD_DATA_KEY=${shared.encodeDataKey(dataKey)}`;
  }, 300_000);

  afterAll(async () => {
    await runner?.stop().catch(() => undefined);
    if (runner !== undefined) {
      const service = `dev.mloureiro.agent-board-local-server.headless:${runner.appRoot}`;
      for (let item = 0; item < 4; item += 1)
        if (spawnSync('security', ['delete-generic-password', '-s', service], { stdio: 'ignore' }).status !== 0) break;
    }
    killStrays();
  }, 60_000);

  it('connects a token issued to another name under the name the board gives, then follows a token for a third', async () => {
    runner.writeLine(`AGENT_BOARD_TOKEN=${await issue('tnk_one')}`);
    runner.writeLine(dataKeyLine);
    await runner.waitFor(/worker enrolled as tnk_one/, 120_000);
    expect(machineInConfig()).toBe('tnk_one');
    await runner.waitFor(/worker: ok/, 120_000);

    runner.writeLine(`AGENT_BOARD_TOKEN=${await issue('tnk_two')}`);
    runner.writeLine(dataKeyLine);
    await runner.waitFor(/worker re-enrolled as tnk_two/, 120_000);
    expect(machineInConfig()).toBe('tnk_two');
  }, 300_000);

  it("refuses a wrong token with the board's reason and writes nothing", async () => {
    const before = readFileSync(join(runner.appRoot, 'worker/config/web.json'), 'utf8');

    runner.writeLine('abm_this-token-was-never-issued-by-this-board');
    runner.writeLine(dataKeyLine);

    const failed = await runner.waitFor(/worker re-enrolment failed: (.*)/, 120_000);
    expect(failed[1]).toMatch(/refused the token/);
    expect(failed[1]).toMatch(/nothing was written/);
    expect(failed[1]).not.toContain('never-issued');
    expect(readFileSync(join(runner.appRoot, 'worker/config/web.json'), 'utf8')).toBe(before);
    expect(machineInConfig()).toBe('tnk_two');
  }, 180_000);

  it("shows the board's refusal on the worker line at the next dispatch tick", async () => {
    const configPath = join(runner.appRoot, 'worker/config/config.json');
    const config = JSON.parse(readFileSync(configPath, 'utf8')) as Record<string, unknown>;
    writeFileSync(configPath, JSON.stringify({ ...config, machine: 'tnk_other' }));
    // A token issued again for the same name replaces the one the worker holds.
    await issue('tnk_two');

    const line = await runner.waitFor(/worker: (refused|error) — (.*)/, 200_000);

    expect(line[1]).toBe('refused');
  }, 260_000);
});
