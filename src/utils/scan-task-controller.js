import {
  connectScanStream,
  getActiveScan,
  getScanResult,
  startScan,
  stopScan,
} from './api.js';
import { formatSize } from './storage.js';
import * as storage from './storage.js';
import { t } from './i18n.js';

const ACTIVE_SCAN_STATUSES = new Set(['idle', 'scanning', 'analyzing']);
const SCAN_LOG_CACHE_KEY = 'wipeout.scanner.global.log.v1';
const PROGRESS_LOG_INTERVAL_MS = 1000;

function isActiveScanStatus(status) {
  return ACTIVE_SCAN_STATUSES.has(String(status || '').trim());
}

function normalizePersistedLogEntries(raw) {
  if (!Array.isArray(raw)) return [];
  return raw
    .filter((entry) => entry && typeof entry === 'object')
    .map((entry) => ({
      id: Number.isFinite(Number(entry.id)) ? Number(entry.id) : undefined,
      kind: entry.kind === 'detail' ? 'detail' : 'simple',
      type: String(entry.type || 'scanning'),
      text: String(entry.text || ''),
      summary: String(entry.summary || ''),
      detailHtml: String(entry.detailHtml || ''),
      time: String(entry.time || ''),
      expandByDefault: !!entry.expandByDefault,
    }))
    .slice(-200);
}

function readPersistedScanLog() {
  const raw = storage.get(SCAN_LOG_CACHE_KEY, null);
  if (!raw || typeof raw !== 'object') return { entries: [], nextId: 0 };
  const entries = normalizePersistedLogEntries(raw.entries);
  const maxId = entries.reduce((acc, entry) => Math.max(acc, Number.isFinite(entry.id) ? entry.id : -1), -1);
  return {
    entries,
    nextId: Math.max(Number(raw.nextId) || 0, maxId + 1),
  };
}

class ScanTaskController {
  constructor() {
    const persistedLog = readPersistedScanLog();
    this.listeners = new Set();
    this.lastProgressLogAt = 0;
    this.lastProgressLogStatus = '';
    this.state = {
      activeTaskId: null,
      latestTaskId: null,
      activeEventSource: null,
      snapshot: storage.get('lastScan', null),
      logEntries: persistedLog.entries,
      nextLogEntryId: persistedLog.nextId,
    };
  }

  subscribe(listener) {
    this.listeners.add(listener);
    listener({ kind: 'state', state: this.getState() });
    return () => {
      this.listeners.delete(listener);
    };
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
    const event = { kind: 'state', state: this.getState() };
    for (const listener of this.listeners) {
      listener(event);
    }
  }

  emit(kind, payload = {}) {
    const event = { kind, ...payload, state: this.getState() };
    for (const listener of this.listeners) {
      listener(event);
    }
  }

  persistScanLog() {
    storage.set(SCAN_LOG_CACHE_KEY, {
      entries: this.state.logEntries,
      nextId: this.state.nextLogEntryId,
      updatedAt: Date.now(),
    });
  }

  maybePersistScanLog(force = false) {
    if (force || !this.state.activeTaskId) {
      this.persistScanLog();
    }
  }

  replaceLogEntries(nextEntries = [], { persist = true } = {}) {
    this.state.logEntries = normalizePersistedLogEntries(nextEntries);
    const maxId = this.state.logEntries.reduce((acc, entry) => Math.max(acc, Number.isFinite(entry.id) ? entry.id : -1), -1);
    this.state.nextLogEntryId = maxId + 1;
    if (!this.state.logEntries.length) {
      this.resetProgressLogThrottle();
    }
    if (persist) {
      this.persistScanLog();
    }
    this.notifyState();
  }

  trimLogEntries() {
    while (this.state.logEntries.length > 200) {
      this.state.logEntries.shift();
    }
  }

  addLog(type, text, { persist = false } = {}) {
    const time = new Date().toLocaleTimeString('zh-CN', { hour12: false });
    this.state.logEntries.push({ kind: 'simple', type, text, time });
    this.trimLogEntries();
    this.maybePersistScanLog(persist);
    this.notifyState();
  }

