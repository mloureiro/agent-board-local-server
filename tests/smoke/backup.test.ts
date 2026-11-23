import { cpSync, existsSync, readdirSync, renameSync } from 'node:fs';
import { join } from 'node:path';
import { afterAll, beforeAll, describe, expect, it } from 'vitest';
import { killStrays, requireStagedCore, startRunner, type Runner } from './harness.ts';

const SEALED_LOOKING = (seed: string) => `abe1.${seed.padEnd(48, 'A')}`;

describe('a backup restored by the documented steps', () => {
  let runner: Runner;
  let token: string;

  const trpc = (port: number, path: string, input: unknown) =>
    fetch(`http://127.0.0.1:${port}/trpc/${path}`, {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify(input),
    });

  const createCard = async (id: string) => {
    const created = await trpc(runner.proxyPort, 'tasks.create', {
      id,
      title: SEALED_LOOKING('title'),
      input: SEALED_LOOKING('input'),
    });
    expect(created.status).toBe(200);
  };

  const listedIds = async (port: number): Promise<string[]> => {
    const response = await fetch(`http://127.0.0.1:${port}/api/machine/list-tasks`, {
      method: 'POST',
      headers: { authorization: `Bearer ${token}`, 'content-type': 'application/json' },
      body: '{}',
    });
    const body = (await response.json()) as { result: Array<{ id: string }> };
    return body.result.map((task) => task.id).sort();
  };

  beforeAll(async () => {
    requireStagedCore();
    runner = await startRunner();
  }, 240_000);

  afterAll(async () => {
    await runner?.stop().catch(() => undefined);
    killStrays();
  }, 60_000);

  it('brings back the cards of the moment of the backup and not the ones made after it', async () => {
    const issued = await trpc(runner.proxyPort, 'machines.issue', { name: 'smoke' });
    token = ((await issued.json()) as { result: { data: { token: string } } }).result.data.token;
    await createCard('card-a');

    const written = runner.waitForNew(/backup written to (.+)/, 120_000);
    const resumed = runner.waitForNew(/Backend running on 127\.0\.0\.1:\d+/, 120_000);
    runner.signal('SIGUSR1');
    const backup = (await written)[1] as string;
    await resumed;
    expect(backup.startsWith(join(runner.appRoot, 'backups'))).toBe(true);

    await createCard('card-b');
    expect(await listedIds(runner.proxyPort)).toEqual(['card-a', 'card-b']);
    await runner.stop();

    const data = join(runner.appRoot, 'data');
    renameSync(join(data, 'pglite'), join(data, 'pglite.before-restore'));
    cpSync(join(backup, 'pglite'), join(data, 'pglite'), { recursive: true });
    expect(existsSync(join(data, 'pglite'))).toBe(true);
    expect(readdirSync(join(runner.appRoot, 'backups'))).toEqual([backup.split('/').pop()]);

    const restored = await startRunner({ appRoot: runner.appRoot });
    runner = restored;

    expect(await listedIds(restored.proxyPort)).toEqual(['card-a']);
  }, 600_000);
});
