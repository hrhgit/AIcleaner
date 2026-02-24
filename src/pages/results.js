/**
 * src/pages/results.js
 * 分析结果页面 — 展示可删除文件建议列表
 */
import * as storage from '../utils/storage.js';
import { formatSize } from '../utils/storage.js';
import { openFileLocation, deleteFiles } from '../utils/api.js';
import { showToast } from '../main.js';
import { t } from '../utils/i18n.js';

let sortField = 'size';
let sortDir = 'desc';
let currentData = [];

export function renderResults(container) {
  currentData = storage.get('scanResults', []);
  const lastScan = storage.get('lastScan', null);

  container.innerHTML = `
    <div class="page-header animate-in">
      <h1 class="page-title">${t('results.title')}</h1>
      <p class="page-subtitle">${t('results.subtitle')}</p>
    </div>

    <!-- Summary -->
    <div class="stats-grid animate-in" style="animation-delay: 0.05s">
      <div class="stat-card">
        <span class="stat-label">${t('results.safe_to_clean')}</span>
        <span class="stat-value accent" id="res-count">${currentData.length}</span>
      </div>
      <div class="stat-card">
        <span class="stat-label">${t('results.space_freed')}</span>
        <span class="stat-value success" id="res-size">${formatSize(currentData.reduce((s, i) => s + (i.size || 0), 0))}</span>
      </div>
      <div class="stat-card">
        <span class="stat-label">${t('results.risk_safe')}</span>
        <span class="stat-value" id="res-low" style="color: var(--accent-success);">${currentData.filter(i => i.risk === 'low').length}</span>
      </div>
      <div class="stat-card">
        <span class="stat-label">${t('results.risk_danger')}</span>
        <span class="stat-value warning" id="res-high">${currentData.filter(i => i.risk !== 'low').length}</span>
      </div>
    </div>

    <!-- Filter Bar -->
    <div class="card animate-in mb-24" style="animation-delay: 0.1s; padding: 14px 20px;">
      <div class="flex items-center justify-between">
        <div class="flex items-center gap-16">
          <button class="btn btn-ghost filter-btn active" data-filter="all">${t('results.filter_all')}</button>
          <button class="btn btn-ghost filter-btn" data-filter="low">🟢 ${t('results.filter_safe')}</button>
          <button class="btn btn-ghost filter-btn" data-filter="medium">🟡 ${t('results.filter_warning')}</button>
          <button class="btn btn-ghost filter-btn" data-filter="high">🔴 ${t('results.filter_danger')}</button>
          <div style="width: 1px; height: 24px; background: rgba(255,255,255,0.1); margin: 0 8px;"></div>
          <button id="batch-delete-btn" class="btn btn-danger" style="display: none;">
            ${t('results.clean_selected')} (<span id="selected-count">0</span>)
          </button>
        </div>
        <div style="font-size: 0.8rem; color: var(--text-muted);">
          ${lastScan?.lastScanTime ? `Last Scan: ${new Date(lastScan.lastScanTime).toLocaleString('zh-CN')}` : ''}
        </div>
      </div>
    </div>

    <!-- Results Table -->
    <div class="card animate-in" style="animation-delay: 0.15s; padding: 0; overflow: hidden;">
      <div style="overflow-x: auto;">
        <table class="data-table" id="results-table">
          <thead>
            <tr>
              <th style="width: 40px; text-align: center;">
                <input type="checkbox" id="select-all-cb" />
              </th>
              <th data-sort="name" style="width: 20%;">${t('results.table_path')} ↕</th>
              <th data-sort="size" class="sorted" style="width: 10%;">${t('results.table_size')} ↓</th>
              <th data-sort="risk" style="width: 8%;">${t('results.risk_warning')}</th>
              <th style="width: 20%;">${t('results.table_reason')}</th>
              <th style="width: 25%;">${t('results.table_reason')}</th>
              <th style="width: 10%; text-align: center;">${t('results.table_action')}</th>
            </tr>
          </thead>
          <tbody id="results-body">
          </tbody>
        </table>
      </div>
      <div id="results-empty" class="empty-state" style="display: none;">
        <div class="empty-state-icon">📭</div>
        <div class="empty-state-text">${t('results.scan_not_started')}</div>
        <div class="empty-state-hint">${t('results.go_scan')}</div>
      </div>
    </div>
  `;

  // Render initial data
  renderTable(currentData);

  // Sort headers
  document.querySelectorAll('[data-sort]').forEach(th => {
    th.addEventListener('click', () => {
      const field = th.dataset.sort;
      if (sortField === field) {
        sortDir = sortDir === 'asc' ? 'desc' : 'asc';
      } else {
        sortField = field;
        sortDir = field === 'name' ? 'asc' : 'desc';
      }
      document.querySelectorAll('[data-sort]').forEach(h => h.classList.remove('sorted'));
      th.classList.add('sorted');
      renderTable(getFilteredData());
    });
  });

  // Filter buttons
  document.querySelectorAll('.filter-btn').forEach(btn => {
    btn.addEventListener('click', () => {
      document.querySelectorAll('.filter-btn').forEach(b => b.classList.remove('active'));
      btn.classList.add('active');
      renderTable(getFilteredData());
      updateBatchDeleteBtn();
    });
  });

  // Select all events
  const selectAllCb = document.getElementById('select-all-cb');
  if (selectAllCb) {
    selectAllCb.addEventListener('change', (e) => {
      const isChecked = e.target.checked;
      document.querySelectorAll('.row-cb').forEach(cb => {
        cb.checked = isChecked;
      });
      updateBatchDeleteBtn();
    });
  }

  // Batch delete event
  const batchDeleteBtn = document.getElementById('batch-delete-btn');
  if (batchDeleteBtn) {
    batchDeleteBtn.addEventListener('click', async () => {
      const selectedPaths = Array.from(document.querySelectorAll('.row-cb:checked')).map(cb => cb.dataset.path);
      if (selectedPaths.length === 0) return;

      if (confirm(`${t('results.clean_selected')}?`)) {
        try {
          batchDeleteBtn.disabled = true;
          batchDeleteBtn.innerHTML = `<span class="spinner"></span> ${t('results.cleaning')}`;

          const res = await deleteFiles(selectedPaths);
          if (res.success) {
            showToast(t('results.cleaned_success').replace('{count}', res.results.deleted.length), 'success');

            // Update UI data state
            currentData = currentData.filter(item => !res.results.deleted.includes(item.path));
            storage.set('scanResults', currentData);

            // Refresh display
            document.getElementById('res-count').textContent = currentData.length;
            document.getElementById('res-size').textContent = formatSize(currentData.reduce((s, i) => s + (i.size || 0), 0));
            document.getElementById('res-low').textContent = currentData.filter(i => i.risk === 'low').length;
            document.getElementById('res-high').textContent = currentData.filter(i => i.risk !== 'low').length;

            renderTable(getFilteredData());
            updateBatchDeleteBtn();
          } else {
            showToast(t('results.toast_clean_failed') + res.error, 'error');
          }
        } catch (err) {
          showToast(t('results.toast_clean_failed') + err.message, 'error');
        } finally {
          batchDeleteBtn.disabled = false;
          batchDeleteBtn.innerHTML = `${t('results.clean_selected')} (<span id="selected-count">0</span>)`;
          updateBatchDeleteBtn();
        }
      }
    });
  }
}

