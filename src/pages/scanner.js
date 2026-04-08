import {
  browseFolder,
  deleteScanHistory,
  getScanResult,
  getSettings,
  listScanHistory,
  saveSettings,
} from '../utils/api.js';
import { showToast } from '../main.js';
import { getLang, t } from '../utils/i18n.js';
import { formatSize } from '../utils/storage.js';
import { scanTaskController } from '../utils/scan-task-controller.js';

const SCANNER_FORM_DRAFT_KEY = 'wipeout.scanner.global.form.v3';
const ADVISOR_HANDOFF_KEY = 'wipeout.advisor.global.handoff.v1';
const HISTORY_LIMIT = 12;
const DEFAULT_MAX_DEPTH = 5;

let pageContainer = null;
let scannerControllerUnsubscribe = null;
let scannerState = scanTaskController.getState();
let scannerHistory = [];
let scannerSettings = null;
let loadingHistory = false;

function escapeHtml(value) {
  const div = document.createElement('div');
  div.textContent = String(value ?? '');
  return div.innerHTML;
}

function clampDepth(value) {
  const parsed = Number(value);
  if (!Number.isFinite(parsed)) return DEFAULT_MAX_DEPTH;
  return Math.max(1, Math.min(16, Math.round(parsed)));
}

function normalizeIgnoredPaths(value) {
  const text = String(value || '');
  const seen = new Set();
  return text
    .split(/\r?\n/)
    .map((item) => item.trim())
    .filter((item) => {
      if (!item) return false;
      const key = item.toLowerCase();
      if (seen.has(key)) return false;
      seen.add(key);
      return true;
    });
}

function serializeIgnoredPaths(paths) {
  return (Array.isArray(paths) ? paths : []).join('\n');
}

function setAdvisorHandoff(payload) {
  try {
    localStorage.setItem(ADVISOR_HANDOFF_KEY, JSON.stringify(payload));
  } catch {
    // ignore storage failures
  }
}

function openAdvisor(rootPath, scanTaskId = null) {
  const normalizedRoot = String(rootPath || '').trim();
  if (!normalizedRoot) {
    showToast(getLang() === 'en' ? 'Select a folder first' : '请先选择或加载一个盘点目录', 'error');
    return;
  }
  setAdvisorHandoff({
    rootPath: normalizedRoot,
    scanTaskId: scanTaskId ? String(scanTaskId).trim() : null,
  });
  window.location.hash = '#/advisor';
}

function loadDraft() {
  try {
    const raw = localStorage.getItem(SCANNER_FORM_DRAFT_KEY);
    const parsed = raw ? JSON.parse(raw) : {};
    return {
      targetPath: String(parsed?.targetPath || ''),
      maxDepth: clampDepth(parsed?.maxDepth ?? DEFAULT_MAX_DEPTH),
      maxDepthUnlimited: !!parsed?.maxDepthUnlimited,
      ignoredPathsText: String(parsed?.ignoredPathsText || ''),
    };
  } catch {
    return {
      targetPath: '',
      maxDepth: DEFAULT_MAX_DEPTH,
      maxDepthUnlimited: false,
      ignoredPathsText: '',
    };
  }
}

function saveDraft(form) {
  try {
    localStorage.setItem(SCANNER_FORM_DRAFT_KEY, JSON.stringify(form));
  } catch {
    // ignore storage failures
  }
}

function getFormState() {
  const draft = loadDraft();
  const settings = scannerSettings || {};
  return {
    targetPath: draft.targetPath || String(settings?.scanPath || '').trim(),
    maxDepth: draft.maxDepth || clampDepth(settings?.maxDepth ?? DEFAULT_MAX_DEPTH),
    maxDepthUnlimited: draft.maxDepthUnlimited || !!settings?.maxDepthUnlimited,
    ignoredPathsText: draft.ignoredPathsText || serializeIgnoredPaths(settings?.scanIgnorePaths || []),
  };
}

function getStatusText(snapshot) {
  const status = String(snapshot?.status || '').trim();
  const map = {
    idle: t('scanner.prepare'),
    scanning: t('scanner.scanning'),
    analyzing: t('scanner.analyzing'),
    done: t('scanner.done'),
    stopped: t('scanner.stopped'),
    error: t('toast.error'),
  };
  return map[status] || t('scanner.not_set');
}

function getActiveSnapshot() {
  return scannerState?.snapshot || null;
}

