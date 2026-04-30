import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import type {
  AdvisorSessionData,
  BrowseFolderResult,
  CredentialsPayload,
  CredentialsReadResult,
  CredentialsSaveResult,
  OrganizeErrorEvent,
  OrganizeFileDoneEvent,
  OrganizeProgressEvent,
  OrganizeSnapshot,
  OrganizeSummaryReadyEvent,
  OrganizeTerminalEvent,
  Settings,
  StreamHandle,
  SummaryMode,
} from '../types';

const isTauri = typeof window !== 'undefined' && '__TAURI_INTERNALS__' in window;

function requireTauri(command: string): void {
  if (!isTauri) {
    throw new Error(`AIcleaner now runs in Tauri only. Unsupported runtime for command: ${command}`);
  }
}

async function call<T>(command: string, args: Record<string, unknown> = {}): Promise<T> {
  requireTauri(command);
  return invoke<T>(command, args);
}

let inflightSettingsRequest: Promise<Settings> | null = null;

function createStream<T extends Record<string, unknown>>(
  taskId: string,
  specs: Array<[string, (payload: T) => void]>,
): StreamHandle {
  requireTauri('event_stream');
  const cleanups: Array<() => void> = [];
  let closed = false;
  const taskIdText = String(taskId || '').trim();
  const matchTask = (payload: Record<string, unknown> = {}) => {
    const pid = String(payload.taskId || payload.id || '').trim();
    return !taskIdText || pid === taskIdText;
  };

  for (const [eventName, handler] of specs) {
    listen<T>(eventName, (event) => {
      if (closed) return;
      const payload = (event.payload || {}) as T;
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
        try {
          fn?.();
        } catch {
          // Ignore listener cleanup failures.
        }
      }
    },
  };
}

export async function getSettings(options: { force?: boolean } = {}): Promise<Settings> {
  const { force = false } = options;
  if (!force && inflightSettingsRequest) return inflightSettingsRequest;
  const request = call<Settings>('settings_get');
  if (force) return request;
  const wrapped = request.finally(() => {
    if (inflightSettingsRequest === wrapped) inflightSettingsRequest = null;
  });
  inflightSettingsRequest = wrapped;
  return wrapped;
}

export async function saveSettings(data: Partial<Settings>): Promise<unknown> {
  return call('settings_save', { data });
}

export async function moveDataDir(path: string): Promise<{ dataDir?: string; cleanupWarning?: string }> {
  return call('settings_move_data_dir', { data: { path } });
}

export async function getCredentials(): Promise<CredentialsReadResult> {
  return call('credentials_get');
}

export async function saveCredentials(payload: CredentialsPayload): Promise<CredentialsSaveResult> {
  return call('credentials_save', { data: payload });
}

export async function browseFolder(): Promise<BrowseFolderResult> {
  return call('settings_browse_folder');
}

export async function getProviderModels(endpoint: string, apiKey?: string): Promise<{ models?: Array<{ value?: string; label?: string }> }> {
  return call('settings_get_provider_models', {
    data: apiKey == null ? { endpoint } : { endpoint, apiKey },
  });
}

export async function openExternalUrl(url: string): Promise<unknown> {
  return call('system_open_external_url', { data: { url } });
}

export async function startOrganize(params: {
  rootPath: string;
  excludedPatterns: string[];
  batchSize: number;
  summaryStrategy: SummaryMode;
  useWebSearch: boolean;
  responseLanguage: string;
}): Promise<{ taskId?: string }> {
  return call('organize_start', { input: params });
}

export async function stopOrganize(taskId: string): Promise<unknown> {
  return call('organize_stop', { taskId });
}

export async function getOrganizeResult(taskId: string): Promise<OrganizeSnapshot> {
  return call('organize_get_result', { taskId });
}

export async function getLatestOrganizeResult(rootPath: string): Promise<OrganizeSnapshot> {
  return call('organize_get_latest_result', { rootPath });
}

export function connectOrganizeStream(taskId: string, handlers: {
  onProgress?: (payload: OrganizeProgressEvent) => void;
  onSummaryReady?: (payload: OrganizeSummaryReadyEvent) => void;
  onFileDone?: (payload: OrganizeFileDoneEvent) => void;
  onDone?: (payload: OrganizeTerminalEvent) => void;
  onError?: (payload: OrganizeErrorEvent) => void;
  onStopped?: (payload: OrganizeTerminalEvent) => void;
}): StreamHandle {
  let stream: StreamHandle;
  stream = createStream(taskId, [
    ['organize_progress', (p) => handlers.onProgress?.(p as OrganizeProgressEvent)],
    ['organize_summary_ready', (p) => handlers.onSummaryReady?.(p as OrganizeSummaryReadyEvent)],
    ['organize_file_done', (p) => handlers.onFileDone?.(p as OrganizeFileDoneEvent)],
    ['organize_done', (p) => {
      handlers.onDone?.(p as OrganizeTerminalEvent);
      stream.close();
    }],
    ['organize_error', (p) => {
      handlers.onError?.(p as OrganizeErrorEvent);
      stream.close();
    }],
    ['organize_stopped', (p) => {
      handlers.onStopped?.(p as OrganizeTerminalEvent);
      stream.close();
    }],
  ]);
  return stream;
}

export async function advisorSessionStart(params: {
  rootPath: string;
  responseLanguage: string;
}): Promise<AdvisorSessionData> {
  return call('advisor_session_start', { input: params });
}

export async function advisorSessionGet(sessionId: string): Promise<AdvisorSessionData> {
  return call('advisor_session_get', { sessionId });
}

export async function advisorMessageSend(params: {
  sessionId: string;
  message: string;
}): Promise<AdvisorSessionData> {
  return call('advisor_message_send', { input: params });
}

export async function advisorCardAction(params: {
  sessionId: string;
  cardId: string;
  action: string;
  payload?: unknown;
}): Promise<AdvisorSessionData> {
  return call('advisor_card_action', { input: params });
}
