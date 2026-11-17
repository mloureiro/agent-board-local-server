export const CORE_REPOSITORIES = ['mloureiro/agent-board', 'codeplant-de/agent-board'] as const;

export type CoreRepository = (typeof CORE_REPOSITORIES)[number];

export interface CorePin {
  repository: CoreRepository;
  commit: string;
  reason: string;
}

const isRecord = (value: unknown): value is Record<string, unknown> =>
  typeof value === 'object' && value !== null && !Array.isArray(value);

const isCoreRepository = (value: unknown): value is CoreRepository =>
  CORE_REPOSITORIES.some((repository) => repository === value);

export function parseCorePin(value: unknown): CorePin {
  if (!isRecord(value)) throw new Error('The core pin must be an object');
  const { repository, commit, reason } = value;
  if (!isCoreRepository(repository))
    throw new Error(`The core pin must name one of ${CORE_REPOSITORIES.join(', ')}, not ${JSON.stringify(repository)}`);
  if (typeof commit !== 'string' || !/^[0-9a-f]{40}$/.test(commit))
    throw new Error('The core pin must name a full 40-character commit, never a branch or a short hash');
  if (typeof reason !== 'string' || reason.trim() === '')
    throw new Error('The core pin must say why it names this repository and commit');
  return { repository, commit, reason };
}