  addDetailLog(type, summary, detailHtml, { persist = false, expandByDefault = false } = {}) {
    const time = new Date().toLocaleTimeString('zh-CN', { hour12: false });
    if (expandByDefault) {
      for (const entry of this.state.logEntries) {
        if (entry?.kind === 'detail') {
          entry.expandByDefault = false;
        }
      }
    }
    this.state.logEntries.push({
      id: this.state.nextLogEntryId++,
      kind: 'detail',
      type,
      summary,
      detailHtml,
      time,
      expandByDefault,
    });
    this.trimLogEntries();
    this.maybePersistScanLog(persist);
    this.notifyState();
  }

  updateTaskIds(taskId) {
    const normalized = String(taskId || '').trim();
    if (!normalized) return;
    this.state.latestTaskId = normalized;
    storage.set('lastScanTaskId', normalized);
  }

  updateSnapshot(snapshot, { persist = true } = {}) {
    this.state.snapshot = snapshot || null;
    if (persist && snapshot) {
      storage.set('lastScan', snapshot);
    }
    this.notifyState();
  }

  closeStream() {
    if (this.state.activeEventSource) {
      this.state.activeEventSource.close();
      this.state.activeEventSource = null;
    }
  }

  connect(taskId) {
    const normalizedTaskId = String(taskId || '').trim();
    if (!normalizedTaskId) return;
    this.closeStream();
    this.state.activeEventSource = connectScanStream(normalizedTaskId, {
      onProgress: (data) => this.handleProgress(data),
      onFound: (item) => this.handleFound(item),
      onAgentCall: (data) => this.handleAgentCall(data),
      onAgentResponse: (data) => this.handleAgentResponse(data),
      onCache: (info) => this.handleCache(info),
      onWarning: (info) => this.handleWarning(info),
      onDone: (data) => this.handleDone(data),
      onError: (err) => this.handleError(err),
      onStopped: (data) => this.handleStopped(data),
    });
  }

  activateTask(taskId, { snapshot = null, resetLogs = false, appendStartLog = false } = {}) {
    const normalizedTaskId = String(taskId || '').trim();
    if (!normalizedTaskId) return;
    this.state.activeTaskId = normalizedTaskId;
    this.updateTaskIds(normalizedTaskId);
    this.resetProgressLogThrottle();
    if (resetLogs) {
      this.replaceLogEntries([], { persist: true });
    }
    if (snapshot) {
      this.updateSnapshot(snapshot, { persist: true });
    } else {
      this.notifyState();
    }
    this.connect(normalizedTaskId);
    if (appendStartLog) {
      this.addLog('scanning', `${t('scanner.log_start')} [${normalizedTaskId}]`);
    }
  }

  resetActiveTask({ keepLatestTaskId = true } = {}) {
    const latestTaskId = keepLatestTaskId ? this.state.latestTaskId : null;
    this.state.activeTaskId = null;
    this.closeStream();
    if (!keepLatestTaskId) {
      this.state.latestTaskId = null;
    } else {
      this.state.latestTaskId = latestTaskId;
    }
    this.resetProgressLogThrottle();
    this.notifyState();
  }

  resetProgressLogThrottle() {
    this.lastProgressLogAt = 0;
    this.lastProgressLogStatus = '';
  }

  shouldLogProgress(status) {
    const normalizedStatus = String(status || '').trim();
    const now = Date.now();
    if (normalizedStatus !== this.lastProgressLogStatus) {
      this.lastProgressLogStatus = normalizedStatus;
      this.lastProgressLogAt = now;
      return true;
    }
    if ((now - this.lastProgressLogAt) < PROGRESS_LOG_INTERVAL_MS) {
      return false;
    }
    this.lastProgressLogAt = now;
    return true;
  }

  async startTask(params) {
    const result = await startScan(params);
    this.activateTask(result.taskId, { resetLogs: true, appendStartLog: true });
    return result;
  }

  async stopTask() {
    if (!this.state.activeTaskId) return;
    return stopScan(this.state.activeTaskId);
  }

