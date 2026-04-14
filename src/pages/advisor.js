import {
  advisorCardAction,
  advisorMessageSend,
  advisorSessionGet,
  advisorSessionStart,
  browseFolder,
  listScanHistory,
} from '../utils/api.js';
import { showToast } from '../main.js';
import { getLang } from '../utils/i18n.js';
import { formatSize } from '../utils/storage.js';
import { scanTaskController } from '../utils/scan-task-controller.js';

const PERSIST_KEYS = {
  rootPath: 'wipeout.advisor.global.root_path.v2',
  sessionId: 'wipeout.advisor.global.session_id.v2',
  messageDraft: 'wipeout.advisor.global.message_draft.v2',
  handoff: 'wipeout.advisor.global.handoff.v1',
};

const QUICK_SCAN_LIMIT = 8;

let pageContainer = null;
let state = createInitialState();

function text(zh, en) {
  return getLang() === 'en' ? en : zh;
}

function createInitialState() {
  return {
    rootPath: resolveInitialRootPath(),
    sessionId: getPersisted(PERSIST_KEYS.sessionId, ''),
    messageDraft: getPersisted(PERSIST_KEYS.messageDraft, ''),
    sessionData: null,
    quickScans: [],
    loading: false,
    sending: false,
    acting: false,
  };
}

function getPersisted(key, fallback) {
  try {
    const raw = localStorage.getItem(key);
    return raw == null ? fallback : JSON.parse(raw);
  } catch {
    return fallback;
  }
}

function setPersisted(key, value) {
  try {
    localStorage.setItem(key, JSON.stringify(value));
  } catch {
    // ignore
  }
}

function removePersisted(key) {
  try {
    localStorage.removeItem(key);
  } catch {
    // ignore
  }
}

function escapeHtml(value) {
  const div = document.createElement('div');
  div.textContent = String(value ?? '');
  return div.innerHTML;
}

function getCurrentScanSnapshot() {
  return scanTaskController.getState()?.snapshot || null;
}

function takeAdvisorHandoff() {
  const handoff = getPersisted(PERSIST_KEYS.handoff, null);
  removePersisted(PERSIST_KEYS.handoff);
  return handoff && typeof handoff === 'object' ? handoff : null;
}

function resolveInitialRootPath() {
  const handoff = takeAdvisorHandoff();
  if (handoff?.rootPath) {
    setPersisted(PERSIST_KEYS.rootPath, handoff.rootPath);
    window.__wipeoutAdvisorHandoff = handoff;
    return String(handoff.rootPath).trim();
  }
  const persisted = String(getPersisted(PERSIST_KEYS.rootPath, '') || '').trim();
  if (persisted) return persisted;
  const snapshot = getCurrentScanSnapshot();
  return String(snapshot?.targetPath || snapshot?.rootPath || '').trim();
}

function getPendingHandoff() {
  const value = window.__wipeoutAdvisorHandoff;
  if (!value) return null;
  window.__wipeoutAdvisorHandoff = null;
  return value;
}

function formatDateTime(value) {
  if (!value) return '-';
  const date = new Date(value);
  if (Number.isNaN(date.getTime())) return String(value);
  return date.toLocaleString(getLang() === 'en' ? 'en-US' : 'zh-CN');
}

function getWorkflowStage() {
  return state.sessionData?.session?.workflowStage || state.sessionData?.workflowStage || 'understand';
}

function getStageLabel() {
  const stage = getWorkflowStage();
  if (stage === 'execute_ready') return text('可执行', 'Ready to Execute');
  if (stage === 'preview_ready') return text('可预览', 'Ready to Preview');
  return text('理解中', 'Understanding');
}

