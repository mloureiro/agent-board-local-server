import { describe, expect, it } from 'vitest';
import {
  bunVersionError,
  caddyfileCopyError,
  caddyfileRules,
  isBuildCurrent,
  missingTools,
  needsCheckoutRestore,
  parseBuildArgs,
  parsePinnedBun,
  parseCoreBuild,
  stagingPlan,
  stagedBinaries,
} from './core-build-plan.ts';

const expected = { commit: 'a'.repeat(40), bun: '1.2.18', recipe: 'r1' };
const builtAt = '2026-11-20T08:15:00.000Z';
const files = { 'binaries/board': 'h1', 'resources/drizzle/0000.sql': 'h2' };

describe('isBuildCurrent', () => {
  it('accepts a build recorded for the pin and the Bun version whose files all match', () => {
    expect(isBuildCurrent({ ...expected, builtAt, files }, expected, { ...files })).toBe(true);
  });

  it.each([
    ['another core commit', { ...expected, builtAt, commit: 'b'.repeat(40), files }, files],
    ['another build recipe', { ...expected, builtAt, recipe: 'r2', files }, files],
    ['another Bun version', { ...expected, builtAt, bun: '1.4.4', files }, files],
    ['an edited staged file', { ...expected, builtAt, files }, { ...files, 'binaries/board': 'other' }],
    ['a deleted staged file', { ...expected, builtAt, files }, { 'binaries/board': 'h1' }],
    ['an extra staged file', { ...expected, builtAt, files }, { ...files, 'resources/extra': 'h3' }],
    ['no recorded files', { ...expected, builtAt, files: {} }, {}],
  ])('rejects %s', (_name, recorded, actual) => {
    expect(isBuildCurrent(recorded, expected, actual)).toBe(false);
  });

  it('does not compare when the recorded build was made', () => {
    expect(isBuildCurrent({ ...expected, builtAt: '2020-01-01T00:00:00.000Z', files }, expected, { ...files })).toBe(
      true,
    );
  });

  it('rejects a missing manifest', () => {
    expect(isBuildCurrent(null, expected, files)).toBe(false);
  });
});

describe('needsCheckoutRestore', () => {
  it.each([
    ['the stage is current and the checkout is complete', true, { present: true, hasNodeModules: true }, false],
    ['the stage is current and the checkout is missing', true, { present: false, hasNodeModules: false }, true],
    ['the stage is current and node_modules is missing', true, { present: true, hasNodeModules: false }, true],
    [
      'the stage is stale, since the full build fetches the checkout anyway',
      false,
      { present: false, hasNodeModules: false },
      false,
    ],
  ])('when %s', (_name, stageCurrent, checkout, expected) => {
    expect(needsCheckoutRestore(stageCurrent, checkout)).toBe(expected);
  });
});

describe('parseCoreBuild', () => {
  it('reads what a build writes', () => {
    expect(parseCoreBuild({ ...expected, builtAt, files })).toEqual({ ...expected, builtAt, files });
  });

  it.each([
    null,
    [],
    { commit: 'x' },
    { ...expected, builtAt, files: { a: 1 } },
    { ...expected, files },
    { ...expected, builtAt: 20261120, files },
    { commit: 'x', bun: '1', files: {} },
  ])('treats %j as no manifest', (value) => {
    expect(parseCoreBuild(value)).toBeNull();
  });
});

describe('missingTools', () => {
  it('names every tool that is absent', () => {
    expect(missingTools({ git: true, pnpm: false, bun: false })).toEqual(['pnpm', 'bun']);
  });
});

describe('caddyfileCopyError', () => {
  const pinned = '\t\tReferrer-Policy "no-referrer"\n';

  it('accepts a copy equal to the pinned file', () => {
    expect(caddyfileCopyError(pinned, pinned)).toBeNull();
  });

  it('names the committed copy when a header line differs', () => {
    expect(caddyfileCopyError(pinned, pinned.replace('no-referrer', 'origin'))).toContain('core.Caddyfile');
  });
});

describe('stagingPlan', () => {
  it('stages the frontend, migrations and the three PGlite files under resources', () => {
    expect(stagingPlan().map((step) => step.to)).toEqual([
      'resources/frontend',
      'resources/drizzle',
      'resources/pglite/pglite.wasm',
      'resources/pglite/initdb.wasm',
      'resources/pglite/pglite.data',
    ]);
  });

  it('names the sidecars the way Tauri resolves an external binary for this target', () => {
    expect(stagedBinaries).toEqual({
      server: 'binaries/board-server-aarch64-apple-darwin',
      cli: 'binaries/board-aarch64-apple-darwin',
    });
  });
});

describe('the pinned Bun version', () => {
  it('reads the version under [tools]', () => {
    expect(parsePinnedBun('[tools]\nnode = "24.22.2"\nbun = "1.4.4"\n')).toBe('1.4.4');
  });

  it('reads nothing when the core pins no Bun', () => {
    expect(parsePinnedBun('[tools]\nnode = "24.22.2"\n')).toBeNull();
  });

  it('refuses a different version, naming both', () => {
    expect(bunVersionError('1.4.4', '1.2.18')).toBe(
      'The core pins Bun 1.4.4 but this machine has Bun 1.2.18; install Bun 1.4.4 to build it',
    );
  });

  it('accepts the same version and refuses a core that pins none', () => {
    expect(bunVersionError('1.4.4', '1.4.4')).toBeNull();
    expect(bunVersionError(null, '1.4.4')).toContain('pins no Bun');
  });
});

describe('the build options', () => {
  it("builds the app's own pin into the app when given nothing", () => {
    expect(parseBuildArgs([])).toEqual({ pin: null, out: null });
  });

  it('takes a pin and a staging directory together', () => {
    expect(parseBuildArgs(['--pin', 'old.json', '--out', '/tmp/old'])).toEqual({ pin: 'old.json', out: '/tmp/old' });
  });

  it.each([[['--pin', 'old.json']], [['--out', '/tmp/old']]])(
    'refuses %j alone, so the app is never staged from another pin',
    (args) => {
      expect(() => parseBuildArgs(args)).toThrow('go together');
    },
  );

  it('refuses an unknown option and a missing value', () => {
    expect(() => parseBuildArgs(['--force', 'x'])).toThrow('Unknown option --force');
    expect(() => parseBuildArgs(['--pin'])).toThrow('--pin needs a value');
  });
});

describe('caddyfileRules', () => {
  it('checks and fingerprints the Caddyfile copy for the build that stages into the app', () => {
    expect(caddyfileRules(null)).toEqual({ check: true, fingerprint: true });
  });

  it('skips both for a build into a folder of its own, so a stage per commit survives a later pin move', () => {
    expect(caddyfileRules('/tmp/stage')).toEqual({ check: false, fingerprint: false });
  });
});
