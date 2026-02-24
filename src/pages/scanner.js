/**
 * src/pages/scanner.js
 * 扫描控制面板 — 实时进度可视化、层级展示、统计仪表盘
 */
import { getSettings, startScan, stopScan, connectScanStream, getActiveScan } from '../utils/api.js';
import { formatSize } from '../utils/storage.js';
import * as storage from '../utils/storage.js';
import { showToast } from '../main.js';
import { t } from '../utils/i18n.js';

let activeTaskId = null;
let activeEventSource = null;
let logEntries = [];

export async function renderScanner(container) {
  // Restore last scan state
  const lastScan = storage.get('lastScan', null);

  container.innerHTML = `
    <div class="page-header animate-in">
      <h1 class="page-title">${t('scanner.title')}</h1>
      <p class="page-subtitle">${t('scanner.subtitle')}</p>
    </div>

    <!-- Stats Dashboard -->
    <div class="stats-grid animate-in" style="animation-delay: 0.05s">
      <div class="stat-card">
        <span class="stat-label">${t('scanner.progress_scan')}</span>
        <span class="stat-value accent" id="stat-status">${t('scanner.not_set')}</span>
      </div>
      <div class="stat-card">
        <span class="stat-label">${t('results.files_count')}</span>
        <span class="stat-value" id="stat-scanned">0</span>
      </div>
      <div class="stat-card">
        <span class="stat-label">${t('results.safe_to_clean')}</span>
        <span class="stat-value success" id="stat-cleanable">0 B</span>
      </div>
      <div class="stat-card">
        <span class="stat-label">Token 消耗</span>
        <span class="stat-value warning" id="stat-tokens">0</span>
      </div>
    </div>

    <!-- Progress -->
    <div class="card animate-in mb-24" style="animation-delay: 0.1s">
      <div class="card-header">
        <h2 class="card-title">${t('scanner.progress_scan')}</h2>
        <div style="display: flex; gap: 8px; align-items: center;">
          <span id="progress-pct" class="badge badge-info">0.0%</span>
        </div>
      </div>
      <div class="progress-bar mb-16">
        <div class="progress-fill" id="progress-fill" style="width: 0%"></div>
      </div>
      <div id="scan-breadcrumb" class="scan-breadcrumb" style="display: none;">
        <span class="log-icon">📍</span>
        <span id="breadcrumb-path" class="crumb">—</span>
        <span class="separator">|</span>
        <span id="breadcrumb-depth">${t('scanner.not_set')}</span>
      </div>
    </div>

    <!-- Activity Log -->
    <div class="card animate-in mb-24" style="animation-delay: 0.15s">
      <div class="card-header">
        <h2 class="card-title">${t('scanner.activity_log')}</h2>
        <button id="clear-log-btn" class="btn btn-ghost" style="padding: 6px 12px; font-size: 0.75rem;">
          Clear/清空
        </button>
      </div>
      <div class="scan-activity" id="scan-activity-hidden" style="display: none;">
        <div class="scan-log" id="scan-log"></div>
      </div>
      <div id="scan-empty" class="empty-state" style="padding: 30px;">
        <div class="empty-state-icon">🧹</div>
        <div class="empty-state-text">${t('scanner.prepare')}</div>
        <div class="empty-state-hint">${t('scanner.not_set')}</div>
      </div>
    </div>

    <!-- Controls -->
    <div class="flex items-center justify-between animate-in" style="animation-delay: 0.2s">
      <div>
        <span id="scan-path-display" class="form-hint mono"></span>
      </div>
      <div class="flex gap-16">
        <button id="stop-btn" class="btn btn-danger" style="display: none;">
          ${t('scanner.stop')}
        </button>
        <button id="start-btn" class="btn btn-primary btn-lg">
          ${t('scanner.start')}
        </button>
      </div>
    </div>
  `;

  // Load settings to show scan path
  try {
    const settings = await getSettings();
    const pathDisplay = document.getElementById('scan-path-display');
    if (settings.scanPath) {
      pathDisplay.textContent = `目标: ${settings.scanPath}`;
    } else {
      pathDisplay.innerHTML = `⚠️ <a href="#/settings" style="color: var(--accent-warning);">${t('scanner.path_not_configured')}</a>`;
    }
  } catch { }

  // Restore last scan stats
  if (lastScan) {
    updateStats(lastScan);
  }

  // Event bindings
  document.getElementById('start-btn').addEventListener('click', handleStart);
  document.getElementById('stop-btn').addEventListener('click', handleStop);
  document.getElementById('clear-log-btn').addEventListener('click', () => {
    logEntries = [];
    document.getElementById('scan-log').innerHTML = '';
  });

  // Restore active scan state (page switch or page refresh)
  if (activeTaskId) {
    // Module-level state still exists (page switch, not full refresh)
    restoreActiveState();
  } else {
    // Check server for active tasks (handles page refresh scenario)
    try {
      const activeTasks = await getActiveScan();
      if (activeTasks.length > 0) {
        const task = activeTasks[0];
        activeTaskId = task.taskId;
        // Update stats from server snapshot
        updateStats(task);
        storage.set('lastScan', task);
        restoreActiveState();
      }
    } catch { /* ignore */ }
  }
}

