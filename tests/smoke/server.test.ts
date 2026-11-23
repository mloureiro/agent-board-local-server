import { spawnSync } from 'node:child_process';
import { cpSync, readFileSync, readlinkSync, statSync } from 'node:fs';
import { request as httpRequest } from 'node:http';
import { join } from 'node:path';
import { pathToFileURL } from 'node:url';
import { afterAll, beforeAll, describe, expect, it } from 'vitest';
import { killStrays, repoRoot, requireStagedCore, scratchDir, startRunner, type Runner } from './harness.ts';

interface Database {
  query: <Row>(sql: string) => Promise<{ rows: Row[] }>;
  close: () => Promise<void>;
}

const SEALED_LOOKING = (seed: string) => `abe1.${seed.padEnd(48, 'A')}`;

describe('the built backend behind the proxy', () => {
  let runner: Runner;
  let base: string;
  let direct: string;
  let token: string;
  let commit: string;

  beforeAll(async () => {
    commit = requireStagedCore();
    runner = await startRunner();
    base = `http://127.0.0.1:${runner.proxyPort}`;
    direct = `http://127.0.0.1:${runner.backendPort}`;
  }, 240_000);

  afterAll(async () => {
    await runner?.stop().catch(() => undefined);
    killStrays();
  }, 60_000);

  const trpc = (path: string, input: unknown) =>
    fetch(`${base}/trpc/${path}`, {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify(input),
    });

  it('answers /healthz through the proxy with the data headers', async () => {
    const response = await fetch(`${base}/healthz`);

    expect(response.status).toBe(200);
    expect(await response.json()).toEqual({ ok: true });
    expect(response.headers.get('content-security-policy')).toBe("sandbox; default-src 'none'");
    expect(response.headers.get('cache-control')).toBe('no-store');
    expect(response.headers.get('content-type')).toBe('application/json; charset=utf-8');
    expect(readFileSync(join(runner.appRoot, 'logs/app.log'), 'utf8')).toMatch(/Backend running on 127\.0\.0\.1:\d+/);
  });

  it('signs the settings user in through the proxy and refuses the backend without the secret', async () => {
    const through = await fetch(`${base}/trpc/me`);
    const body = (await through.json()) as { result: { data: { username: string } } };

    expect(through.status).toBe(200);
    expect(body.result.data.username).toBe(runner.userName);
    expect((await fetch(`${direct}/trpc/me`)).status).toBe(403);
    expect(
      (await fetch(`${direct}/trpc/me`, { headers: { 'Remote-User': runner.userName, 'Remote-Groups': 'admins' } }))
        .status,
    ).toBe(403);
  });

  it('serves the machine API to a token issued through tRPC and refuses a call without one', async () => {
    const issued = await trpc('machines.issue', { name: 'smoke' });
    const issuedBody = (await issued.json()) as { result: { data: { token: string } } };
    token = issuedBody.result.data.token;

    const authorised = await fetch(`${base}/api/machine/list-tasks`, {
      method: 'POST',
      headers: { authorization: `Bearer ${token}`, 'content-type': 'application/json' },
      body: '{}',
    });
    const refused = await fetch(`${base}/api/machine/list-tasks`, {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: '{}',
    });

    expect(issued.status).toBe(200);
    expect(authorised.status).toBe(200);
    expect(await authorised.json()).toEqual({ result: [] });
    expect(refused.status).toBe(401);
  });

  it('delivers a change to an open event stream within 5 s', async () => {
    const controller = new AbortController();
    const stream = await fetch(`${base}/api/machine/events`, {
      headers: { authorization: `Bearer ${token}` },
      signal: controller.signal,
    });
    const reader = (stream.body as ReadableStream<Uint8Array>).getReader();
    const decoder = new TextDecoder();
    let received = '';
    const readUntil = async (marker: string) => {
      while (!received.includes(marker)) {
        const { value, done } = await reader.read();
        if (done) throw new Error(`The stream ended before ${marker}`);
        received += decoder.decode(value, { stream: true });
      }
    };
    const timeout = (ms: number) =>
      new Promise<never>((_, reject) =>
        setTimeout(() => reject(new Error(`No event within ${ms} ms: ${received}`)), ms),
      );

    await Promise.race([readUntil(': connected'), timeout(5_000)]);
    const created = await trpc('tasks.create', {
      id: 'smoke-task',
      title: SEALED_LOOKING('title'),
      input: SEALED_LOOKING('input'),
    });
    await Promise.race([readUntil('event: tasks'), timeout(5_000)]);
    controller.abort();

    expect(created.status).toBe(200);
  });

  it('refuses a request with a foreign Host end to end', async () => {
    const status = await new Promise<number>((resolve, reject) => {
      httpRequest(
        { host: '127.0.0.1', port: runner.proxyPort, path: '/trpc/me', headers: { Host: 'evil.test' } },
        (response) => {
          response.resume();
          resolve(response.statusCode ?? 0);
        },
      )
        .on('error', reject)
        .end();
    });

    expect(status).toBe(421);
  });

  it('keeps the worker in directories of its own and starts none of it before enrolment', () => {
    const worker = join(runner.appRoot, 'worker');

    for (const dir of ['bin', 'config', 'state', 'run']) expect(statSync(join(worker, dir)).isDirectory()).toBe(true);
    expect(readlinkSync(join(worker, 'bin/board'))).toMatch(/board-aarch64-apple-darwin$/);
    expect(runner.output()).toContain('Worker not connected');
    expect(runner.output()).not.toMatch(/Worker (listener|watch)/);
  });

  it('listens on loopback only', () => {
    const pids = runner.tree();
    const listing = spawnSync('lsof', ['-nP', '-a', '-iTCP', '-sTCP:LISTEN', '-p', pids.join(',')], {
      encoding: 'utf8',
    });
    const addresses = listing.stdout
      .split('\n')
      .slice(1)
      .map((line) => line.match(/TCP (\S+) \(LISTEN\)/)?.[1])
      .filter((address): address is string => address !== undefined);

    expect(addresses.sort()).toEqual([`127.0.0.1:${runner.proxyPort}`, `127.0.0.1:${runner.backendPort}`].sort());
  });

  it('seals or knowingly leaves plain every stored column', async () => {
    const dataDir = join(runner.appRoot, 'data/pglite');
    await runner.stop();

    const copy = join(scratchDir('abls-at-rest-'), 'pglite');
    cpSync(dataDir, copy, { recursive: true });
    const pgliteEntry = join(repoRoot, '.core', commit, 'apps/backend/node_modules/@electric-sql/pglite/dist/index.js');
    const { PGlite } = (await import(pathToFileURL(pgliteEntry).href)) as {
      PGlite: new (dataDir: string) => Database;
    };
    const database = new PGlite(copy);
    try {
      const columns = await database.query<{ table_name: string; column_name: string }>(
        `select table_name, column_name from information_schema.columns
         where table_schema = 'public' order by table_name, ordinal_position`,
      );
      const guards = await database.query<{ conname: string; definition: string }>(
        `select conname, pg_get_constraintdef(oid) as definition from pg_constraint
         where contype = 'c' and connamespace = 'public'::regnamespace`,
      );
      const plain = JSON.parse(readFileSync(join(repoRoot, 'at-rest/plain-columns.json'), 'utf8')) as Record<
        string,
        string
      >;

      const guarded = new Set<string>();
      for (const { table_name: table, column_name: column } of columns.rows) {
        const sealed = guards.rows.some(
          ({ conname, definition }) =>
            (conname === `${table}_${column}_sealed` && definition.includes('abe1')) ||
            (conname === `${table}_${column}_blind` && definition.includes('abh1')),
        );
        if (sealed) guarded.add(`${table}.${column}`);
      }
      const every = columns.rows.map(({ table_name: table, column_name: column }) => `${table}.${column}`);
      const unclassified = every.filter((name) => !guarded.has(name) && !(name in plain));
      const staleOrDoubled = Object.keys(plain).filter((name) => !every.includes(name) || guarded.has(name));
      const unexplained = Object.entries(plain).filter(([, reason]) => reason.trim() === '');

      expect(guarded.size).toBeGreaterThan(20);
      expect(unclassified).toEqual([]);
      expect(staleOrDoubled).toEqual([]);
      expect(unexplained).toEqual([]);
    } finally {
      await database.close();
    }
  }, 120_000);
});