function summarizeCard(card) {
  if (!card || typeof card !== 'object') return '';
  if (card.cardType === 'tree') {
    const count = Number(card?.body?.stats?.itemCount || 0);
    return text(`当前树覆盖 ${count} 个项目。`, `Tree covers ${count} items.`);
  }
  if (card.cardType === 'plan_preview') {
    const summary = card?.body?.summary || {};
    return text(
      `共 ${summary.total || 0} 项，可执行 ${summary.canExecute || 0} 项。`,
      `${summary.total || 0} items, ${summary.canExecute || 0} executable.`,
    );
  }
  if (card.cardType === 'execution_result') {
    const summary = card?.body?.result?.summary || {};
    return text(
      `总计 ${summary.total ?? '-'}，失败 ${summary.failed || 0}。`,
      `Total ${summary.total ?? '-'}, failed ${summary.failed || 0}.`,
    );
  }
  return String(card?.body?.summary || card?.body?.message || '').trim();
}

function renderTreeNode(node) {
  if (!node || typeof node !== 'object') return '';
  const children = Array.isArray(node.children) ? node.children : [];
  return `
    <li>
      <span>${escapeHtml(node.name || '-')}</span>
      <span class="advisor-tree-count">${escapeHtml(node.itemCount || 0)}</span>
      ${children.length ? `<ul>${children.slice(0, 12).map(renderTreeNode).join('')}</ul>` : ''}
    </li>
  `;
}

function renderPlanEntries(entries) {
  return entries.slice(0, 10).map((entry) => `
    <div class="advisor-entry-row">
      <div>
        <div>${escapeHtml(entry?.name || entry?.sourcePath || '-')}</div>
        <div class="form-hint">${escapeHtml(entry?.sourcePath || '-')}</div>
      </div>
      <span class="badge ${entry?.canExecute ? 'badge-success' : 'badge-warning'}">${escapeHtml(entry?.action || '-')}</span>
    </div>
  `).join('');
}

function renderCard(card) {
  const actions = Array.isArray(card?.actions) ? card.actions : [];
  const entries = Array.isArray(card?.body?.entries) ? card.body.entries : [];
  return `
    <article class="advisor-card advisor-card-${escapeHtml(card?.cardType || 'generic')}">
      <div class="advisor-card-head">
        <div>
          <div class="card-title">${escapeHtml(card?.title || '-')}</div>
          <div class="form-hint">${escapeHtml(formatDateTime(card?.createdAt))}</div>
        </div>
        <span class="badge badge-info">${escapeHtml(card?.status || 'ready')}</span>
      </div>
      ${card?.cardType === 'tree' ? `
        <div class="advisor-card-copy">${escapeHtml(summarizeCard(card))}</div>
        <div class="advisor-tree-shell">
          <ul class="advisor-tree-list">${Array.isArray(card?.body?.tree?.children) ? card.body.tree.children.map(renderTreeNode).join('') : ''}</ul>
        </div>
      ` : ''}
      ${card?.cardType === 'plan_preview' ? `
        <div class="advisor-card-copy">${escapeHtml(summarizeCard(card))}</div>
        <div class="advisor-entry-list">${renderPlanEntries(entries)}</div>
      ` : ''}
      ${card?.cardType === 'preference_draft' ? `
        <div class="advisor-card-copy">${escapeHtml(card?.body?.summary || '')}</div>
        <div class="form-hint">${escapeHtml(text('建议作用域：', 'Suggested scope: '))}${escapeHtml(card?.body?.suggestedScope || 'session')}</div>
      ` : ''}
      ${card?.cardType === 'reclassification_result' ? `
        <div class="advisor-card-copy">${escapeHtml(card?.body?.summary || summarizeCard(card))}</div>
        <div class="form-hint">${escapeHtml(card?.body?.updatedTreeText || '')}</div>
      ` : ''}
      ${card?.cardType === 'execution_result' ? `
        <div class="advisor-card-copy">${escapeHtml(summarizeCard(card))}</div>
      ` : ''}
      ${actions.length ? `
        <div class="advisor-inline-actions">
          ${actions.map((action) => `
            <button
              class="btn ${action?.variant === 'primary' ? 'btn-primary' : 'btn-secondary'} advisor-card-action"
              type="button"
              ${state.acting ? 'disabled' : ''}
              data-card-id="${escapeHtml(card?.cardId || '')}"
              data-action="${escapeHtml(action?.action || '')}"
            >${escapeHtml(action?.label || action?.action || 'Action')}</button>
          `).join('')}
        </div>
      ` : ''}
    </article>
  `;
}