/**
 * Restore UI state for an active scan task.
 * Rebuilds log entries, toggles buttons, and reconnects SSE.
 */
function restoreActiveState() {
  const startBtn = document.getElementById('start-btn');
  const stopBtn = document.getElementById('stop-btn');
  const activityEl = document.getElementById('scan-activity-hidden');
  const emptyEl = document.getElementById('scan-empty');
  const breadcrumb = document.getElementById('scan-breadcrumb');

  // Toggle buttons
  if (startBtn) startBtn.style.display = 'none';
  if (stopBtn) stopBtn.style.display = '';
  if (activityEl) activityEl.style.display = '';
  if (emptyEl) emptyEl.style.display = 'none';
  if (breadcrumb) breadcrumb.style.display = '';

  // Rebuild log entries from cached array
  const log = document.getElementById('scan-log');
  if (log && logEntries.length > 0) {
    log.innerHTML = '';
    for (const entry of logEntries) {
      const el = document.createElement('div');
      el.className = `scan-log-entry ${entry.type}`;
      el.innerHTML = `
            <span class="log-icon">${entry.type === 'found' ? '●' : entry.type === 'analyzing' ? '◆' : '▸'}</span>
            <span style="color: var(--text-muted); margin-right: 6px;">[${entry.time}]</span>
            <span>${entry.text}</span>
            `;
      log.appendChild(el);
    }
    log.scrollTop = log.scrollHeight;
  }

  // Restore stats from localStorage
  const lastScan = storage.get('lastScan', null);
  if (lastScan) {
    updateStats(lastScan);
    // Restore breadcrumb
    const pathEl = document.getElementById('breadcrumb-path');
    const depthEl = document.getElementById('breadcrumb-depth');
    if (pathEl && lastScan.currentPath) pathEl.textContent = lastScan.currentPath;
    if (depthEl) depthEl.textContent = `Depth ${lastScan.currentDepth || 0}`;
    // Restore status text
    const statusEl = document.getElementById('stat-status');
    if (statusEl) {
      const statusMap = { scanning: t('scanner.scanning'), analyzing: t('scanner.analyzing'), idle: t('scanner.not_set'), done: t('scanner.completed').split('!')[0], stopped: t('scanner.stopped'), error: t('toast.error') };
      statusEl.textContent = statusMap[lastScan.status] || lastScan.status || t('scanner.scanning');
    }
    // Restore progress bar
    const scanPct = lastScan.totalEntries > 0
      ? Math.min(100, (lastScan.processedEntries || 0) / lastScan.totalEntries * 100)
      : 0;
    const fill = document.getElementById('progress-fill');
    const label = document.getElementById('progress-pct');
    if (fill) fill.style.width = `${scanPct}%`;
    if (label) label.textContent = `${scanPct.toFixed(1)}%`;
  }

  // Reconnect SSE (close old connection first)
  if (activeEventSource) {
    activeEventSource.close();
    activeEventSource = null;
  }
  activeEventSource = connectScanStream(activeTaskId, {
    onProgress: handleProgress,
    onFound: handleFound,
    onAgentCall: handleAgentCall,
    onAgentResponse: handleAgentResponse,
    onDone: handleDone,
    onError: handleError,
    onStopped: handleStopped,
  });
}