  async restoreAnyActiveTask(preferredTaskId = null) {
    const activeTasks = await getActiveScan();
    if (!Array.isArray(activeTasks) || activeTasks.length === 0) {
      return false;
    }
    const preferredId = String(preferredTaskId || '').trim();
    const task = activeTasks.find((item) => {
      const taskId = String(item?.taskId || item?.id || '').trim();
      return taskId && taskId === preferredId;
    }) || activeTasks[0];

    const cachedLastScan = storage.get('lastScan', null);
    const taskId = String(task?.taskId || task?.id || '').trim();
    const sameTaskAsCache = String(cachedLastScan?.id || '').trim() === taskId;
    this.activateTask(taskId, {
      snapshot: task,
      resetLogs: !sameTaskAsCache,
      appendStartLog: !sameTaskAsCache,
    });
    return true;
  }

  async restoreTaskById(taskId) {
    const preferredTaskId = String(taskId || '').trim();
    if (!preferredTaskId) return false;
    let snapshot = await getScanResult(preferredTaskId);
    if (!snapshot?.id || !isActiveScanStatus(snapshot.status)) {
      return false;
    }
    if (String(snapshot.status || '').trim() === 'idle') {
      const activeTasks = await getActiveScan();
      const runtimeTask = Array.isArray(activeTasks)
        ? activeTasks.find((item) => {
          const taskIdValue = String(item?.taskId || item?.id || '').trim();
          return taskIdValue && taskIdValue === preferredTaskId;
        })
        : null;
      if (!runtimeTask) {
        return false;
      }
      snapshot = runtimeTask;
    }
    const cachedLastScan = storage.get('lastScan', null);
    const sameTaskAsCache = String(cachedLastScan?.id || '').trim() === String(snapshot.id || '').trim();
    this.activateTask(snapshot.id, {
      snapshot,
      resetLogs: !sameTaskAsCache,
      appendStartLog: !sameTaskAsCache,
    });
    return true;
  }

  handleProgress(data) {
    if (data?.id) {
      this.updateTaskIds(data.id);
    }
    this.updateSnapshot(data, { persist: true });

    if (!this.shouldLogProgress(data?.status)) {
      return;
    }

    if (data?.status === 'analyzing') {
      this.addLog('analyzing', t('scanner.log_ai_analyzing', { path: data.currentPath || '-' }));
    } else if (data?.status === 'scanning') {
      this.addLog('scanning', t('scanner.log_scanning_path', { path: data.currentPath || '-' }));
    }
  }

  handleFound(item) {
    this.addLog('found', t('scanner.log_found_item', {
      name: item?.name || '-',
      size: formatSize(item?.size || 0),
      reason: item?.reason || '-',
    }));
  }

  handleWarning(info) {
    if (info?.type !== 'permission_denied') return;
    const path = String(info.path || '').trim();
    this.addLog('analyzing', `${t('scanner.permission_denied_skip')}${path || info.message || ''}`);
  }

  handleCache(info) {
    if (!info || typeof info !== 'object') return;
    const action = String(info.action || '').trim();
    if (action === 'prepare_incremental_reuse_tree') {
      this.addLog('analyzing', `继续深入：直接复用当前扫描树（${info.count || 0} 个边界目录）`);
      return;
    }
    if (action === 'prepare_incremental_prune') {
      this.addLog('analyzing', `继续深入：正在裁剪待深入的边界目录（${info.count || 0} 个）`);
      return;
    }
    if (action === 'prepare_incremental_ready') {
      this.addLog('analyzing', '继续深入：增量基线准备完成，开始读取更深层目录');
      return;
    }
    if (action === 'reuse') {
      this.addLog('analyzing', `${t('scanner.cache_reuse')}: ${info.path || info.name || ''}`);
      return;
    }
    if (action === 'rescan_changed') {
      this.addLog('analyzing', `${t('scanner.cache_rescan')}: ${info.path || info.name || ''}`);
      return;
    }
    if (action === 'skip_deleted') {
      this.addLog('analyzing', t('scanner.cache_deleted', { count: info.count || 0 }));
    }
  }