function renderTimeline() {
  const timeline = Array.isArray(state.sessionData?.timeline) ? state.sessionData.timeline : [];
  if (!timeline.length) {
    return `
      <div class="empty-state advisor-empty-compact">
        <div class="empty-state-text">${escapeHtml(text('启动会话后，消息流和结果卡会显示在这里。', 'The timeline and cards will appear here once the session starts.'))}</div>
      </div>
    `;
  }
  return timeline.map((turn) => `
    <section class="advisor-turn advisor-turn-${escapeHtml(turn?.role || 'assistant')}">
      <div class="advisor-turn-head">
        <span class="badge ${turn?.role === 'user' ? 'badge-info' : 'badge-success'}">${escapeHtml(turn?.role === 'user' ? text('你', 'You') : text('顾问', 'Advisor'))}</span>
        <span class="form-hint">${escapeHtml(formatDateTime(turn?.createdAt))}</span>
      </div>
      ${(turn?.text || '').trim() ? `<div class="advisor-turn-text">${escapeHtml(turn?.text || '')}</div>` : ''}
      <div class="advisor-turn-cards">${(Array.isArray(turn?.cards) ? turn.cards : []).map(renderCard).join('')}</div>
    </section>
  `).join('');
}

function renderQuickScans() {
  if (!state.quickScans.length) {
    return `<div class="form-hint">${escapeHtml(text('暂无可复用的盘点记录', 'No reusable scan records yet'))}</div>`;
  }
  return state.quickScans.map((task) => {
    const taskId = String(task?.taskId || task?.id || '').trim();
    const rootPath = String(task?.targetPath || task?.rootPath || '').trim();
    return `
      <button class="advisor-quick-scan" type="button" data-task-id="${escapeHtml(taskId)}" data-root-path="${escapeHtml(rootPath)}">
        <span class="advisor-quick-scan-title">${escapeHtml(rootPath || '-')}</span>
        <span class="advisor-quick-scan-meta">${escapeHtml(formatSize(task?.totalCleanable || 0))} · ${escapeHtml(formatDateTime(task?.updatedAt || task?.createdAt))}</span>
      </button>
    `;
  }).join('');
}