async function handleStart() {
  const startBtn = document.getElementById('start-btn');
  const stopBtn = document.getElementById('stop-btn');

  try {
    const settings = await getSettings();
    if (!settings.scanPath) {
      showToast(t('scanner.path_not_configured'), 'error');
      return;
    }
    if (!settings.apiKey) {
      showToast(t('settings.api_key_placeholder'), 'error');
      return;
    }

    startBtn.disabled = true;
    startBtn.innerHTML = `<span class="spinner"></span> ${t('scanner.prepare')}`;

    const result = await startScan({
      targetPath: settings.scanPath,
      targetSizeGB: settings.targetSizeGB || 1,
      maxDepth: settings.maxDepth || 5,
    });

    activeTaskId = result.taskId;

    // Show activity area
    document.getElementById('scan-activity-hidden').style.display = '';
    document.getElementById('scan-empty').style.display = 'none';
    document.getElementById('scan-breadcrumb').style.display = '';

    startBtn.style.display = 'none';
    stopBtn.style.display = '';

    addLog('scanning', `${t('scanner.log_start')} [${activeTaskId}]`);

    // Connect SSE
    activeEventSource = connectScanStream(activeTaskId, {
      onProgress: handleProgress,
      onFound: handleFound,
      onAgentCall: handleAgentCall,
      onAgentResponse: handleAgentResponse,
      onDone: handleDone,
      onError: handleError,
      onStopped: handleStopped,
    });

  } catch (err) {
    showToast(t('scanner.toast_start_failed') + err.message, 'error');
    startBtn.disabled = false;
    startBtn.innerHTML = t('scanner.start');
  }
}

async function handleStop() {
  if (!activeTaskId) return;
  try {
    await stopScan(activeTaskId);
    showToast(t('scanner.stopped'), 'info');
  } catch (err) {
    showToast(t('scanner.toast_stop_failed') + err.message, 'error');
  }
}

function handleProgress(data) {
  updateStats(data);
  storage.set('lastScan', data);

  // Update breadcrumb
  const pathEl = document.getElementById('breadcrumb-path');
  const depthEl = document.getElementById('breadcrumb-depth');
  if (pathEl && data.currentPath) {
    pathEl.textContent = data.currentPath;
  }
  if (depthEl) {
    depthEl.textContent = `Depth ${data.currentDepth || 0}`;
  }

  // Update status text
  const statusEl = document.getElementById('stat-status');
  if (statusEl) {
    const statusMap = {
      scanning: '扫描中',
      analyzing: '分析中',
      idle: '就绪',
      done: '完成',
      stopped: '已停止',
      error: '错误',
    };
    statusEl.textContent = statusMap[data.status] || data.status;
  }

  // Progress bar — based on scanned/analyzed entries
  const scanPct = data.totalEntries > 0
    ? Math.min(100, (data.processedEntries || 0) / data.totalEntries * 100)
    : 0;
  const fill = document.getElementById('progress-fill');
  const label = document.getElementById('progress-pct');
  if (fill) fill.style.width = `${scanPct}%`;
  if (label) label.textContent = `${scanPct.toFixed(1)}%`;

  if (data.status === 'analyzing') {
    addLog('analyzing', `🧠 ${t('scanner.analyzing')}: ${data.currentPath}`);
  } else if (data.status === 'scanning') {
    addLog('scanning', `🔍 ${t('scanner.scanning')}: ${data.currentPath}`);
  }
}

function handleFound(item) {
  addLog('found', `✅ ${t('results.safe_to_clean')}: ${item.name} (${formatSize(item.size)}) — ${item.reason}`);
}

function handleDone(data) {
  updateStats(data);
  storage.set('lastScan', data);
  storage.set('scanResults', data.deletable);
  resetButtons();

  // Set progress to 100%
  const fill = document.getElementById('progress-fill');
  const label = document.getElementById('progress-pct');
  if (fill) fill.style.width = '100%';
  if (label) label.textContent = '100.0%';
  // Update status
  const statusEl = document.getElementById('stat-status');
  if (statusEl) statusEl.textContent = '完成';

  addLog('found', t('scanner.completed').replace('{count}', data.deletableCount));
  showToast(t('scanner.completed').replace('{count}', data.deletableCount), 'success');
}

