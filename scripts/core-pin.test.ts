import { readFileSync } from 'node:fs';
import { describe, expect, it } from 'vitest';
import { parseCorePin } from './core-pin.ts';

const valid = {
  repository: 'mloureiro/agent-board',
  commit: '5127f96be50dd4c11fce7e750ccc97ec291384e9',
  reason: 'needs the pglite driver',
};

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
});