function renderPage() {
  if (!pageContainer) return;
  const contextBar = state.sessionData?.contextBar || {};
  const collapsed = !!contextBar?.collapsed;
  const stageLabel = getStageLabel();
  const modeLabel = contextBar?.mode?.label || text('顾问模式：单智能体', 'Advisor Mode: Single Agent');
  pageContainer.innerHTML = `
    <section class="advisor-v2-shell">
      <section class="card advisor-v2-init">
        <div class="advisor-v2-header">
          <div>
            <h1>${escapeHtml(text('顾问工作流', 'Advisor Workflow'))}</h1>
            <p>${escapeHtml(text('扫描页继续作为入口，这里改成单列消息流、附着式结果卡和底部固定输入区。', 'Inventory stays as the entry. This page is now a single-column session flow with attached cards and a bottom composer.'))}</p>
          </div>
          <button id="advisor-start-btn" class="btn btn-primary" type="button" ${state.loading ? 'disabled' : ''}>${escapeHtml(state.sessionId ? text('重建会话', 'Restart Session') : text('开始会话', 'Start Session'))}</button>
        </div>
        <div class="advisor-path-actions">
          <input id="advisor-root-path" class="input" type="text" value="${escapeHtml(state.rootPath)}" placeholder="${escapeHtml(text('选择目录，或从扫描页带入目录', 'Choose a folder or hand off from the scanner'))}">
          <button id="advisor-browse-btn" class="btn btn-secondary" type="button">${escapeHtml(text('浏览', 'Browse'))}</button>
        </div>
        <div class="advisor-quick-scan-grid">${renderQuickScans()}</div>
      </section>

      ${state.sessionData ? `
        <section class="card advisor-v2-context ${collapsed ? 'collapsed' : ''}">
          <div class="advisor-card-head">
            <div>
              <div class="card-title">${escapeHtml(text('上下文条', 'Context Bar'))}</div>
              <div class="form-hint">${escapeHtml(contextBar?.rootPath || state.rootPath || '-')}</div>
            </div>
            <span class="badge badge-info">${escapeHtml(stageLabel)}</span>
            <button id="advisor-toggle-context" class="btn btn-ghost" type="button" ${state.acting ? 'disabled' : ''}>${escapeHtml(collapsed ? text('展开', 'Expand') : text('折叠', 'Collapse'))}</button>
          </div>
          ${collapsed ? '' : `
            <div class="advisor-context-grid">
              <div class="advisor-context-chip">${escapeHtml(modeLabel)}</div>
              <div class="advisor-context-chip">${escapeHtml(text('扫描记录', 'Scan'))}: ${escapeHtml(contextBar?.scanTaskId || '-')}</div>
              <div class="advisor-context-chip">${escapeHtml(text('分类记录', 'Organize'))}: ${escapeHtml(contextBar?.organizeTaskId || '-')}</div>
              <div class="advisor-context-chip">${escapeHtml(text('项目数', 'Items'))}: ${escapeHtml(contextBar?.inventorySummary?.itemCount || 0)}</div>
              <div class="advisor-context-chip">${escapeHtml(text('可复用树', 'Reusable Tree'))}: ${escapeHtml(contextBar?.inventorySummary?.treeAvailable ? text('是', 'Yes') : text('否', 'No'))}</div>
            </div>
            <div class="form-hint">${escapeHtml(contextBar?.memorySummary?.message || '')}</div>
            <div class="form-hint">${escapeHtml(contextBar?.inventorySummary?.message || '')}</div>
          `}
        </section>
      ` : ''}

      <section class="advisor-v2-timeline-wrap">
        <div class="advisor-v2-timeline">${renderTimeline()}</div>
      </section>

      <section class="card advisor-v2-composer">
        <textarea id="advisor-message" class="textarea" rows="4" placeholder="${escapeHtml(state.sessionData?.composer?.placeholder || text('告诉我你想先处理哪些文件。', 'Tell me which files you want to handle first.'))}">${escapeHtml(state.messageDraft)}</textarea>
        <div class="form-hint">${escapeHtml(text('当前阶段：', 'Current stage: '))}${escapeHtml(stageLabel)}</div>
        <div class="advisor-inline-actions">
          <button id="advisor-send-btn" class="btn btn-primary" type="button" ${state.sending || !state.sessionId ? 'disabled' : ''}>${escapeHtml(state.sessionData?.composer?.submitLabel || text('发送', 'Send'))}</button>
        </div>
      </section>
    </section>
  `;

  bindEvents();
}

async function refreshQuickScans() {
  try {
    state.quickScans = await listScanHistory(QUICK_SCAN_LIMIT);
  } catch {
    state.quickScans = [];
  }
}

async function hydrateSession(sessionId) {
  if (!sessionId) return;
  state.loading = true;
  renderPage();
  try {
    state.sessionData = await advisorSessionGet(sessionId);
    state.sessionId = String(state.sessionData?.sessionId || sessionId);
    setPersisted(PERSIST_KEYS.sessionId, state.sessionId);
  } finally {
    state.loading = false;
    renderPage();
  }
}

async function handleBrowse() {
  try {
    const picked = await browseFolder();
    if (picked?.cancelled || !picked?.path) return;
    state.rootPath = String(picked.path).trim();
    setPersisted(PERSIST_KEYS.rootPath, state.rootPath);
    renderPage();
  } catch (err) {
    showToast(`${text('选择目录失败: ', 'Failed to select folder: ')}${err?.message || err}`, 'error');
  }
}

async function handleStart(scanTaskId = null) {
  if (!state.rootPath.trim()) {
    showToast(text('请先选择目录', 'Select a folder first'), 'error');
    return;
  }
  state.loading = true;
  renderPage();
  try {
    const payload = await advisorSessionStart({
      rootPath: state.rootPath.trim(),
      scanTaskId,
      responseLanguage: getLang(),
    });
    state.sessionData = payload;
    state.sessionId = String(payload?.sessionId || '');
    setPersisted(PERSIST_KEYS.sessionId, state.sessionId);
  } catch (err) {
    showToast(`${text('启动会话失败: ', 'Failed to start session: ')}${err?.message || err}`, 'error');
  } finally {
    state.loading = false;
    renderPage();
    scrollComposerIntoView();
  }
}

