import { describe, expect, it } from 'vitest';
import {
  bunVersionError,
  isBuildCurrent,
  missingTools,
  parsePinnedBun,
  parseCoreBuild,
  stagingPlan,
  stagedBinaries,
} from './core-build-plan.ts';

const expected = { commit: 'a'.repeat(40), bun: '1.2.18', recipe: 'r1' };
const files = { 'binaries/board': 'h1', 'resources/drizzle/0000.sql': 'h2' };

describe('isBuildCurrent', () => {
  it('accepts a build recorded for the pin and the Bun version whose files all match', () => {
    expect(isBuildCurrent({ ...expected, files }, expected, { ...files })).toBe(true);
  });

  it.each([
    ['another core commit', { ...expected, commit: 'b'.repeat(40), files }, files],
    ['another build recipe', { ...expected, recipe: 'r2', files }, files],
    ['another Bun version', { ...expected, bun: '1.4.4', files }, files],
    ['an edited staged file', { ...expected, files }, { ...files, 'binaries/board': 'other' }],
    ['a deleted staged file', { ...expected, files }, { 'binaries/board': 'h1' }],
    ['an extra staged file', { ...expected, files }, { ...files, 'resources/extra': 'h3' }],
    ['no recorded files', { ...expected, files: {} }, {}],
  ])('rejects %s', (_name, recorded, actual) => {
    expect(isBuildCurrent(recorded, expected, actual)).toBe(false);
  });

  it('rejects a missing manifest', () => {
    expect(isBuildCurrent(null, expected, files)).toBe(false);
  });
});

describe('parseCoreBuild', () => {
  it('reads what a build writes', () => {
    expect(parseCoreBuild({ ...expected, files })).toEqual({ ...expected, files });
  });

  it.each([null, [], { commit: 'x' }, { ...expected, files: { a: 1 } }, { commit: 'x', bun: '1', files: {} }])(
    'treats %j as no manifest',
    (value) => {
      expect(parseCoreBuild(value)).toBeNull();
    },
  );
});

describe('missingTools', () => {
  it('names every tool that is absent', () => {
    expect(missingTools({ git: true, pnpm: false, bun: false })).toEqual(['pnpm', 'bun']);
  });
});

describe('stagingPlan', () => {
  it('stages the frontend, migrations, Caddyfile and the three PGlite files under resources', () => {
    expect(stagingPlan().map((step) => step.to)).toEqual([
      'resources/frontend',
      'resources/drizzle',
      'resources/Caddyfile',
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
