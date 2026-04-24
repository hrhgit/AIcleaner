import {
  advisorCardAction,
  advisorMessageSend,
  advisorSessionGet,
  advisorSessionStart,
  browseFolder,
  connectOrganizeStream,
  getOrganizeResult,
  getSettings,
  saveSettings,
  startOrganize,
  stopOrganize,
} from '../utils/api.js';
import { getLang } from '../utils/i18n.js';
import { ensureRequiredCredentialsConfigured } from '../utils/secret-ui.js';
import {
  DEFAULT_BATCH_SIZE,
  DEFAULT_EXCLUSIONS,
  DEFAULT_SUMMARY_MODE,
  PERSIST_KEYS as ORGANIZER_PERSIST_KEYS,
  SUMMARY_MODES,
} from './organizer-storage.js';
import { showToast } from '../utils/toast.js';

const PERSIST_KEYS = {
  rootPath: 'wipeout.advisor.global.root_path.v2',
  sessionId: 'wipeout.advisor.global.session_id.v2',
  messageDraft: 'wipeout.advisor.global.message_draft.v2',
};

let pageContainer = null;
let state = createInitialState();

function text(zh, en) {
  return getLang() === 'en' ? en : zh;
}

function readPersisted(key, fallback) {
  try {
    const raw = localStorage.getItem(key);
    return raw == null ? fallback : JSON.parse(raw);
  } catch {
    return fallback;
  }
}

