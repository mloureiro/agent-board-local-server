/**
 * @typedef {{ phase: 'form' } | { phase: 'connecting' } | { phase: 'connected', machine: string | null } | { phase: 'failed', reason: string }} Phase
 */

/**
 * What the page shows for a phase of the enrolment.
 * @param {Phase} phase
 */
export function viewFor(phase) {
  switch (phase.phase) {
    case 'connecting':
      return { form: true, fieldsDisabled: true, cancelDisabled: true, status: 'Connecting…', error: null, done: null };
    case 'connected':
      return {
        form: false,
        fieldsDisabled: true,
        cancelDisabled: true,
        status: '',
        error: null,
        done: phase.machine === null ? 'Connected.' : `Connected as ${phase.machine}`,
      };
    case 'failed':
      return { form: true, fieldsDisabled: false, cancelDisabled: false, status: '', error: phase.reason, done: null };
    default:
      return { form: true, fieldsDisabled: false, cancelDisabled: false, status: '', error: null, done: null };
  }
}

/**
 * How many characters a masked field holds, counted as the field shows them: one each.
 * @param {string} value
 */
export function countLabel(value) {
  const characters = [...value].length;
  if (characters === 0) return '';
  return characters === 1 ? '1 character' : `${characters} characters`;
}
