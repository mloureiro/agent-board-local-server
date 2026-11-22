import { spawnSync } from 'node:child_process';
import { readFileSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';
import { freePort, pinnedCommit, repoRoot, scratchDir, pinnedCaddyfile, stagedFrontend } from './harness.ts';

export function dockerAvailable(): boolean {
  return spawnSync('docker', ['info'], { stdio: 'ignore' }).status === 0;
}

export function pinnedCaddyImage(): string {
  const dockerfile = readFileSync(join(repoRoot, '.core', pinnedCommit(), 'apps/frontend/Dockerfile'), 'utf8');
  const image = dockerfile.match(/^ARG CADDY_IMAGE=(\S+)$/m)?.[1];
  if (image === undefined) throw new Error('The pinned apps/frontend/Dockerfile names no ARG CADDY_IMAGE');
  return image;
}

function docker(args: string[]): string {
  const result = spawnSync('docker', args, { encoding: 'utf8' });
  if (result.status !== 0) throw new Error(`docker ${args[0]} failed: ${result.stderr}`);
  return result.stdout.trim();
}

export interface Caddy {
  port: number;
  stop: () => void;
}

/** The pinned Caddyfile serves the staged frontend and proxies the stub, reached through the host alias instead of the `backend` service name. */
export async function startCaddy(options: { image: string; stubPort: number; secret: string }): Promise<Caddy> {
  if (spawnSync('docker', ['image', 'inspect', options.image], { stdio: 'ignore' }).status !== 0) {
    docker(['pull', options.image]);
  }
  const caddyfile = readFileSync(pinnedCaddyfile, 'utf8');
  if (caddyfile.split('backend:4000').length !== 2)
    throw new Error('The pinned Caddyfile no longer proxies backend:4000 once');
  const file = join(scratchDir('abls-caddy-'), 'Caddyfile');
  writeFileSync(file, caddyfile.replace('backend:4000', `host.docker.internal:${options.stubPort}`));

  const port = await freePort();
  const name = `abls-parity-${port}`;
  docker([
    'run',
    '-d',
    '--rm',
    '--name',
    name,
    '-p',
    `127.0.0.1:${port}:8080`,
    '--add-host',
    'host.docker.internal:host-gateway',
    '-e',
    `PROXY_SECRET=${options.secret}`,
    '-v',
    `${file}:/etc/caddy/Caddyfile:ro`,
    '-v',
    `${stagedFrontend}:/srv:ro`,
    options.image,
  ]);
  const caddy: Caddy = { port, stop: () => void spawnSync('docker', ['rm', '-f', name], { stdio: 'ignore' }) };

  const deadline = Date.now() + 30_000;
  while (Date.now() < deadline) {
    const response = await fetch(`http://127.0.0.1:${port}/`, {
      signal: AbortSignal.timeout(Math.max(1, deadline - Date.now())),
    }).catch(() => undefined);
    if (response?.status === 200) return caddy;
    await response?.body?.cancel();
    await new Promise((done) => setTimeout(done, 200));
  }
  const logs = spawnSync('docker', ['logs', name], { encoding: 'utf8' });
  caddy.stop();
  throw new Error(`Caddy did not answer within 30 s:\n${logs.stdout}${logs.stderr}`);
}
