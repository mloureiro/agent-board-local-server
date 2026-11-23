import type { CoreInclude } from './core-pin.ts';

export type BumpRequest = { kind: 'bump'; commit: string } | { kind: 'verify' };

const COMMIT = /^[0-9a-f]{40}$/;

export function parseBumpArgs(argv: string[]): BumpRequest {
  if (argv.length === 0) throw new Error('Usage: pnpm core:bump <40-character commit> | --verify');
  if (argv.length > 1) throw new Error('core:bump takes one argument: a 40-character commit, or --verify');
  const [argument] = argv as [string];
  if (argument === '--verify') return { kind: 'verify' };
  if (!COMMIT.test(argument))
    throw new Error(`core:bump needs a full 40-character lowercase hex commit, not ${JSON.stringify(argument)}`);
  return { kind: 'bump', commit: argument };
}

export function rewritePinCommit(pinText: string, commit: string): string {
  const pin: Record<string, unknown> = JSON.parse(pinText);
  return `${JSON.stringify({ ...pin, commit }, null, 2)}\n`;
}

export function failedIncludesSummary(failed: CoreInclude[], commit: string): string {
  const lines = failed.map((include) => `  ${include.branch} (${include.commit})`);
  return [`${failed.length} included branch(es) are not ancestors of ${commit}:`, ...lines].join('\n');
}

export type BumpFiles = { pin: string; previous: string; caddyfile: string };

export function planBumpFiles(pinText: string, commit: string, caddyfile: string): BumpFiles {
  return { pin: rewritePinCommit(pinText, commit), previous: pinText, caddyfile };
}
