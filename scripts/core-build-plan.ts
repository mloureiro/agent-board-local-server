import { posix } from 'node:path';

export const TARGET_TRIPLE = 'aarch64-apple-darwin';
export const BUN_TARGET = 'bun-darwin-arm64';
export const CLI_TARGET = 'darwin-arm64';
export const BUILD_MANIFEST = 'resources/core-build.json';
export const CADDYFILE_COPY = 'core.Caddyfile';

export const REQUIRED_TOOLS = ['git', 'pnpm', 'bun'] as const;

export interface CoreBuild {
  commit: string;
  bun: string;
  recipe: string;
  builtAt: string;
  files: Record<string, string>;
}

export interface StagingStep {
  from: string;
  to: string;
}

export const stagingPlan = (): StagingStep[] => [
  { from: 'apps/frontend/dist', to: 'resources/frontend' },
  { from: 'apps/backend/drizzle', to: 'resources/drizzle' },
  ...['pglite.wasm', 'initdb.wasm', 'pglite.data'].map((name) => ({
    from: posix.join('apps/backend/node_modules/@electric-sql/pglite/dist', name),
    to: posix.join('resources/pglite', name),
  })),
];

export const caddyfileCopyError = (pinned: string, committed: string): string | null =>
  pinned === committed
    ? null
    : `${CADDYFILE_COPY} differs from apps/frontend/Caddyfile at the pinned commit; copy the pinned file over ${CADDYFILE_COPY} and run pnpm test so the header tables are checked against it`;

export const caddyfileRules = (out: string | null): { check: boolean; fingerprint: boolean } => ({
  check: out === null,
  fingerprint: out === null,
});

export const stagedBinaries = {
  server: `binaries/board-server-${TARGET_TRIPLE}`,
  cli: `binaries/board-${TARGET_TRIPLE}`,
};

export type ToolAvailability = Record<(typeof REQUIRED_TOOLS)[number], boolean>;

export const missingTools = (found: ToolAvailability): string[] => REQUIRED_TOOLS.filter((tool) => !found[tool]);

export const parsePinnedBun = (miseToml: string): string | null =>
  /^\s*bun\s*=\s*"([^"]+)"\s*$/m.exec(miseToml)?.[1] ?? null;

export function bunVersionError(pinned: string | null, actual: string): string | null {
  if (pinned === null) return 'The core pins no Bun version in mise.toml under [tools]';
  if (pinned === actual) return null;
  return `The core pins Bun ${pinned} but this machine has Bun ${actual}; install Bun ${pinned} to build it`;
}

const isRecord = (value: unknown): value is Record<string, unknown> =>
  typeof value === 'object' && value !== null && !Array.isArray(value);

export function parseCoreBuild(value: unknown): CoreBuild | null {
  if (!isRecord(value)) return null;
  const { commit, bun, recipe, builtAt, files } = value;
  if (
    typeof commit !== 'string' ||
    typeof bun !== 'string' ||
    typeof recipe !== 'string' ||
    typeof builtAt !== 'string' ||
    !isRecord(files)
  )
    return null;
  const entries = Object.entries(files);
  if (!entries.every((entry): entry is [string, string] => typeof entry[1] === 'string')) return null;
  return { commit, bun, recipe, builtAt, files: Object.fromEntries(entries) };
}

export function isBuildCurrent(
  recorded: CoreBuild | null,
  expected: { commit: string; bun: string; recipe: string },
  actualFiles: Record<string, string>,
): boolean {
  if (
    recorded === null ||
    recorded.commit !== expected.commit ||
    recorded.bun !== expected.bun ||
    recorded.recipe !== expected.recipe
  )
    return false;
  const recordedPaths = Object.keys(recorded.files);
  if (recordedPaths.length === 0 || recordedPaths.length !== Object.keys(actualFiles).length) return false;
  return recordedPaths.every((path) => recorded.files[path] === actualFiles[path]);
}

export const needsCheckoutRestore = (
  stageCurrent: boolean,
  checkout: { present: boolean; hasNodeModules: boolean },
): boolean => stageCurrent && !(checkout.present && checkout.hasNodeModules);

export function parseBuildArgs(argv: string[]): { pin: string | null; out: string | null } {
  let pin: string | null = null;
  let out: string | null = null;
  for (let index = 0; index < argv.length; index += 2) {
    const value = argv[index + 1];
    if (value === undefined) throw new Error(`${argv[index]} needs a value`);
    if (argv[index] === '--pin') pin = value;
    else if (argv[index] === '--out') out = value;
    else throw new Error(`Unknown option ${argv[index]}`);
  }
  if ((pin === null) !== (out === null))
    throw new Error('--pin and --out go together: a pin other than core.pin.json is never staged into the app');
  return { pin, out };
}