function updateBatchDeleteBtn() {
  const selectedCount = document.querySelectorAll('.row-cb:checked').length;
  const btn = document.getElementById('batch-delete-btn');
  const countSpan = document.getElementById('selected-count');
  const selectAllCb = document.getElementById('select-all-cb');
  const totalVisible = document.querySelectorAll('.row-cb').length;

  if (btn && countSpan) {
    countSpan.textContent = selectedCount;
    btn.style.display = selectedCount > 0 ? '' : 'none';
  }

  if (selectAllCb) {
    selectAllCb.checked = selectedCount > 0 && selectedCount === totalVisible;
    selectAllCb.indeterminate = selectedCount > 0 && selectedCount < totalVisible;
  }
}

function getFilteredData() {
  const activeFilter = document.querySelector('.filter-btn.active')?.dataset.filter || 'all';
  let data = [...currentData];
  if (activeFilter !== 'all') {
    data = data.filter(i => i.risk === activeFilter);
  }
  return data;
}

function renderTable(data) {
  const body = document.getElementById('results-body');
  const empty = document.getElementById('results-empty');

  if (!data.length) {
    body.innerHTML = '';
    empty.style.display = '';
    return;
  }
  empty.style.display = 'none';

  // Sort
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
      va = (va || '').toLowerCase();
      vb = (vb || '').toLowerCase();
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
        <div class="file-name">${item.type === 'directory' ? '📁' : '📄'} ${escapeHtml(item.name)}</div>
        <div class="file-path">${escapeHtml(item.path || '')}</div>
      </td>
      <td>
        <span class="mono" style="font-size: 0.82rem; font-weight: 600;">${formatSize(item.size)}</span>
      </td>
      <td>
        <span class="badge badge-${riskBadge(item.risk)}">${riskLabel(item.risk)}</span>
      </td>
      <td>
        <div class="file-purpose">${escapeHtml(item.purpose || '—')}</div>
      </td>
      <td>
        <div class="file-purpose">${escapeHtml(item.reason || '—')}</div>
      </td>
      <td style="text-align: center;">
        <button class="btn btn-ghost open-loc-btn" data-path="${escapeHtml(item.path || '')}" style="padding: 4px; font-size: 1.1rem;" title="${t('results.open_folder')}">
          📂
        </button>
      </td>
    </tr>
  `).join('');

  // Attach row checkbox events
  document.querySelectorAll('.row-cb').forEach(cb => {
    cb.addEventListener('change', updateBatchDeleteBtn);
  });

  // Attach open location events
  document.querySelectorAll('.open-loc-btn').forEach(btn => {
    btn.addEventListener('click', async (e) => {
      e.preventDefault();
      const btnOriginalHtml = btn.innerHTML;
      try {
        btn.innerHTML = '⏳';
        btn.disabled = true;
        const path = btn.dataset.path;
        const res = await openFileLocation(path);
        if (!res.success) {
          showToast(t('results.toast_open_failed') + res.error, 'error');
        }
      } catch (err) {
        showToast(t('results.toast_open_failed') + err.message, 'error');
      } finally {
        btn.innerHTML = btnOriginalHtml;
        btn.disabled = false;
      }
    });
  });

  // reset select all checkbox
  updateBatchDeleteBtn();
}

function riskBadge(risk) {
  return risk === 'low' ? 'success' : risk === 'high' ? 'danger' : 'warning';
}

function riskLabel(risk) {
  return risk === 'low' ? t('results.risk_safe') : risk === 'high' ? t('results.risk_danger') : t('results.risk_warning');
}

function escapeHtml(str) {
  const div = document.createElement('div');
  div.textContent = str;
  return div.innerHTML;
}
