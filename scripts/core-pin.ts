export const CORE_REPOSITORIES = ['mloureiro/agent-board', 'codeplant-de/agent-board'] as const;

export type CoreRepository = (typeof CORE_REPOSITORIES)[number];

export const FORK_REPOSITORY: CoreRepository = 'mloureiro/agent-board';

export interface CoreInclude {
  branch: string;
  why: string;
  commit: string;
}

export interface CorePin {
  repository: CoreRepository;
  commit: string;
  reason: string;
  branch?: string;
  includes?: CoreInclude[];
}

const isRecord = (value: unknown): value is Record<string, unknown> =>
  typeof value === 'object' && value !== null && !Array.isArray(value);

const isCoreRepository = (value: unknown): value is CoreRepository =>
  CORE_REPOSITORIES.some((repository) => repository === value);

const isCommit = (value: unknown): value is string => typeof value === 'string' && /^[0-9a-f]{40}$/.test(value);

const isNonEmptyString = (value: unknown): value is string => typeof value === 'string' && value.trim() !== '';

function parseInclude(value: unknown): CoreInclude {
  if (!isRecord(value) || !isNonEmptyString(value.branch))
    throw new Error('Every included branch of the core pin must name its branch');
  if (!isCommit(value.commit))
    throw new Error(`The included branch ${value.branch} must name a full 40-character commit`);
  if (!isNonEmptyString(value.why) || /[\r\n]/.test(value.why))
    throw new Error(`The included branch ${value.branch} must say in one line why the pin includes it`);
  return { branch: value.branch, why: value.why, commit: value.commit };
}

export function parseCorePin(value: unknown): CorePin {
  if (!isRecord(value)) throw new Error('The core pin must be an object');
  const { repository, commit, reason, branch, includes } = value;
  if (!isCoreRepository(repository))
    throw new Error(`The core pin must name one of ${CORE_REPOSITORIES.join(', ')}, not ${JSON.stringify(repository)}`);
  if (!isCommit(commit))
    throw new Error('The core pin must name a full 40-character commit, never a branch or a short hash');
  if (!isNonEmptyString(reason)) throw new Error('The core pin must say why it names this repository and commit');
  if (repository === FORK_REPOSITORY) {
    if (!isNonEmptyString(branch)) throw new Error('A pin on the fork must name the branch the commit is the head of');
    if (!Array.isArray(includes)) throw new Error('A pin on the fork must list the branches it includes');
    return { repository, commit, reason, branch, includes: includes.map(parseInclude) };
  }
  if (branch !== undefined || (Array.isArray(includes) ? includes.length > 0 : includes !== undefined))
    throw new Error('A pin on upstream names no branch and includes no branches');
  return { repository, commit, reason };
}