function handleError(err) {
  resetButtons();
  addLog('analyzing', `❌ ${t('toast.error')}: ${err.message || t('toast.error')}`);
  showToast(t('toast.error'), 'error');
}

function handleStopped(data) {
  updateStats(data);
  storage.set('lastScan', data);
  storage.set('scanResults', data.deletable);
  resetButtons();
  addLog('scanning', `⏹ ${t('scanner.stopped')}`);
}

function resetButtons() {
  const startBtn = document.getElementById('start-btn');
  const stopBtn = document.getElementById('stop-btn');
  if (startBtn) {
    startBtn.style.display = '';
    startBtn.disabled = false;
    startBtn.innerHTML = t('scanner.start');
  }
  if (stopBtn) stopBtn.style.display = 'none';
  activeTaskId = null;
  if (activeEventSource) {
    activeEventSource.close();
    activeEventSource = null;
  }
}

function updateStats(data) {
  const el = (id) => document.getElementById(id);
  if (el('stat-scanned')) el('stat-scanned').textContent = data.scannedCount || 0;
  if (el('stat-cleanable')) el('stat-cleanable').textContent = formatSize(data.totalCleanable || 0);
  if (el('stat-tokens')) el('stat-tokens').textContent = (data.tokenUsage?.total || 0).toLocaleString();
}

function addLog(type, text) {
  const log = document.getElementById('scan-log');
  if (!log) return;

  const entry = document.createElement('div');
  entry.className = `scan-log-entry ${type}`;
  const time = new Date().toLocaleTimeString('zh-CN', { hour12: false });
  const iconMap = { found: '●', analyzing: '◆', agent_call: '⚡', agent_response: '🤖' };
  entry.innerHTML = `
    <span class="log-icon">${iconMap[type] || '▸'}</span>
    <span style="color: var(--text-muted); margin-right: 6px;">[${time}]</span>
    <span>${text}</span>
  `;
  log.appendChild(entry);
  log.scrollTop = log.scrollHeight;

  logEntries.push({ type, text, time });
  // Keep max 200 entries
  while (log.children.length > 200) {
    log.removeChild(log.firstChild);
  }
}

/**
 * Add a collapsible detail log entry (for agent call/response).
 */
function addDetailLog(type, summary, detailHtml) {
  const log = document.getElementById('scan-log');
  if (!log) return;

  const wrapper = document.createElement('div');
  wrapper.className = `scan-log-entry ${type}`;
  const time = new Date().toLocaleTimeString('zh-CN', { hour12: false });
  const iconMap = { agent_call: '⚡', agent_response: '🤖' };

  wrapper.innerHTML = `
    <span class="log-icon">${iconMap[type] || '◆'}</span>
    <div style="flex: 1; min-width: 0;">
      <div class="log-detail-header" style="cursor: pointer; user-select: none; display: flex; align-items: center; gap: 6px;">
        <span style="color: var(--text-muted); margin-right: 4px;">[${time}]</span>
        <span class="log-detail-arrow" style="transition: transform 0.2s; display: inline-block; font-size: 0.65rem;">▶</span>
        <span>${summary}</span>
      </div>
      <div class="log-detail-body" style="display: none; margin-top: 8px; padding: 10px 12px; background: rgba(0,0,0,0.35); border-radius: 6px; border: 1px solid rgba(255,255,255,0.06); font-size: 0.72rem; line-height: 1.7; word-break: break-all; white-space: pre-wrap; max-height: 600px; overflow-y: auto; color: var(--text-secondary);">
        ${detailHtml}
      </div>
    </div>
  `;

  // Toggle collapse
  const header = wrapper.querySelector('.log-detail-header');
  const body = wrapper.querySelector('.log-detail-body');
  const arrow = wrapper.querySelector('.log-detail-arrow');
  header.addEventListener('click', () => {
    const open = body.style.display !== 'none';
    body.style.display = open ? 'none' : 'block';
    arrow.style.transform = open ? 'rotate(0deg)' : 'rotate(90deg)';
  });

  log.appendChild(wrapper);
  log.scrollTop = log.scrollHeight;

  logEntries.push({ type, text: summary, time });
  while (log.children.length > 200) {
    log.removeChild(log.firstChild);
  }
}

