import {
  connectScanStream,
  getActiveScan,
  getScanResult,
  startScan,
  stopScan,
} from './api.js';
import * as storage from './storage.js';
import { t } from './i18n.js';

const ACTIVE_SCAN_STATUSES = new Set(['idle', 'scanning', 'analyzing']);
const SCAN_LOG_CACHE_KEY = 'wipeout.scanner.global.log.v2';

function normalizePersistedLogEntries(raw) {
  if (!Array.isArray(raw)) return [];
  return raw
    .filter((entry) => entry && typeof entry === 'object')
    .map((entry) => ({
      type: String(entry.type || 'info'),
      text: String(entry.text || ''),
      time: String(entry.time || ''),
    }))
    .slice(-200);
}

function readPersistedScanLog() {
  const raw = storage.get(SCAN_LOG_CACHE_KEY, null);
  return normalizePersistedLogEntries(raw?.entries);
}

function createLog(type, text) {
  return {
    type,
    text: String(text || ''),
    time: new Date().toLocaleTimeString('zh-CN', { hour12: false }),
  };
}

class ScanTaskController {
  constructor() {
    this.listeners = new Set();
    this.state = {
      activeTaskId: null,
      latestTaskId: storage.get('lastScanTaskId', null),
      activeEventSource: null,
      snapshot: storage.get('lastScan', null),
      logEntries: readPersistedScanLog(),
    };
  }

  subscribe(listener) {
    this.listeners.add(listener);
    listener({ kind: 'state', state: this.getState() });
    return () => this.listeners.delete(listener);
  }

  getState() {
    return {
      activeTaskId: this.state.activeTaskId,
      latestTaskId: this.state.latestTaskId,
      snapshot: this.state.snapshot,
      logEntries: [...this.state.logEntries],
    };
  }

  notifyState() {
    const payload = { kind: 'state', state: this.getState() };
    for (const listener of this.listeners) {
      listener(payload);
    }
  }

  persistState() {
    storage.set(SCAN_LOG_CACHE_KEY, { entries: this.state.logEntries });
    if (this.state.snapshot) {
      storage.set('lastScan', this.state.snapshot);
    }
    if (this.state.latestTaskId) {
      storage.set('lastScanTaskId', this.state.latestTaskId);
    }
  }

  setSnapshot(snapshot) {
    this.state.snapshot = snapshot || null;
    if (snapshot?.id) {
      this.state.latestTaskId = String(snapshot.id).trim();
    }
    this.persistState();
    this.notifyState();
  }

  appendLog(type, text) {
    this.state.logEntries.push(createLog(type, text));
    if (this.state.logEntries.length > 200) {
      this.state.logEntries.splice(0, this.state.logEntries.length - 200);
    }
    this.persistState();
    this.notifyState();
  }

  replaceLogs(entries = []) {
    this.state.logEntries = normalizePersistedLogEntries(entries);
    this.persistState();
    this.notifyState();
  }

  closeStream() {
    if (this.state.activeEventSource) {
      this.state.activeEventSource.close();
      this.state.activeEventSource = null;
    }
  }

  connect(taskId) {
    this.closeStream();
    this.state.activeEventSource = connectScanStream(taskId, {
      onProgress: (payload) => this.handleProgress(payload),
      onWarning: (payload) => this.handleWarning(payload),
      onDone: (payload) => this.handleDone(payload),
      onError: (payload) => this.handleError(payload),
      onStopped: (payload) => this.handleStopped(payload),
    });
  }

  activateTask(taskId, snapshot = null) {
    const normalized = String(taskId || '').trim();
    if (!normalized) return;
    this.state.activeTaskId = normalized;
    this.state.latestTaskId = normalized;
    if (snapshot) {
      this.state.snapshot = snapshot;
    }
    this.persistState();
    this.notifyState();
    this.connect(normalized);
  }

  resetActiveTask() {
    this.state.activeTaskId = null;
    this.closeStream();
    this.persistState();
    this.notifyState();
  }

  async startTask(params) {
    this.replaceLogs([]);
    const result = await startScan(params);
    this.appendLog('info', `${t('scanner.log_start')} [${result.taskId}]`);
    this.activateTask(result.taskId);
    return result;
  }

  async stopTask() {
    if (!this.state.activeTaskId) return null;
    return stopScan(this.state.activeTaskId);
  }

  async restoreAnyActiveTask(preferredTaskId = null) {
    const activeTasks = await getActiveScan();
    if (!Array.isArray(activeTasks) || !activeTasks.length) {
      return false;
    }
    const preferred = String(preferredTaskId || '').trim();
    const selected = activeTasks.find((item) => String(item?.id || item?.taskId || '').trim() === preferred) || activeTasks[0];
    const taskId = String(selected?.id || selected?.taskId || '').trim();
    if (!taskId) return false;
    this.activateTask(taskId, selected);
    this.appendLog('info', `${t('scanner.scanning')}: ${selected?.targetPath || selected?.currentPath || taskId}`);
    return true;
  }

  async restoreTaskById(taskId) {
    const normalized = String(taskId || '').trim();
    if (!normalized) return false;
    const snapshot = await getScanResult(normalized);
    if (!snapshot?.id) return false;
    this.setSnapshot(snapshot);
    if (ACTIVE_SCAN_STATUSES.has(String(snapshot.status || '').trim())) {
      this.activateTask(snapshot.id, snapshot);
    }
    return true;
  }

  handleProgress(payload) {
    this.setSnapshot(payload);
    const label = String(payload?.status || '').trim() === 'analyzing'
      ? t('scanner.analyzing')
      : t('scanner.scanning');
    this.appendLog('info', `${label}: ${payload?.currentPath || payload?.targetPath || ''}`);
  }

  handleWarning(payload) {
    if (payload?.type === 'permission_denied') {
      this.appendLog('warning', `${t('scanner.permission_denied_skip')}${payload?.path || payload?.message || ''}`);
    } else {
      this.appendLog('warning', String(payload?.message || 'warning'));
    }
  }

  handleDone(payload) {
    this.setSnapshot(payload);
    this.appendLog('success', `${t('scanner.done')}: ${payload?.targetPath || payload?.currentPath || payload?.id || ''}`);
    this.resetActiveTask();
  }

  handleError(payload) {
    this.setSnapshot(payload?.snapshot || this.state.snapshot);
    this.appendLog('error', `${t('scanner.toast_failed_detail')}${payload?.message || payload?.error || ''}`);
    this.resetActiveTask();
  }

  handleStopped(payload) {
    this.setSnapshot(payload);
    this.appendLog('info', t('scanner.stopped'));
    this.resetActiveTask();
  }
}

export const scanTaskController = new ScanTaskController();
