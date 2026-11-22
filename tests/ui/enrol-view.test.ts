import { describe, expect, it } from 'vitest';
import { countLabel, viewFor } from '../../ui/enrol-view.js';

describe('the enrolment window', () => {
  it('shows "Connecting…" with the form disabled and nothing to cancel while the board is asked', () => {
    expect(viewFor({ phase: 'connecting' })).toMatchObject({
      form: true,
      fieldsDisabled: true,
      cancelDisabled: true,
      status: 'Connecting…',
      error: null,
    });
  });

  it('says which machine it connected as and leaves the form', () => {
    expect(viewFor({ phase: 'connected', machine: 'tnk_one' })).toMatchObject({
      form: false,
      done: 'Connected as tnk_one',
    });
    expect(viewFor({ phase: 'connected', machine: null }).done).toBe('Connected.');
  });

  it('stays on an enabled form with the reason when the connection fails', () => {
    expect(viewFor({ phase: 'failed', reason: 'refused the token; nothing was written' })).toMatchObject({
      form: true,
      fieldsDisabled: false,
      cancelDisabled: false,
      error: 'refused the token; nothing was written',
      done: null,
    });
  });

  it('counts a pasted value one per character as the masked field draws it', () => {
    expect(countLabel('')).toBe('');
    expect(countLabel('a')).toBe('1 character');
    expect(countLabel(`abm_${'x'.repeat(43)}`)).toBe('47 characters');
    expect(countLabel('😀😀')).toBe('2 characters');
  });
});
