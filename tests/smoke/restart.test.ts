import { afterAll, beforeAll, describe, expect, it } from 'vitest';
import { findByName, killStrays, requireStagedCore, startRunner, type Runner } from './harness.ts';

describe('a backend that is killed', () => {
  let runner: Runner;

  beforeAll(async () => {
    requireStagedCore();
    runner = await startRunner();
  }, 240_000);

  afterAll(async () => {
    await runner?.stop().catch(() => undefined);
    killStrays();
  }, 60_000);

  it('is started again and answers through the proxy', async () => {
    const [backend] = findByName(runner.tree(), 'board-server');
    expect(backend).toBeDefined();

    const running = runner.waitForNew(/Backend running on 127\.0\.0\.1:\d+/, 180_000);
    process.kill(backend as number, 'SIGKILL');
    await running;

    const response = await fetch(`http://127.0.0.1:${runner.proxyPort}/healthz`);
    expect(response.status).toBe(200);
  }, 240_000);
});
