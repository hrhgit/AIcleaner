import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';

const isTauri = typeof window !== 'undefined' && '__TAURI_INTERNALS__' in window;

function requireTauri(command) {
  if (!isTauri) {
    throw new Error(`AIcleaner now runs in Tauri only. Unsupported runtime for command: ${command}`);
  }
}

async function call(command, args = {}) {
  requireTauri(command);
  return invoke(command, args);
}

function createStream(taskId, specs) {
  requireTauri('event_stream');
  const cleanups = [];
  let closed = false;
  const taskIdText = String(taskId || '').trim();
  const matchTask = (payload = {}) => {
    const pid = String(payload?.taskId || payload?.id || '').trim();
    return !taskIdText || pid === taskIdText;
  };
  for (const [eventName, handler] of specs) {
    listen(eventName, (event) => {
      if (closed) return;
      const payload = event?.payload || {};
      if (!matchTask(payload)) return;
      handler(payload);
    }).then((unlisten) => {
      if (closed) unlisten();
      else cleanups.push(unlisten);
    });
  }
  return {
    close() {
      if (closed) return;
      closed = true;
      while (cleanups.length) {
        const fn = cleanups.pop();
        try { fn?.(); } catch { /* ignore */ }
      }
    },
  };
}

export async function getSettings() {
  return call('settings_get');
}

export async function saveSettings(data) {
  return call('settings_save', { data });
}

export async function getSecretStatus() {
  return call('secret_status');
}

export async function setupSecretVault(password) {
  return call('secret_setup', { data: { password } });
}

export async function unlockSecretVault(password) {
  return call('secret_unlock', { data: { password } });
}

export async function lockSecretVault() {
  return call('secret_lock');
}

export async function resetSecretVault() {
  return call('secret_reset', { data: { confirmed: true } });
}

export async function getEditableSecrets() {
  return call('secret_get_editable');
}

export async function saveSecrets(payload) {
  return call('secret_save', { data: payload });
}

export async function browseFolder() {
  return call('settings_browse_folder');
}

export async function getProviderModels(endpoint, apiKey = undefined) {
  return call('settings_get_provider_models', {
    data: apiKey == null ? { endpoint } : { endpoint, apiKey },
  });
}

export async function getPrivilegeStatus() {
  return call('system_get_privilege');
}

export async function requestElevation() {
  return call('system_request_elevation');
}

export async function getActiveScan() {
  return call('scan_get_active');
}

export async function listScanHistory(limit = 20) {
  return call('scan_list_history', { limit });
}

export async function deleteScanHistory(taskId) {
  return call('scan_delete_history', { task_id: taskId });
}

export async function startScan(params) {
  return call('scan_start', { input: params });
}

export async function stopScan(taskId) {
  return call('scan_stop', { task_id: taskId });
}

export async function getScanResult(taskId) {
  return call('scan_get_result', { task_id: taskId });
}

export function connectScanStream(taskId, handlers) {
  const stream = createStream(taskId, [
    ['scan_progress', (p) => handlers.onProgress?.(p)],
    ['scan_found', (p) => handlers.onFound?.(p)],
    ['scan_agent_call', (p) => handlers.onAgentCall?.(p)],
    ['scan_agent_response', (p) => handlers.onAgentResponse?.(p)],
    ['scan_warning', (p) => handlers.onWarning?.(p)],
    ['scan_done', (p) => { handlers.onDone?.(p); stream.close(); }],
    ['scan_error', (p) => { handlers.onError?.(p); stream.close(); }],
    ['scan_stopped', (p) => { handlers.onStopped?.(p); stream.close(); }],
  ]);
  return stream;
}

export async function openFileLocation(path) {
  return call('files_open_location', { data: { path } });
}

export async function cleanFiles(paths, scanTaskId = null) {
  const body = scanTaskId ? { paths, scanTaskId } : { paths };
  return call('files_clean', { data: body });
}

export async function suggestOrganizeCategories(params) {
  return call('organize_suggest_categories', { input: params });
}

export async function getOrganizeCapability() {
  return call('organize_get_capability');
}

export async function startOrganize(params) {
  return call('organize_start', { input: params });
}

export async function stopOrganize(taskId) {
  return call('organize_stop', { task_id: taskId });
}

export async function getOrganizeResult(taskId) {
  return call('organize_get_result', { task_id: taskId });
}

export async function applyOrganize(taskId) {
  return call('organize_apply', { task_id: taskId });
}

export async function rollbackOrganize(jobId) {
  return call('organize_rollback', { job_id: jobId });
}

export function connectOrganizeStream(taskId, handlers) {
  const stream = createStream(taskId, [
    ['organize_progress', (p) => handlers.onProgress?.(p)],
    ['organize_file_done', (p) => handlers.onFileDone?.(p)],
    ['organize_done', (p) => { handlers.onDone?.(p); stream.close(); }],
    ['organize_error', (p) => { handlers.onError?.(p); stream.close(); }],
    ['organize_stopped', (p) => { handlers.onStopped?.(p); stream.close(); }],
  ]);
  return stream;
}
