import {
  applyOrganize,
  browseFolder,
  connectOrganizeStream,
  getOrganizeCapability,
  getOrganizeResult,
  rollbackOrganize,
  startOrganize,
  stopOrganize,
  suggestOrganizeCategories,
} from '../utils/api.js';
import { showToast } from '../main.js';
import { t } from '../utils/i18n.js';

const PERSIST_KEYS = {
  rootPath: 'wipeout.organizer.global.root_path.v1',
  recursive: 'wipeout.organizer.global.recursive.v1',
  mode: 'wipeout.organizer.global.mode.v1',
  categories: 'wipeout.organizer.global.categories.v1',
  exclusions: 'wipeout.organizer.global.exclusions.v1',
  parallelism: 'wipeout.organizer.global.parallelism.v1',
  lastJobId: 'wipeout.organizer.global.last_job_id.v1',
};

const DEFAULT_CATEGORIES = [
  '工作学习',
  '财务票据',
  '媒体素材',
  '开发项目',
  '安装与压缩',
  '临时下载',
  '其他待定',
];

const DEFAULT_EXCLUSIONS = [
  '.git',
  'node_modules',
  'dist',
  'build',
  'out',
  'Windows',
  'Program Files',
  'Program Files (x86)',
];

let activeTaskId = null;
let activeEventSource = null;
let latestSnapshot = null;
let latestCapability = null;

function getPersisted(key, fallback) {
  try {
    const raw = localStorage.getItem(key);
    if (!raw) return fallback;
    return JSON.parse(raw);
  } catch {
    return fallback;
  }
}

function setPersisted(key, value) {
  try {
    localStorage.setItem(key, JSON.stringify(value));
  } catch {
    // ignore quota errors
  }
}

function parseListInput(text) {
  return String(text || '')
    .split(/[\n,]/)
    .map((x) => x.trim())
    .filter(Boolean)
    .filter((x, idx, arr) => arr.indexOf(x) === idx);
}

function collectForm() {
  const rootPath = document.getElementById('org-root-path')?.value?.trim() || '';
  const recursive = !!document.getElementById('org-recursive')?.checked;
  const mode = document.getElementById('org-mode')?.value || 'fast';
  const categories = parseListInput(document.getElementById('org-categories')?.value || '');
  const excludedPatterns = parseListInput(document.getElementById('org-exclusions')?.value || '');
  const parallelism = Number(document.getElementById('org-parallelism')?.value || 5);

  return {
    rootPath,
    recursive,
    mode,
    categories: categories.length ? categories : [...DEFAULT_CATEGORIES],
    excludedPatterns: excludedPatterns.length ? excludedPatterns : [...DEFAULT_EXCLUSIONS],
    parallelism: Number.isFinite(parallelism) ? Math.max(1, Math.min(20, Math.floor(parallelism))) : 5,
  };
}

function persistForm(data) {
  setPersisted(PERSIST_KEYS.rootPath, data.rootPath);
  setPersisted(PERSIST_KEYS.recursive, data.recursive);
  setPersisted(PERSIST_KEYS.mode, data.mode);
  setPersisted(PERSIST_KEYS.categories, data.categories);
  setPersisted(PERSIST_KEYS.exclusions, data.excludedPatterns);
  setPersisted(PERSIST_KEYS.parallelism, data.parallelism);
}

function restoreDefaults() {
  return {
    rootPath: getPersisted(PERSIST_KEYS.rootPath, ''),
    recursive: getPersisted(PERSIST_KEYS.recursive, true),
    mode: getPersisted(PERSIST_KEYS.mode, 'fast'),
    categories: getPersisted(PERSIST_KEYS.categories, DEFAULT_CATEGORIES),
    excludedPatterns: getPersisted(PERSIST_KEYS.exclusions, DEFAULT_EXCLUSIONS),
    parallelism: getPersisted(PERSIST_KEYS.parallelism, 5),
  };
}

function escapeHtml(value) {
  const div = document.createElement('div');
  div.textContent = String(value ?? '');
  return div.innerHTML;
}

