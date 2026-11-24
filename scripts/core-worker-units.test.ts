import { describe, expect, it } from 'vitest';
import { parseKnownUnits, workerUnitsError } from './core-worker-units.ts';

const known = parseKnownUnits({ schemas: [1], units: ['dispatch', 'listen', 'watch'] });
const unit = (name: string) => ({ name });
const output = { schema: 1, units: [unit('dispatch'), unit('listen'), unit('watch')] };

describe('workerUnitsError', () => {
  it('accepts the known schema with exactly the known units', () => {
    expect(workerUnitsError(output, known)).toBeNull();
  });

  it.each([
    ['an unknown schema', { ...output, schema: 2 }, 'schema version 2'],
    ['an unknown unit', { ...output, units: [...output.units, unit('sweep')] }, '"sweep"'],
    ['a missing known unit', { ...output, units: [unit('dispatch'), unit('listen')] }, 'unit watch'],
    ['something that is not an object', [output], 'JSON object'],
    ['a missing units list', { schema: 1 }, 'no units list'],
  ])('refuses %s, naming it', (_label, value, fragment) => {
    expect(workerUnitsError(value, known)).toContain(fragment);
  });
});

describe('parseKnownUnits', () => {
  it('refuses a file without schemas or units', () => {
    expect(() => parseKnownUnits({ schemas: [], units: ['dispatch'] })).toThrow('schema versions');
    expect(() => parseKnownUnits({ schemas: [1] })).toThrow('unit names');
  });
});