function buildHistoryRows() {
  if (loadingHistory) {
    return `<div class="form-hint">${escapeHtml(t('scanner.history_loading'))}</div>`;
  }

  if (!scannerHistory.length) {
    return `
      <div class="empty-state">
        <div class="empty-state-text">${escapeHtml(t('scanner.history_empty'))}</div>
      </div>
    `;
  }

  return scannerHistory.map((task) => {
    const taskId = String(task?.taskId || task?.id || '').trim();
    const running = ['idle', 'scanning', 'analyzing'].includes(String(task?.status || '').trim());
    const rootPath = String(task?.targetPath || task?.rootPath || '').trim();
    return `
      <div class="card" style="padding: 16px;">
        <div style="display:flex;justify-content:space-between;gap:12px;align-items:flex-start;">
          <div style="min-width:0;">
            <div class="card-title" style="font-size:14px;">${escapeHtml(task?.targetPath || task?.rootPath || '-')}</div>
            <div class="form-hint" style="margin-top:6px;">
              ${escapeHtml(getStatusText(task))}
              · ${escapeHtml(formatSize(task?.totalCleanable || 0))}
              · ${escapeHtml(new Date(task?.updatedAt || task?.createdAt || Date.now()).toLocaleString('zh-CN'))}
            </div>
          </div>
          <div style="display:flex;gap:8px;flex-shrink:0;">
            <button class="btn btn-primary scanner-history-advisor" type="button" data-task-id="${escapeHtml(taskId)}" data-root-path="${escapeHtml(rootPath)}">${escapeHtml(getLang() === 'en' ? 'Advisor' : '顾问')}</button>
            <button class="btn btn-ghost scanner-history-load" type="button" data-task-id="${escapeHtml(taskId)}">${escapeHtml(t('scanner.history_load'))}</button>
            <button class="btn btn-secondary scanner-history-delete" type="button" data-task-id="${escapeHtml(taskId)}" ${running ? 'disabled' : ''}>${escapeHtml(t('scanner.history_delete'))}</button>
          </div>
        </div>
      </div>
    `;
  }).join('');
}

function buildLogRows() {
  const logEntries = Array.isArray(scannerState?.logEntries) ? scannerState.logEntries : [];
  if (!logEntries.length) {
    return `
      <div class="empty-state">
        <div class="empty-state-text">${escapeHtml(t('scanner.activity_log'))}</div>
        <div class="empty-state-hint">${escapeHtml(t('scanner.log_preview_hint'))}</div>
      </div>
    `;
  }

  return logEntries.slice(-80).map((entry) => `
    <div style="display:flex;gap:10px;padding:10px 0;border-bottom:1px solid rgba(255,255,255,0.06);">
      <div class="form-hint" style="flex:0 0 72px;">${escapeHtml(entry.time || '')}</div>
      <div style="min-width:0;word-break:break-word;">${escapeHtml(entry.text || entry.summary || '')}</div>
    </div>
  `).join('');
}

