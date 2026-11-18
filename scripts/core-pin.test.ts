import { readFileSync } from 'node:fs';
import { describe, expect, it } from 'vitest';
import { parseCorePin } from './core-pin.ts';

const valid = {
  repository: 'mloureiro/agent-board',
  commit: '5127f96be50dd4c11fce7e750ccc97ec291384e9',
  reason: 'needs the pglite driver',
};

const fork = {
  ...valid,
  branch: 'local-server',
  includes: [{ branch: 'feat/listen-host', commit: '72ad101dba00b800ede728a07409a39b1ac05606' }],
};

const upstream = { repository: 'codeplant-de/agent-board', commit: valid.commit, reason: 'upstream has it all' };

describe('the core pin', () => {
  it('accepts the pin this repository commits', () => {
    const committed: unknown = JSON.parse(readFileSync(new URL('../core.pin.json', import.meta.url), 'utf8'));
    expect(() => parseCorePin(committed)).not.toThrow();
  });

  it('refuses a branch name or a short hash, which would let the packaged core change without a commit here', () => {
    expect(() => parseCorePin({ ...valid, commit: 'main' })).toThrow('full 40-character commit');
    expect(() => parseCorePin({ ...valid, commit: '5127f96' })).toThrow('full 40-character commit');
  });

  it('refuses a repository other than the fork and upstream', () => {
    expect(() => parseCorePin({ ...valid, repository: 'someone/agent-board' })).toThrow('must name one of');
  });

  it('refuses a pin without a reason', () => {
    expect(() => parseCorePin({ ...valid, reason: ' ' })).toThrow('must say why');
  });

  it('accepts a pin on the fork with its branch and included branches', () => {
    expect(parseCorePin(fork)).toEqual(fork);
  });

  it('refuses a pin on the fork without a branch or without the list of included branches', () => {
    expect(() => parseCorePin({ ...fork, branch: undefined })).toThrow('must name the branch');
    expect(() => parseCorePin({ ...fork, includes: undefined })).toThrow('must list the branches');
  });

  it('refuses an included branch without a full commit', () => {
    expect(() => parseCorePin({ ...fork, includes: [{ branch: 'feat/x', commit: 'abc' }] })).toThrow(
      'feat/x must name a full 40-character commit',
    );
    expect(() => parseCorePin({ ...fork, includes: [{ commit: valid.commit }] })).toThrow('must name its branch');
  });

  it('accepts upstream with no branch and with an empty list', () => {
    expect(parseCorePin(upstream)).toEqual(upstream);
    expect(parseCorePin({ ...upstream, includes: [] })).toEqual(upstream);
  });

  it('refuses a branch or included branches on upstream', () => {
    expect(() => parseCorePin({ ...upstream, branch: 'main' })).toThrow('names no branch');
    expect(() => parseCorePin({ ...upstream, includes: fork.includes })).toThrow('names no branch');
  });
});
