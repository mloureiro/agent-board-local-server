import { posix } from 'node:path';

export const TARGET_TRIPLE = 'aarch64-apple-darwin';
export const BUN_TARGET = 'bun-darwin-arm64';
export const CLI_TARGET = 'darwin-arm64';
export const BUILD_MANIFEST = 'resources/core-build.json';

export const REQUIRED_TOOLS = ['git', 'pnpm', 'bun'] as const;

export interface CoreBuild {
  commit: string;
  bun: string;
  files: Record<string, string>;
}

export interface StagingStep {
  from: string;
  to: string;
}

export const stagingPlan = (): StagingStep[] => [
  { from: 'apps/frontend/dist', to: 'resources/frontend' },
  { from: 'apps/backend/drizzle', to: 'resources/drizzle' },
  { from: 'apps/frontend/Caddyfile', to: 'resources/Caddyfile' },
  ...['pglite.wasm', 'initdb.wasm', 'pglite.data'].map((name) => ({
    from: posix.join('apps/backend/node_modules/@electric-sql/pglite/dist', name),
    to: posix.join('resources/pglite', name),
  })),
];

export const stagedBinaries = {
  server: `binaries/board-server-${TARGET_TRIPLE}`,
  cli: `binaries/board-${TARGET_TRIPLE}`,
};

export const missingTools = (found: Record<(typeof REQUIRED_TOOLS)[number], boolean>): string[] =>
  REQUIRED_TOOLS.filter((tool) => !found[tool]);

const isRecord = (value: unknown): value is Record<string, unknown> =>
  typeof value === 'object' && value !== null && !Array.isArray(value);

export function parseCoreBuild(value: unknown): CoreBuild | null {
  if (!isRecord(value)) return null;
  const { commit, bun, files } = value;
  if (typeof commit !== 'string' || typeof bun !== 'string' || !isRecord(files)) return null;
  const entries = Object.entries(files);
  if (!entries.every((entry): entry is [string, string] => typeof entry[1] === 'string')) return null;
  return { commit, bun, files: Object.fromEntries(entries) };
}

export function isBuildCurrent(
  recorded: CoreBuild | null,
  expected: { commit: string; bun: string },
  actualFiles: Record<string, string>,
): boolean {
  if (recorded === null || recorded.commit !== expected.commit || recorded.bun !== expected.bun) return false;
  const recordedPaths = Object.keys(recorded.files);
  if (recordedPaths.length === 0 || recordedPaths.length !== Object.keys(actualFiles).length) return false;
  return recordedPaths.every((path) => recorded.files[path] === actualFiles[path]);
}
