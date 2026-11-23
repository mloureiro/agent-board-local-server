import { readdirSync } from 'node:fs';
import { join } from 'node:path';
import { afterAll, beforeAll, describe, expect, it } from 'vitest';
import {
  buildStage,
  killStrays,
  pinFileCommit,
  repoRoot,
  requireStagedCore,
  startRunner,
  type Runner,
} from './harness.ts';

const SEALED_LOOKING = (seed: string) => `abe1.${seed.padEnd(48, 'A')}`;

describe('a data folder made by the previous pin, opened by the current one', () => {
  let runner: Runner | undefined;
  let previousStage: string;

  const trpc = (port: number, path: string, input: unknown) =>
    fetch(`http://127.0.0.1:${port}/trpc/${path}`, {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify(input),
    });

  const listedIds = async (port: number, token: string): Promise<string[]> => {
    const response = await fetch(`http://127.0.0.1:${port}/api/machine/list-tasks`, {
      method: 'POST',
      headers: { authorization: `Bearer ${token}`, 'content-type': 'application/json' },
      body: '{}',
    });
    const body = (await response.json()) as { result: Array<{ id: string }> };
    return body.result.map((task) => task.id).sort();
  };

  beforeAll(() => {
    requireStagedCore();
    const previousPin = join(repoRoot, 'core.pin.previous.json');
    if (pinFileCommit(previousPin) === pinFileCommit(join(repoRoot, 'core.pin.json')))
      throw new Error('core.pin.previous.json names the commit core.pin.json names, so there is no upgrade to test');
    previousStage = buildStage(previousPin);
  }, 1_800_000);

  afterAll(async () => {
    await runner?.stop().catch(() => undefined);
    killStrays();
  }, 60_000);

  it('keeps the cards and takes one pre-upgrade backup', async () => {
    const old = await startRunner({ coreDir: previousStage });
    runner = old;
    const issued = await trpc(old.proxyPort, 'machines.issue', { name: 'smoke' });
    const token = ((await issued.json()) as { result: { data: { token: string } } }).result.data.token;
    const created = await trpc(old.proxyPort, 'tasks.create', {
      id: 'card-a',
      title: SEALED_LOOKING('title'),
      input: SEALED_LOOKING('input'),
    });
    expect(created.status).toBe(200);
    await old.stop();

    const upgraded = await startRunner({ appRoot: old.appRoot });
    runner = upgraded;

    expect(await listedIds(upgraded.proxyPort, token)).toEqual(['card-a']);
    const backups = readdirSync(join(upgraded.appRoot, 'backups'));
    expect(backups).toHaveLength(1);
    expect(backups[0]).toMatch(/^pre-upgrade-/);
  }, 600_000);
});