  handleDone(data) {
    if (data?.id) {
      this.updateTaskIds(data.id);
    }
    this.updateSnapshot(data, { persist: true });
    const doneText = t('scanner.completed', { count: data?.deletableCount ?? 0 });
    this.addLog('found', doneText);
    let permissionText = '';
    if (data?.permissionDeniedCount > 0) {
      permissionText = t('scanner.permission_denied_summary', { count: data.permissionDeniedCount ?? 0 });
      this.addLog('analyzing', permissionText);
    }
    this.resetActiveTask();
    this.persistScanLog();
    this.emit('done', { data, doneText, permissionText });
  }

  handleError(err) {
    const message = err?.message || t('toast.error');
    if (err?.snapshot) {
      this.updateSnapshot(err.snapshot, { persist: true });
    }
    this.addLog('analyzing', `${t('scanner.toast_failed_detail')}${message}`);
    this.resetActiveTask();
    this.persistScanLog();
    this.emit('error', { error: err, message });
  }

  handleStopped(data) {
    if (data?.id) {
      this.updateTaskIds(data.id);
    }
    this.updateSnapshot(data, { persist: true });
    this.addLog('scanning', t('scanner.stopped'));
    this.resetActiveTask();
    this.persistScanLog();
    this.emit('stopped', { data });
  }

  handleAgentCall(data) {
    const childDirList = (data.childDirectories || [])
      .map((entry) => `- ${entry.name} (${formatSize(entry.size)})`)
      .join('\n');

    let detailHtml = `
      <div style="margin-bottom: 8px;"><strong>Type:</strong> ${this.escHtml(data.nodeType)}</div>
      <div style="margin-bottom: 8px;"><strong>Path:</strong> ${this.escHtml(data.nodePath)}</div>
      <div style="margin-bottom: 8px;"><strong>Name:</strong> ${this.escHtml(data.nodeName)}</div>
      <div style="margin-bottom: 8px;"><strong>Size:</strong> ${this.escHtml(formatSize(data.nodeSize || 0))}</div>
    `;

    if (data.nodeType === 'directory') {
      detailHtml += `
        <div style="margin-bottom: 4px;"><strong>Direct Child Directories</strong></div>
        <div style="padding-left: 8px; border-left: 2px solid rgba(6, 182, 212, 0.3);">${this.escHtml(childDirList || '(none)')}</div>
      `;
    }

    this.addDetailLog('agent_call', `LLM call - ${data.nodeType}: ${data.nodeName}`, detailHtml);
  }