function setStatusText(snapshot) {
  const el = document.getElementById('org-status');
  if (!el) return;
  if (!snapshot) {
    el.textContent = t('organizer.status_idle');
    return;
  }

  const statusMap = {
    idle: t('organizer.status_idle'),
    scanning: t('organizer.status_scanning'),
    classifying: t('organizer.status_classifying'),
    stopped: t('organizer.status_stopped'),
    completed: t('organizer.status_completed'),
    moving: t('organizer.status_moving'),
    done: t('organizer.status_done'),
    error: t('organizer.status_error'),
  };

  el.textContent = statusMap[snapshot.status] || snapshot.status;
}

function renderCapability(snapshot) {
  const modelEl = document.getElementById('org-model-name');
  const mmEl = document.getElementById('org-mm-badge');
  if (!modelEl || !mmEl) return;

  const model = snapshot?.selectedModel || latestCapability?.selectedModel || '-';
  const supports = typeof snapshot?.supportsMultimodal === 'boolean'
    ? snapshot.supportsMultimodal
    : latestCapability?.supportsMultimodal;

  modelEl.textContent = model;

  mmEl.classList.remove('badge-success', 'badge-warning', 'badge-danger');
  if (supports === true) {
    mmEl.textContent = t('organizer.multimodal_supported');
    mmEl.classList.add('badge-success');
  } else if (supports === false) {
    mmEl.textContent = t('organizer.multimodal_not_supported');
    mmEl.classList.add('badge-warning');
  } else {
    mmEl.textContent = t('organizer.multimodal_unknown');
    mmEl.classList.add('badge-danger');
  }
}
function renderPreview(snapshot) {
  const tbody = document.getElementById('org-preview-body');
  const empty = document.getElementById('org-preview-empty');
  if (!tbody || !empty) return;

  const preview = snapshot?.preview || [];
  const resultsMap = new Map((snapshot?.results || []).map((x) => [x.path, x]));

  if (!preview.length) {
    tbody.innerHTML = '';
    empty.style.display = '';
    return;
  }

  empty.style.display = 'none';

  tbody.innerHTML = preview.map((item, idx) => {
    const row = resultsMap.get(item.sourcePath);
    const degraded = row?.degraded ? `<span class="badge badge-warning">${t('organizer.degraded')}</span>` : '';

    return `
      <tr style="animation: slideUp 0.2s var(--ease-out) ${Math.min(idx * 0.01, 0.3)}s both;">
        <td>
          <div class="file-name">${escapeHtml(row?.name || '')}</div>
          <div class="file-path">${escapeHtml(item.sourcePath)}</div>
        </td>
        <td><span class="badge badge-info">${escapeHtml(item.category)}</span>${degraded}</td>
        <td><div class="file-path">${escapeHtml(item.targetPath)}</div></td>
      </tr>
    `;
  }).join('');
}

function renderDegradedPanel(snapshot) {
  const tbody = document.getElementById('org-degraded-body');
  const empty = document.getElementById('org-degraded-empty');
  const count = document.getElementById('org-degraded-count');
  if (!tbody || !empty || !count) return;

  const rows = (snapshot?.results || []).filter((x) => x.degraded);
  count.textContent = String(rows.length);

  if (!rows.length) {
    tbody.innerHTML = '';
    empty.style.display = '';
    return;
  }

  empty.style.display = 'none';
  tbody.innerHTML = rows.map((row, idx) => {
    const reason = Array.isArray(row.warnings) && row.warnings.length
      ? row.warnings.join(' | ')
      : t('organizer.degraded_reason_unknown');

    return `
      <tr style="animation: slideUp 0.2s var(--ease-out) ${Math.min(idx * 0.01, 0.3)}s both;">
        <td>
          <div class="file-name">${escapeHtml(row.name || '')}</div>
          <div class="file-path">${escapeHtml(row.path || '')}</div>
        </td>
        <td><div class="file-purpose">${escapeHtml(reason)}</div></td>
      </tr>
    `;
  }).join('');
}
function updateStats(snapshot) {
  const total = snapshot?.totalFiles || 0;
  const done = snapshot?.processedFiles || 0;
  const token = snapshot?.tokenUsage?.total || 0;
  const degraded = (snapshot?.results || []).filter((x) => x.degraded).length;

  const totalEl = document.getElementById('org-total');
  const doneEl = document.getElementById('org-done');
  const tokenEl = document.getElementById('org-token');
  const degEl = document.getElementById('org-degraded');

  if (totalEl) totalEl.textContent = String(total);
  if (doneEl) doneEl.textContent = String(done);
  if (tokenEl) tokenEl.textContent = token.toLocaleString();
  if (degEl) degEl.textContent = String(degraded);

  const pct = total > 0 ? ((done / total) * 100).toFixed(1) : '0.0';
  const fill = document.getElementById('org-progress-fill');
  const pctEl = document.getElementById('org-progress-pct');
  if (fill) fill.style.width = `${pct}%`;
  if (pctEl) pctEl.textContent = `${pct}%`;
}

