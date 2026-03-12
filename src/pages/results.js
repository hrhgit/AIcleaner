import * as storage from '../utils/storage.js';
import { formatSize } from '../utils/storage.js';
import {
  cleanFiles,
  deleteScanHistory,
  getSettings,
  getScanResult,
  listScanHistory,
  openFileLocation,
  requestElevation,
  saveSettings,
  startScan,
} from '../utils/api.js';
import { handleElevationTransition } from '../utils/elevation.js';
import { showToast } from '../main.js';
import { getLang, t } from '../utils/i18n.js';

let sortField = 'size';
let sortDir = 'desc';
let currentTaskId = null;
let currentSnapshot = null;
let currentData = [];
let historyTasks = [];
let ignoredPaths = [];
let renderVersion = 0;
let continueModalEscapeBound = false;
const CONTINUE_SCAN_DRAFT_KEY = 'wipeout.results.global.continue.v1';
const CONTINUE_SCAN_DRAFT_VERSION = 1;
const CONTINUE_DEPTH_MAX = 16;
const CONTINUE_TARGET_MIN_GB = 0.1;
const CONTINUE_TARGET_MAX_GB = 1000;

function getCachedLastSnapshot() {
  return storage.get('lastScan', null);
}

function getPreferredTaskId() {
  return storage.get('lastScanTaskId', null) || getCachedLastSnapshot()?.id || currentTaskId || null;
}

function escapeHtml(str) {
  const div = document.createElement('div');
  div.textContent = String(str ?? '');
  return div.innerHTML;
}

function clampNumber(value, min, max, fallback) {
  const n = Number(value);
  if (!Number.isFinite(n)) return fallback;
  if (n < min) return min;
  if (n > max) return max;
  return n;
}

function riskBadge(risk) {
  return risk === 'low' ? 'success' : risk === 'high' ? 'danger' : 'warning';
}

function riskLabel(risk) {
  return risk === 'low' ? t('results.risk_safe') : risk === 'high' ? t('results.risk_danger') : t('results.risk_warning');
}

function getScanStatusLabel(status) {
  const statusMap = {
    scanning: t('scanner.scanning'),
    analyzing: t('scanner.analyzing'),
    idle: t('scanner.not_set'),
    done: t('scanner.done'),
    stopped: t('scanner.stopped'),
    error: t('toast.error'),
  };
  return statusMap[status] || status || t('scanner.not_set');
}

function formatHistoryTime(value) {
  if (!value) return '-';
  try {
    return new Date(value).toLocaleString('zh-CN');
  } catch {
    return String(value);
  }
}

