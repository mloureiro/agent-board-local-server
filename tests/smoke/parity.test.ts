import { readdirSync } from 'node:fs';
import { request as httpRequest } from 'node:http';
import { afterAll, beforeAll, describe, expect, it } from 'vitest';
import { dockerAvailable, pinnedCaddyImage, startCaddy, type Caddy } from './caddy.ts';
import { killStrays, requireStagedCore, startRunner, stagedFrontend, type Runner } from './harness.ts';
import { startStubBackend, type Stub } from './stub-backend.ts';

interface Probe {
  method: string;
  path: string;
  headers?: Record<string, string>;
  body?: string;
}

interface Answer {
  status: number;
  headers: Record<string, string>;
}

const EXCLUDED_HEADERS: Record<string, string> = {
  date: 'per-response clock',
  server: 'names the software',
  'content-length': 'framing; the proxy streams',
  etag: 'static validator the proxy does not make',
  'last-modified': 'static validator the proxy does not make',
  'content-encoding': 'Caddy compresses (encode gzip zstd); the proxy does not on loopback',
  vary: 'follows from content-encoding',
  via: 'Caddy marks a proxied answer with its own name and version; the proxy is not Caddy and adds no hop marker',
  'accept-ranges': 'Caddy offers byte ranges on files; the proxy serves whole files and does not advertise ranges',
};
const IGNORED = Object.keys(EXCLUDED_HEADERS);

function send(port: number, probe: Probe): Promise<Answer> {
  return new Promise((resolve, reject) => {
    const outgoing = httpRequest(
      { host: '127.0.0.1', port, method: probe.method, path: probe.path, headers: probe.headers, agent: false },
      (response) => {
        response.resume();
        response.on('end', () => {
          const headers: Record<string, string> = {};
          for (const [name, value] of Object.entries(response.headers)) {
            if (!IGNORED.includes(name)) headers[name] = Array.isArray(value) ? value.join(', ') : String(value);
          }
          resolve({ status: response.statusCode ?? 0, headers });
        });
      },
    );
    outgoing.on('error', reject);
    outgoing.end(probe.body);
  });
}

const json = { 'content-type': 'application/json' };

describe('the proxy against the pinned Caddy', () => {
  let stub: Stub;
  let runner: Runner;
  let caddy: Caddy | undefined;
  const skipped = process.env['SKIP_CADDY_PARITY'] === '1';

  beforeAll(async () => {
    requireStagedCore();
    stub = await startStubBackend();
    runner = await startRunner({ upstreamPort: stub.port });
    if (skipped) return;
    if (!dockerAvailable())
      throw new Error('Docker is not available: start it, or set SKIP_CADDY_PARITY=1 to skip the parity test');
    caddy = await startCaddy({ image: pinnedCaddyImage(), stubPort: stub.port, secret: 'parity-secret' });
  }, 240_000);

  afterAll(async () => {
    caddy?.stop();
    await runner?.stop().catch(() => undefined);
    await stub?.stop().catch(() => undefined);
    killStrays();
  }, 60_000);

  const asset = () => {
    const name = readdirSync(`${stagedFrontend}/assets`).find((file) => file.endsWith('.js'));
    if (name === undefined) throw new Error('The staged frontend has no script asset');
    return `/assets/${name}`;
  };

  const both = async (probe: Probe) => {
    const [proxy, pinned] = await Promise.all([send(runner.proxyPort, probe), send((caddy as Caddy).port, probe)]);
    return { proxy, pinned };
  };

  it.skipIf(skipped)('sends the same status and header set as Caddy for each request', async () => {
    const probes: Probe[] = [
      { method: 'GET', path: '/' },
      { method: 'HEAD', path: '/' },
      { method: 'POST', path: '/', headers: json, body: '{}' },
      { method: 'GET', path: '/t/some-task' },
      { method: 'GET', path: asset() },
      { method: 'GET', path: '/assets/missing.js' },
      { method: 'GET', path: '/healthz' },
      { method: 'GET', path: '/trpc/me' },
      { method: 'POST', path: '/trpc/tasks.create', headers: json, body: '{}' },
      { method: 'GET', path: '/api/machine/list-tasks' },
      { method: 'POST', path: '/api/machine/list-tasks', headers: json, body: '{}' },
    ];

    for (const probe of probes) {
      const { proxy, pinned } = await both(probe);
      expect({ probe, ...proxy }).toEqual({ probe, ...pinned });
    }
  });

  it.skipIf(skipped)('differs from Caddy only where the proxy deliberately refuses or the guard applies', async () => {
    const expected: { name: string; probe: Probe; proxy: number; caddy: number }[] = [
      {
        name: 'an encoded path traversal',
        probe: { method: 'GET', path: '/assets/..%2f..%2fCaddyfile' },
        proxy: 400,
        caddy: 200,
      },
      {
        name: 'a cross-site navigation',
        probe: { method: 'GET', path: '/', headers: { 'sec-fetch-site': 'cross-site' } },
        proxy: 403,
        caddy: 200,
      },
      {
        name: 'a foreign Origin on a write',
        probe: {
          method: 'POST',
          path: '/trpc/tasks.create',
          headers: { ...json, origin: 'https://evil.test' },
          body: '{}',
        },
        proxy: 403,
        caddy: 200,
      },
      {
        name: 'a foreign Host',
        probe: { method: 'GET', path: '/healthz', headers: { host: 'evil.test' } },
        proxy: 421,
        caddy: 200,
      },
    ];

    for (const { name, probe, proxy, caddy: pinned } of expected) {
      const answers = await both(probe);
      expect({ name, proxy: answers.proxy.status, caddy: answers.pinned.status }).toEqual({
        name,
        proxy,
        caddy: pinned,
      });
    }
  });

  it.skipIf(skipped)('forwards the sign-in secret and a client address, each its own way', async () => {
    const received = async (port: number) => {
      await send(port, { method: 'GET', path: '/trpc/me' });
      return stub.lastRequestHeaders();
    };

    const throughProxy = await received(runner.proxyPort);
    const throughCaddy = await received((caddy as Caddy).port);

    expect(throughCaddy['board-proxy-secret']).toBe('parity-secret');
    expect(throughProxy['board-proxy-secret']).toEqual(expect.any(String));
    expect(throughProxy['board-proxy-secret']).not.toBe('parity-secret');
    expect(throughProxy['remote-user']).toBe(runner.userName);
    expect(throughProxy['x-forwarded-for']).toBe('127.0.0.1');
    expect(throughCaddy['x-forwarded-for']).toEqual(expect.any(String));
  });

  it.skipIf(skipped)('answers a backend that is gone like Caddy does', async () => {
    await stub.stop();

    for (const probe of [
      { method: 'GET', path: '/healthz' },
      { method: 'POST', path: '/trpc/tasks.create', headers: json, body: '{}' },
    ]) {
      const { proxy, pinned } = await both(probe);
      expect(pinned.status).toBe(502);
      expect({ probe, ...proxy }).toEqual({ probe, ...pinned });
    }
  });
});