  handleAgentResponse(data) {
    const elapsed = Number(data.elapsed || 0) / 1000;
    const classStr = String(data.classification || 'expand_analysis');
    const riskStr = String(data.risk || 'medium');
    const shouldExpand = data.nodeType === 'directory'
      ? (data.shouldExpand ? 'true' : 'false')
      : 'n/a';

    let detailSections = '';
    detailSections += `<div style="margin-bottom: 10px;">
      <strong>Type:</strong> ${this.escHtml(data.nodeType)} | <strong>Model:</strong> ${this.escHtml(data.model)} | <strong>Elapsed:</strong> ${elapsed.toFixed(1)}s | <strong>Token:</strong> ${(data.tokenUsage?.total || 0).toLocaleString()}
    </div>`;

    detailSections += `<div style="margin-bottom: 10px;"><strong>Path:</strong> ${this.escHtml(data.nodePath)}</div>`;
    detailSections += `<div style="margin-bottom: 10px;"><strong>Classification:</strong> ${this.escHtml(classStr)} | <strong>Risk:</strong> ${this.escHtml(riskStr)} | <strong>Expand Decision:</strong> ${this.escHtml(shouldExpand)}</div>`;

    if (data.error) {
      detailSections += `<div style="margin-bottom: 10px; color: var(--accent-danger);"><strong>Error:</strong> ${this.escHtml(data.error)}</div>`;
    }

    if (data.userPrompt) {
      detailSections += `<div style="margin-bottom: 10px;">
        <strong>Prompt:</strong>
        <div style="margin-top: 4px; padding: 8px; background: rgba(0,0,0,0.3); border-radius: 4px; max-height: 300px; overflow-y: auto;">${this.escHtml(data.userPrompt)}</div>
      </div>`;
    }

    if (data.reasoning) {
      detailSections += `<div style="margin-bottom: 10px;">
        <strong>Reasoning:</strong>
        <div style="margin-top: 4px; padding: 8px; background: rgba(245, 158, 11, 0.08); border: 1px solid rgba(245, 158, 11, 0.15); border-radius: 4px; max-height: 400px; overflow-y: auto;">${this.escHtml(data.reasoning)}</div>
      </div>`;
    }

    const search = data.search && typeof data.search === 'object' ? data.search : null;
    const searchQuery = String(search?.request?.query || '').trim();
    if (searchQuery) {
      const searchReason = String(search?.request?.reason || '').trim();
      const searchResults = Array.isArray(search?.results) ? search.results : [];
      const formattedResults = searchResults
        .slice(0, 5)
        .map((item, index) => {
          const title = String(item?.title || '').trim() || `Result ${index + 1}`;
          const url = String(item?.url || '').trim();
          const content = String(item?.content || '').trim();
          return [title, url, content].filter(Boolean).join('\n');
        })
        .filter(Boolean)
        .join('\n\n');

      detailSections += `<div style="margin-bottom: 10px;">
        <strong>Web Search:</strong>
        <div style="margin-top: 4px; padding: 8px; background: rgba(6, 182, 212, 0.08); border: 1px solid rgba(6, 182, 212, 0.15); border-radius: 4px; max-height: 400px; overflow-y: auto;">${this.escHtml(`Query: ${searchQuery}${searchReason ? `\nReason: ${searchReason}` : ''}${formattedResults ? `\n\n${formattedResults}` : ''}`)}</div>
      </div>`;
    }

    if (data.rawContent) {
      const raw = String(data.rawContent);
      const truncated = raw.length > 2000 ? `${raw.slice(0, 2000)}\n...` : raw;
      detailSections += `<div>
        <strong>Raw Response:</strong>
        <div style="margin-top: 4px; padding: 8px; background: rgba(0,0,0,0.3); border-radius: 4px; max-height: 400px; overflow-y: auto;">${this.escHtml(truncated)}</div>
      </div>`;
    }

    const statusIcon = data.error ? 'X' : 'OK';
    this.addDetailLog(
      'agent_response',
      `${statusIcon} LLM response - ${data.nodeType}: ${data.nodeName} (${classStr})`,
      detailSections
    );
  }

  getNodeTypeLabel(nodeType) {
    return nodeType === 'directory'
      ? t('scanner.log_type_directory')
      : t('scanner.log_type_file');
  }

  getClassificationLabel(classification) {
    const key = `scanner.classification_${String(classification || 'expand_analysis')}`;
    const translated = t(key);
    return translated === key ? String(classification || 'expand_analysis') : translated;
  }

  getRiskLabel(risk) {
    const key = `scanner.risk_${String(risk || 'medium')}`;
    const translated = t(key);
    return translated === key ? String(risk || 'medium') : translated;
  }

  formatSearchContext(search) {
    const request = search && typeof search === 'object' ? search.request : null;
    const query = String(request?.query || '').trim();
    if (!query) return '';

    const reason = String(request?.reason || '').trim();
    const results = Array.isArray(search?.results) ? search.results : [];
    const formattedResults = results
      .slice(0, 5)
      .map((item, index) => {
        const title = String(item?.title || '').trim() || `Result ${index + 1}`;
        const url = String(item?.url || '').trim();
        const content = String(item?.content || '').trim();
        return [title, url, content].filter(Boolean).join('\n');
      })
      .filter(Boolean)
      .join('\n\n');

    return [
      `${t('scanner.log_search_query')}: ${query}`,
      reason ? `${t('scanner.log_search_reason')}: ${reason}` : '',
      formattedResults,
    ].filter(Boolean).join('\n\n');
  }

  handleProgress(data) {
    if (data?.id) {
      this.updateTaskIds(data.id);
    }
    this.updateSnapshot(data, { persist: true });

    if (!this.shouldLogProgress(data?.status)) {
      return;
    }

    if (data?.status === 'analyzing') {
      this.addLog('analyzing', t('scanner.log_ai_analyzing', { path: data.currentPath || '-' }));
    } else if (data?.status === 'scanning') {
      this.addLog('scanning', t('scanner.log_scanning_path', { path: data.currentPath || '-' }));
    }
  }

