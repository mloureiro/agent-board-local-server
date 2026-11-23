import { randomBytes } from 'node:crypto';
import { spawnSync } from 'node:child_process';
import { readFileSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';
import { pathToFileURL } from 'node:url';
import { afterAll, beforeAll, describe, expect, it } from 'vitest';
import {
  alive,
  findByCommand,
  findByName,
  killStrays,
  repoRoot,
  requireStagedCore,
  rootKeychainService,
  startRunner,
  tearDown,
  uninstallScratch,
  type Runner,
} from './harness.ts';

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

  const issuedTokens: string[] = [];
  const issue = async (name: string): Promise<string> => {
    const token = ((await trpc('machines.issue', { name })) as { token: string }).token;
    issuedTokens.push(token);
    return token;
  };

  const credentialsLink = (name: string) =>
    spawnSync(join(runner.appRoot, 'worker', name), [], {
      encoding: 'utf8',
      timeout: 30_000,
      env: { ...process.env, AGENT_BOARD_CONFIG_DIR: join(runner.appRoot, 'worker/config') },
    });

  const keychainHas = (account: string) =>
    spawnSync('security', ['find-generic-password', '-s', rootKeychainService(runner.appRoot), '-a', account])
      .status === 0;

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

  const uninstallRoot = () => {
    if (runner !== undefined) uninstallScratch(runner.appRoot);
  };

  afterAll(() => tearDown(() => runner?.stop(), uninstallRoot, killStrays), 60_000);

  it('connects a token issued to another name under the name the board gives, then follows a token for a third', async () => {
    runner.writeLine(`AGENT_BOARD_TOKEN=${await issue('tnk_one')}`);
    runner.writeLine(dataKeyLine);
    await runner.waitFor(/worker enrolled as tnk_one/, 120_000);
    expect(machineInConfig()).toBe('tnk_one');
    await runner.waitFor(/Worker: ok/, 120_000);

    runner.writeLine(`AGENT_BOARD_TOKEN=${await issue('tnk_two')}`);
    runner.writeLine(dataKeyLine);
    await runner.waitFor(/worker re-enrolled as tnk_two/, 120_000);
    expect(machineInConfig()).toBe('tnk_two');
  }, 300_000);

  it("refuses a wrong token with the board's reason and writes nothing", async () => {
    const before = readFileSync(join(runner.appRoot, 'worker/config/web.json'), 'utf8');
    let treeBefore = runner.tree();
    for (let settled = false; !settled;) {
      await new Promise((done) => setTimeout(done, 2_000));
      const now = runner.tree();
      settled = now.join() === treeBefore.join();
      treeBefore = now;
    }

    runner.writeLine('abm_this-token-was-never-issued-by-this-board');
    runner.writeLine(dataKeyLine);

    const failed = await runner.waitFor(/worker re-enrolment failed: (.*)/, 120_000);
    expect(failed[1]).toMatch(/refused the token/);
    expect(failed[1]).toMatch(/nothing was written/);
    expect(failed[1]).not.toContain('never-issued');
    expect(readFileSync(join(runner.appRoot, 'worker/config/web.json'), 'utf8')).toBe(before);
    expect(machineInConfig()).toBe('tnk_two');
    expect(runner.tree().filter((pid) => treeBefore.includes(pid))).toEqual(treeBefore);
    expect(keychainHas('worker-token-pending')).toBe(false);
    expect(keychainHas('worker-data-key-pending')).toBe(false);
    expect(credentialsLink('worker-credentials-pending').status).toBe(1);
  }, 180_000);

  it('never serves a pending pair through the link the running worker uses', () => {
    const service = rootKeychainService(runner.appRoot);
    const plant = (account: string, value: string) =>
      spawnSync('security', ['add-generic-password', '-U', '-A', '-s', service, '-a', account, '-w', value]);
    expect(plant('worker-token-pending', 'abm_pending-marker').status).toBe(0);
    expect(plant('worker-data-key-pending', 'abk1_pending-marker').status).toBe(0);

    const enrolled = credentialsLink('worker-credentials');

    expect(enrolled.status).toBe(0);
    expect(enrolled.stdout).toContain(`AGENT_BOARD_TOKEN=${issuedTokens[1]}`);
    expect(enrolled.stdout).not.toContain('pending-marker');

    for (const account of ['worker-token-pending', 'worker-data-key-pending'])
      spawnSync('security', ['delete-generic-password', '-s', service, '-a', account], { stdio: 'ignore' });
  });

  it("shows the board's refusal on the worker line at the next dispatch tick", async () => {
    const configPath = join(runner.appRoot, 'worker/config/config.json');
    const config = JSON.parse(readFileSync(configPath, 'utf8')) as Record<string, unknown>;
    writeFileSync(configPath, JSON.stringify({ ...config, machine: 'tnk_other' }));
    // A token issued again for the same name replaces the one the worker holds.
    await issue('tnk_two');

    const line = await runner.waitFor(/Worker: (refused|error) — (.*)/, 200_000);

    expect(line[1]).toBe('refused');
  }, 260_000);

  it('writes no token, data key, proxy secret or sign-in value to the app logs', async () => {
    await fetch(`${base}/trpc/me`);
    const logs = ['app.log', 'proxy.log']
      .map((name) => readFileSync(join(runner.appRoot, 'logs', name), 'utf8'))
      .join('\n');
    const dataKey = dataKeyLine.split('=')[1] as string;

    expect(logs).toContain('/trpc/me');
    expect(issuedTokens.length).toBeGreaterThan(0);
    for (const token of issuedTokens) expect(logs).not.toContain(token);
    expect(logs).not.toContain('never-issued');
    expect(logs).not.toContain(dataKey);
    expect(logs).not.toMatch(/[0-9a-f]{64}/);
    expect(logs).not.toMatch(/proxy-secret|Board-Proxy-Secret|Remote-User|Remote-Groups|authorization/i);
  });

  it('sweeps the backend and the worker processes a killed runner left behind when the same root starts again', async () => {
    const before = runner.tree();
    const backend = findByName(before, 'board-server');
    const worker = findByCommand(before, /internal (listen|watch)/);
    expect(backend).toHaveLength(1);
    expect(worker).toHaveLength(2);
    const orphans = [...backend, ...worker];

    await runner.kill();

    expect(orphans.filter((pid) => !alive(pid))).toEqual([]);

    runner = await startRunner({ appRoot: runner.appRoot, worker: true });

    expect(orphans.filter(alive)).toEqual([]);
  }, 300_000);
});