async function handleSend() {
  if (!state.sessionId || !state.messageDraft.trim()) return;
  state.sending = true;
  renderPage();
  try {
    const payload = await advisorMessageSend({
      sessionId: state.sessionId,
      message: state.messageDraft.trim(),
    });
    state.sessionData = payload;
    state.messageDraft = '';
    setPersisted(PERSIST_KEYS.messageDraft, state.messageDraft);
  } catch (err) {
    showToast(`${text('发送失败: ', 'Send failed: ')}${err?.message || err}`, 'error');
  } finally {
    state.sending = false;
    renderPage();
    scrollComposerIntoView();
  }
}

async function handleCardAction(cardId, action) {
  if (!state.sessionId || !action) return;
  state.acting = true;
  renderPage();
  try {
    const payload = action === 'toggle_context_bar'
      ? { collapsed: !state.sessionData?.contextBar?.collapsed }
      : undefined;
    state.sessionData = await advisorCardAction({
      sessionId: state.sessionId,
      cardId: cardId || '',
      action,
      payload,
    });
  } catch (err) {
    showToast(`${text('卡片动作失败: ', 'Card action failed: ')}${err?.message || err}`, 'error');
  } finally {
    state.acting = false;
    renderPage();
    scrollComposerIntoView();
  }
}

function scrollComposerIntoView() {
  window.setTimeout(() => {
    pageContainer?.querySelector('.advisor-v2-composer')?.scrollIntoView?.({ behavior: 'smooth', block: 'end' });
  }, 30);
}

function bindEvents() {
  const rootInput = document.getElementById('advisor-root-path');
  rootInput?.addEventListener('input', (event) => {
    state.rootPath = String(event.target?.value || '').trim();
    setPersisted(PERSIST_KEYS.rootPath, state.rootPath);
  });

  document.getElementById('advisor-browse-btn')?.addEventListener('click', handleBrowse);
  document.getElementById('advisor-start-btn')?.addEventListener('click', () => handleStart());
  document.getElementById('advisor-toggle-context')?.addEventListener('click', () => {
    handleCardAction('', 'toggle_context_bar');
  });

  const messageInput = document.getElementById('advisor-message');
  messageInput?.addEventListener('input', (event) => {
    state.messageDraft = String(event.target?.value || '');
    setPersisted(PERSIST_KEYS.messageDraft, state.messageDraft);
  });
  messageInput?.addEventListener('keydown', (event) => {
    if ((event.ctrlKey || event.metaKey) && event.key === 'Enter') {
      event.preventDefault();
      handleSend();
    }
  });

  document.getElementById('advisor-send-btn')?.addEventListener('click', handleSend);

  pageContainer.querySelectorAll('.advisor-quick-scan').forEach((button) => {
    button.addEventListener('click', () => {
      state.rootPath = String(button.dataset.rootPath || '').trim();
      setPersisted(PERSIST_KEYS.rootPath, state.rootPath);
      handleStart(String(button.dataset.taskId || '').trim() || null);
    });
  });

  pageContainer.querySelectorAll('.advisor-card-action').forEach((button) => {
    button.addEventListener('click', () => handleCardAction(button.dataset.cardId, button.dataset.action));
  });
}

async function bootstrap() {
  await refreshQuickScans();
  renderPage();
  const handoff = getPendingHandoff();
  if (handoff?.rootPath) {
    state.rootPath = String(handoff.rootPath).trim();
    setPersisted(PERSIST_KEYS.rootPath, state.rootPath);
    await handleStart(handoff?.scanTaskId ? String(handoff.scanTaskId).trim() : null);
    return;
  }
  if (state.sessionId) {
    try {
      await hydrateSession(state.sessionId);
      return;
    } catch {
      state.sessionId = '';
      state.sessionData = null;
      removePersisted(PERSIST_KEYS.sessionId);
    }
  }
  renderPage();
}

export function renderAdvisor(container) {
  pageContainer = container;
  state = createInitialState();
  bootstrap();
}