function renderPage() {
  if (!pageContainer) return;
  const form = getFormState();
  const snapshot = getActiveSnapshot();
  const running = !!scannerState?.activeTaskId;
  const scannedCount = Number(snapshot?.scannedCount || 0);
  const totalEntries = Number(snapshot?.totalEntries || 0);
  const processedEntries = Number(snapshot?.processedEntries || 0);
  const deniedCount = Number(snapshot?.permissionDeniedCount || 0);
  const maxScannedDepth = Number(snapshot?.maxScannedDepth || 0);

  pageContainer.innerHTML = `
    <section class="page-header">
      <div>
        <h1>${escapeHtml(getLang() === 'en' ? 'Inventory' : '盘点')}</h1>
        <p>${escapeHtml(getLang() === 'en'
          ? 'Keep local inventory, scan history, and folder selection here, then hand the context to the advisor for chat-driven suggestions.'
          : '这里保留本地盘点、扫描历史和目录选择，完成后再把目录上下文交给顾问页进行对话式整理与清理建议。')}</p>
      </div>
    </section>

    <section class="card" style="padding:20px;margin-bottom:20px;">
      <div class="card-title" style="margin-bottom:16px;">${escapeHtml(t('settings.scan_params'))}</div>
      <div class="form-group">
        <label class="form-label" for="scan-target-path">${escapeHtml(t('settings.scan_path'))}</label>
        <div style="display:flex;gap:8px;">
          <input id="scan-target-path" class="form-input" type="text" value="${escapeHtml(form.targetPath)}" placeholder="${escapeHtml(t('settings.browse_hint'))}" />
          <button id="scan-browse-btn" class="btn btn-secondary" type="button">${escapeHtml(t('settings.browse'))}</button>
        </div>
      </div>

      <div style="display:grid;grid-template-columns:repeat(auto-fit,minmax(220px,1fr));gap:16px;">
        <div class="form-group">
          <label class="form-label" for="scan-max-depth">${escapeHtml(t('settings.max_depth'))}</label>
          <input id="scan-max-depth" class="form-input" type="number" min="1" max="16" value="${escapeHtml(form.maxDepth)}" ${form.maxDepthUnlimited ? 'disabled' : ''} />
        </div>
        <div class="form-group" style="justify-content:flex-end;">
          <label class="form-label" for="scan-max-depth-unlimited">${escapeHtml(t('settings.max_depth_unlimited'))}</label>
          <input id="scan-max-depth-unlimited" type="checkbox" ${form.maxDepthUnlimited ? 'checked' : ''} />
        </div>
      </div>

      <div class="form-group">
        <label class="form-label" for="scan-ignore-paths">${escapeHtml('忽略路径')}</label>
        <textarea id="scan-ignore-paths" class="form-input" rows="6" placeholder="每行一个路径">${escapeHtml(form.ignoredPathsText)}</textarea>
      </div>

      <div style="display:flex;gap:10px;flex-wrap:wrap;">
        <button id="scan-start-btn" class="btn btn-primary" type="button" ${running ? 'disabled' : ''}>${escapeHtml(t('scanner.start'))}</button>
        <button id="scan-stop-btn" class="btn btn-danger" type="button" ${running ? '' : 'disabled'}>${escapeHtml(t('scanner.stop'))}</button>
        <button id="scan-open-advisor-btn" class="btn btn-secondary" type="button">${escapeHtml(getLang() === 'en' ? 'Open Advisor' : '进入顾问')}</button>
        <button id="scan-history-refresh-btn" class="btn btn-ghost" type="button">${escapeHtml(t('scanner.history_refresh'))}</button>
      </div>
    </section>

    <section class="stats-grid" style="margin-bottom:20px;">
      <div class="card" style="padding:16px;">
        <div class="form-hint">${escapeHtml('状态')}</div>
        <div class="card-title">${escapeHtml(getStatusText(snapshot))}</div>
      </div>
      <div class="card" style="padding:16px;">
        <div class="form-hint">${escapeHtml('已扫描')}</div>
        <div class="card-title">${scannedCount}</div>
      </div>
      <div class="card" style="padding:16px;">
        <div class="form-hint">${escapeHtml('总条目')}</div>
        <div class="card-title">${totalEntries}</div>
      </div>
      <div class="card" style="padding:16px;">
        <div class="form-hint">${escapeHtml('已处理')}</div>
        <div class="card-title">${processedEntries}</div>
      </div>
      <div class="card" style="padding:16px;">
        <div class="form-hint">${escapeHtml('最大深度')}</div>
        <div class="card-title">${maxScannedDepth}</div>
      </div>
      <div class="card" style="padding:16px;">
        <div class="form-hint">${escapeHtml('权限不足')}</div>
        <div class="card-title">${deniedCount}</div>
      </div>
    </section>

    <section class="card" style="padding:20px;margin-bottom:20px;">
      <div class="card-title" style="margin-bottom:12px;">${escapeHtml(t('scanner.activity_log'))}</div>
      <div>${buildLogRows()}</div>
    </section>

    <section class="card" style="padding:20px;">
      <div class="card-title" style="margin-bottom:12px;">${escapeHtml(t('scanner.history_title'))}</div>
      <div id="scanner-history-list">${buildHistoryRows()}</div>
    </section>
  `;

  bindEvents();
}

function collectForm() {
  return {
    targetPath: String(document.getElementById('scan-target-path')?.value || '').trim(),
    maxDepth: clampDepth(document.getElementById('scan-max-depth')?.value ?? DEFAULT_MAX_DEPTH),
    maxDepthUnlimited: !!document.getElementById('scan-max-depth-unlimited')?.checked,
    ignoredPathsText: String(document.getElementById('scan-ignore-paths')?.value || ''),
  };
}

async function persistScannerSettings(form) {
  const ignoredPaths = normalizeIgnoredPaths(form.ignoredPathsText);
  const payload = {
    scanPath: form.targetPath,
    maxDepth: form.maxDepth,
    maxDepthUnlimited: form.maxDepthUnlimited,
    scanIgnorePaths: ignoredPaths,
    lastScanTime: new Date().toISOString(),
  };
  await saveSettings(payload);
  scannerSettings = {
    ...(scannerSettings || {}),
    ...payload,
  };
}