function normalizePathKey(value) {
  let normalized = String(value || '').trim().replace(/\//g, '\\').toLowerCase();
  while (normalized.length > 3 && normalized.endsWith('\\')) {
    normalized = normalized.slice(0, -1);
  }
  return normalized;
}

function isSameOrDescendantPath(path, parent) {
  return path === parent || path.startsWith(`${parent}\\`);
}

function normalizeIgnoredPaths(paths) {
  const seen = new Set();
  const normalized = [];
  for (const raw of Array.isArray(paths) ? paths : []) {
    const path = String(raw || '').trim();
    const key = normalizePathKey(path);
    if (!key || seen.has(key)) continue;
    seen.add(key);
    normalized.push(path);
  }
  return normalized;
}

function mergeIgnoredPaths(existingPaths, nextPaths) {
  return normalizeIgnoredPaths([...(existingPaths || []), ...(nextPaths || [])]);
}

function isIgnoredPath(path) {
  const pathKey = normalizePathKey(path);
  if (!pathKey) return false;
  return ignoredPaths.some((entry) => isSameOrDescendantPath(pathKey, normalizePathKey(entry)));
}

function getVisibleData() {
  return currentData.filter((item) => !isIgnoredPath(item.path));
}

function getFilteredData() {
  const activeFilter = document.querySelector('.filter-btn.active')?.dataset.filter || 'all';
  let data = [...getVisibleData()];
  if (activeFilter !== 'all') {
    data = data.filter((item) => item.risk === activeFilter);
  }
  return data;
}

function updateSummary(snapshot = currentSnapshot) {
  const countEl = document.getElementById('res-count');
  const sizeEl = document.getElementById('res-size');
  const lowEl = document.getElementById('res-low');
  const highEl = document.getElementById('res-high');
  const summaryEl = document.getElementById('results-scan-summary');
  const rootEl = document.getElementById('results-scan-root');
  const visibleData = getVisibleData();

  const totalSize = visibleData.reduce((sum, item) => sum + (item.size || 0), 0);
  if (countEl) countEl.textContent = String(visibleData.length);
  if (sizeEl) sizeEl.textContent = formatSize(totalSize);
  if (lowEl) lowEl.textContent = String(visibleData.filter((item) => item.risk === 'low').length);
  if (highEl) highEl.textContent = String(visibleData.filter((item) => item.risk !== 'low').length);
  if (summaryEl) {
    if (snapshot?.id) {
      const depthLabel = snapshot.configuredMaxDepth == null
        ? t('settings.max_depth_unlimited')
        : `${getContinueDepthBase(snapshot)} ${t('settings.depth_unit')}`;
      const targetSizeGb = Number(snapshot?.targetSize || 0) / 1024 / 1024 / 1024;
      const targetLabel = targetSizeGb > 0 ? `${targetSizeGb.toFixed(1)} GB` : '-';
      summaryEl.textContent = [
        `${t('settings.max_depth')}: ${depthLabel}`,
        `${t('settings.target_size')}: ${targetLabel}`,
        `${t('results.space_freed')}: ${formatSize(snapshot?.totalCleanable || 0)}`,
      ].join(' · ');
    } else {
      summaryEl.textContent = t('results.scan_not_started');
    }
  }
  if (rootEl) {
    rootEl.textContent = snapshot?.targetPath ? `${t('settings.scan_path')}: ${snapshot.targetPath}` : '';
  }
}

function getContinueDepthBase(snapshot = currentSnapshot) {
  const configuredDepth = Number(snapshot?.configuredMaxDepth);
  if (Number.isFinite(configuredDepth) && configuredDepth > 0) {
    return configuredDepth;
  }
  const scannedDepth = Number(snapshot?.maxScannedDepth);
  if (Number.isFinite(scannedDepth) && scannedDepth > 0) {
    return scannedDepth;
  }
  return 0;
}

function getContinueDepthMin(snapshot = currentSnapshot) {
  return Math.min(CONTINUE_DEPTH_MAX, Math.max(1, getContinueDepthBase(snapshot) + 1));
}

function getContinueBaseDraft(snapshot = currentSnapshot) {
  const currentTargetGb = Number(snapshot?.targetSize || 0) / 1024 / 1024 / 1024;
  const currentFoundGb = Number(snapshot?.totalCleanable || 0) / 1024 / 1024 / 1024;
  const suggestedTarget = clampNumber(
    Math.max(currentFoundGb, currentTargetGb || currentFoundGb || 1),
    CONTINUE_TARGET_MIN_GB,
    CONTINUE_TARGET_MAX_GB,
    1
  );

  return {
    depth: getContinueDepthMin(snapshot),
    targetSizeGb: Number(suggestedTarget.toFixed(1)),
    unlimited: false,
  };
}

function normalizeContinueDraft(raw = {}, snapshot = currentSnapshot) {
  const baseDraft = getContinueBaseDraft(snapshot);
  return {
    depth: Math.floor(clampNumber(raw?.depth, getContinueDepthMin(snapshot), CONTINUE_DEPTH_MAX, baseDraft.depth)),
    targetSizeGb: Number(clampNumber(
      raw?.targetSizeGb,
      CONTINUE_TARGET_MIN_GB,
      CONTINUE_TARGET_MAX_GB,
      baseDraft.targetSizeGb
    ).toFixed(1)),
    unlimited: !!raw?.unlimited,
  };
}

function readContinueDraft(snapshot = currentSnapshot) {
  const raw = storage.get(CONTINUE_SCAN_DRAFT_KEY, null);
  if (!raw || typeof raw !== 'object' || Number(raw.version) !== CONTINUE_SCAN_DRAFT_VERSION) {
    return getContinueBaseDraft(snapshot);
  }
  return normalizeContinueDraft(raw.data, snapshot);
}

function writeContinueDraft(draft, snapshot = currentSnapshot) {
  storage.set(CONTINUE_SCAN_DRAFT_KEY, {
    version: CONTINUE_SCAN_DRAFT_VERSION,
    updatedAt: Date.now(),
    data: normalizeContinueDraft(draft, snapshot),
  });
}

function readContinueDraftFromDom(snapshot = currentSnapshot) {
  const depthRange = document.getElementById('continue-depth-range');
  const depthInput = document.getElementById('continue-depth-input');
  const sizeRange = document.getElementById('continue-target-size-range');
  const sizeInput = document.getElementById('continue-target-size-input');
  const unlimitedToggle = document.getElementById('continue-unlimited-toggle');
  const baseDraft = getContinueBaseDraft(snapshot);

  return normalizeContinueDraft({
    depth: depthInput?.value ?? depthRange?.value ?? baseDraft.depth,
    targetSizeGb: sizeInput?.value ?? sizeRange?.value ?? baseDraft.targetSizeGb,
    unlimited: !!unlimitedToggle?.checked,
  }, snapshot);
}

function syncContinueDraftInputs(draft, snapshot = currentSnapshot) {
  const depthMin = getContinueDepthMin(snapshot);
  const depthRange = document.getElementById('continue-depth-range');
  const depthInput = document.getElementById('continue-depth-input');
  const sizeRange = document.getElementById('continue-target-size-range');
  const sizeInput = document.getElementById('continue-target-size-input');

  if (depthRange) {
    depthRange.min = String(depthMin);
    depthRange.max = String(CONTINUE_DEPTH_MAX);
    depthRange.value = String(draft.depth);
  }
  if (depthInput) {
    depthInput.min = String(depthMin);
    depthInput.max = String(CONTINUE_DEPTH_MAX);
    depthInput.value = String(draft.depth);
  }
  if (sizeRange) {
    sizeRange.min = String(CONTINUE_TARGET_MIN_GB);
    sizeRange.max = String(CONTINUE_TARGET_MAX_GB);
    sizeRange.value = String(draft.targetSizeGb);
  }
  if (sizeInput) {
    sizeInput.min = String(CONTINUE_TARGET_MIN_GB);
    sizeInput.max = String(CONTINUE_TARGET_MAX_GB);
    sizeInput.value = draft.targetSizeGb.toFixed(1);
  }
}

function updateContinueModalState(snapshot = currentSnapshot) {
  const openBtn = document.getElementById('continue-scan-btn');
  const summaryEl = document.getElementById('continue-scan-summary');
  const hintEl = document.getElementById('continue-scan-hint');
  const rootEl = document.getElementById('continue-scan-root');
  const submitBtn = document.getElementById('continue-scan-submit-btn');
  const unlimitedToggle = document.getElementById('continue-unlimited-toggle');
  const depthRange = document.getElementById('continue-depth-range');
  const depthInput = document.getElementById('continue-depth-input');
  const hasSnapshot = !!snapshot?.id;
  if (openBtn) {
    openBtn.disabled = !hasSnapshot;
  }

  if (!summaryEl || !hintEl || !rootEl || !submitBtn || !unlimitedToggle || !depthRange || !depthInput) return;

  if (!hasSnapshot) {
    summaryEl.textContent = '';
    hintEl.textContent = '';
    rootEl.textContent = '';
    submitBtn.disabled = true;
    return;
  }

  const taskUsesUnlimitedDepth = snapshot.configuredMaxDepth == null;
  const draft = readContinueDraft(snapshot);
  syncContinueDraftInputs(draft, snapshot);
  writeContinueDraft(draft, snapshot);

  rootEl.textContent = snapshot?.targetPath || '';
  summaryEl.textContent = t('results.continue_summary', {
    depth: getContinueDepthBase(snapshot),
    size: formatSize(snapshot?.totalCleanable || 0),
  });

  unlimitedToggle.checked = draft.unlimited;
  unlimitedToggle.disabled = taskUsesUnlimitedDepth;
  depthRange.disabled = draft.unlimited || taskUsesUnlimitedDepth;
  depthInput.disabled = draft.unlimited || taskUsesUnlimitedDepth;
  submitBtn.disabled = taskUsesUnlimitedDepth;
  hintEl.textContent = taskUsesUnlimitedDepth
    ? t('results.continue_unlimited_hint')
    : (draft.unlimited ? t('settings.max_depth_unlimited_hint') : t('results.continue_hint'));
}

function openContinueScanModal() {
  const modal = document.getElementById('continue-scan-modal');
  if (!modal || !currentSnapshot?.id) return;
  updateContinueModalState(currentSnapshot);
  modal.classList.add('open');
  modal.setAttribute('aria-hidden', 'false');
}

function closeContinueScanModal() {
  const modal = document.getElementById('continue-scan-modal');
  if (!modal) return;
  modal.classList.remove('open');
  modal.setAttribute('aria-hidden', 'true');
}

function renderAiInsight(item) {
  const purpose = String(item?.purpose || '').trim();
  const reason = String(item?.reason || '').trim();
  const primaryText = purpose || reason || '-';
  const secondaryText = purpose && reason && purpose !== reason ? reason : '';

  return `
    <div class="results-ai-cell">
      <div class="results-ai-primary">${escapeHtml(primaryText)}</div>
      ${secondaryText ? `<div class="results-ai-secondary">${escapeHtml(secondaryText)}</div>` : ''}
    </div>
  `;
}

function updateBatchDeleteBtn() {
  const selectedCount = document.querySelectorAll('.row-cb:checked').length;
  const btn = document.getElementById('batch-delete-btn');
  const countSpan = document.getElementById('selected-count');
  const selectAllCb = document.getElementById('select-all-cb');
  const totalVisible = document.querySelectorAll('.row-cb').length;

  if (btn && countSpan) {
    countSpan.textContent = String(selectedCount);
    btn.style.display = selectedCount > 0 ? '' : 'none';
  }

  if (selectAllCb) {
    selectAllCb.checked = selectedCount > 0 && selectedCount === totalVisible;
    selectAllCb.indeterminate = selectedCount > 0 && selectedCount < totalVisible;
  }
}

function renderHistoryList() {
  const listEl = document.getElementById('results-history-list');
  if (!listEl) return;

  if (!historyTasks.length) {
    listEl.innerHTML = `<div class="form-hint">${t('scanner.history_empty')}</div>`;
    return;
  }

  listEl.innerHTML = historyTasks.map((task) => {
    const selected = currentTaskId === task.taskId;
    const running = ['idle', 'scanning', 'analyzing'].includes(task.status);
    return `
      <div style="padding:10px 0; border-bottom:1px solid rgba(255,255,255,0.06); ${selected ? 'background: rgba(255,255,255,0.03); border-radius: 8px; padding-left: 10px; padding-right: 10px;' : ''}">
        <div style="display:flex; align-items:flex-start; justify-content:space-between; gap:12px;">
          <div style="min-width:0; flex:1;">
            <div style="display:flex; align-items:center; gap:8px; flex-wrap:wrap;">
              <div class="results-history-root-path" title="${escapeHtml(task.rootPath)}">${escapeHtml(task.rootPath)}</div>
              <span class="badge badge-info">${escapeHtml(getScanStatusLabel(task.status))}</span>
            </div>
            <div class="form-hint" style="margin-top:4px;">
              ${t('scanner.history_updated')}: ${escapeHtml(formatHistoryTime(task.updatedAt))}
            </div>
            <div class="form-hint" style="margin-top:2px;">
              ${task.scannedCount || 0} items · ${formatSize(task.totalCleanable || 0)} · Token ${(task.tokenUsage?.total || 0).toLocaleString()}
            </div>
          </div>
          <div class="results-history-actions">
            <button class="btn btn-secondary results-history-load-btn" data-task-id="${escapeHtml(task.taskId)}">${t('scanner.history_load')}</button>
            <button class="btn btn-ghost results-history-delete-btn" data-task-id="${escapeHtml(task.taskId)}" ${running ? 'disabled' : ''}>${t('scanner.history_delete')}</button>
          </div>
        </div>
      </div>
    `;
  }).join('');

  document.querySelectorAll('.results-history-load-btn').forEach((btn) => {
    btn.addEventListener('click', async () => {
      const taskId = String(btn.dataset.taskId || '').trim();
      if (!taskId) return;
      await loadHistoryTask(taskId);
    });
  });

  document.querySelectorAll('.results-history-delete-btn').forEach((btn) => {
    btn.addEventListener('click', async () => {
      const taskId = String(btn.dataset.taskId || '').trim();
      if (!taskId) return;
      await deleteHistoryTask(taskId);
    });
  });
}

async function refreshHistoryList() {
  const refreshBtn = document.getElementById('results-history-refresh-btn');
  if (refreshBtn) refreshBtn.disabled = true;

  try {
    historyTasks = await listScanHistory(20);
  } catch (err) {
    console.warn('Failed to refresh scan history:', err);
  } finally {
    renderHistoryList();
    if (refreshBtn) refreshBtn.disabled = false;
  }
}

function renderTable(data) {
  const body = document.getElementById('results-body');
  const empty = document.getElementById('results-empty');
  if (!body || !empty) return;

  if (!data.length) {
    body.innerHTML = '';
    const textEl = empty.querySelector('.empty-state-text');
    const hintEl = empty.querySelector('.empty-state-hint');
    const hasData = currentData.length > 0;
    if (textEl) {
      textEl.textContent = hasData ? t('results.empty_filtered') : t('results.scan_not_started');
    }
    if (hintEl) {
      hintEl.textContent = hasData ? t('results.empty_filtered_hint') : t('results.go_scan');
    }
    empty.style.display = '';
    updateBatchDeleteBtn();
    return;
  }
  empty.style.display = 'none';

  data.sort((a, b) => {
    let va = a[sortField];
    let vb = b[sortField];
    if (sortField === 'size') {
      va = va || 0;
      vb = vb || 0;
    } else if (sortField === 'risk') {
      const riskOrder = { low: 0, medium: 1, high: 2 };
      va = riskOrder[va] ?? 1;
      vb = riskOrder[vb] ?? 1;
    } else {
      va = String(va || '').toLowerCase();
      vb = String(vb || '').toLowerCase();
    }
    if (va < vb) return sortDir === 'asc' ? -1 : 1;
    if (va > vb) return sortDir === 'asc' ? 1 : -1;
    return 0;
  });

  body.innerHTML = data.map((item, idx) => `
    <tr style="animation: slideUp 0.2s var(--ease-out) ${Math.min(idx * 0.02, 0.5)}s both;">
      <td style="text-align: center;">
        <input type="checkbox" class="row-cb" data-path="${escapeHtml(item.path || '')}" />
      </td>
      <td>
        <div class="file-name" title="${escapeHtml(`${item.type === 'directory' ? 'DIR' : 'FILE'} ${item.name || ''}`)}">${item.type === 'directory' ? 'DIR' : 'FILE'} ${escapeHtml(item.name)}</div>
        <div class="file-path" title="${escapeHtml(item.path || '')}">${escapeHtml(item.path || '')}</div>
      </td>
      <td>
        <span class="mono" style="font-size: 0.82rem; font-weight: 600;">${formatSize(item.size || 0)}</span>
      </td>
      <td>
        <span class="badge badge-${riskBadge(item.risk)}">${riskLabel(item.risk)}</span>
      </td>
      <td>
        ${renderAiInsight(item)}
      </td>
      <td style="text-align: center;">
        <div class="results-row-actions">
          <button class="btn btn-ghost open-loc-btn" data-path="${escapeHtml(item.path || '')}" style="padding: 4px; font-size: 0.85rem;" title="${t('results.open_folder')}">
            Open
          </button>
          <button class="btn btn-ghost whitelist-btn" data-path="${escapeHtml(item.path || '')}" style="padding: 4px; font-size: 0.85rem;" title="${t('results.add_to_whitelist')}">
            ${t('results.add_to_whitelist')}
          </button>
        </div>
      </td>
    </tr>
  `).join('');

  document.querySelectorAll('.row-cb').forEach((cb) => {
    cb.addEventListener('change', updateBatchDeleteBtn);
  });

  document.querySelectorAll('.open-loc-btn').forEach((btn) => {
    btn.addEventListener('click', async (event) => {
      event.preventDefault();
      const originalHtml = btn.innerHTML;
      try {
        btn.innerHTML = '...';
        btn.disabled = true;
        const res = await openFileLocation(btn.dataset.path);
        if (!res.success) {
          showToast(t('results.toast_open_failed') + res.error, 'error');
        }
      } catch (err) {
        showToast(t('results.toast_open_failed') + err.message, 'error');
      } finally {
        btn.innerHTML = originalHtml;
        btn.disabled = false;
      }
    });
  });

  document.querySelectorAll('.whitelist-btn').forEach((btn) => {
    btn.addEventListener('click', async (event) => {
      event.preventDefault();
      const path = String(btn.dataset.path || '').trim();
      if (!path) return;
      const originalHtml = btn.innerHTML;
      try {
        btn.innerHTML = t('results.adding_to_whitelist');
        btn.disabled = true;
        await addPathToWhitelist(path);
      } finally {
        btn.innerHTML = originalHtml;
        btn.disabled = false;
      }
    });
  });

  updateBatchDeleteBtn();
}

function applySnapshot(snapshot) {
  currentSnapshot = snapshot || null;
  currentTaskId = snapshot?.id || null;
  currentData = Array.isArray(snapshot?.deletable) ? snapshot.deletable : [];

  if (snapshot?.id) {
    storage.set('lastScanTaskId', snapshot.id);
    storage.set('lastScan', snapshot);
  }

  updateSummary(snapshot);
  renderTable(getFilteredData());
  renderHistoryList();
  updateContinueModalState(snapshot);
}

function clearCurrentSnapshot() {
  currentTaskId = null;
  currentSnapshot = null;
  currentData = [];
  closeContinueScanModal();
  storage.remove('lastScanTaskId');
  storage.remove('lastScan');
  updateSummary(null);
  renderTable([]);
  renderHistoryList();
  updateContinueModalState(null);
}

async function loadIgnoredPaths() {
  try {
    const settings = await getSettings();
    ignoredPaths = normalizeIgnoredPaths(settings?.scanIgnorePaths);
  } catch (err) {
    ignoredPaths = [];
    console.warn('Failed to load scan ignore paths:', err);
  }
}

async function addPathToWhitelist(path) {
  const nextIgnoredPaths = mergeIgnoredPaths(ignoredPaths, [path]);
  if (nextIgnoredPaths.length === ignoredPaths.length) {
    showToast(t('results.whitelist_added'), 'info');
    return;
  }

  try {
    await saveSettings({ scanIgnorePaths: nextIgnoredPaths });
    ignoredPaths = nextIgnoredPaths;
    updateSummary(currentSnapshot);
    renderTable(getFilteredData());
    showToast(t('results.whitelist_added'), 'success');
  } catch (err) {
    showToast(t('results.whitelist_failed') + err.message, 'error');
  }
}

async function handleContinueScan() {
  if (!currentSnapshot?.id) return;
  const submitBtn = document.getElementById('continue-scan-submit-btn');
  const draft = readContinueDraftFromDom(currentSnapshot);
  writeContinueDraft(draft, currentSnapshot);

  try {
    if (submitBtn) {
      submitBtn.disabled = true;
      submitBtn.innerHTML = `<span class="spinner"></span> ${t('scanner.prepare')}`;
    }
    const result = await startScan({
      targetPath: currentSnapshot.targetPath,
      targetSizeGB: draft.targetSizeGb,
      maxDepth: draft.unlimited ? null : draft.depth,
      baselineTaskId: currentSnapshot.id,
      scanMode: 'deepen_incremental',
      autoAnalyze: true,
      responseLanguage: getLang(),
    });
    storage.set('lastScanTaskId', result.taskId);
    closeContinueScanModal();
    showToast(t('results.continue_started'), 'success');
    window.location.hash = '#/scanner';
  } catch (err) {
    showToast(t('results.continue_failed') + err.message, 'error');
  } finally {
    if (submitBtn) {
      submitBtn.disabled = false;
      submitBtn.textContent = t('results.continue_action');
    }
    updateContinueModalState(currentSnapshot);
  }
}

async function loadHistoryTask(taskId) {
  try {
    showToast(t('scanner.history_loading'), 'info');
    const snapshot = await getScanResult(taskId);
    applySnapshot(snapshot);
    await refreshHistoryList();
    showToast(t('scanner.history_loaded'), 'success');
  } catch (err) {
    showToast(t('scanner.history_load_failed') + err.message, 'error');
  }
}

async function deleteHistoryTask(taskId) {
  if (!confirm(t('scanner.history_delete_confirm'))) return;

  try {
    await deleteScanHistory(taskId);
    if (currentTaskId === taskId) {
      clearCurrentSnapshot();
    }
    await refreshHistoryList();
    showToast(t('scanner.history_deleted'), 'success');
  } catch (err) {
    if (/still running/i.test(err.message)) {
      showToast(t('scanner.history_running'), 'error');
      return;
    }
    showToast(t('scanner.history_delete_failed') + err.message, 'error');
  }
}

async function refreshSnapshot({ silent = false, expectedRenderVersion = null } = {}) {
  const refreshBtn = document.getElementById('results-refresh-btn');
  const taskId = getPreferredTaskId();
  const isStale = () => expectedRenderVersion != null && expectedRenderVersion !== renderVersion;

  if (refreshBtn) refreshBtn.disabled = true;

  try {
    if (!taskId) {
      const cachedSnapshot = getCachedLastSnapshot();
      if (cachedSnapshot) {
        applySnapshot(cachedSnapshot);
      } else {
        clearCurrentSnapshot();
      }
      return;
    }

    const snapshot = await getScanResult(taskId);
    if (isStale()) return;
    applySnapshot(snapshot);
    if (!silent) {
      showToast(t('scanner.history_loaded'), 'success');
    }
  } catch (err) {
    if (isStale()) return;
    const cachedSnapshot = currentSnapshot || getCachedLastSnapshot();
    if (cachedSnapshot) {
      applySnapshot(cachedSnapshot);
    } else {
      clearCurrentSnapshot();
    }
    if (!silent) {
      showToast(t('scanner.history_load_failed') + err.message, 'error');
    }
  } finally {
    if (refreshBtn) refreshBtn.disabled = false;
  }
}

async function handleBatchClean() {
  const batchDeleteBtn = document.getElementById('batch-delete-btn');
  const selectedPaths = Array.from(document.querySelectorAll('.row-cb:checked')).map((cb) => cb.dataset.path);
  if (selectedPaths.length === 0) return;
  if (!confirm(`${t('results.clean_selected')}?`)) return;

  try {
    if (batchDeleteBtn) {
      batchDeleteBtn.disabled = true;
      batchDeleteBtn.innerHTML = `<span class="spinner"></span> ${t('results.cleaning')}`;
    }

    const res = await cleanFiles(selectedPaths, currentTaskId);
    if (!res.success) {
      showToast(t('results.toast_clean_failed') + (res.error || ''), 'error');
      return;
    }

    const cleanedPaths = Array.isArray(res.results?.cleaned) ? res.results.cleaned : [];
    const failedItems = Array.isArray(res.results?.failed) ? res.results.failed : [];
    const elevationRequiredItems = failedItems.filter((item) => item?.requiresElevation);

    if (res.scanSnapshot && typeof res.scanSnapshot === 'object') {
      applySnapshot(res.scanSnapshot);
      await refreshHistoryList();
    } else {
      currentData = currentData.filter((item) => !cleanedPaths.includes(item.path));
      updateSummary(currentSnapshot);
      renderTable(getFilteredData());
    }

    if (cleanedPaths.length > 0 && failedItems.length > 0) {
      showToast(t('results.cleaned_partial', { cleaned: cleanedPaths.length, failed: failedItems.length }), 'warning');
    } else if (cleanedPaths.length > 0) {
      showToast(t('results.cleaned_success', { count: cleanedPaths.length }), 'success');
    } else {
      showToast(t('results.cleaned_none', { count: failedItems.length || selectedPaths.length }), 'error');
    }

    if (elevationRequiredItems.length > 0 && confirm(t('results.elevation_needed_confirm', { count: elevationRequiredItems.length }))) {
      try {
        const result = await requestElevation();
        showToast(t('settings.elevation_uac_prompt'), 'info');
        if (result?.restarting) {
          handleElevationTransition({ showToast, t });
        }
      } catch (err) {
        showToast(t('settings.elevation_failed') + err.message, 'error');
      }
    }
  } catch (err) {
    showToast(t('results.toast_clean_failed') + err.message, 'error');
  } finally {
    if (batchDeleteBtn) {
      batchDeleteBtn.disabled = false;
      batchDeleteBtn.innerHTML = `${t('results.clean_selected')} (<span id="selected-count">0</span>)`;
    }
    updateBatchDeleteBtn();
  }
}

export async function renderResults(container) {
  const expectedRenderVersion = ++renderVersion;
  const cachedSnapshot = getCachedLastSnapshot();
  currentTaskId = storage.get('lastScanTaskId', null) || cachedSnapshot?.id || null;
  currentSnapshot = null;
  currentData = [];
  historyTasks = [];
  ignoredPaths = [];

  container.innerHTML = `
    <div class="page-header animate-in">
      <h1 class="page-title">${t('results.title')}</h1>
      <p class="page-subtitle">${t('results.subtitle')}</p>
    </div>

    <div class="stats-grid animate-in" style="animation-delay: 0.05s">
      <div class="stat-card">
        <span class="stat-label">${t('results.safe_to_clean')}</span>
        <span class="stat-value accent" id="res-count">0</span>
      </div>
      <div class="stat-card">
        <span class="stat-label">${t('results.space_freed')}</span>
        <span class="stat-value success" id="res-size">0 B</span>
      </div>
      <div class="stat-card">
        <span class="stat-label">${t('results.risk_safe')}</span>
        <span class="stat-value" id="res-low" style="color: var(--accent-success);">0</span>
      </div>
      <div class="stat-card">
        <span class="stat-label">${t('results.risk_danger')}</span>
        <span class="stat-value warning" id="res-high">0</span>
      </div>
    </div>

    <div class="card animate-in mb-24" style="animation-delay: 0.08s">
      <div class="card-header">
        <h2 class="card-title">${t('scanner.history_title')}</h2>
        <button id="results-history-refresh-btn" class="btn btn-ghost" type="button" style="padding: 6px 12px; font-size: 0.75rem;">${t('scanner.history_refresh')}</button>
      </div>
      <div id="results-history-list"></div>
    </div>

    <div class="card animate-in mb-24 results-control-card" style="animation-delay: 0.1s;">
      <div class="results-control-top">
        <div class="results-filter-group">
          <button class="btn btn-ghost filter-btn active" data-filter="all">${t('results.filter_all')}</button>
          <button class="btn btn-ghost filter-btn filter-btn-safe" data-filter="low">${t('results.filter_safe')}</button>
          <button class="btn btn-ghost filter-btn filter-btn-warning" data-filter="medium">${t('results.filter_warning')}</button>
          <button class="btn btn-ghost filter-btn filter-btn-danger" data-filter="high">${t('results.filter_danger')}</button>
        </div>
        <div class="results-toolbar-actions">
          <button id="batch-delete-btn" class="btn btn-danger" style="display: none;">
            ${t('results.clean_selected')} (<span id="selected-count">0</span>)
          </button>
          <button id="results-refresh-btn" class="btn btn-ghost" type="button">${t('results.refresh')}</button>
          <button id="continue-scan-btn" class="btn btn-primary" type="button">${t('results.continue_action')}</button>
        </div>
      </div>
      <div class="results-scan-brief">
        <div id="results-scan-summary" class="results-scan-summary">${t('results.scan_not_started')}</div>
        <div id="results-scan-root" class="results-scan-root"></div>
      </div>
    </div>

    <div id="continue-scan-modal" class="app-modal" aria-hidden="true">
      <div class="app-modal-overlay" data-modal-close="true"></div>
      <div class="app-modal-panel card results-continue-modal">
        <div class="app-modal-header">
          <div>
            <h2 id="continue-scan-modal-title" class="card-title">${t('results.continue_action')}</h2>
            <div class="modal-subtitle">${t('results.continue_hint')}</div>
          </div>
          <button id="continue-scan-modal-close" class="btn btn-ghost" type="button">${t('provider_modal.cancel')}</button>
        </div>
        <div class="results-continue-modal-body">
          <div class="results-continue-overview">
            <div id="continue-scan-summary" class="results-continue-overview-title"></div>
            <div class="results-continue-overview-root">
              <span class="results-continue-overview-label">${t('settings.scan_path')}</span>
              <div id="continue-scan-root" class="results-continue-overview-value"></div>
            </div>
            <div id="continue-scan-hint" class="form-hint"></div>
          </div>

          <div class="results-continue-grid">
            <div class="form-group">
              <label class="form-label">${t('settings.target_size')}</label>
              <div class="range-container">
                <input id="continue-target-size-range" type="range" class="range-slider" min="0.1" max="1000" step="0.1" value="1" />
                <div class="results-range-inline">
                  <input id="continue-target-size-input" class="form-input no-spin" type="number" min="0.1" max="1000" step="0.1" value="1" />
                  <span class="range-value" style="min-width: unset;">GB</span>
                </div>
              </div>
              <div class="form-hint">${t('settings.target_size_hint')}</div>
            </div>

            <div class="form-group">
              <label class="form-label">${t('settings.max_depth')}</label>
              <div class="range-container">
                <input id="continue-depth-range" type="range" class="range-slider" min="1" max="16" step="1" value="6" />
                <div class="results-range-inline">
                  <input id="continue-depth-input" class="form-input no-spin" type="number" min="1" max="16" step="1" value="6" />
                  <span class="range-value" style="min-width: unset;">${t('settings.depth_unit')}</span>
                </div>
              </div>
              <label class="results-toggle-row" for="continue-unlimited-toggle">
                <input id="continue-unlimited-toggle" type="checkbox" class="results-toggle-checkbox" />
                <span class="results-toggle-track" aria-hidden="true"></span>
                <span>${t('settings.max_depth_unlimited')}</span>
              </label>
            </div>
          </div>
        </div>
        <div class="app-modal-actions">
          <button id="continue-scan-modal-cancel" class="btn btn-ghost" type="button">${t('provider_modal.cancel')}</button>
          <button id="continue-scan-submit-btn" class="btn btn-primary" type="button">${t('results.continue_action')}</button>
        </div>
      </div>
    </div>

    <div class="card animate-in" style="animation-delay: 0.15s; padding: 0; overflow: hidden;">
      <div style="overflow-x: auto;">
        <table class="data-table" id="results-table">
          <thead>
            <tr>
              <th style="width: 40px; text-align: center;">
                <input type="checkbox" id="select-all-cb" />
              </th>
              <th data-sort="name" style="width: 26%;">${t('results.table_path')}</th>
              <th data-sort="size" class="sorted" style="width: 10%;">${t('results.table_size')}</th>
              <th data-sort="risk" style="width: 12%;">${t('results.table_risk')}</th>
              <th style="width: 38%;">${t('results.table_reason')}</th>
              <th style="width: 14%; text-align: center;">${t('results.table_action')}</th>
            </tr>
          </thead>
          <tbody id="results-body"></tbody>
        </table>
      </div>
      <div id="results-empty" class="empty-state" style="display: none;">
        <div class="empty-state-icon">...</div>
        <div class="empty-state-text">${t('results.scan_not_started')}</div>
        <div class="empty-state-hint">${t('results.go_scan')}</div>
      </div>
    </div>
  `;

  document.querySelectorAll('[data-sort]').forEach((th) => {
    th.addEventListener('click', () => {
      const field = th.dataset.sort;
      if (sortField === field) {
        sortDir = sortDir === 'asc' ? 'desc' : 'asc';
      } else {
        sortField = field;
        sortDir = field === 'name' ? 'asc' : 'desc';
      }
      document.querySelectorAll('[data-sort]').forEach((header) => header.classList.remove('sorted'));
      th.classList.add('sorted');
      renderTable(getFilteredData());
    });
  });

  document.querySelectorAll('.filter-btn').forEach((btn) => {
    btn.addEventListener('click', () => {
      document.querySelectorAll('.filter-btn').forEach((item) => item.classList.remove('active'));
      btn.classList.add('active');
      renderTable(getFilteredData());
    });
  });

  document.getElementById('select-all-cb')?.addEventListener('change', (event) => {
    const isChecked = event.target.checked;
    document.querySelectorAll('.row-cb').forEach((cb) => {
      cb.checked = isChecked;
    });
    updateBatchDeleteBtn();
  });

  const syncContinueModalFromControls = ({ source } = {}) => {
    const snapshot = currentSnapshot;
    if (!snapshot?.id) return;
    const depthRange = document.getElementById('continue-depth-range');
    const depthInput = document.getElementById('continue-depth-input');
    const sizeRange = document.getElementById('continue-target-size-range');
    const sizeInput = document.getElementById('continue-target-size-input');
    const unlimitedToggle = document.getElementById('continue-unlimited-toggle');
    const draft = readContinueDraftFromDom(snapshot);

    if (source === 'depth-range' && depthInput) depthInput.value = String(draft.depth);
    if (source === 'depth-input' && depthRange) depthRange.value = String(draft.depth);
    if (source === 'size-range' && sizeInput) sizeInput.value = draft.targetSizeGb.toFixed(1);
    if (source === 'size-input' && sizeRange) sizeRange.value = String(draft.targetSizeGb);
    if (source === 'toggle' && unlimitedToggle) unlimitedToggle.checked = draft.unlimited;

    writeContinueDraft(draft, snapshot);
    updateContinueModalState(snapshot);
  };

  document.getElementById('batch-delete-btn')?.addEventListener('click', handleBatchClean);
  document.getElementById('continue-scan-btn')?.addEventListener('click', openContinueScanModal);
  document.getElementById('continue-scan-modal-close')?.addEventListener('click', closeContinueScanModal);
  document.getElementById('continue-scan-modal-cancel')?.addEventListener('click', closeContinueScanModal);
  document.getElementById('continue-scan-submit-btn')?.addEventListener('click', handleContinueScan);
  document.getElementById('results-refresh-btn')?.addEventListener('click', () => refreshSnapshot());
  document.getElementById('results-history-refresh-btn')?.addEventListener('click', () => refreshHistoryList());
  document.getElementById('continue-depth-range')?.addEventListener('input', () => syncContinueModalFromControls({ source: 'depth-range' }));
  document.getElementById('continue-depth-input')?.addEventListener('input', () => syncContinueModalFromControls({ source: 'depth-input' }));
  document.getElementById('continue-target-size-range')?.addEventListener('input', () => syncContinueModalFromControls({ source: 'size-range' }));
  document.getElementById('continue-target-size-input')?.addEventListener('input', () => syncContinueModalFromControls({ source: 'size-input' }));
  document.getElementById('continue-unlimited-toggle')?.addEventListener('change', () => syncContinueModalFromControls({ source: 'toggle' }));
  document.getElementById('continue-scan-modal')?.addEventListener('click', (event) => {
    if (event.target?.getAttribute('data-modal-close') === 'true') {
      closeContinueScanModal();
    }
  });
  if (!continueModalEscapeBound) {
    continueModalEscapeBound = true;
    document.addEventListener('keydown', (event) => {
      if (event.key === 'Escape') {
        closeContinueScanModal();
      }
    });
  }

  await loadIgnoredPaths();
  updateSummary(null);
  renderTable([]);
  renderHistoryList();
  updateContinueModalState(null);
  if (cachedSnapshot) {
    applySnapshot(cachedSnapshot);
  }
  await refreshHistoryList();
  await refreshSnapshot({ silent: true, expectedRenderVersion });
}