function updateButtons(snapshot) {
  const status = snapshot?.status || 'idle';
  const startBtn = document.getElementById('org-start-btn');
  const stopBtn = document.getElementById('org-stop-btn');
  const applyBtn = document.getElementById('org-apply-btn');
  const rollbackBtn = document.getElementById('org-rollback-btn');

  if (startBtn) {
    const running = status === 'scanning' || status === 'classifying' || status === 'moving';
    startBtn.disabled = running;
  }

  if (stopBtn) {
    const stoppable = status === 'scanning' || status === 'classifying';
    stopBtn.disabled = !stoppable;
  }

  if (applyBtn) {
    applyBtn.disabled = !(status === 'completed' || status === 'done');
  }

  if (rollbackBtn) {
    rollbackBtn.disabled = !getPersisted(PERSIST_KEYS.lastJobId, null);
  }
}

function refreshView(snapshot) {
  latestSnapshot = snapshot || null;
  setStatusText(snapshot);
  renderCapability(snapshot);
  updateStats(snapshot);
  renderPreview(snapshot);
  renderDegradedPanel(snapshot);
  updateButtons(snapshot);
}

function closeActiveSSE() {
  if (activeEventSource) {
    activeEventSource.close();
    activeEventSource = null;
  }
}

function connectTaskStream(taskId) {
  closeActiveSSE();
  activeEventSource = connectOrganizeStream(taskId, {
    onProgress: (snap) => {
      refreshView(snap);
    },
    onFileDone: () => {
      // no-op, progress snapshot already contains latest aggregate
    },
    onDone: (snap) => {
      refreshView(snap);
      showToast(t('organizer.toast_classify_done'), 'success');
    },
    onStopped: (snap) => {
      refreshView(snap);
    },
    onError: (err) => {
      showToast(`${t('organizer.toast_failed')}${err?.message || ''}`, 'error');
    },
  });
}

async function handleBrowse() {
  const btn = document.getElementById('org-browse-btn');
  if (!btn) return;

  btn.disabled = true;
  btn.textContent = t('organizer.browsing');
  try {
    const result = await browseFolder();
    if (!result.cancelled && result.path) {
      const input = document.getElementById('org-root-path');
      if (input) input.value = result.path;
      const data = collectForm();
      persistForm(data);
    }
  } catch (err) {
    showToast(`${t('organizer.toast_failed')}${err.message}`, 'error');
  } finally {
    btn.disabled = false;
    btn.textContent = t('settings.browse');
  }
}

async function handleSuggest() {
  const form = collectForm();
  if (!form.rootPath) {
    showToast(t('organizer.path_required'), 'error');
    return;
  }

  const btn = document.getElementById('org-suggest-btn');
  if (btn) {
    btn.disabled = true;
    btn.innerHTML = `<span class="spinner"></span> ${t('organizer.suggesting')}`;
  }

  try {
    const resp = await suggestOrganizeCategories({
      rootPath: form.rootPath,
      recursive: form.recursive,
      excludedPatterns: form.excludedPatterns,
      manualCategories: form.categories,
    });

    const categories = resp?.suggestedCategories || form.categories;
    const textArea = document.getElementById('org-categories');
    if (textArea) {
      textArea.value = categories.join('\n');
    }

    persistForm({ ...form, categories });
    showToast(t('organizer.toast_suggest_done'), 'success');
  } catch (err) {
    showToast(`${t('organizer.toast_failed')}${err.message}`, 'error');
  } finally {
    if (btn) {
      btn.disabled = false;
      btn.textContent = t('organizer.suggest_categories');
    }
  }
}