async function refreshHistory() {
  loadingHistory = true;
  renderPage();
  try {
    scannerHistory = await listScanHistory(HISTORY_LIMIT);
  } catch (err) {
    showToast(`${t('scanner.history_load_failed')}${err?.message || err}`, 'error');
    scannerHistory = [];
  } finally {
    loadingHistory = false;
    renderPage();
  }
}

async function handleBrowse() {
  try {
    const picked = await browseFolder();
    if (picked?.cancelled || !picked?.path) return;
    const current = collectForm();
    current.targetPath = picked.path;
    saveDraft(current);
    renderPage();
  } catch (err) {
    showToast(`${t('settings.toast_browse_failed')}${err?.message || err}`, 'error');
  }
}

async function handleStart() {
  const form = collectForm();
  if (!form.targetPath) {
    showToast(t('scanner.path_not_configured'), 'error');
    return;
  }
  saveDraft(form);
  try {
    await persistScannerSettings(form);
    await scanTaskController.startTask({
      targetPath: form.targetPath,
      maxDepth: form.maxDepthUnlimited ? null : form.maxDepth,
    });
    renderPage();
    refreshHistory();
  } catch (err) {
    showToast(`${t('scanner.toast_start_failed')}${err?.message || err}`, 'error');
  }
}

async function handleStop() {
  try {
    await scanTaskController.stopTask();
  } catch (err) {
    showToast(`${t('scanner.toast_stop_failed')}${err?.message || err}`, 'error');
  }
}

async function handleHistoryLoad(taskId) {
  try {
    const snapshot = await getScanResult(taskId);
    if (snapshot?.targetPath) {
      saveDraft({
        ...getFormState(),
        targetPath: String(snapshot.targetPath || ''),
        maxDepth: clampDepth(snapshot?.configuredMaxDepth ?? DEFAULT_MAX_DEPTH),
        maxDepthUnlimited: snapshot?.configuredMaxDepth == null,
      });
    }
    await scanTaskController.restoreTaskById(taskId);
    renderPage();
  } catch (err) {
    showToast(`${t('scanner.history_load_failed')}${err?.message || err}`, 'error');
  }
}

async function handleHistoryDelete(taskId) {
  try {
    await deleteScanHistory(taskId);
    showToast(t('scanner.history_deleted'), 'success');
    refreshHistory();
  } catch (err) {
    showToast(`${t('scanner.history_delete_failed')}${err?.message || err}`, 'error');
  }
}

function bindEvents() {
  document.getElementById('scan-browse-btn')?.addEventListener('click', handleBrowse);
  document.getElementById('scan-start-btn')?.addEventListener('click', handleStart);
  document.getElementById('scan-stop-btn')?.addEventListener('click', handleStop);
  document.getElementById('scan-open-advisor-btn')?.addEventListener('click', () => {
    const form = collectForm();
    const snapshot = getActiveSnapshot();
    openAdvisor(snapshot?.targetPath || form.targetPath, snapshot?.id || scannerState?.latestTaskId || null);
  });
  document.getElementById('scan-history-refresh-btn')?.addEventListener('click', refreshHistory);
  document.getElementById('scan-max-depth-unlimited')?.addEventListener('change', (event) => {
    const form = collectForm();
    form.maxDepthUnlimited = !!event.target?.checked;
    saveDraft(form);
    renderPage();
  });

  ['scan-target-path', 'scan-max-depth', 'scan-ignore-paths'].forEach((id) => {
    document.getElementById(id)?.addEventListener('input', () => {
      saveDraft(collectForm());
    });
  });

  document.querySelectorAll('.scanner-history-load').forEach((button) => {
    button.addEventListener('click', () => handleHistoryLoad(button.dataset.taskId));
  });
  document.querySelectorAll('.scanner-history-advisor').forEach((button) => {
    button.addEventListener('click', () => openAdvisor(button.dataset.rootPath, button.dataset.taskId));
  });
  document.querySelectorAll('.scanner-history-delete').forEach((button) => {
    button.addEventListener('click', () => handleHistoryDelete(button.dataset.taskId));
  });
}

export async function renderScanner(container) {
  pageContainer = container;
  if (scannerControllerUnsubscribe) {
    scannerControllerUnsubscribe();
  }
  scannerControllerUnsubscribe = scanTaskController.subscribe((event) => {
    scannerState = event.state;
    renderPage();
  });

  try {
    scannerSettings = await getSettings();
  } catch {
    scannerSettings = {};
  }

  renderPage();
  refreshHistory();
  scanTaskController.restoreAnyActiveTask().catch(() => {});
}
