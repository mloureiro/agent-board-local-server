import { readdirSync } from 'node:fs';
import { request as httpRequest } from 'node:http';
import { afterAll, beforeAll, describe, expect, it } from 'vitest';
import { dockerAvailable, pinnedCaddyImage, startCaddy, type Caddy } from './caddy.ts';
import { killStrays, requireStagedCore, startRunner, stagedFrontend, tearDown, type Runner } from './harness.ts';
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
  length: number;
  body: string;
  validator: string | undefined;
}

const shape = ({ status, headers }: Answer) => ({ status, headers });
const BOUNDARY = /boundary=([0-9a-f]+)/;

const EXCLUDED_HEADERS: Record<string, string> = {
  date: 'per-response clock',
  server: 'names the software',
  'content-length': 'framing; the proxy streams',
  etag: 'static validator the proxy does not make',
  'last-modified': 'static validator the proxy does not make',
  'content-encoding': 'Caddy compresses (encode gzip zstd); the proxy does not on loopback',
  vary: 'follows from content-encoding',
  via: 'Caddy marks a proxied answer with its own name and version; the proxy is not Caddy and adds no hop marker',
};
const IGNORED = Object.keys(EXCLUDED_HEADERS);

function send(port: number, probe: Probe): Promise<Answer> {
  return new Promise((resolve, reject) => {
    const outgoing = httpRequest(
      { host: '127.0.0.1', port, method: probe.method, path: probe.path, headers: probe.headers, agent: false },
      (response) => {
        const chunks: Buffer[] = [];
        response.on('data', (chunk: Buffer) => chunks.push(chunk));
        response.on('end', () => {
          const headers: Record<string, string> = {};
          for (const [name, value] of Object.entries(response.headers)) {
            if (!IGNORED.includes(name)) headers[name] = Array.isArray(value) ? value.join(', ') : String(value);
          }
          const boundary = BOUNDARY.exec(headers['content-type'] ?? '')?.[1];
          let body = Buffer.concat(chunks).toString('latin1');
          if (boundary !== undefined) {
            headers['content-type'] = (headers['content-type'] ?? '').replace(boundary, 'BOUNDARY');
            body = body.replaceAll(boundary, 'BOUNDARY');
          }
          resolve({
            status: response.statusCode ?? 0,
            headers,
            length: Number(response.headers['content-length'] ?? -1),
            body,
            validator: response.headers.etag,
          });
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
    caddy = await startCaddy({ image: pinnedCaddyImage(), stubPort: stub.port });
  }, 240_000);

  afterAll(
    () =>
      tearDown(
        () => caddy?.stop(),
        () => runner?.stop(),
        () => stub?.stop(),
        killStrays,
      ),
    60_000,
  );

  const asset = () => {
    const name = readdirSync(`${stagedFrontend}/assets`).find((file) => file.endsWith('.js'));
    if (name === undefined) throw new Error('The staged frontend has no script asset');
    return `/assets/${name}`;
  };

  const both = async (probe: Probe) => {
    const [proxy, pinned] = await Promise.all([send(runner.proxyPort, probe), send((caddy as Caddy).port, probe)]);
    return { proxy, pinned };
  };

  // The proxy forwards no backend Set-Cookie and no SAML 303 redirect, unlike the pinned Caddyfile, because a no-login backend sends neither.
  it.skipIf(skipped)('sends the same status and header set as Caddy for each request', async () => {
    const probes: Probe[] = [
      { method: 'GET', path: '/' },
      { method: 'HEAD', path: '/' },
      { method: 'POST', path: '/', headers: json, body: '{}' },
      { method: 'GET', path: '/t/some-task' },
      { method: 'GET', path: asset() },
      { method: 'GET', path: '/assets/missing.js' },
      { method: 'GET', path: '/sw.js' },
      { method: 'GET', path: '/manifest.webmanifest' },
      { method: 'GET', path: '/healthz' },
      { method: 'GET', path: '/trpc/me' },
      { method: 'POST', path: '/api/auth/saml/acs', headers: json, body: '{}' },
      { method: 'POST', path: '/trpc/tasks.create', headers: json, body: '{}' },
      { method: 'GET', path: '/api/machine/list-tasks' },
      { method: 'POST', path: '/api/machine/list-tasks', headers: json, body: '{}' },
    ];

    for (const probe of probes) {
      const { proxy, pinned } = await both(probe);
      expect({ probe, ...shape(proxy) }).toEqual({ probe, ...shape(pinned) });
    }
  });

  it.skipIf(skipped)('serves byte ranges on files like Caddy: status, headers, length and body', async () => {
    const probes: Probe[] = [
      ['GET', asset(), 'bytes=0-4'],
      ['GET', asset(), 'bytes=10-'],
      ['GET', asset(), 'bytes=-5'],
      ['GET', asset(), 'bytes=-0'],
      ['GET', asset(), 'bytes=0-99999999'],
      ['GET', asset(), 'bytes=99999999-'],
      ['GET', asset(), 'bytes=abc'],
      ['GET', asset(), 'bytes=5-2'],
      ['GET', asset(), 'items=0-4'],
      ['GET', asset(), 'bytes=0-1,4-5'],
      ['GET', asset(), `bytes=${Array(200).fill('0-0').join(',')}`],
      ['GET', asset(), `bytes=${Array(201).fill('0-0').join(',')}`],
      ['GET', asset(), `bytes=${Array(200).fill('0-0').join(',')},`],
      ['GET', asset(), `bytes=${Array(250).fill('99999999-').join(',')}`],
      ['GET', asset(), 'bytes=-9223372036854775807'],
      ['GET', asset(), 'bytes=-9223372036854775808'],
      ['GET', asset(), 'bytes=0-9223372036854775807'],
      ['GET', asset(), 'bytes=0-9223372036854775808'],
      ['GET', asset(), 'bytes=9223372036854775807-'],
      ['GET', asset(), 'bytes=9223372036854775808-'],
      ['GET', asset(), 'bytes=0--0'],
      ['GET', asset(), 'bytes=--0'],
      ['GET', asset(), 'bytes=0-1,99999999-'],
      ['GET', asset(), 'bytes=0-999999,4-5'],
      ['GET', '/', 'bytes=0-4'],
      ['GET', '/t/some-task', 'bytes=-3'],
      ['HEAD', asset(), 'bytes=0-4'],
      ['HEAD', asset(), 'bytes=99999999-'],
      ['HEAD', asset(), 'bytes=0-1,4-5'],
      ['GET', '/assets/missing.js', 'bytes=0-4'],
      ['POST', asset(), 'bytes=0-4'],
    ].map(([method, path, range]) => ({ method, path, headers: { range } }) as Probe);
    probes.push(
      { method: 'GET', path: asset() },
      { method: 'HEAD', path: asset() },
      { method: 'GET', path: asset(), headers: { range: 'bytes=0-4', 'if-range': '"not-the-validator"' } },
    );

    for (const probe of probes) {
      const { proxy, pinned } = await both(probe);
      expect({ probe, ...proxy }).toEqual({ probe, ...pinned, validator: undefined });
    }
  });

  it.skipIf(skipped)(
    'answers an If-Range that matches the file validator with the whole file where Caddy slices',
    async () => {
      const probe = { method: 'GET', path: asset() };
      const validator = (await send((caddy as Caddy).port, probe)).validator;
      expect(validator).toEqual(expect.any(String));
      const ranged: Probe = { ...probe, headers: { range: 'bytes=0-4', 'if-range': validator as string } };

      const { proxy, pinned } = await both(ranged);

      expect({ status: pinned.status, range: pinned.headers['content-range'] }).toEqual({
        status: 206,
        range: expect.stringMatching(/^bytes 0-4\//),
      });
      expect({ status: proxy.status, range: proxy.headers['content-range'] }).toEqual({
        status: 200,
        range: undefined,
      });
    },
  );

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

  it.skipIf(skipped)('forwards no sign-in header and a client address, each its own way', async () => {
    const received = async (port: number) => {
      await send(port, { method: 'GET', path: '/trpc/me' });
      return stub.lastRequestHeaders();
    };

    const throughProxy = await received(runner.proxyPort);
    const throughCaddy = await received((caddy as Caddy).port);

    for (const headers of [throughProxy, throughCaddy]) {
      expect(headers['board-proxy-secret']).toBeUndefined();
      expect(headers['remote-user']).toBeUndefined();
    }
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
      expect({ probe, ...shape(proxy) }).toEqual({ probe, ...shape(pinned) });
    }
  });
});
