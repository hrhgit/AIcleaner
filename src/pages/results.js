import * as storage from '../utils/storage.js';
import { formatSize } from '../utils/storage.js';
import { cleanFiles, getScanResult, openFileLocation, requestElevation } from '../utils/api.js';
import { handleElevationTransition } from '../utils/elevation.js';
import { showToast } from '../main.js';
import { t } from '../utils/i18n.js';

let sortField = 'size';
let sortDir = 'desc';
let currentTaskId = null;
let currentSnapshot = null;
let currentData = [];

function escapeHtml(str) {
  const div = document.createElement('div');
  div.textContent = String(str ?? '');
  return div.innerHTML;
}

function riskBadge(risk) {
  return risk === 'low' ? 'success' : risk === 'high' ? 'danger' : 'warning';
}

function riskLabel(risk) {
  return risk === 'low' ? t('results.risk_safe') : risk === 'high' ? t('results.risk_danger') : t('results.risk_warning');
}

function getFilteredData() {
  const activeFilter = document.querySelector('.filter-btn.active')?.dataset.filter || 'all';
  let data = [...currentData];
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
  const taskEl = document.getElementById('results-task-meta');

  const totalSize = currentData.reduce((sum, item) => sum + (item.size || 0), 0);
  if (countEl) countEl.textContent = String(currentData.length);
  if (sizeEl) sizeEl.textContent = formatSize(totalSize);
  if (lowEl) lowEl.textContent = String(currentData.filter((item) => item.risk === 'low').length);
  if (highEl) highEl.textContent = String(currentData.filter((item) => item.risk !== 'low').length);
  if (taskEl) {
    taskEl.textContent = snapshot?.id ? `Task: ${snapshot.id}` : '';
  }
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

function renderTable(data) {
  const body = document.getElementById('results-body');
  const empty = document.getElementById('results-empty');
  if (!body || !empty) return;

  if (!data.length) {
    body.innerHTML = '';
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
        <div class="file-name">${item.type === 'directory' ? 'DIR' : 'FILE'} ${escapeHtml(item.name)}</div>
        <div class="file-path">${escapeHtml(item.path || '')}</div>
      </td>
      <td>
        <span class="mono" style="font-size: 0.82rem; font-weight: 600;">${formatSize(item.size || 0)}</span>
      </td>
      <td>
        <span class="badge badge-${riskBadge(item.risk)}">${riskLabel(item.risk)}</span>
      </td>
      <td>
        <div class="file-purpose">${escapeHtml(item.purpose || '-')}</div>
      </td>
      <td>
        <div class="file-purpose">${escapeHtml(item.reason || '-')}</div>
      </td>
      <td style="text-align: center;">
        <button class="btn btn-ghost open-loc-btn" data-path="${escapeHtml(item.path || '')}" style="padding: 4px; font-size: 0.85rem;" title="${t('results.open_folder')}">
          Open
        </button>
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

  updateBatchDeleteBtn();
}

function applySnapshot(snapshot) {
  currentSnapshot = snapshot || null;
  currentTaskId = snapshot?.id || currentTaskId || null;
  currentData = Array.isArray(snapshot?.deletable) ? snapshot.deletable : [];

  if (snapshot?.id) {
    storage.set('lastScanTaskId', snapshot.id);
    storage.set('lastScan', snapshot);
  }

  updateSummary(snapshot);
  renderTable(getFilteredData());
}

async function refreshSnapshot({ silent = false } = {}) {
  const refreshBtn = document.getElementById('results-refresh-btn');
  const taskId = storage.get('lastScanTaskId', null) || currentTaskId;

  if (refreshBtn) refreshBtn.disabled = true;

  try {
    if (!taskId) {
      currentTaskId = null;
      applySnapshot(null);
      return;
    }
    const snapshot = await getScanResult(taskId);
    applySnapshot(snapshot);
    if (!silent) {
      showToast(t('scanner.history_loaded'), 'success');
    }
  } catch (err) {
    currentTaskId = null;
    currentSnapshot = null;
    currentData = [];
    storage.remove('lastScanTaskId');
    storage.remove('lastScan');
    updateSummary(null);
    renderTable([]);
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
  currentTaskId = storage.get('lastScanTaskId', null);
  currentSnapshot = null;
  currentData = [];

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

    <div class="card animate-in mb-24" style="animation-delay: 0.1s; padding: 14px 20px;">
      <div class="flex items-center justify-between" style="gap: 16px; flex-wrap: wrap;">
        <div class="flex items-center gap-16" style="flex-wrap: wrap;">
          <button class="btn btn-ghost filter-btn active" data-filter="all">${t('results.filter_all')}</button>
          <button class="btn btn-ghost filter-btn" data-filter="low">${t('results.filter_safe')}</button>
          <button class="btn btn-ghost filter-btn" data-filter="medium">${t('results.filter_warning')}</button>
          <button class="btn btn-ghost filter-btn" data-filter="high">${t('results.filter_danger')}</button>
          <div style="width: 1px; height: 24px; background: rgba(255,255,255,0.1); margin: 0 8px;"></div>
          <button id="batch-delete-btn" class="btn btn-danger" style="display: none;">
            ${t('results.clean_selected')} (<span id="selected-count">0</span>)
          </button>
        </div>
        <div class="flex items-center gap-8" style="font-size: 0.8rem; color: var(--text-muted);">
          <span id="results-task-meta"></span>
          <button id="results-refresh-btn" class="btn btn-ghost" type="button" style="padding: 6px 12px; font-size: 0.75rem;">刷新</button>
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
              <th data-sort="name" style="width: 20%;">${t('results.table_path')}</th>
              <th data-sort="size" class="sorted" style="width: 10%;">${t('results.table_size')}</th>
              <th data-sort="risk" style="width: 8%;">${t('results.risk_warning')}</th>
              <th style="width: 20%;">${t('results.table_reason')}</th>
              <th style="width: 25%;">${t('results.table_reason')}</th>
              <th style="width: 10%; text-align: center;">${t('results.table_action')}</th>
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

  document.getElementById('batch-delete-btn')?.addEventListener('click', handleBatchClean);
  document.getElementById('results-refresh-btn')?.addEventListener('click', () => refreshSnapshot());

  updateSummary(null);
  renderTable([]);
  await refreshSnapshot({ silent: true });
}
