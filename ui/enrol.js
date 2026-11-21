import { countLabel, viewFor } from './enrol-view.js';

const tauri = /** @type {any} */ (window).__TAURI__;
const { invoke } = tauri.core;

/** @param {string} id */
const byId = (id) => {
  const element = document.getElementById(id);
  if (element === null) throw new Error(`the page has no #${id}`);
  return element;
};

const form = /** @type {HTMLFormElement} */ (byId('form'));
const token = /** @type {HTMLInputElement} */ (byId('token'));
const dataKey = /** @type {HTMLInputElement} */ (byId('data-key'));
const cancel = /** @type {HTMLButtonElement} */ (byId('cancel'));
const connect = /** @type {HTMLButtonElement} */ (byId('connect'));
const done = byId('done');

/** @param {import('./enrol-view.js').Phase} phase */
function show(phase) {
  const view = viewFor(phase);
  form.hidden = !view.form;
  done.hidden = view.done === null;
  for (const input of [token, dataKey]) input.disabled = view.fieldsDisabled;
  connect.disabled = view.fieldsDisabled;
  cancel.disabled = view.cancelDisabled;
  byId('status').textContent = view.status;
  const error = byId('error');
  error.hidden = view.error === null;
  error.textContent = view.error ?? '';
  byId('done-text').textContent = view.done ?? '';
  if (view.done !== null) {
    token.value = '';
    dataKey.value = '';
    updateCounts();
    byId('close').focus();
  }
}

function updateCounts() {
  byId('token-count').textContent = countLabel(token.value);
  byId('data-key-count').textContent = countLabel(dataKey.value);
}

token.addEventListener('input', updateCounts);
dataKey.addEventListener('input', updateCounts);

form.addEventListener('submit', async (event) => {
  event.preventDefault();
  show({ phase: 'connecting' });
  try {
    show(await invoke('enrol_submit', { token: token.value, dataKey: dataKey.value }));
  } catch (error) {
    show({ phase: 'failed', reason: String(error) });
  }
});

const closeWindow = () => invoke('enrol_cancel');
cancel.addEventListener('click', closeWindow);
byId('close').addEventListener('click', closeWindow);

show({ phase: 'form' });
token.focus();