async function handleStart() {
  const form = collectForm();
  if (!form.rootPath) {
    showToast(t('organizer.path_required'), 'error');
    return;
  }

  if (form.mode === 'deep') {
    showToast(t('organizer.deep_warning'), 'info');
  }

  persistForm(form);

  const btn = document.getElementById('org-start-btn');
  if (btn) {
    btn.disabled = true;
    btn.innerHTML = `<span class="spinner"></span> ${t('organizer.starting')}`;
  }

  try {
    const result = await startOrganize(form);
    activeTaskId = result.taskId;
    latestCapability = {
      selectedModel: result.selectedModel,
      supportsMultimodal: result.supportsMultimodal,
    };
    renderCapability();
    setPersisted('wipeout.organizer.global.last_task_id.v1', activeTaskId);
    connectTaskStream(activeTaskId);
    showToast(t('organizer.toast_started'), 'success');
  } catch (err) {
    showToast(`${t('organizer.toast_failed')}${err.message}`, 'error');
  } finally {
    if (btn) {
      btn.disabled = false;
      btn.textContent = t('organizer.start');
    }
  }
}

async function handleApply() {
  if (!activeTaskId) {
    showToast(t('organizer.no_task'), 'error');
    return;
  }

  if (!confirm(t('organizer.confirm_apply'))) {
    return;
  }

  const btn = document.getElementById('org-apply-btn');
  if (btn) {
    btn.disabled = true;
    btn.innerHTML = `<span class="spinner"></span> ${t('organizer.applying')}`;
  }

  try {
    const result = await applyOrganize(activeTaskId);
    const jobId = result?.manifest?.jobId;
    if (jobId) {
      setPersisted(PERSIST_KEYS.lastJobId, jobId);
    }

    const summary = result?.manifest?.summary;
    if (summary) {
      showToast(
        t('organizer.toast_apply_done') + ` (${summary.moved}/${summary.total})`,
        summary.failed > 0 ? 'info' : 'success'
      );
    } else {
      showToast(t('organizer.toast_apply_done'), 'success');
    }

    const snapshot = await getOrganizeResult(activeTaskId);
    refreshView(snapshot);
  } catch (err) {
    showToast(`${t('organizer.toast_failed')}${err.message}`, 'error');
  } finally {
    if (btn) {
      btn.disabled = false;
      btn.textContent = t('organizer.apply_move');
    }
    updateButtons(latestSnapshot);
  }
}

async function handleStop() {
  if (!activeTaskId) {
    showToast(t('organizer.no_task'), 'error');
    return;
  }

  const btn = document.getElementById('org-stop-btn');
  if (btn) {
    btn.disabled = true;
    btn.innerHTML = `<span class="spinner"></span> ${t('organizer.stopping')}`;
  }

  try {
    await stopOrganize(activeTaskId);
    showToast(t('organizer.toast_stopped'), 'info');
  } catch (err) {
    showToast(`${t('organizer.toast_stop_failed')}${err.message}`, 'error');
  } finally {
    if (btn) {
      btn.textContent = t('organizer.stop');
    }
    updateButtons(latestSnapshot);
  }
}

async function handleRollback() {
  const jobId = getPersisted(PERSIST_KEYS.lastJobId, null);
  if (!jobId) {
    showToast(t('organizer.no_rollback_job'), 'error');
    return;
  }

  if (!confirm(t('organizer.confirm_rollback'))) {
    return;
  }

  const btn = document.getElementById('org-rollback-btn');
  if (btn) {
    btn.disabled = true;
    btn.innerHTML = `<span class="spinner"></span> ${t('organizer.rolling_back')}`;
  }

  try {
    const result = await rollbackOrganize(jobId);
    const summary = result?.rollback?.summary;
    if (summary) {
      showToast(
        `${t('organizer.toast_rollback_done')} (${summary.rolledBack}/${summary.total})`,
        summary.failed > 0 ? 'info' : 'success'
      );
    } else {
      showToast(t('organizer.toast_rollback_done'), 'success');
    }
  } catch (err) {
    showToast(`${t('organizer.toast_failed')}${err.message}`, 'error');
  } finally {
    if (btn) {
      btn.disabled = false;
      btn.textContent = t('organizer.rollback');
    }
  }
}

