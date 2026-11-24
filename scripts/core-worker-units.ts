export interface KnownUnits {
  schemas: number[];
  units: string[];
}

const isRecord = (value: unknown): value is Record<string, unknown> =>
  typeof value === 'object' && value !== null && !Array.isArray(value);

export function parseKnownUnits(value: unknown): KnownUnits {
  if (!isRecord(value)) throw new Error('The known worker units must be an object');
  const { schemas, units } = value;
  if (!Array.isArray(schemas) || schemas.length === 0 || !schemas.every((schema) => Number.isInteger(schema)))
    throw new Error('The known worker units must list the schema versions as integers');
  if (!Array.isArray(units) || units.length === 0 || !units.every((unit) => typeof unit === 'string' && unit !== ''))
    throw new Error('The known worker units must list the unit names as strings');
  return { schemas: schemas as number[], units: units as string[] };
}

export function workerUnitsError(output: unknown, known: KnownUnits): string | null {
  if (!isRecord(output)) return 'board internal units did not print a JSON object';
  if (typeof output.schema !== 'number' || !known.schemas.includes(output.schema))
    return `board internal units printed schema version ${JSON.stringify(output.schema)}; this app knows version ${known.schemas.join(', ')}`;
  if (!Array.isArray(output.units)) return 'board internal units printed no units list';
  const names = output.units.map((unit) => (isRecord(unit) ? unit.name : undefined));
  const unknown = names.find((name) => typeof name !== 'string' || !known.units.includes(name));
  if (unknown !== undefined)
    return `board internal units printed the worker unit ${JSON.stringify(unknown)}, which this app does not know`;
  const missing = known.units.find((unit) => !names.includes(unit));
  if (missing !== undefined) return `board internal units did not print the worker unit ${missing}`;
  return null;
}