/** Handle LLM tool call (before sending) */
function handleAgentCall(data) {
  const entryList = data.entries
    .map(e => `  • [${e.type}] ${e.name} (${formatSize(e.size)})`)
    .join('\n');

  const detailHtml = `
    <div style="margin-bottom: 8px;"><strong style="color: var(--accent-info);">📂 目录:</strong> ${escHtml(data.dirPath)}</div>
    <div style="margin-bottom: 8px;"><strong style="color: var(--accent-info);">📋 批次 #${data.batchIndex}</strong> (${data.batchSize} 个条目)</div>
    <div style="margin-bottom: 4px;"><strong style="color: var(--accent-info);">📁 发送条目:</strong></div>
    <div style="padding-left: 8px; border-left: 2px solid rgba(6, 182, 212, 0.3);">${escHtml(entryList)}</div>
  `;

  addDetailLog('agent_call', `⚡ 调用 LLM — 批次 #${data.batchIndex}，${data.batchSize} 个条目`, detailHtml);
}

/** Handle LLM response (after receiving) */
function handleAgentResponse(data) {
  const elapsed = (data.elapsed / 1000).toFixed(1);
  const classLabels = { safe_to_delete: '🗑️可删除', suspicious: '🔍待探查', keep: '✅保留' };
  const classStr = Object.entries(data.classifications || {})
    .map(([k, v]) => `${classLabels[k] || k}: ${v}`)
    .join('，');

  let detailSections = '';

  // Model & timing
  detailSections += `<div style="margin-bottom: 10px;">
    <strong style="color: var(--accent-warning);">🤖 模型:</strong> ${escHtml(data.model)} &nbsp;|&nbsp;
    <strong>⏱ 耗时:</strong> ${elapsed}s &nbsp;|&nbsp;
    <strong>🎯 Token:</strong> ${(data.tokenUsage?.total || 0).toLocaleString()}
  </div>`;

  // Classification summary
  if (classStr) {
    detailSections += `<div style="margin-bottom: 10px;">
      <strong style="color: var(--accent-success);">📊 分类结果:</strong> ${classStr}
    </div>`;
  }

  // Error
  if (data.error) {
    detailSections += `<div style="margin-bottom: 10px; color: var(--accent-danger);">
      <strong>❌ 错误:</strong> ${escHtml(data.error)}
    </div>`;
  }

  // Prompt
  if (data.userPrompt) {
    detailSections += `<div style="margin-bottom: 10px;">
      <strong style="color: var(--accent-info);">📝 Prompt:</strong>
      <div style="margin-top: 4px; padding: 8px; background: rgba(0,0,0,0.3); border-radius: 4px; max-height: 300px; overflow-y: auto;">${escHtml(data.userPrompt)}</div>
    </div>`;
  }

  // Reasoning / Thinking
  if (data.reasoning) {
    detailSections += `<div style="margin-bottom: 10px;">
      <strong style="color: var(--accent-warning);">💭 模型思考:</strong>
      <div style="margin-top: 4px; padding: 8px; background: rgba(245, 158, 11, 0.08); border: 1px solid rgba(245, 158, 11, 0.15); border-radius: 4px; max-height: 400px; overflow-y: auto;">${escHtml(data.reasoning)}</div>
    </div>`;
  }

  // Raw content
  if (data.rawContent) {
    const truncated = data.rawContent.length > 2000
      ? data.rawContent.slice(0, 2000) + '\n...（已截断）'
      : data.rawContent;
    detailSections += `<div>
      <strong style="color: var(--accent-primary);">📄 原始响应:</strong>
      <div style="margin-top: 4px; padding: 8px; background: rgba(0,0,0,0.3); border-radius: 4px; max-height: 400px; overflow-y: auto;">${escHtml(truncated)}</div>
    </div>`;
  }

  const statusIcon = data.error ? '❌' : '✅';
  addDetailLog(
    'agent_response',
    `${statusIcon} LLM 响应 — ${elapsed}s, ${data.resultsCount} 项 (${classStr || '无结果'})`,
    detailSections
  );
}

/** Escape HTML special characters */
function escHtml(str) {
  if (!str) return '';
  return str
    .replace(/&/g, '&amp;')
    .replace(/</g, '&lt;')
    .replace(/>/g, '&gt;')
    .replace(/"/g, '&quot;')
    .replace(/\n/g, '<br>');
}