function bindPersistenceListeners() {
  ['org-root-path', 'org-recursive', 'org-mode', 'org-categories', 'org-exclusions', 'org-parallelism'].forEach((id) => {
    const el = document.getElementById(id);
    if (!el) return;
    const eventName = id === 'org-recursive' ? 'change' : 'input';
    el.addEventListener(eventName, () => {
      persistForm(collectForm());
    });
  });
}

export async function renderOrganizer(container) {
  const defaults = restoreDefaults();

  container.innerHTML = `
    <div class="page-header animate-in">
      <h1 class="page-title">${t('organizer.title')}</h1>
      <p class="page-subtitle">${t('organizer.subtitle')}</p>
    </div>

    <div class="card animate-in mb-24" style="animation-delay: 0.05s">
      <div class="card-header">
        <h2 class="card-title">${t('organizer.config')}</h2>
        <span class="badge badge-info" id="org-status">${t('organizer.status_idle')}</span>
      </div>

      <div class="form-group">
        <label class="form-label">${t('organizer.root_path')}</label>
        <div style="display:flex;gap:8px;align-items:center;">
          <input id="org-root-path" class="form-input" style="flex:1;" value="${escapeHtml(defaults.rootPath)}" placeholder="C:\\Users\\..." />
          <button id="org-browse-btn" class="btn btn-secondary" type="button">${t('settings.browse')}</button>
        </div>
      </div>

      <div class="grid-2">
        <div class="form-group">
          <label class="form-label">${t('organizer.scope')}</label>
          <label style="display:flex;align-items:center;gap:8px;">
            <input id="org-recursive" type="checkbox" ${defaults.recursive ? 'checked' : ''} />
            <span>${t('organizer.scope_recursive')}</span>
          </label>
        </div>
        <div class="form-group">
          <label class="form-label">${t('organizer.mode')}</label>
          <select id="org-mode" class="form-input">
            <option value="fast" ${defaults.mode === 'fast' ? 'selected' : ''}>${t('organizer.mode_fast')}</option>
            <option value="balanced" ${defaults.mode === 'balanced' ? 'selected' : ''}>${t('organizer.mode_balanced')}</option>
            <option value="deep" ${defaults.mode === 'deep' ? 'selected' : ''}>${t('organizer.mode_deep')}</option>
          </select>
        </div>
      </div>

      <div class="grid-2">
        <div class="form-group">
          <label class="form-label">${t('organizer.parallelism')}</label>
          <input id="org-parallelism" type="number" min="1" max="20" class="form-input no-spin" value="${Number(defaults.parallelism) || 5}" />
        </div>
        <div class="form-group">
          <label class="form-label">${t('organizer.cost_notice')}</label>
          <div class="form-hint" id="org-deep-warning">${t('organizer.deep_hint')}</div>
        </div>
      </div>

      <div class="grid-2">
        <div class="form-group">
          <label class="form-label">${t('organizer.current_model')}</label>
          <div id="org-model-name" class="form-hint mono">-</div>
        </div>
        <div class="form-group">
          <label class="form-label">${t('organizer.multimodal')}</label>
          <span id="org-mm-badge" class="badge badge-danger">${t('organizer.multimodal_unknown')}</span>
        </div>
      </div>

      <div class="grid-2">
        <div class="form-group">
          <label class="form-label">${t('organizer.categories')}</label>
          <textarea id="org-categories" class="form-input" rows="8">${escapeHtml((defaults.categories || []).join('\n'))}</textarea>
          <div class="form-hint">${t('organizer.categories_hint')}</div>
        </div>
        <div class="form-group">
          <label class="form-label">${t('organizer.exclusions')}</label>
          <textarea id="org-exclusions" class="form-input" rows="8">${escapeHtml((defaults.excludedPatterns || []).join('\n'))}</textarea>
          <div class="form-hint">${t('organizer.exclusions_hint')}</div>
        </div>
      </div>

      <div class="flex items-center gap-16">
        <button id="org-suggest-btn" class="btn btn-ghost" type="button">${t('organizer.suggest_categories')}</button>
        <button id="org-start-btn" class="btn btn-primary" type="button">${t('organizer.start')}</button>
        <button id="org-stop-btn" class="btn btn-danger" type="button" disabled>${t('organizer.stop')}</button>
        <button id="org-apply-btn" class="btn btn-success" type="button" disabled>${t('organizer.apply_move')}</button>
        <button id="org-rollback-btn" class="btn btn-secondary" type="button">${t('organizer.rollback')}</button>
      </div>
    </div>

    <div class="stats-grid animate-in" style="animation-delay: 0.1s">
      <div class="stat-card">
        <span class="stat-label">${t('organizer.total_files')}</span>
        <span class="stat-value" id="org-total">0</span>
      </div>
      <div class="stat-card">
        <span class="stat-label">${t('organizer.done_files')}</span>
        <span class="stat-value success" id="org-done">0</span>
      </div>
      <div class="stat-card">
        <span class="stat-label">Token</span>
        <span class="stat-value warning" id="org-token">0</span>
      </div>
      <div class="stat-card">
        <span class="stat-label">${t('organizer.degraded')}</span>
        <span class="stat-value danger" id="org-degraded">0</span>
      </div>
    </div>

    <div class="card animate-in mb-24" style="animation-delay:0.12s;">
      <div class="card-header">
        <h2 class="card-title">${t('organizer.progress')}</h2>
        <span id="org-progress-pct" class="badge badge-info">0.0%</span>
      </div>
      <div class="progress-bar">
        <div id="org-progress-fill" class="progress-fill" style="width:0%;"></div>
      </div>
    </div>

    <div class="card animate-in" style="animation-delay: 0.15s; padding: 0; overflow: hidden;">
      <div class="card-header" style="padding: 16px 20px; margin-bottom: 0; border-bottom: 1px solid var(--bg-glass-border);">
        <h2 class="card-title">${t('organizer.preview_title')}</h2>
      </div>
      <div style="overflow-x:auto;">
        <table class="data-table">
          <thead>
            <tr>
              <th>${t('organizer.source')}</th>
              <th style="width: 180px;">${t('organizer.category')}</th>
              <th>${t('organizer.target')}</th>
            </tr>
          </thead>
          <tbody id="org-preview-body"></tbody>
        </table>
      </div>
      <div id="org-preview-empty" class="empty-state" style="padding: 32px;">
        <div class="empty-state-icon">📁</div>
        <div class="empty-state-text">${t('organizer.preview_empty')}</div>
      </div>
    </div>

    <div class="card animate-in mt-24" style="animation-delay: 0.18s; padding: 0; overflow: hidden;">
      <div class="card-header" style="padding: 16px 20px; margin-bottom: 0; border-bottom: 1px solid var(--bg-glass-border);">
        <h2 class="card-title">${t('organizer.degraded_panel_title')}</h2>
        <span class="badge badge-warning">${t('organizer.degraded')}: <span id="org-degraded-count">0</span></span>
      </div>
      <div style="overflow-x:auto;">
        <table class="data-table">
          <thead>
            <tr>
              <th>${t('organizer.source')}</th>
              <th>${t('organizer.degraded_reason')}</th>
            </tr>
          </thead>
          <tbody id="org-degraded-body"></tbody>
        </table>
      </div>
      <div id="org-degraded-empty" class="empty-state" style="padding: 24px;">
        <div class="empty-state-text">${t('organizer.degraded_empty')}</div>
      </div>
    </div>
  `;

  document.getElementById('org-browse-btn')?.addEventListener('click', handleBrowse);
  document.getElementById('org-suggest-btn')?.addEventListener('click', handleSuggest);
  document.getElementById('org-start-btn')?.addEventListener('click', handleStart);
  document.getElementById('org-stop-btn')?.addEventListener('click', handleStop);
  document.getElementById('org-apply-btn')?.addEventListener('click', handleApply);
  document.getElementById('org-rollback-btn')?.addEventListener('click', handleRollback);

  bindPersistenceListeners();

  try {
    latestCapability = await getOrganizeCapability();
  } catch {
    latestCapability = null;
  }
  renderCapability();

  // Try to reconnect to a running task.
  const lastTaskId = getPersisted('wipeout.organizer.global.last_task_id.v1', null);
  if (lastTaskId) {
    try {
      const snapshot = await getOrganizeResult(lastTaskId);
      activeTaskId = lastTaskId;
      refreshView(snapshot);

      if (['scanning', 'classifying', 'moving'].includes(snapshot.status)) {
        connectTaskStream(lastTaskId);
      }
    } catch {
      refreshView(null);
    }
  } else {
    refreshView(null);
  }
}