  handleFound(item) {
    this.addLog('found', t('scanner.log_found_item', {
      name: item?.name || '-',
      size: formatSize(item?.size || 0),
      reason: item?.reason || '-',
    }));
  }

  handleCache(info) {
    if (!info || typeof info !== 'object') return;
    const action = String(info.action || '').trim();
    if (action === 'prepare_incremental_reuse_tree') {
      this.addLog('analyzing', t('scanner.cache_prepare_reuse_tree', { count: info.count || 0 }));
      return;
    }
    if (action === 'prepare_incremental_prune') {
      this.addLog('analyzing', t('scanner.cache_prepare_prune', { count: info.count || 0 }));
      return;
    }
    if (action === 'prepare_incremental_ready') {
      this.addLog('analyzing', t('scanner.cache_prepare_ready'));
      return;
    }
    if (action === 'reuse') {
      this.addLog('analyzing', t('scanner.cache_reuse_detail', { path: info.path || info.name || '-' }));
      return;
    }
    if (action === 'rescan_changed') {
      this.addLog('analyzing', t('scanner.cache_rescan_detail', { path: info.path || info.name || '-' }));
      return;
    }
    if (action === 'skip_deleted') {
      this.addLog('analyzing', t('scanner.cache_deleted', { count: info.count || 0 }));
    }
  }

  handleAgentCall(data) {
    const nodeTypeLabel = this.getNodeTypeLabel(data?.nodeType);
    const childDirList = (data?.childDirectories || [])
      .map((entry) => `- ${entry.name} (${formatSize(entry.size)})`)
      .join('\n');

    let detailHtml = `
      <div style="margin-bottom: 8px;"><strong>${this.escHtml(t('scanner.log_detail_type'))}:</strong> ${this.escHtml(nodeTypeLabel)}</div>
      <div style="margin-bottom: 8px;"><strong>${this.escHtml(t('scanner.log_detail_path'))}:</strong> ${this.escHtml(data?.nodePath)}</div>
      <div style="margin-bottom: 8px;"><strong>${this.escHtml(t('scanner.log_detail_name'))}:</strong> ${this.escHtml(data?.nodeName)}</div>
      <div style="margin-bottom: 8px;"><strong>${this.escHtml(t('scanner.log_detail_size'))}:</strong> ${this.escHtml(formatSize(data?.nodeSize || 0))}</div>
    `;

    if (data?.nodeType === 'directory') {
      detailHtml += `
        <div style="margin-bottom: 4px;"><strong>${this.escHtml(t('scanner.log_detail_children'))}</strong></div>
        <div style="padding-left: 8px; border-left: 2px solid rgba(6, 182, 212, 0.3);">${this.escHtml(childDirList || '(none)')}</div>
      `;
    }

    this.addDetailLog(
      'agent_call',
      t('scanner.log_llm_call_summary', {
        type: nodeTypeLabel,
        name: data?.nodeName || '-',
      }),
      detailHtml,
    );
  }

