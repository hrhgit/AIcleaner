import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { logClientError } from './client-log.js';
import { normalizeAppError, normalizeTaskErrorPayload } from './errors.js';

const isTauri = typeof window !== 'undefined' && '__TAURI_INTERNALS__' in window;

function requireTauri(command) {
  if (!isTauri) {
    throw new Error(`AIcleaner now runs in Tauri only. Unsupported runtime for command: ${command}`);
  }
}

async function invokeSafe(command, args = {}) {
  requireTauri(command);
  try {
    return await invoke(command, args);
  } catch (err) {
    const normalized = normalizeAppError(err, { context: { operation: command } });
    logClientError(`tauri:${command}`, normalized);
    throw normalized;
  }
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
      const normalizedPayload = eventName.endsWith('_error')
        ? normalizeTaskErrorPayload(payload, { context: { operation: eventName } })
        : payload;
      handler(normalizedPayload);
    }).then((unlisten) => {
      if (closed) unlisten();
      else cleanups.push(unlisten);
    }).catch((err) => {
      logClientError(`tauri-event:${eventName}`, err, { context: { operation: eventName } });
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
  return invokeSafe('settings_get');
}

export async function saveSettings(data) {
  return invokeSafe('settings_save', { data });
}

export async function moveDataDir(path) {
  return invokeSafe('settings_move_data_dir', { data: { path } });
}

export async function getCredentials() {
  return invokeSafe('credentials_get');
}

export async function saveCredentials(payload) {
  return invokeSafe('credentials_save', { data: payload });
}

export async function browseFolder() {
  return invokeSafe('settings_browse_folder');
}

export async function getProviderModels(endpoint, apiKey = undefined) {
  return invokeSafe('settings_get_provider_models', {
    data: apiKey == null ? { endpoint } : { endpoint, apiKey },
  });
}

export async function getPrivilegeStatus() {
  return invokeSafe('system_get_privilege');
}

export async function requestElevation() {
  return invokeSafe('system_request_elevation');
}

export async function openExternalUrl(url) {
  return invokeSafe('system_open_external_url', { data: { url } });
}

export async function getActiveScan() {
  return invokeSafe('scan_get_active');
}

export async function listScanHistory(limit = 20) {
  return invokeSafe('scan_list_history', { limit });
}

export async function findLatestScanForPath(path) {
  return invokeSafe('scan_find_latest_for_path', { path });
}

export async function deleteScanHistory(taskId) {
  return invokeSafe('scan_delete_history', { taskId });
}

export async function startScan(params) {
  return invokeSafe('scan_start', { input: params });
}

export async function stopScan(taskId) {
  return invokeSafe('scan_stop', { taskId });
}

export async function getScanResult(taskId) {
  return invokeSafe('scan_get_result', { taskId });
}

export function connectScanStream(taskId, handlers) {
  const stream = createStream(taskId, [
    ['scan_progress', (p) => handlers.onProgress?.(p)],
    ['scan_found', (p) => handlers.onFound?.(p)],
    ['scan_agent_call', (p) => handlers.onAgentCall?.(p)],
    ['scan_agent_response', (p) => handlers.onAgentResponse?.(p)],
    ['scan_cache', (p) => handlers.onCache?.(p)],
    ['scan_warning', (p) => handlers.onWarning?.(p)],
    ['scan_done', (p) => { handlers.onDone?.(p); stream.close(); }],
    ['scan_error', (p) => { handlers.onError?.(p); stream.close(); }],
    ['scan_stopped', (p) => { handlers.onStopped?.(p); stream.close(); }],
  ]);
  return stream;
}

export async function openFileLocation(path) {
  return invokeSafe('files_open_location', { data: { path } });
}

export async function cleanFiles(paths, scanTaskId = null) {
  const body = scanTaskId ? { paths, scanTaskId } : { paths };
  return invokeSafe('files_clean', { data: body });
}

export async function getOrganizeCapability() {
  return invokeSafe('organize_get_capability');
}

export async function startOrganize(params) {
  return invokeSafe('organize_start', { input: params });
}

export async function stopOrganize(taskId) {
  return invokeSafe('organize_stop', { taskId });
}

export async function getOrganizeResult(taskId) {
  return invokeSafe('organize_get_result', { taskId });
}

export async function applyOrganize(taskId) {
  return invokeSafe('organize_apply', { taskId });
}

export async function rollbackOrganize(jobId) {
  return invokeSafe('organize_rollback', { jobId });
}

export function connectOrganizeStream(taskId, handlers) {
  const stream = createStream(taskId, [
    ['organize_progress', (p) => handlers.onProgress?.(p)],
    ['organize_summary_ready', (p) => handlers.onSummaryReady?.(p)],
    ['organize_file_done', (p) => handlers.onFileDone?.(p)],
    ['organize_done', (p) => { handlers.onDone?.(p); stream.close(); }],
    ['organize_error', (p) => { handlers.onError?.(p); stream.close(); }],
    ['organize_stopped', (p) => { handlers.onStopped?.(p); stream.close(); }],
  ]);
  return stream;
}