function writePersisted(key, value) {
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

function createInitialState() {
  return {
    rootPath: resolveInitialRootPath(),
    summaryStrategy: resolveInitialSummaryStrategy(),
    useWebSearch: resolveInitialUseWebSearch(),
    sessionId: readPersisted(PERSIST_KEYS.sessionId, ''),
    messageDraft: readPersisted(PERSIST_KEYS.messageDraft, ''),
    sessionData: null,
    organizeTaskId: String(readPersisted(ORGANIZER_PERSIST_KEYS.lastTaskId, '') || ''),
    organizeSnapshot: sanitizeSnapshot(readPersisted(ORGANIZER_PERSIST_KEYS.lastSnapshot, null)),
    organizeStream: null,
    loading: false,
    sending: false,
    acting: false,
    organizeStarting: false,
    organizeStopping: false,
    syncingSearch: false,
  };
}

function sanitizeSnapshot(snapshot) {
  return snapshot && typeof snapshot === 'object' ? snapshot : null;
}

function resolveInitialRootPath() {
  const persisted = String(readPersisted(PERSIST_KEYS.rootPath, '') || '').trim();
  if (persisted) return persisted;
  return String(readPersisted(ORGANIZER_PERSIST_KEYS.rootPath, '') || '').trim();
}

function resolveInitialSummaryStrategy() {
  const persisted = String(readPersisted(ORGANIZER_PERSIST_KEYS.summaryStrategy, DEFAULT_SUMMARY_MODE) || '').trim();
  return SUMMARY_MODES.includes(persisted) ? persisted : DEFAULT_SUMMARY_MODE;
}

function resolveInitialUseWebSearch() {
  return !!readPersisted(ORGANIZER_PERSIST_KEYS.useWebSearch, false);
}

function escapeHtml(value) {
  const div = document.createElement('div');
  div.textContent = String(value ?? '');
  return div.innerHTML;
}

function formatDateTime(value) {
  if (!value) return '-';
  const date = new Date(value);
  if (Number.isNaN(date.getTime())) return String(value);
  return date.toLocaleString(getLang() === 'en' ? 'en-US' : 'zh-CN');
}

function normalizePath(value) {
  return String(value || '').trim().replace(/[\\/]+/g, '/').toLowerCase();
}

function getCurrentSnapshot() {
  const snapshot = sanitizeSnapshot(state.organizeSnapshot);
  if (!snapshot) return null;
  const rootPath = String(state.rootPath || '').trim();
  const snapshotRoot = String(snapshot.rootPath || snapshot.root_path || '').trim();
  if (!rootPath || !snapshotRoot) return snapshot;
  return normalizePath(rootPath) === normalizePath(snapshotRoot) ? snapshot : null;
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

function getOrganizeStatus(snapshot = getCurrentSnapshot()) {
  return String(snapshot?.status || '').trim().toLowerCase();
}

function isOrganizeRunning(snapshot = getCurrentSnapshot()) {
  return ['idle', 'scanning', 'classifying', 'stopping', 'moving'].includes(getOrganizeStatus(snapshot));
}

function isOrganizeFinished(snapshot = getCurrentSnapshot()) {
  return ['completed', 'done'].includes(getOrganizeStatus(snapshot));
}

function getOrganizeStatusLabel(snapshot = getCurrentSnapshot()) {
  const status = getOrganizeStatus(snapshot);
  if (status === 'scanning') return text('扫描中', 'Scanning');
  if (status === 'classifying') return text('归类中', 'Classifying');
  if (status === 'stopping') return text('停止中', 'Stopping');
  if (status === 'moving') return text('执行中', 'Applying');
  if (status === 'completed' || status === 'done') return text('归类完成', 'Completed');
  if (status === 'stopped') return text('已停止', 'Stopped');
  if (status === 'error') return text('出错', 'Error');
  if (state.organizeStarting) return text('启动中', 'Starting');
  return text('待开始', 'Idle');
}

function getOrganizeProgress(snapshot = getCurrentSnapshot()) {
  const total = Number(snapshot?.totalFiles || snapshot?.total_files || 0);
  const processed = Number(snapshot?.processedFiles || snapshot?.processed_files || 0);
  if (total <= 0) {
    return isOrganizeFinished(snapshot) ? 100 : 0;
  }
  return Math.max(0, Math.min(100, Math.round((processed / total) * 100)));
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
      <div class="advisor-entry-copy">
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
        <div class="advisor-card-title-group">
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
  const snapshot = getCurrentSnapshot();
  if (!timeline.length) {
    const finished = isOrganizeFinished(snapshot);
    return `
      <div class="card advisor-empty-panel">
        <div class="empty-state advisor-empty-compact">
          <div class="empty-state-text">${escapeHtml(finished
            ? text('归类结果已经准备好，可以开始对话。', 'Organize results are ready. Start the conversation when you are ready.')
            : text('会话启动后，消息流和结果卡会显示在这里。', 'The timeline and cards will appear here once the session starts.'))}</div>
          <div class="empty-state-hint">${escapeHtml(finished
            ? text('上方已经展示当前目录的归类结果；开始对话后，顾问会直接复用它。', 'The latest organize result is already shown above. The advisor will reuse it once the session starts.')
            : text('可以先启动归类，也可以直接开始会话；没有归类结果时，顾问会先基于目录元信息工作。', 'You can organize first or start the session directly. Without organize results, the advisor starts from directory metadata.'))}</div>
        </div>
      </div>
    `;
  }
  return timeline.map((turn) => `
    <section class="advisor-message-section advisor-message-section-${escapeHtml(turn?.role || 'assistant')}">
      <div class="advisor-message-rail" aria-hidden="true">
        <span class="advisor-message-node"></span>
      </div>
      <div class="advisor-message-stack">
        ${(turn?.text || '').trim() ? `
          <article class="advisor-message-bubble">
            <div class="advisor-message-meta">
              <span class="advisor-message-role">${escapeHtml(turn?.role === 'user' ? text('你', 'You') : text('顾问', 'Advisor'))}</span>
              <span class="advisor-message-time">${escapeHtml(formatDateTime(turn?.createdAt))}</span>
            </div>
            <div class="advisor-message-text">${escapeHtml(turn?.text || '')}</div>
          </article>
        ` : `
          <div class="advisor-message-meta advisor-message-meta-inline">
            <span class="advisor-message-role">${escapeHtml(turn?.role === 'user' ? text('你', 'You') : text('顾问', 'Advisor'))}</span>
            <span class="advisor-message-time">${escapeHtml(formatDateTime(turn?.createdAt))}</span>
          </div>
        `}
        <div class="advisor-turn-cards">${(Array.isArray(turn?.cards) ? turn.cards : []).map(renderCard).join('')}</div>
      </div>
    </section>
  `).join('');
}

function renderContextSummary() {
  if (!state.sessionData) return '';
  const contextBar = state.sessionData?.contextBar || {};
  const collapsed = !!contextBar?.collapsed;
  const rootPath = contextBar?.rootPath || state.rootPath || '-';
  const modeLabel = contextBar?.mode?.label || text('顾问模式：单智能体', 'Advisor Mode: Single Agent');
  const directorySummary = contextBar?.directorySummary || {};
  const webSearch = contextBar?.webSearch || {};
  const stageLabel = getStageLabel();
  return `
    <section class="card advisor-context-summary ${collapsed ? 'collapsed' : ''}">
      <div class="advisor-context-head">
        <div>
          <div class="workflow-kicker workflow-kicker-subtle">${escapeHtml(text('会话上下文', 'Session Context'))}</div>
          <div class="card-title">${escapeHtml(rootPath)}</div>
        </div>
        <div class="advisor-context-actions">
          <span class="advisor-stage-chip advisor-stage-chip-muted">${escapeHtml(stageLabel)}</span>
          <button id="advisor-toggle-context" class="btn btn-ghost" type="button" ${state.acting ? 'disabled' : ''}>${escapeHtml(collapsed ? text('展开', 'Expand') : text('折叠', 'Collapse'))}</button>
        </div>
      </div>
      ${collapsed ? `
        <div class="form-hint">${escapeHtml(modeLabel)}</div>
      ` : `
        <div class="advisor-context-grid">
          <div class="advisor-context-chip">${escapeHtml(modeLabel)}</div>
          <div class="advisor-context-chip">${escapeHtml(text('分类记录', 'Organize'))}: ${escapeHtml(contextBar?.organizeTaskId || '-')}</div>
          <div class="advisor-context-chip">${escapeHtml(text('项目数', 'Items'))}: ${escapeHtml(directorySummary?.itemCount || 0)}</div>
          <div class="advisor-context-chip">${escapeHtml(text('可复用树', 'Reusable Tree'))}: ${escapeHtml(directorySummary?.treeAvailable ? text('是', 'Yes') : text('否', 'No'))}</div>
          <div class="advisor-context-chip">${escapeHtml(text('联网搜索', 'Web Search'))}: ${escapeHtml(webSearch?.webSearchEnabled ? text('可用', 'Available') : (webSearch?.useWebSearch ? text('已开启但缺少密钥', 'Enabled but unavailable') : text('关闭', 'Off')))}</div>
        </div>
        <div class="advisor-context-notes">
          ${contextBar?.memorySummary?.message ? `<div class="form-hint">${escapeHtml(contextBar.memorySummary.message)}</div>` : ''}
          ${directorySummary?.message ? `<div class="form-hint">${escapeHtml(directorySummary.message)}</div>` : ''}
          ${webSearch?.message ? `<div class="form-hint">${escapeHtml(webSearch.message)}</div>` : ''}
        </div>
      `}
    </section>
  `;
}

function renderOrganizeSummary(snapshot) {
  if (!snapshot) {
    return `
      <div class="advisor-organize-summary">
        <div class="form-hint">${escapeHtml(text(
          '还没有当前目录的归类结果。你可以先启动归类，也可以直接开始会话。',
          'There is no organize result for this folder yet. You can organize first or start the session directly.',
        ))}</div>
      </div>
    `;
  }

  const totalFiles = Number(snapshot.totalFiles || snapshot.total_files || 0);
  const processedFiles = Number(snapshot.processedFiles || snapshot.processed_files || 0);
  const error = String(snapshot.error || '').trim();
  const treeChildren = Array.isArray(snapshot?.tree?.children) ? snapshot.tree.children : [];

  return `
    <section class="advisor-organize-summary">
      <div class="advisor-organize-summary-head">
        <div class="advisor-hero-stat">
          <span class="advisor-hero-stat-label">${escapeHtml(text('归类状态', 'Organize Status'))}</span>
          <strong>${escapeHtml(getOrganizeStatusLabel(snapshot))}</strong>
        </div>
        <div class="advisor-organize-summary-meta">
          <span class="badge badge-info">${escapeHtml(text('摘要模式', 'Summary'))}: ${escapeHtml(snapshot.summaryStrategy || snapshot.summary_strategy || state.summaryStrategy)}</span>
          <span class="badge ${snapshot.webSearchEnabled ? 'badge-success' : (snapshot.useWebSearch ? 'badge-warning' : 'badge-info')}">${escapeHtml(snapshot.webSearchEnabled ? text('联网可用', 'Web Search Ready') : (snapshot.useWebSearch ? text('联网未就绪', 'Web Search Unavailable') : text('联网关闭', 'Web Search Off')))}</span>
        </div>
      </div>
      <div class="advisor-organize-stats">
        <div class="advisor-context-chip">${escapeHtml(text('文件数', 'Files'))}: ${escapeHtml(totalFiles)}</div>
        <div class="advisor-context-chip">${escapeHtml(text('已处理', 'Processed'))}: ${escapeHtml(processedFiles)}</div>
        <div class="advisor-context-chip">${escapeHtml(text('任务 ID', 'Task ID'))}: ${escapeHtml(snapshot.id || '-')}</div>
      </div>
      <div class="advisor-organize-progress">
        <div class="advisor-organize-progress-track">
          <div class="advisor-organize-progress-fill" style="width: ${getOrganizeProgress(snapshot)}%"></div>
        </div>
        <div class="form-hint">${escapeHtml(text('当前进度', 'Progress'))}: ${escapeHtml(getOrganizeProgress(snapshot))}%</div>
      </div>
      ${error ? `<div class="form-hint">${escapeHtml(text('错误: ', 'Error: '))}${escapeHtml(error)}</div>` : ''}
      ${treeChildren.length ? `
        <div class="advisor-tree-shell">
          <ul class="advisor-tree-list">${treeChildren.slice(0, 18).map(renderTreeNode).join('')}</ul>
        </div>
      ` : `
        <div class="form-hint">${escapeHtml(text('当前还没有可展示的分类树。', 'There is no tree to show yet.'))}</div>
      `}
    </section>
  `;
}

function renderOrganizePanel() {
  const snapshot = getCurrentSnapshot();
  const hasSession = !!state.sessionData;
  const sessionBtnLabel = hasSession
    ? text('重建会话', 'Restart Session')
    : isOrganizeFinished(snapshot)
      ? text('开始对话', 'Start Conversation')
      : text('直接开始会话', 'Start Conversation');
  const conversationHint = isOrganizeFinished(snapshot)
    ? text('归类完成后，顾问会直接复用上面的结果树和统计。', 'Once the conversation starts, the advisor will reuse the organize tree and stats above.')
    : text('如果你先开始会话，顾问会暂时基于目录元信息工作。', 'If you start the conversation now, the advisor will temporarily work from directory metadata.');

  return `
    <section class="card advisor-organize-panel">
      <div class="advisor-section-head">
        <div>
          <div class="workflow-kicker workflow-kicker-subtle">${escapeHtml(text('前置归类', 'Organize First'))}</div>
          <h2 class="card-title">${escapeHtml(text('先归类，再进入顾问对话。', 'Organize first, then continue in the advisor conversation.'))}</h2>
        </div>
        <div class="advisor-inline-actions advisor-organize-actions">
          <button id="advisor-organize-start-btn" class="btn btn-primary" type="button" ${state.organizeStarting || isOrganizeRunning(snapshot) ? 'disabled' : ''}>${escapeHtml(state.organizeStarting ? text('启动中...', 'Starting...') : text('开始归类', 'Start Organizing'))}</button>
          <button id="advisor-organize-stop-btn" class="btn btn-secondary" type="button" ${state.organizeStopping || !isOrganizeRunning(snapshot) ? 'disabled' : ''}>${escapeHtml(state.organizeStopping ? text('停止中...', 'Stopping...') : text('停止归类', 'Stop Organizing'))}</button>
          <button id="advisor-start-btn" class="btn btn-secondary" type="button" ${(state.loading || state.organizeStarting) ? 'disabled' : ''}>${escapeHtml(sessionBtnLabel)}</button>
        </div>
      </div>
      <div class="advisor-organize-config-grid">
        <div class="advisor-source-field">
          <label class="form-label" for="advisor-root-path">${escapeHtml(text('工作目录', 'Working Directory'))}</label>
          <div class="advisor-path-actions">
            <input id="advisor-root-path" class="form-input advisor-input-path" type="text" value="${escapeHtml(state.rootPath)}" placeholder="${escapeHtml(text('选择目录', 'Choose a folder'))}">
            <button id="advisor-browse-btn" class="btn btn-secondary" type="button">${escapeHtml(text('浏览', 'Browse'))}</button>
          </div>
        </div>
        <div class="advisor-organize-fields">
          <div class="form-group">
            <label class="form-label" for="advisor-summary-mode">${escapeHtml(text('摘要模式', 'Summary Mode'))}</label>
            <select id="advisor-summary-mode" class="form-input">
              ${SUMMARY_MODES.map((mode) => `
                <option value="${escapeHtml(mode)}" ${state.summaryStrategy === mode ? 'selected' : ''}>${escapeHtml(summaryModeLabel(mode))}</option>
              `).join('')}
            </select>
            <div class="form-hint">${escapeHtml(summaryModeHint(state.summaryStrategy))}</div>
          </div>
          <label class="advisor-organize-toggle">
            <input id="advisor-workflow-web-search" type="checkbox" ${state.useWebSearch ? 'checked' : ''} ${state.syncingSearch ? 'disabled' : ''} />
            <span class="advisor-organize-toggle-copy">
              <span class="advisor-organize-toggle-title">${escapeHtml(text('为当前整理工作流启用联网搜索', 'Enable Web Search for the current cleanup workflow'))}</span>
              <span class="advisor-organize-toggle-hint">${escapeHtml(text('这个开关会同时影响归类阶段和顾问对话中的 web_search 工具。', 'This single switch controls organize-time search and the advisor web_search tool.'))}</span>
            </span>
          </label>
        </div>
      </div>
      <div class="form-hint">${escapeHtml(conversationHint)}</div>
      ${renderOrganizeSummary(snapshot)}
    </section>
  `;
}

function renderPage() {
  if (!pageContainer) return;
  const stageLabel = getStageLabel();
  pageContainer.innerHTML = `
    <section class="workflow-shell advisor-workspace">
      <section class="card workflow-hero-panel advisor-hero-panel">
        <div class="workflow-hero-row">
          <div class="workflow-hero-copy">
            <div class="workflow-kicker">${escapeHtml(text('顾问工作流', 'Advisor Workflow'))}</div>
            <h1>${escapeHtml(text('在同一页面里完成归类、建议、预览和执行。', 'Organize, advise, preview, and execute from one page.'))}</h1>
            <p>${escapeHtml(text('顶部先跑归类，拿到结果后直接进入顾问对话；如果你不想等待，也可以直接开始会话。', 'Run organize at the top first, then continue straight into the advisor conversation. If you do not want to wait, you can still start the session immediately.'))}</p>
          </div>
          <div class="workflow-hero-actions advisor-hero-actions">
            <span class="advisor-stage-chip">${escapeHtml(stageLabel)}</span>
          </div>
        </div>
        ${renderOrganizePanel()}
      </section>

      ${renderContextSummary()}

      <section class="advisor-timeline-shell">
        <div class="advisor-timeline">${renderTimeline()}</div>
      </section>

      <section class="card advisor-composer-panel">
        <div class="advisor-composer-grid">
          <div class="advisor-composer-main">
            <label class="form-label" for="advisor-message">${escapeHtml(text('下一步指令', 'Next Instruction'))}</label>
            <textarea id="advisor-message" class="form-input advisor-composer-input" rows="4" placeholder="${escapeHtml(state.sessionData?.composer?.placeholder || text('告诉我你想先处理哪些文件。', 'Tell me which files you want to handle first.'))}">${escapeHtml(state.messageDraft)}</textarea>
          </div>
          <div class="advisor-composer-side">
            <div class="advisor-composer-stage">
              <div class="workflow-kicker workflow-kicker-subtle">${escapeHtml(text('当前阶段', 'Current Stage'))}</div>
              <div class="advisor-composer-stage-value">${escapeHtml(stageLabel)}</div>
              <div class="form-hint">${escapeHtml(text('按 Ctrl/Cmd + Enter 快速发送。', 'Press Ctrl/Cmd + Enter to send quickly.'))}</div>
            </div>
            <button id="advisor-send-btn" class="btn btn-primary advisor-send-btn" type="button" ${state.sending || !state.sessionId ? 'disabled' : ''}>${escapeHtml(state.sessionData?.composer?.submitLabel || text('发送', 'Send'))}</button>
          </div>
        </div>
      </section>
    </section>
  `;

  bindEvents();
}

function summaryModeLabel(mode) {
  if (mode === 'agent_summary') return text('AI 摘要', 'Agent Summary');
  if (mode === 'local_summary') return text('本地摘要', 'Local Summary');
  return text('仅文件名', 'Filename Only');
}

function summaryModeHint(mode) {
  if (mode === 'agent_summary') {
    return text('先提取文本层，再调用摘要模型生成标准化摘要。', 'Extract text locally first, then call the summary model for normalized summaries.');
  }
  if (mode === 'local_summary') {
    return text('只做本地提取和模板摘要，不额外调用摘要模型。', 'Only use local extraction and template summaries, without extra summary model calls.');
  }
  return text('最低成本，只用文件名、路径和基础元信息归类。', 'Lowest-cost mode. Classify from filenames, paths, and metadata only.');
}

async function hydrateSession(sessionId) {
  if (!sessionId) return;
  state.loading = true;
  renderPage();
  try {
    state.sessionData = await advisorSessionGet(sessionId);
    state.sessionId = String(state.sessionData?.sessionId || sessionId);
    writePersisted(PERSIST_KEYS.sessionId, state.sessionId);
  } finally {
    state.loading = false;
    renderPage();
  }
}

async function ensureWorkflowCredentials(requireSearchApi) {
  const settings = await getSettings();
  const defaultProviderEndpoint = String(settings?.defaultProviderEndpoint || '').trim() || 'https://api.openai.com/v1';
  await ensureRequiredCredentialsConfigured({
    providerEndpoints: [defaultProviderEndpoint],
    requireSearchApi,
    reasonText: text('请先在“服务商 API”里补齐当前工作流所需密钥。', 'Configure the required API keys in Service Provider API first.'),
  });
}

function persistRootPath(rootPath) {
  const value = String(rootPath || '').trim();
  state.rootPath = value;
  writePersisted(PERSIST_KEYS.rootPath, value);
  writePersisted(ORGANIZER_PERSIST_KEYS.rootPath, value);
}

function persistSummaryStrategy(summaryStrategy) {
  state.summaryStrategy = SUMMARY_MODES.includes(summaryStrategy) ? summaryStrategy : DEFAULT_SUMMARY_MODE;
  writePersisted(ORGANIZER_PERSIST_KEYS.summaryStrategy, state.summaryStrategy);
}

function persistUseWebSearch(useWebSearch) {
  state.useWebSearch = !!useWebSearch;
  writePersisted(ORGANIZER_PERSIST_KEYS.useWebSearch, state.useWebSearch);
}

function persistOrganizeSnapshot(snapshot) {
  state.organizeSnapshot = sanitizeSnapshot(snapshot);
  if (state.organizeSnapshot) {
    writePersisted(ORGANIZER_PERSIST_KEYS.lastSnapshot, state.organizeSnapshot);
  } else {
    removePersisted(ORGANIZER_PERSIST_KEYS.lastSnapshot);
  }
}

function persistOrganizeTaskId(taskId) {
  state.organizeTaskId = String(taskId || '').trim();
  if (state.organizeTaskId) {
    writePersisted(ORGANIZER_PERSIST_KEYS.lastTaskId, state.organizeTaskId);
  } else {
    removePersisted(ORGANIZER_PERSIST_KEYS.lastTaskId);
  }
}

function closeOrganizeStream() {
  try {
    state.organizeStream?.close?.();
  } catch {
    // ignore
  }
  state.organizeStream = null;
}

function applyLocalWebSearchState() {
  if (!state.sessionData || typeof state.sessionData !== 'object') return;
  state.sessionData.useWebSearch = !!state.useWebSearch;
  state.sessionData.webSearchEnabled = !!state.useWebSearch;
  if (state.sessionData.session && typeof state.sessionData.session === 'object') {
    state.sessionData.session.useWebSearch = !!state.useWebSearch;
    state.sessionData.session.webSearchEnabled = !!state.useWebSearch;
  }
  if (!state.sessionData.contextBar || typeof state.sessionData.contextBar !== 'object') {
    state.sessionData.contextBar = {};
  }
  state.sessionData.contextBar.webSearch = {
    useWebSearch: !!state.useWebSearch,
    webSearchEnabled: !!state.useWebSearch,
    message: state.useWebSearch
      ? text('下一轮对话会按当前设置开放联网搜索。', 'Web search will be available on the next turn with the current setting.')
      : text('下一轮对话会关闭联网搜索。', 'Web search will be disabled on the next turn.'),
  };
}

function applyOrganizeSnapshot(snapshot) {
  const nextSnapshot = sanitizeSnapshot(snapshot);
  if (!nextSnapshot) return;
  persistOrganizeSnapshot(nextSnapshot);
  persistOrganizeTaskId(nextSnapshot.id || state.organizeTaskId);
  if (!isOrganizeRunning(nextSnapshot)) {
    closeOrganizeStream();
  }
  renderPage();
}

function connectTaskStream(taskId) {
  closeOrganizeStream();
  if (!taskId) return;
  state.organizeStream = connectOrganizeStream(taskId, {
    onProgress: (snapshot) => applyOrganizeSnapshot(snapshot),
    onDone: (snapshot) => {
      applyOrganizeSnapshot(snapshot);
      showToast(text('归类完成，可以开始对话。', 'Organize finished. You can start the conversation now.'), 'success');
    },
    onError: (payload) => {
      if (payload?.snapshot) applyOrganizeSnapshot(payload.snapshot);
      showToast(`${text('归类失败: ', 'Organize failed: ')}${payload?.message || text('未知错误', 'Unknown error')}`, 'error');
    },
    onStopped: (snapshot) => {
      applyOrganizeSnapshot(snapshot);
      showToast(text('归类任务已停止。', 'The organize task has been stopped.'), 'info');
    },
  });
}

async function hydrateOrganizeSnapshot(taskId, { reconnect = true } = {}) {
  if (!taskId) return;
  try {
    const snapshot = await getOrganizeResult(taskId);
    applyOrganizeSnapshot(snapshot);
    if (reconnect && isOrganizeRunning(snapshot)) {
      connectTaskStream(taskId);
    }
  } catch {
    if (state.organizeTaskId === taskId) {
      persistOrganizeTaskId('');
    }
  }
}

async function syncWorkflowSearchSetting(nextValue) {
  state.syncingSearch = true;
  renderPage();
  try {
    await saveSettings({
      searchApi: {
        provider: 'tavily',
        enabled: !!nextValue,
        scopes: {
          classify: !!nextValue,
          organizer: !!nextValue,
        },
      },
    });
    persistUseWebSearch(nextValue);
    applyLocalWebSearchState();
  } finally {
    state.syncingSearch = false;
    renderPage();
  }
}

async function loadWorkflowSettings() {
  const settings = await getSettings();
  const searchApi = settings?.searchApi && typeof settings.searchApi === 'object'
    ? settings.searchApi
    : {};
  const scopes = searchApi?.scopes && typeof searchApi.scopes === 'object'
    ? searchApi.scopes
    : {};
  const workflowEnabled = !!(searchApi.enabled || scopes.classify || scopes.organizer);
  persistUseWebSearch(workflowEnabled);
}

async function handleBrowse() {
  try {
    const picked = await browseFolder();
    if (picked?.cancelled || !picked?.path) return;
    persistRootPath(picked.path);
    renderPage();
  } catch (err) {
    showToast(`${text('选择目录失败: ', 'Failed to select folder: ')}${err?.message || err}`, 'error');
  }
}

async function handleStartOrganize() {
  if (!state.rootPath.trim()) {
    showToast(text('请先选择目录', 'Select a folder first'), 'error');
    return;
  }
  await ensureWorkflowCredentials(state.useWebSearch);
  state.organizeStarting = true;
  renderPage();
  try {
    const result = await startOrganize({
      rootPath: state.rootPath.trim(),
      excludedPatterns: DEFAULT_EXCLUSIONS,
      batchSize: DEFAULT_BATCH_SIZE,
      summaryStrategy: state.summaryStrategy,
      maxClusterDepth: null,
      useWebSearch: state.useWebSearch,
      responseLanguage: getLang(),
    });
    const taskId = String(result?.taskId || '').trim();
    persistOrganizeTaskId(taskId);
    persistOrganizeSnapshot({
      id: taskId,
      status: 'idle',
      rootPath: state.rootPath.trim(),
      excludedPatterns: DEFAULT_EXCLUSIONS,
      batchSize: DEFAULT_BATCH_SIZE,
      summaryStrategy: state.summaryStrategy,
      maxClusterDepth: null,
      useWebSearch: state.useWebSearch,
      webSearchEnabled: state.useWebSearch,
      totalFiles: 0,
      processedFiles: 0,
      tree: { children: [] },
    });
    connectTaskStream(taskId);
    await hydrateOrganizeSnapshot(taskId, { reconnect: true });
    showToast(text('归类任务已启动。', 'Organize task started.'), 'success');
  } catch (err) {
    showToast(`${text('启动归类失败: ', 'Failed to start organize: ')}${err?.message || err}`, 'error');
  } finally {
    state.organizeStarting = false;
    renderPage();
  }
}

async function handleStopOrganize() {
  if (!state.organizeTaskId) return;
  state.organizeStopping = true;
  renderPage();
  try {
    await stopOrganize(state.organizeTaskId);
  } catch (err) {
    showToast(`${text('停止归类失败: ', 'Failed to stop organize: ')}${err?.message || err}`, 'error');
  } finally {
    state.organizeStopping = false;
    renderPage();
  }
}

async function handleStartSession() {
  if (!state.rootPath.trim()) {
    showToast(text('请先选择目录', 'Select a folder first'), 'error');
    return;
  }
  await ensureWorkflowCredentials(state.useWebSearch);
  state.loading = true;
  renderPage();
  try {
    const payload = await advisorSessionStart({
      rootPath: state.rootPath.trim(),
      responseLanguage: getLang(),
    });
    state.sessionData = payload;
    state.sessionId = String(payload?.sessionId || '');
    writePersisted(PERSIST_KEYS.sessionId, state.sessionId);
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
    writePersisted(PERSIST_KEYS.messageDraft, state.messageDraft);
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
    pageContainer?.querySelector('.advisor-composer-panel')?.scrollIntoView?.({ behavior: 'smooth', block: 'end' });
  }, 30);
}

function bindEvents() {
  const rootInput = document.getElementById('advisor-root-path');
  rootInput?.addEventListener('input', (event) => {
    persistRootPath(event.target?.value || '');
    renderPage();
  });

  document.getElementById('advisor-browse-btn')?.addEventListener('click', handleBrowse);
  document.getElementById('advisor-organize-start-btn')?.addEventListener('click', () => {
    handleStartOrganize().catch((err) => {
      showToast(`${text('启动归类失败: ', 'Failed to start organize: ')}${err?.message || err}`, 'error');
    });
  });
  document.getElementById('advisor-organize-stop-btn')?.addEventListener('click', () => {
    handleStopOrganize().catch((err) => {
      showToast(`${text('停止归类失败: ', 'Failed to stop organize: ')}${err?.message || err}`, 'error');
    });
  });
  document.getElementById('advisor-start-btn')?.addEventListener('click', () => {
    handleStartSession().catch((err) => {
      showToast(`${text('启动会话失败: ', 'Failed to start session: ')}${err?.message || err}`, 'error');
    });
  });
  document.getElementById('advisor-toggle-context')?.addEventListener('click', () => {
    handleCardAction('', 'toggle_context_bar');
  });

  document.getElementById('advisor-summary-mode')?.addEventListener('change', (event) => {
    persistSummaryStrategy(String(event.target?.value || ''));
    renderPage();
  });
  document.getElementById('advisor-workflow-web-search')?.addEventListener('change', (event) => {
    const nextValue = !!event.target?.checked;
    syncWorkflowSearchSetting(nextValue).catch((err) => {
      showToast(`${text('保存联网搜索开关失败: ', 'Failed to save web search setting: ')}${err?.message || err}`, 'error');
      persistUseWebSearch(!nextValue);
      renderPage();
    });
  });

  const messageInput = document.getElementById('advisor-message');
  messageInput?.addEventListener('input', (event) => {
    state.messageDraft = String(event.target?.value || '');
    writePersisted(PERSIST_KEYS.messageDraft, state.messageDraft);
  });
  messageInput?.addEventListener('keydown', (event) => {
    if ((event.ctrlKey || event.metaKey) && event.key === 'Enter') {
      event.preventDefault();
      handleSend();
    }
  });

  document.getElementById('advisor-send-btn')?.addEventListener('click', handleSend);

  pageContainer.querySelectorAll('.advisor-card-action').forEach((button) => {
    button.addEventListener('click', () => handleCardAction(button.dataset.cardId, button.dataset.action));
  });
}

async function bootstrap() {
  renderPage();
  try {
    await loadWorkflowSettings();
  } catch {
    // keep persisted fallback
  }
  if (state.organizeTaskId) {
    await hydrateOrganizeSnapshot(state.organizeTaskId, { reconnect: true });
  } else {
    renderPage();
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
  closeOrganizeStream();
  pageContainer = container;
  state = createInitialState();
  bootstrap();
}