  handleAgentResponse(data) {
    const nodeTypeLabel = this.getNodeTypeLabel(data?.nodeType);
    const classificationLabel = this.getClassificationLabel(data?.classification);
    const riskLabel = this.getRiskLabel(data?.risk);
    const elapsedMs = Number(data?.elapsed || 0);
    const elapsedText = `${(elapsedMs > 0 ? elapsedMs / 1000 : 0).toFixed(1)}s`;
    const shouldExpandLabel = data?.nodeType === 'directory'
      ? (data?.shouldExpand ? t('scanner.log_expand_yes') : t('scanner.log_expand_no'))
      : t('scanner.log_expand_na');

    let detailSections = `
      <div style="margin-bottom: 10px;">
        <strong>${this.escHtml(t('scanner.log_detail_model'))}:</strong> ${this.escHtml(data?.model)} &nbsp;|&nbsp;
        <strong>${this.escHtml(t('scanner.log_detail_elapsed'))}:</strong> ${this.escHtml(elapsedText)} &nbsp;|&nbsp;
        <strong>${this.escHtml(t('scanner.log_detail_token'))}:</strong> ${(data?.tokenUsage?.total || 0).toLocaleString()}
      </div>
      <div style="margin-bottom: 10px;">
        <strong>${this.escHtml(t('scanner.log_detail_classification'))}:</strong> ${this.escHtml(classificationLabel)} &nbsp;|&nbsp;
        <strong>${this.escHtml(t('scanner.log_detail_risk'))}:</strong> ${this.escHtml(riskLabel)} &nbsp;|&nbsp;
        <strong>${this.escHtml(t('scanner.log_detail_expand'))}:</strong> ${this.escHtml(shouldExpandLabel)}
      </div>
      <div style="margin-bottom: 10px;"><strong>${this.escHtml(t('scanner.log_detail_type'))}:</strong> ${this.escHtml(nodeTypeLabel)}</div>
      <div style="margin-bottom: 10px;"><strong>${this.escHtml(t('scanner.log_detail_path'))}:</strong> ${this.escHtml(data?.nodePath)}</div>
    `;

    if (data?.error) {
      detailSections += `<div style="margin-bottom: 10px; color: var(--accent-danger);"><strong>${this.escHtml(t('scanner.log_detail_error'))}:</strong> ${this.escHtml(data.error)}</div>`;
    }

    if (data?.userPrompt) {
      detailSections += `<div style="margin-bottom: 10px;">
        <strong>${this.escHtml(t('scanner.log_detail_prompt'))}:</strong>
        <div style="margin-top: 4px; padding: 8px; background: rgba(0,0,0,0.3); border-radius: 4px; max-height: 300px; overflow-y: auto;">${this.escHtml(data.userPrompt)}</div>
      </div>`;
    }

    if (data?.reasoning) {
      detailSections += `<div style="margin-bottom: 10px;">
        <strong>${this.escHtml(t('scanner.log_detail_reasoning'))}:</strong>
        <div style="margin-top: 4px; padding: 8px; background: rgba(245, 158, 11, 0.08); border: 1px solid rgba(245, 158, 11, 0.15); border-radius: 4px; max-height: 400px; overflow-y: auto;">${this.escHtml(data.reasoning)}</div>
      </div>`;
    }

    const searchContext = this.formatSearchContext(data?.search);
    if (searchContext) {
      detailSections += `<div style="margin-bottom: 10px;">
        <strong>${this.escHtml(t('scanner.log_detail_search'))}:</strong>
        <div style="margin-top: 4px; padding: 8px; background: rgba(6, 182, 212, 0.08); border: 1px solid rgba(6, 182, 212, 0.15); border-radius: 4px; max-height: 400px; overflow-y: auto;">${this.escHtml(searchContext)}</div>
      </div>`;
    }

    if (data?.rawContent) {
      const raw = String(data.rawContent);
      const truncated = raw.length > 2000 ? `${raw.slice(0, 2000)}\n...（已截断）` : raw;
      detailSections += `<div>
        <strong>${this.escHtml(t('scanner.log_detail_raw'))}:</strong>
        <div style="margin-top: 4px; padding: 8px; background: rgba(0,0,0,0.3); border-radius: 4px; max-height: 400px; overflow-y: auto;">${this.escHtml(truncated)}</div>
      </div>`;
    }

    const summaryParts = [
      t('scanner.log_llm_response_summary', {
        type: nodeTypeLabel,
        name: data?.nodeName || '-',
      }),
      elapsedText,
      classificationLabel,
      riskLabel,
    ];
    if (data?.nodeType === 'directory') {
      summaryParts.push(t('scanner.log_expand_summary', { value: shouldExpandLabel }));
    }

    this.addDetailLog(
      'agent_response',
      summaryParts.join(' · '),
      detailSections,
      { expandByDefault: !!(data?.reasoning || data?.error) },
    );
  }

  escHtml(str) {
    return String(str || '')
      .replace(/&/g, '&amp;')
      .replace(/</g, '&lt;')
      .replace(/>/g, '&gt;')
      .replace(/"/g, '&quot;')
      .replace(/\n/g, '<br>');
  }
}

export const scanTaskController = new ScanTaskController();
