import { readFileSync } from 'node:fs';
import { describe, expect, it } from 'vitest';
import { failedIncludesSummary, parseBumpArgs, rewritePinCommit } from './core-bump-plan.ts';

const commit = 'b587c6ac0954ab467e4a3d07abece368af9ee524';

describe('parseBumpArgs', () => {
  it('takes a full lowercase commit', () => {
    expect(parseBumpArgs([commit])).toEqual({ kind: 'bump', commit });
  });

  it('takes --verify', () => {
    expect(parseBumpArgs(['--verify'])).toEqual({ kind: 'verify' });
  });

  it.each([
    ['a short hash', ['b587c6a']],
    ['a branch name', ['local-server']],
    ['a value that git would read as an option', ['--upload-pack=x']],
    ['an uppercase commit', [commit.toUpperCase()]],
  ])('refuses %s', (_label, argv) => {
    expect(() => parseBumpArgs(argv)).toThrow('40-character');
  });

  it('refuses no argument and both together', () => {
    expect(() => parseBumpArgs([])).toThrow('Usage');
    expect(() => parseBumpArgs([commit, '--verify'])).toThrow('one argument');
  });
});

describe('rewritePinCommit', () => {
  const next = 'a62d3eda34979b4b8a064f9f24cf9b0c9cd0309e';
  const committed = readFileSync(new URL('../core.pin.json', import.meta.url), 'utf8');

  it('changes the commit value and nothing else, keeping the file format', () => {
    const rewritten = rewritePinCommit(committed, next);
    expect(rewritten).toBe(committed.replace(`"commit": "${commit}"`, `"commit": "${next}"`));
    expect(rewritten).not.toBe(committed);
  });
});

describe('failedIncludesSummary', () => {
  it('names every include that is not an ancestor', () => {
    const summary = failedIncludesSummary(
      [
        { branch: 'feat/a', why: 'a', commit: '1'.repeat(40) },
        { branch: 'feat/b', why: 'b', commit: '2'.repeat(40) },
      ],
      commit,
    );
    expect(summary).toContain('2 included branch(es)');
    expect(summary).toContain(`feat/a (${'1'.repeat(40)})`);
    expect(summary).toContain(`feat/b (${'2'.repeat(40)})`);
  });
});
