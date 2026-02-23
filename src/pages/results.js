/**
 * src/pages/results.js
 * 分析结果页面 — 展示可删除文件建议列表
 */
import * as storage from '../utils/storage.js';
import { formatSize } from '../utils/storage.js';
import { openFileLocation, deleteFiles } from '../utils/api.js';
import { showToast } from '../main.js';

let sortField = 'size';
let sortDir = 'desc';
let currentData = [];

export function renderResults(container) {
  currentData = storage.get('scanResults', []);
  const lastScan = storage.get('lastScan', null);

  container.innerHTML = `
    <div class="page-header animate-in">
      <h1 class="page-title">📋 分析结果</h1>
      <p class="page-subtitle">AI 建议的可清理文件与文件夹</p>
    </div>

    <!-- Summary -->
    <div class="stats-grid animate-in" style="animation-delay: 0.05s">
      <div class="stat-card">
        <span class="stat-label">可删除项目</span>
        <span class="stat-value accent" id="res-count">${currentData.length}</span>
      </div>
      <div class="stat-card">
        <span class="stat-label">可清理总空间</span>
        <span class="stat-value success" id="res-size">${formatSize(currentData.reduce((s, i) => s + (i.size || 0), 0))}</span>
      </div>
      <div class="stat-card">
        <span class="stat-label">低风险项</span>
        <span class="stat-value" id="res-low" style="color: var(--accent-success);">${currentData.filter(i => i.risk === 'low').length}</span>
      </div>
      <div class="stat-card">
        <span class="stat-label">中/高风险项</span>
        <span class="stat-value warning" id="res-high">${currentData.filter(i => i.risk !== 'low').length}</span>
      </div>
    </div>

    <!-- Filter Bar -->
    <div class="card animate-in mb-24" style="animation-delay: 0.1s; padding: 14px 20px;">
      <div class="flex items-center justify-between">
        <div class="flex items-center gap-16">
          <button class="btn btn-ghost filter-btn active" data-filter="all">全部</button>
          <button class="btn btn-ghost filter-btn" data-filter="low">🟢 低风险</button>
          <button class="btn btn-ghost filter-btn" data-filter="medium">🟡 中风险</button>
          <button class="btn btn-ghost filter-btn" data-filter="high">🔴 高风险</button>
          <div style="width: 1px; height: 24px; background: rgba(255,255,255,0.1); margin: 0 8px;"></div>
          <button id="batch-delete-btn" class="btn btn-danger" style="display: none;">
            🗑️ 删除选中项 (<span id="selected-count">0</span>)
          </button>
        </div>
        <div style="font-size: 0.8rem; color: var(--text-muted);">
          ${lastScan?.lastScanTime ? `上次扫描: ${new Date(lastScan.lastScanTime).toLocaleString('zh-CN')}` : ''}
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
              <th data-sort="name" style="width: 20%;">文件名 ↕</th>
              <th data-sort="size" class="sorted" style="width: 10%;">大小 ↓</th>
              <th data-sort="risk" style="width: 8%;">风险</th>
              <th style="width: 20%;">功能推测</th>
              <th style="width: 25%;">删除理由</th>
              <th style="width: 10%; text-align: center;">操作</th>
            </tr>
          </thead>
          <tbody id="results-body">
          </tbody>
        </table>
      </div>
      <div id="results-empty" class="empty-state" style="display: none;">
        <div class="empty-state-icon">📭</div>
        <div class="empty-state-text">暂无分析结果</div>
        <div class="empty-state-hint">请先在扫描页面启动一次扫描</div>
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

      if (confirm(`确定要删除选中的 ${selectedPaths.length} 个项目吗？此操作无法撤销！`)) {
        try {
          batchDeleteBtn.disabled = true;
          batchDeleteBtn.innerHTML = '<span class="spinner"></span> 删除中...';

          const res = await deleteFiles(selectedPaths);
          if (res.success) {
            showToast(`成功删除 ${res.results.deleted.length} 个项目`, 'success');

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
            showToast('删除失败: ' + res.error, 'error');
          }
        } catch (err) {
          showToast('删除失败: ' + err.message, 'error');
        } finally {
          batchDeleteBtn.disabled = false;
          batchDeleteBtn.innerHTML = `🗑️ 删除选中项 (<span id="selected-count">0</span>)`;
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
        <button class="btn btn-ghost open-loc-btn" data-path="${escapeHtml(item.path || '')}" style="padding: 4px; font-size: 1.1rem;" title="打开文件位置">
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
          showToast('无法打开文件位置: ' + res.error, 'error');
        }
      } catch (err) {
        showToast('无法打开文件位置: ' + err.message, 'error');
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
  return risk === 'low' ? '低风险' : risk === 'high' ? '高风险' : '中风险';
}

function escapeHtml(str) {
  const div = document.createElement('div');
  div.textContent = str;
  return div.innerHTML;
}
