import {
  browseFolder,
  deleteScanHistory,
  getScanResult,
  getSettings,
  listScanHistory,
  saveSettings,
} from '../utils/api.js';
import { getLang, t } from '../utils/i18n.js';
import { formatSize } from '../utils/storage.js';
import { scanTaskController } from '../utils/scan-task-controller.js';
import { showToast } from '../utils/toast.js';

const SCANNER_FORM_DRAFT_KEY = 'wipeout.scanner.global.form.v3';
const ADVISOR_HANDOFF_KEY = 'wipeout.advisor.global.handoff.v1';
const ORGANIZER_ROOT_PATH_KEY = 'wipeout.organizer.global.root_path.v2';
const HISTORY_LIMIT = 12;
const DEFAULT_MAX_DEPTH = 5;

let pageContainer = null;
let scannerControllerUnsubscribe = null;
let scannerState = scanTaskController.getState();
let scannerHistory = [];
let scannerSettings = null;
let loadingHistory = false;
let hasLoadedHistory = false;

function text(zh, en) {
  return getLang() === 'en' ? en : zh;
}

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
  const textValue = String(value || '');
  const seen = new Set();
  return textValue
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

function openOrganizer(rootPath) {
  const normalizedRoot = String(rootPath || '').trim();
  if (!normalizedRoot) {
    showToast(getLang() === 'en' ? 'Select a folder first' : '请先选择或加载一个归类目录', 'error');
    return;
  }
  try {
    localStorage.setItem(ORGANIZER_ROOT_PATH_KEY, JSON.stringify(normalizedRoot));
  } catch {
    // ignore storage failures
  }
  window.location.hash = '#/organizer';
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

function renderMetricCard(label, value, detail = '') {
  return `
    <article class="card scanner-metric-card">
      <div class="scanner-metric-label">${escapeHtml(label)}</div>
      <div class="scanner-metric-value">${escapeHtml(value)}</div>
      ${detail ? `<div class="scanner-metric-detail">${escapeHtml(detail)}</div>` : ''}
    </article>
  `;
}

function buildHistoryRows() {
  if (loadingHistory) {
    return `<div class="form-hint">${escapeHtml(t('scanner.history_loading'))}</div>`;
  }

  if (!hasLoadedHistory) {
    return `
      <div class="empty-state scanner-empty-state">
        <div class="empty-state-text">${escapeHtml(text('历史记录尚未加载', 'History has not been loaded yet'))}</div>
        <div class="empty-state-hint">${escapeHtml(text('启动时不会主动读取后台数据库。需要查看历史时，点击“刷新历史”再加载。', 'The app no longer reads the background database on startup. Click "Refresh History" when you actually need it.'))}</div>
      </div>
    `;
  }

  if (!scannerHistory.length) {
    return `
      <div class="empty-state scanner-empty-state">
        <div class="empty-state-text">${escapeHtml(t('scanner.history_empty'))}</div>
        <div class="empty-state-hint">${escapeHtml(text('完成一次盘点后，这里会沉淀可直接接入归类或顾问的历史记录。', 'Once you finish a scan, reusable history records will appear here for organizer or advisor handoff.'))}</div>
      </div>
    `;
  }

  return scannerHistory.map((task) => {
    const taskId = String(task?.taskId || task?.id || '').trim();
    const running = ['idle', 'scanning', 'analyzing'].includes(String(task?.status || '').trim());
    const rootPath = String(task?.targetPath || task?.rootPath || '').trim();
    return `
      <article class="scanner-history-item">
        <div class="scanner-history-main">
          <div class="scanner-history-title">${escapeHtml(rootPath || '-')}</div>
          <div class="scanner-history-meta">
            <span>${escapeHtml(getStatusText(task))}</span>
            <span>${escapeHtml(formatSize(task?.totalCleanable || 0))}</span>
            <span>${escapeHtml(new Date(task?.updatedAt || task?.createdAt || Date.now()).toLocaleString(getLang() === 'en' ? 'en-US' : 'zh-CN'))}</span>
          </div>
        </div>
        <div class="scanner-history-actions">
          <button class="btn btn-secondary scanner-history-organizer" type="button" data-root-path="${escapeHtml(rootPath)}">${escapeHtml(getLang() === 'en' ? 'Organizer' : '归类')}</button>
          <button class="btn btn-primary scanner-history-advisor" type="button" data-task-id="${escapeHtml(taskId)}" data-root-path="${escapeHtml(rootPath)}">${escapeHtml(getLang() === 'en' ? 'Advisor' : '顾问')}</button>
          <button class="btn btn-ghost scanner-history-load" type="button" data-task-id="${escapeHtml(taskId)}">${escapeHtml(t('scanner.history_load'))}</button>
          <button class="btn btn-secondary scanner-history-delete" type="button" data-task-id="${escapeHtml(taskId)}" ${running ? 'disabled' : ''}>${escapeHtml(t('scanner.history_delete'))}</button>
        </div>
      </article>
    `;
  }).join('');
}

function buildLogRows() {
  const logEntries = Array.isArray(scannerState?.logEntries) ? scannerState.logEntries : [];
  if (!logEntries.length) {
    return `
      <div class="empty-state scanner-empty-state">
        <div class="empty-state-text">${escapeHtml(t('scanner.activity_log'))}</div>
        <div class="empty-state-hint">${escapeHtml(t('scanner.log_preview_hint'))}</div>
      </div>
    `;
  }

  return logEntries.slice(-80).map((entry) => `
    <div class="scanner-log-entry">
      <div class="scanner-log-time">${escapeHtml(entry.time || '')}</div>
      <div class="scanner-log-text">${escapeHtml(entry.text || entry.summary || '')}</div>
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
  const statusText = getStatusText(snapshot);

  pageContainer.innerHTML = `
    <section class="workflow-shell scanner-workspace">
      <section class="card workflow-hero-panel scanner-hero-panel">
        <div class="workflow-hero-row">
          <div class="workflow-hero-copy">
            <div class="workflow-kicker">${escapeHtml(text('盘点工作流', 'Inventory Workflow'))}</div>
            <h1>${escapeHtml(text('先盘点目录，再把上下文交给顾问继续处理。', 'Inventory folders first, then hand the context to the advisor.'))}</h1>
            <p>${escapeHtml(text('这里负责目录选择、扫描参数、活动日志和历史记录。完成后可一键把当前目录和扫描任务带入顾问页。', 'This page handles folder selection, scan parameters, activity logs, and history. Once ready, you can hand the current folder and scan task to the advisor in one click.'))}</p>
          </div>
          <div class="workflow-hero-actions scanner-hero-actions">
            <span class="scanner-status-pill">${escapeHtml(statusText)}</span>
            <div class="form-hint">${escapeHtml(text('当前页聚焦盘点输入与状态，建议整理动作放到顾问页继续。', 'This page focuses on inventory inputs and status. Continue organizing actions in the advisor page.'))}</div>
          </div>
        </div>

        <div class="scanner-control-grid">
          <div class="scanner-config-panel">
            <div class="form-group">
              <label class="form-label" for="scan-target-path">${escapeHtml(t('settings.scan_path'))}</label>
              <div class="scanner-path-row">
                <input id="scan-target-path" class="form-input" type="text" value="${escapeHtml(form.targetPath)}" placeholder="${escapeHtml(t('settings.browse_hint'))}" />
                <button id="scan-browse-btn" class="btn btn-secondary" type="button">${escapeHtml(t('settings.browse'))}</button>
              </div>
            </div>

            <div class="scanner-settings-grid">
              <div class="form-group">
                <label class="form-label" for="scan-max-depth">${escapeHtml(t('settings.max_depth'))}</label>
                <input id="scan-max-depth" class="form-input" type="number" min="1" max="16" value="${escapeHtml(form.maxDepth)}" ${form.maxDepthUnlimited ? 'disabled' : ''} />
                <div class="form-hint">${escapeHtml(text('建议先从较浅层级开始，再按需要继续深入。', 'Start with a shallower depth first, then deepen only when needed.'))}</div>
              </div>
              <label class="scanner-toggle-field" for="scan-max-depth-unlimited">
                <input id="scan-max-depth-unlimited" type="checkbox" ${form.maxDepthUnlimited ? 'checked' : ''} />
                <span>
                  <strong>${escapeHtml(t('settings.max_depth_unlimited'))}</strong>
                  <span class="form-hint">${escapeHtml(t('settings.max_depth_unlimited_hint'))}</span>
                </span>
              </label>
            </div>

            <div class="form-group">
              <label class="form-label" for="scan-ignore-paths">${escapeHtml(text('忽略路径', 'Ignored Paths'))}</label>
              <textarea id="scan-ignore-paths" class="form-input scanner-textarea" rows="6" placeholder="${escapeHtml(text('每行一个路径', 'One path per line'))}">${escapeHtml(form.ignoredPathsText)}</textarea>
            </div>
          </div>

          <div class="scanner-action-panel">
            <div class="scanner-action-summary">
              <div class="scanner-summary-row">
                <span>${escapeHtml(text('当前目录', 'Current Directory'))}</span>
                <strong>${escapeHtml(form.targetPath || '-')}</strong>
              </div>
              <div class="scanner-summary-row">
                <span>${escapeHtml(text('最新状态', 'Latest Status'))}</span>
                <strong>${escapeHtml(statusText)}</strong>
              </div>
              <div class="scanner-summary-row">
                <span>${escapeHtml(text('归类接力', 'Organizer Handoff'))}</span>
                <strong>${escapeHtml(text('扫描完成后可直接进入摘要与归类流程', 'Jump directly into summary extraction and organizing after scanning'))}</strong>
              </div>
              <div class="scanner-summary-row">
                <span>${escapeHtml(text('顾问接力', 'Advisor Handoff'))}</span>
                <strong>${escapeHtml(text('支持当前扫描或历史扫描直接接入', 'Current or historical scans can jump directly into advisor'))}</strong>
              </div>
            </div>
            <div class="scanner-action-buttons">
              <button id="scan-start-btn" class="btn btn-primary" type="button" ${running ? 'disabled' : ''}>${escapeHtml(t('scanner.start'))}</button>
              <button id="scan-stop-btn" class="btn btn-danger" type="button" ${running ? '' : 'disabled'}>${escapeHtml(t('scanner.stop'))}</button>
              <button id="scan-open-organizer-btn" class="btn btn-secondary" type="button">${escapeHtml(getLang() === 'en' ? 'Open Organizer' : '进入归类')}</button>
              <button id="scan-open-advisor-btn" class="btn btn-secondary" type="button">${escapeHtml(getLang() === 'en' ? 'Open Advisor' : '进入顾问')}</button>
              <button id="scan-history-refresh-btn" class="btn btn-ghost" type="button">${escapeHtml(t('scanner.history_refresh'))}</button>
            </div>
          </div>
        </div>
      </section>

      <section class="scanner-metric-grid">
        ${renderMetricCard(text('状态', 'Status'), statusText)}
        ${renderMetricCard(text('已扫描', 'Scanned'), scannedCount)}
        ${renderMetricCard(text('总条目', 'Total Entries'), totalEntries)}
        ${renderMetricCard(text('已处理', 'Processed'), processedEntries)}
        ${renderMetricCard(text('最大深度', 'Max Depth'), maxScannedDepth)}
        ${renderMetricCard(text('权限不足', 'Permission Denied'), deniedCount)}
      </section>

      <section class="scanner-detail-grid">
        <section class="card scanner-log-panel">
          <div class="scanner-section-head">
            <div>
              <div class="card-title">${escapeHtml(t('scanner.activity_log'))}</div>
              <div class="form-hint">${escapeHtml(text('保留最近的运行轨迹，方便确认扫描是否还在推进。', 'Keeps the latest execution trail so you can confirm scan progress quickly.'))}</div>
            </div>
          </div>
          <div class="scanner-log-list">${buildLogRows()}</div>
        </section>

        <section class="card scanner-history-panel">
          <div class="scanner-section-head">
            <div>
              <div class="card-title">${escapeHtml(t('scanner.history_title'))}</div>
              <div class="form-hint">${escapeHtml(text('历史扫描可以直接载入继续查看，也可以把目录接给归类，或把目录和任务接给顾问。', 'Load a previous scan to continue reviewing it, hand the directory to organizer, or send the directory and task into advisor.'))}</div>
            </div>
          </div>
          <div id="scanner-history-list" class="scanner-history-list">${buildHistoryRows()}</div>
        </section>
      </section>
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
  hasLoadedHistory = true;
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
  document.getElementById('scan-open-organizer-btn')?.addEventListener('click', () => {
    const form = collectForm();
    const snapshot = getActiveSnapshot();
    openOrganizer(snapshot?.targetPath || form.targetPath);
  });
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
  document.querySelectorAll('.scanner-history-organizer').forEach((button) => {
    button.addEventListener('click', () => openOrganizer(button.dataset.rootPath));
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

  renderPage();

  try {
    scannerSettings = await getSettings();
  } catch {
    scannerSettings = {};
  }

  renderPage();
  scanTaskController.restoreAnyActiveTask().catch(() => {});
}
