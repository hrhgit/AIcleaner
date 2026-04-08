import {
  advisorExecuteConfirm,
  advisorExecutePreview,
  advisorExecutionRollback,
  advisorMessageSend,
  advisorPreferenceApply,
  advisorSessionGet,
  advisorSessionStart,
  advisorSuggestionUpdate,
  browseFolder,
  listScanHistory,
} from '../utils/api.js';
import { showToast } from '../main.js';
import { getLang, t } from '../utils/i18n.js';
import { formatSize } from '../utils/storage.js';
import { scanTaskController } from '../utils/scan-task-controller.js';

const PERSIST_KEYS = {
  rootPath: 'wipeout.advisor.global.root_path.v1',
  mode: 'wipeout.advisor.global.mode.v1',
  sessionId: 'wipeout.advisor.global.session_id.v1',
  messageDraft: 'wipeout.advisor.global.message_draft.v1',
  handoff: 'wipeout.advisor.global.handoff.v1',
};

const DEFAULT_MODE = 'organize_first';
const QUICK_SCAN_LIMIT = 8;

let pageContainer = null;
let state = createInitialState();

function createInitialState() {
  return {
    rootPath: resolveInitialRootPath(),
    mode: getPersisted(PERSIST_KEYS.mode, DEFAULT_MODE),
    sessionId: getPersisted(PERSIST_KEYS.sessionId, ''),
    messageDraft: getPersisted(PERSIST_KEYS.messageDraft, ''),
    sessionData: null,
    quickScans: [],
    previewData: null,
    executionData: null,
    loading: false,
    sending: false,
    previewing: false,
    confirming: false,
    rollingBack: false,
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
    // ignore storage failures
  }
}

function removePersisted(key) {
  try {
    localStorage.removeItem(key);
  } catch {
    // ignore storage failures
  }
}

function advisorText(zh, en) {
  return getLang() === 'en' ? en : zh;
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
    if (handoff?.scanTaskId) {
      setPersisted(PERSIST_KEYS.sessionId, '');
    }
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

function modeOptions() {
  return [
    { value: 'organize_first', label: advisorText('整理优先', 'Organize First') },
    { value: 'cleanup_first', label: advisorText('清理优先', 'Cleanup First') },
    { value: 'balanced', label: advisorText('均衡', 'Balanced') },
  ];
}

function formatDateTime(value) {
  if (!value) return '-';
  const date = new Date(value);
  if (Number.isNaN(date.getTime())) return String(value);
  return date.toLocaleString(getLang() === 'en' ? 'en-US' : 'zh-CN');
}

function getStatusBadgeClass(status) {
  if (['accepted', 'active', 'moved', 'recycled', 'rolled_back', 'done'].includes(status)) {
    return 'badge-success';
  }
  if (['ignored', 'snoozed', 'skipped'].includes(status)) {
    return 'badge-warning';
  }
  if (['failed', 'high', 'error'].includes(status)) {
    return 'badge-danger';
  }
  return 'badge-info';
}

function getSuggestionKindLabel(kind) {
  return {
    move: advisorText('移动', 'Move'),
    archive: advisorText('归档', 'Archive'),
    delete: advisorText('删除到回收站', 'Recycle'),
    keep: advisorText('保留', 'Keep'),
    review: advisorText('人工判断', 'Review'),
  }[String(kind || '').trim()] || kind || '-';
}

function getSuggestionStatusLabel(status) {
  return {
    new: advisorText('待处理', 'New'),
    accepted: advisorText('已采纳', 'Accepted'),
    ignored: advisorText('已忽略', 'Ignored'),
    snoozed: advisorText('稍后处理', 'Snoozed'),
    restored: advisorText('已恢复', 'Restored'),
    active: advisorText('进行中', 'Active'),
  }[String(status || '').trim()] || status || '-';
}

function getRiskLabel(risk) {
  return {
    low: advisorText('低风险', 'Low Risk'),
    medium: advisorText('中风险', 'Medium Risk'),
    high: advisorText('高风险', 'High Risk'),
  }[String(risk || '').trim()] || risk || '-';
}

function getActionLabel(action) {
  return {
    move: advisorText('移动', 'Move'),
    archive: advisorText('归档', 'Archive'),
    recycle: advisorText('回收站', 'Recycle'),
    review: advisorText('人工判断', 'Review'),
  }[String(action || '').trim()] || action || '-';
}

function getWarningLabel(code) {
  const map = {
    source_not_found: advisorText('源文件不存在', 'Source not found'),
    high_risk_requires_review: advisorText('高风险项必须人工判断', 'High-risk items require review'),
    delete_requires_low_risk: advisorText('只有低风险项才能删除到回收站', 'Only low-risk items can be recycled'),
    protected_system_path: advisorText('系统路径受保护', 'Protected system path'),
    project_path_protected: advisorText('项目目录受保护', 'Project path is protected'),
    target_path_missing: advisorText('缺少目标路径', 'Target path missing'),
    target_conflict: advisorText('目标路径已存在', 'Target path already exists'),
    source_equals_target: advisorText('源路径与目标路径相同', 'Source and target are identical'),
    review_only: advisorText('仅供人工判断，不能直接执行', 'Review only; cannot execute directly'),
    preview_blocked: advisorText('预览阶段已阻止执行', 'Blocked during preview'),
    recycle_requires_manual_restore: advisorText('回收站项需在系统中手动恢复', 'Recycle items must be restored from the OS'),
    not_moved_in_confirm: advisorText('本次确认阶段未实际移动', 'This item was not moved during confirm'),
    target_not_found: advisorText('回滚目标不存在', 'Rollback target not found'),
    source_already_exists: advisorText('原路径已存在，无法回滚', 'Original path already exists'),
  };
  if (String(code || '').startsWith('blocked_by_preference:')) {
    const kind = String(code).split(':').slice(1).join(':') || 'keep';
    return advisorText(`命中偏好保护：${kind}`, `Blocked by preference: ${kind}`);
  }
  return map[String(code || '').trim()] || String(code || '').trim();
}

function getMessageText(message) {
  const content = message?.content && typeof message.content === 'object' ? message.content : {};
  return String(content.text || content.reply || '').trim();
}

function summarizeSuggestions(suggestions = []) {
  return {
    total: suggestions.length,
    accepted: suggestions.filter((row) => row?.status === 'accepted').length,
    executable: suggestions.filter((row) => row?.status === 'accepted' && row?.executable).length,
    review: suggestions.filter((row) => row?.kind === 'review').length,
  };
}

function getExecutionActionHint({ hasSession, counts, previewData, previewing }) {
  if (!hasSession) {
    return advisorText(
      '先点击“开始顾问会话”，系统会先准备初始建议。',
      'Start an advisor session first. The app will prepare initial suggestions automatically.',
    );
  }
  if (!counts.total) {
    return advisorText(
      '当前还没有拿到初始建议。若这里持续为空，请检查最近盘点结果和 AI API 配置。',
      'No initial suggestions are available yet. If this stays empty, check recent inventory results and the AI API setup.',
    );
  }
  if (!counts.accepted) {
    return advisorText(
      '先采纳你认可的建议。采纳后系统会自动生成执行预览。',
      'Accept the suggestions you want. The execution preview will be generated automatically afterward.',
    );
  }
  if (previewing) {
    return advisorText(
      `已采纳 ${counts.accepted} 条建议，正在自动生成执行预览。`,
      `Accepted ${counts.accepted} suggestions. Building the execution preview automatically.`,
    );
  }
  if (!previewData?.previewId) {
    return advisorText(
      '已采纳建议，但执行预览暂未就绪，请稍候。',
      'Accepted suggestions are ready, but the execution preview is not available yet.',
    );
  }

  const total = Number(previewData?.summary?.total || 0);
  const canExecute = Number(previewData?.summary?.canExecute || 0);
  if (canExecute > 0) {
    return advisorText(
      `当前预览可执行 ${canExecute}/${total} 条，点击“确认执行”后才会真正落地。`,
      `${canExecute}/${total} items in the preview can be executed. Confirm to apply them.`,
    );
  }
  if (total > 0) {
    return advisorText(
      `当前预览 0/${total} 条可执行，请先查看下方“执行预览”里的阻止原因。`,
      `0/${total} preview items are executable. Check the blocked reasons in Execution Preview below.`,
    );
  }
  return advisorText(
    '当前没有可预览的采纳建议，请先调整建议状态。',
    'There are no accepted suggestions available for preview yet.',
  );
}

async function refreshExecutionPreview({ announce = false, showErrors = true } = {}) {
  const sessionId = state.sessionId;
  const counts = summarizeSuggestions(state.sessionData?.suggestions || []);
  if (!sessionId || !counts.accepted) {
    state.previewData = null;
    return null;
  }

  state.previewing = true;
  renderPage();
  try {
    state.previewData = await advisorExecutePreview({ sessionId });
    if (announce) {
      showToast(advisorText('已自动生成执行预览', 'Execution preview refreshed automatically'), 'success');
    }
    return state.previewData;
  } catch (err) {
    state.previewData = null;
    if (showErrors) {
      showToast(`${advisorText('自动生成预览失败：', 'Failed to refresh preview automatically: ')}${err?.message || err}`, 'error');
    }
    return null;
  } finally {
    state.previewing = false;
    renderPage();
  }
}

function renderQuickScanRows() {
  if (!state.quickScans.length) {
    return `
      <div class="empty-state advisor-empty-compact">
        <div class="empty-state-text">${escapeHtml(advisorText('暂无可复用的盘点记录', 'No reusable scan records yet'))}</div>
      </div>
    `;
  }

  return state.quickScans.map((task) => {
    const taskId = String(task?.taskId || task?.id || '').trim();
    const rootPath = String(task?.targetPath || task?.rootPath || '').trim();
    return `
      <button class="advisor-quick-scan" type="button" data-task-id="${escapeHtml(taskId)}" data-root-path="${escapeHtml(rootPath)}">
        <span class="advisor-quick-scan-title">${escapeHtml(rootPath || '-')}</span>
        <span class="advisor-quick-scan-meta">
          ${escapeHtml(formatSize(task?.totalCleanable || 0))}
          · ${escapeHtml(formatDateTime(task?.updatedAt || task?.createdAt))}
        </span>
      </button>
    `;
  }).join('');
}

function renderMessageRows() {
  const messages = Array.isArray(state.sessionData?.messages) ? state.sessionData.messages : [];
  if (!messages.length) {
    return `
      <div class="empty-state advisor-empty-compact">
        <div class="empty-state-text">${escapeHtml(advisorText('输入你的偏好，AI 会基于盘点结果重新整理建议。', 'Share your preferences and the advisor will refine the suggestions.'))}</div>
      </div>
    `;
  }

  return messages.map((message) => {
    const role = String(message?.role || 'assistant').trim();
    const text = getMessageText(message);
    const drafts = Array.isArray(message?.content?.preferenceDrafts) ? message.content.preferenceDrafts : [];
    return `
      <div class="advisor-message advisor-message-${escapeHtml(role)}">
        <div class="advisor-message-head">
          <span class="badge ${role === 'user' ? 'badge-info' : 'badge-success'}">${escapeHtml(role === 'user' ? advisorText('你', 'You') : advisorText('顾问', 'Advisor'))}</span>
          <span class="form-hint">${escapeHtml(formatDateTime(message?.createdAt))}</span>
        </div>
        <div class="advisor-message-body">${escapeHtml(text || advisorText('无文本内容', 'No text content'))}</div>
        ${drafts.length ? `<div class="form-hint">${escapeHtml(advisorText(`本轮提取了 ${drafts.length} 条偏好草案`, `${drafts.length} preference drafts extracted`))}</div>` : ''}
      </div>
    `;
  }).join('');
}

function renderPreferenceDrafts() {
  const drafts = Array.isArray(state.sessionData?.pendingPreferenceDrafts) ? state.sessionData.pendingPreferenceDrafts : [];
  if (!drafts.length) {
    return `
      <div class="empty-state advisor-empty-compact">
        <div class="empty-state-text">${escapeHtml(advisorText('当前没有待确认的偏好草案', 'No pending preference drafts'))}</div>
      </div>
    `;
  }

  return drafts.map((draft) => `
    <div class="advisor-draft-card">
      <div class="advisor-draft-head">
        <div>
          <div class="card-title">${escapeHtml(getSuggestionKindLabel(draft?.kind))}</div>
          <div class="form-hint">${escapeHtml(String(draft?.reason || advisorText('未提供原因', 'No reason provided')))}</div>
        </div>
        <span class="badge badge-info">${escapeHtml(draft?.scope === 'global_suggested' ? advisorText('建议全局记住', 'Suggested global') : advisorText('会话临时', 'Session only'))}</span>
      </div>
      <pre class="advisor-json">${escapeHtml(JSON.stringify(draft?.rule || {}, null, 2))}</pre>
      <div class="advisor-inline-actions">
        <button class="btn btn-primary advisor-apply-draft" type="button" data-draft-id="${escapeHtml(draft?.draftId || '')}" data-scope="session">${escapeHtml(advisorText('仅本次会话记住', 'Apply to session'))}</button>
        <button class="btn btn-secondary advisor-apply-draft" type="button" data-draft-id="${escapeHtml(draft?.draftId || '')}" data-scope="global">${escapeHtml(advisorText('设为全局偏好', 'Save globally'))}</button>
      </div>
    </div>
  `).join('');
}

function renderPreferenceRows() {
  const preferences = Array.isArray(state.sessionData?.preferences) ? state.sessionData.preferences : [];
  if (!preferences.length) {
    return `
      <div class="empty-state advisor-empty-compact">
        <div class="empty-state-text">${escapeHtml(advisorText('还没有已保存的偏好', 'No saved preferences yet'))}</div>
      </div>
    `;
  }

  return preferences.map((preference) => `
    <div class="advisor-pref-chip">
      <span class="badge ${preference?.scope === 'global' ? 'badge-success' : 'badge-info'}">${escapeHtml(preference?.scope === 'global' ? advisorText('全局', 'Global') : advisorText('会话', 'Session'))}</span>
      <span>${escapeHtml(getSuggestionKindLabel(preference?.kind))}</span>
      <span class="form-hint">${escapeHtml(String(preference?.reason || advisorText('已保存规则', 'Saved rule')))}</span>
    </div>
  `).join('');
}

function renderSuggestionRows() {
  const suggestions = Array.isArray(state.sessionData?.suggestions) ? state.sessionData.suggestions : [];
  if (!suggestions.length) {
    return `
      <div class="empty-state">
        <div class="empty-state-text">${escapeHtml(advisorText('当前还没有建议。系统会优先自动生成初始建议；如果这里仍为空，再通过对话补充偏好。', 'No suggestions yet. The app tries to generate initial suggestions automatically. If this stays empty, refine it through chat.'))}</div>
      </div>
    `;
  }

  return suggestions.map((suggestion) => {
    const status = String(suggestion?.status || 'new').trim();
    const why = Array.isArray(suggestion?.why) ? suggestion.why : [];
    const triggered = Array.isArray(suggestion?.triggeredPreferences) ? suggestion.triggeredPreferences : [];
    return `
      <article class="advisor-suggestion-card">
        <div class="advisor-suggestion-head">
          <div class="advisor-suggestion-title-wrap">
            <h3 class="card-title">${escapeHtml(String(suggestion?.title || advisorText('未命名建议', 'Untitled suggestion')))}</h3>
            <div class="advisor-badge-row">
              <span class="badge badge-info">${escapeHtml(getSuggestionKindLabel(suggestion?.kind))}</span>
              <span class="badge ${getStatusBadgeClass(status)}">${escapeHtml(getSuggestionStatusLabel(status))}</span>
              <span class="badge ${getStatusBadgeClass(suggestion?.risk)}">${escapeHtml(getRiskLabel(suggestion?.risk))}</span>
            </div>
          </div>
          <div class="form-hint">${escapeHtml(advisorText('置信度', 'Confidence'))}: ${escapeHtml(String(suggestion?.confidence || '-'))}</div>
        </div>
        <div class="advisor-suggestion-summary">${escapeHtml(String(suggestion?.summary || ''))}</div>
        <div class="advisor-path-block">
          <div><span class="form-label">${escapeHtml(advisorText('源路径', 'Source'))}</span><div class="advisor-path mono">${escapeHtml(String(suggestion?.path || '-'))}</div></div>
          ${suggestion?.targetPath ? `<div><span class="form-label">${escapeHtml(advisorText('目标路径', 'Target'))}</span><div class="advisor-path mono">${escapeHtml(String(suggestion.targetPath))}</div></div>` : ''}
        </div>
        ${why.length ? `<div class="advisor-list-block"><div class="form-label">${escapeHtml(advisorText('依据', 'Why'))}</div>${why.map((item) => `<div class="advisor-list-row">${escapeHtml(String(item || ''))}</div>`).join('')}</div>` : ''}
        ${triggered.length ? `<div class="advisor-list-block"><div class="form-label">${escapeHtml(advisorText('命中偏好', 'Triggered Preferences'))}</div>${triggered.map((item) => `<div class="advisor-list-row">${escapeHtml(String(item || ''))}</div>`).join('')}</div>` : ''}
        <div class="advisor-inline-actions">
          <button class="btn btn-primary advisor-set-suggestion" type="button" data-suggestion-id="${escapeHtml(suggestion?.suggestionId || '')}" data-status="accepted">${escapeHtml(advisorText('采纳', 'Accept'))}</button>
          <button class="btn btn-secondary advisor-set-suggestion" type="button" data-suggestion-id="${escapeHtml(suggestion?.suggestionId || '')}" data-status="ignored">${escapeHtml(advisorText('忽略', 'Ignore'))}</button>
          <button class="btn btn-ghost advisor-set-suggestion" type="button" data-suggestion-id="${escapeHtml(suggestion?.suggestionId || '')}" data-status="snoozed">${escapeHtml(advisorText('稍后处理', 'Snooze'))}</button>
          ${(status === 'ignored' || status === 'snoozed') ? `<button class="btn btn-ghost advisor-set-suggestion" type="button" data-suggestion-id="${escapeHtml(suggestion?.suggestionId || '')}" data-status="restored">${escapeHtml(advisorText('恢复', 'Restore'))}</button>` : ''}
        </div>
      </article>
    `;
  }).join('');
}

function renderPreviewRows() {
  const preview = state.previewData;
  if (!preview?.entries?.length) {
    return `
      <div class="empty-state advisor-empty-compact">
        <div class="empty-state-text">${escapeHtml(advisorText('还没有执行预览。先采纳建议，系统会自动生成预览。', 'No execution preview yet. Accept suggestions first and the preview will be generated automatically.'))}</div>
      </div>
    `;
  }

  return preview.entries.map((entry) => {
    const warnings = Array.isArray(entry?.warnings) ? entry.warnings : [];
    return `
      <div class="advisor-preview-row">
        <div class="advisor-preview-head">
          <div class="advisor-badge-row">
            <span class="badge badge-info">${escapeHtml(getActionLabel(entry?.action))}</span>
            <span class="badge ${entry?.canExecute ? 'badge-success' : 'badge-danger'}">${escapeHtml(entry?.canExecute ? advisorText('可执行', 'Executable') : advisorText('已阻止', 'Blocked'))}</span>
            <span class="badge ${getStatusBadgeClass(entry?.risk)}">${escapeHtml(getRiskLabel(entry?.risk))}</span>
          </div>
          <div class="form-hint">${escapeHtml(String(entry?.title || ''))}</div>
        </div>
        <div class="advisor-path mono">${escapeHtml(String(entry?.sourcePath || '-'))}</div>
        ${entry?.targetPath ? `<div class="advisor-path mono">${escapeHtml(String(entry.targetPath))}</div>` : ''}
        ${warnings.length ? `<div class="advisor-list-block">${warnings.map((code) => `<div class="advisor-list-row">${escapeHtml(getWarningLabel(code))}</div>`).join('')}</div>` : ''}
      </div>
    `;
  }).join('');
}

function renderExecutionRows(entries = []) {
  if (!entries.length) {
    return `
      <div class="empty-state advisor-empty-compact">
        <div class="empty-state-text">${escapeHtml(advisorText('还没有执行记录', 'No execution records yet'))}</div>
      </div>
    `;
  }

  return entries.map((entry) => `
    <div class="advisor-preview-row">
      <div class="advisor-preview-head">
        <div class="advisor-badge-row">
          <span class="badge badge-info">${escapeHtml(getActionLabel(entry?.action))}</span>
          <span class="badge ${getStatusBadgeClass(entry?.status)}">${escapeHtml(String(entry?.status || '-'))}</span>
        </div>
        <div class="form-hint">${escapeHtml(String(entry?.error ? getWarningLabel(entry.error) : advisorText('执行成功', 'Succeeded')))}</div>
      </div>
      <div class="advisor-path mono">${escapeHtml(String(entry?.sourcePath || '-'))}</div>
      ${entry?.targetPath ? `<div class="advisor-path mono">${escapeHtml(String(entry.targetPath))}</div>` : ''}
    </div>
  `).join('');
}

function renderContextStats() {
  const context = state.sessionData?.contextSummary || {};
  const scan = context?.scanSummary || {};
  const organize = context?.organizeSummary || {};
  const counts = summarizeSuggestions(state.sessionData?.suggestions || []);
  const lastResult = state.sessionData?.lastExecution?.result?.summary || {};
  return `
    <div class="stats-grid">
      <div class="stat-card">
        <span class="stat-label">${escapeHtml(advisorText('可清理空间', 'Cleanable'))}</span>
        <span class="stat-value accent">${escapeHtml(formatSize(scan?.totalCleanable || 0))}</span>
      </div>
      <div class="stat-card">
        <span class="stat-label">${escapeHtml(advisorText('删除候选', 'Delete Candidates'))}</span>
        <span class="stat-value">${escapeHtml(String(scan?.deletableCount || 0))}</span>
      </div>
      <div class="stat-card">
        <span class="stat-label">${escapeHtml(advisorText('已采纳建议', 'Accepted Suggestions'))}</span>
        <span class="stat-value success">${escapeHtml(String(counts.accepted))}</span>
      </div>
      <div class="stat-card">
        <span class="stat-label">${escapeHtml(advisorText('最近执行', 'Last Execution'))}</span>
        <span class="stat-value warning">${escapeHtml(String(lastResult?.total || 0))}</span>
      </div>
      <div class="stat-card">
        <span class="stat-label">${escapeHtml(advisorText('最近整理文件数', 'Latest Organized Files'))}</span>
        <span class="stat-value">${escapeHtml(String(organize?.processedFiles || 0))}</span>
      </div>
      <div class="stat-card">
        <span class="stat-label">${escapeHtml(advisorText('人工判断项', 'Review Items'))}</span>
        <span class="stat-value danger">${escapeHtml(String(counts.review))}</span>
      </div>
    </div>
  `;
}

function renderPage() {
  if (!pageContainer) return;
  const modes = modeOptions();
  const currentScan = getCurrentScanSnapshot();
  const activeRoot = String(state.rootPath || '').trim();
  const hasSession = !!state.sessionData?.session?.sessionId;
  const counts = summarizeSuggestions(state.sessionData?.suggestions || []);
  const lastExecution = state.executionData || state.sessionData?.lastExecution || null;
  const lastExecutionResult = lastExecution?.result || null;
  const lastRollback = lastExecution?.rollback || null;
  const actionHint = getExecutionActionHint({
    hasSession,
    counts,
    previewData: state.previewData,
    previewing: state.previewing,
  });

  pageContainer.innerHTML = `
    <div class="advisor-shell">
      <section class="card advisor-hero animate-in">
        <div class="advisor-hero-grid">
          <div class="page-header" style="margin-bottom:0;">
            <div class="organizer-kicker">${escapeHtml(advisorText('AI 清理顾问', 'AI Cleanup Advisor'))}</div>
            <h1 class="page-title">${escapeHtml(advisorText('盘点 + 对话 + 人工确认执行', 'Inventory + Chat + Confirmed Execution'))}</h1>
            <p class="page-subtitle">${escapeHtml(advisorText('先复用最近盘点结果，再根据你的偏好生成建议。删除默认只会进入系统回收站。', 'Reuse the latest inventory, refine suggestions through chat, and confirm actions manually. Delete suggestions go to the recycle bin by default.'))}</p>
          </div>

          <div class="advisor-config-grid">
            <div class="form-group">
              <label class="form-label" for="advisor-root-path">${escapeHtml(advisorText('工作目录', 'Working Folder'))}</label>
              <div class="advisor-path-actions">
                <input id="advisor-root-path" class="form-input" type="text" value="${escapeHtml(activeRoot)}" placeholder="C:\\Users\\..." />
                <button id="advisor-browse-btn" class="btn btn-secondary" type="button">${escapeHtml(t('settings.browse'))}</button>
              </div>
              <div class="form-hint">${escapeHtml(currentScan?.targetPath ? advisorText(`最近盘点：${currentScan.targetPath}`, `Latest scan: ${currentScan.targetPath}`) : advisorText('没有活动中的盘点快照时，会尝试读取最近一次历史盘点。', 'If no active scan snapshot exists, the advisor falls back to the latest historical scan.'))}</div>
            </div>

            <div class="form-group">
              <label class="form-label" for="advisor-mode">${escapeHtml(advisorText('建议模式', 'Advisor Mode'))}</label>
              <select id="advisor-mode" class="form-input">
                ${modes.map((mode) => `<option value="${escapeHtml(mode.value)}" ${mode.value === state.mode ? 'selected' : ''}>${escapeHtml(mode.label)}</option>`).join('')}
              </select>
            </div>

            <div class="advisor-inline-actions">
              <button id="advisor-start-btn" class="btn btn-primary" type="button" ${state.loading ? 'disabled' : ''}>${escapeHtml(hasSession ? advisorText('按当前模式重建会话', 'Rebuild Session') : advisorText('开始顾问会话', 'Start Session'))}</button>
              <button id="advisor-confirm-btn" class="btn btn-success" type="button" ${(state.previewData?.summary?.canExecute && !state.confirming) ? '' : 'disabled'}>${escapeHtml(state.confirming ? advisorText('执行中...', 'Executing...') : advisorText('确认执行', 'Confirm Execute'))}</button>
            </div>
            <div class="advisor-action-hint form-hint">${escapeHtml(actionHint)}</div>
          </div>
        </div>
      </section>

      <section class="card animate-in" style="animation-delay:0.05s;">
        <div class="card-header">
          <div>
            <h2 class="card-title">${escapeHtml(advisorText('快速选择最近盘点', 'Quick Pick Recent Inventory'))}</h2>
            <div class="form-hint">${escapeHtml(advisorText('点击任一盘点记录，会直接以该目录和任务上下文启动顾问会话。', 'Pick a recent scan to start the advisor with that directory and scan context.'))}</div>
          </div>
        </div>
        <div class="advisor-quick-scan-grid">${renderQuickScanRows()}</div>
      </section>

      ${hasSession ? renderContextStats() : ''}

      <div class="advisor-main-grid">
        <section class="card animate-in" style="animation-delay:0.08s;">
          <div class="card-header">
            <div>
              <h2 class="card-title">${escapeHtml(advisorText('对话与偏好', 'Chat and Preferences'))}</h2>
              <div class="form-hint">${escapeHtml(advisorText('这里的对话不会直接删除文件，只会更新建议。', 'Conversation updates suggestions only; it does not delete files directly.'))}</div>
            </div>
            <span class="badge badge-info">${escapeHtml(hasSession ? advisorText(`会话 ${state.sessionData?.session?.mode || state.mode}`, `Session ${state.sessionData?.session?.mode || state.mode}`) : advisorText('未开始', 'Not started'))}</span>
          </div>

          <div class="advisor-message-list">${renderMessageRows()}</div>

          <div class="form-group" style="margin-top:18px;">
            <label class="form-label" for="advisor-message-input">${escapeHtml(advisorText('告诉 AI 你的偏好', 'Tell the Advisor Your Preference'))}</label>
            <textarea id="advisor-message-input" class="form-input" rows="4" placeholder="${escapeHtml(advisorText('例如：项目目录不要删，安装包默认归档，三个月前的截图可以优先清理。', 'For example: do not delete project folders, archive installers by default, and prioritize cleaning screenshots older than 3 months.'))}">${escapeHtml(state.messageDraft)}</textarea>
          </div>
          <div class="advisor-inline-actions">
            <button id="advisor-send-btn" class="btn btn-primary" type="button" ${(hasSession && !state.sending) ? '' : 'disabled'}>${escapeHtml(state.sending ? advisorText('发送中...', 'Sending...') : advisorText('发送给顾问', 'Send to Advisor'))}</button>
          </div>

          <div class="advisor-split-grid">
            <div>
              <div class="card-title" style="margin-bottom:12px;">${escapeHtml(advisorText('待确认偏好草案', 'Pending Preference Drafts'))}</div>
              ${renderPreferenceDrafts()}
            </div>
            <div>
              <div class="card-title" style="margin-bottom:12px;">${escapeHtml(advisorText('已生效偏好', 'Applied Preferences'))}</div>
              <div class="advisor-pref-list">${renderPreferenceRows()}</div>
            </div>
          </div>
        </section>

        <section class="card animate-in" style="animation-delay:0.11s;">
          <div class="card-header">
            <div>
              <h2 class="card-title">${escapeHtml(advisorText('建议清单', 'Suggestions'))}</h2>
              <div class="form-hint">${escapeHtml(advisorText(`共 ${counts.total} 条建议，其中 ${counts.accepted} 条已采纳，${counts.executable} 条可执行。`, `${counts.total} suggestions, ${counts.accepted} accepted, ${counts.executable} executable.`))}</div>
            </div>
          </div>
          <div class="advisor-suggestion-list">${renderSuggestionRows()}</div>
        </section>
      </div>

      <div class="advisor-main-grid">
        <section class="card animate-in" style="animation-delay:0.14s;">
          <div class="card-header">
            <div>
              <h2 class="card-title">${escapeHtml(advisorText('执行预览', 'Execution Preview'))}</h2>
              <div class="form-hint">${escapeHtml(advisorText('预览会把建议映射成移动 / 归档 / 回收站动作，并阻止高风险项。', 'Preview maps suggestions into move/archive/recycle actions and blocks risky items.'))}</div>
            </div>
            ${state.previewData?.summary ? `<span class="badge badge-info">${escapeHtml(advisorText(`可执行 ${state.previewData.summary.canExecute}/${state.previewData.summary.total}`, `Executable ${state.previewData.summary.canExecute}/${state.previewData.summary.total}`))}</span>` : ''}
          </div>
          ${renderPreviewRows()}
        </section>

        <section class="card animate-in" style="animation-delay:0.17s;">
          <div class="card-header">
            <div>
              <h2 class="card-title">${escapeHtml(advisorText('执行结果与回滚', 'Execution Results and Rollback'))}</h2>
              <div class="form-hint">${escapeHtml(advisorText('移动 / 归档支持回滚；进入回收站的项需要在系统回收站中恢复。', 'Move/archive entries can be rolled back. Recycled entries must be restored from the OS recycle bin.'))}</div>
            </div>
            <div class="advisor-inline-actions">
              <button id="advisor-rollback-btn" class="btn btn-secondary" type="button" ${(lastExecution?.jobId && lastExecutionResult?.entries?.length && !state.rollingBack) ? '' : 'disabled'}>${escapeHtml(state.rollingBack ? advisorText('回滚中...', 'Rolling Back...') : advisorText('回滚可回滚项', 'Rollback'))}</button>
            </div>
          </div>
          <div class="advisor-list-block">
            <div class="advisor-list-row">${escapeHtml(advisorText('最近执行时间', 'Latest execution'))}: ${escapeHtml(formatDateTime(lastExecution?.createdAt || lastExecutionResult?.at))}</div>
            ${lastExecutionResult?.summary ? `<div class="advisor-list-row">${escapeHtml(advisorText(`结果：移动 ${lastExecutionResult.summary.moved || 0}，回收站 ${lastExecutionResult.summary.recycled || 0}，失败 ${lastExecutionResult.summary.failed || 0}`, `Result: moved ${lastExecutionResult.summary.moved || 0}, recycled ${lastExecutionResult.summary.recycled || 0}, failed ${lastExecutionResult.summary.failed || 0}`))}</div>` : ''}
            ${lastRollback?.summary ? `<div class="advisor-list-row">${escapeHtml(advisorText(`回滚：成功 ${lastRollback.summary.rolledBack || 0}，不可回滚 ${lastRollback.summary.notRollbackable || 0}，失败 ${lastRollback.summary.failed || 0}`, `Rollback: ${lastRollback.summary.rolledBack || 0} rolled back, ${lastRollback.summary.notRollbackable || 0} not rollbackable, ${lastRollback.summary.failed || 0} failed`))}</div>` : ''}
          </div>
          ${renderExecutionRows(lastRollback?.entries || lastExecutionResult?.entries || [])}
        </section>
      </div>
    </div>
  `;

  bindEvents();
}

function bindEvents() {
  document.getElementById('advisor-root-path')?.addEventListener('input', (event) => {
    state.rootPath = String(event.target?.value || '').trim();
    setPersisted(PERSIST_KEYS.rootPath, state.rootPath);
  });

  document.getElementById('advisor-message-input')?.addEventListener('input', (event) => {
    state.messageDraft = String(event.target?.value || '');
    setPersisted(PERSIST_KEYS.messageDraft, state.messageDraft);
  });

  document.getElementById('advisor-mode')?.addEventListener('change', (event) => {
    state.mode = String(event.target?.value || DEFAULT_MODE);
    setPersisted(PERSIST_KEYS.mode, state.mode);
  });

  document.getElementById('advisor-browse-btn')?.addEventListener('click', handleBrowse);
  document.getElementById('advisor-start-btn')?.addEventListener('click', () => startSession());
  document.getElementById('advisor-send-btn')?.addEventListener('click', handleSendMessage);
  document.getElementById('advisor-confirm-btn')?.addEventListener('click', handleConfirmExecute);
  document.getElementById('advisor-rollback-btn')?.addEventListener('click', handleRollback);

  document.querySelectorAll('.advisor-quick-scan').forEach((button) => {
    button.addEventListener('click', () => {
      startSession({
        rootPath: button.dataset.rootPath,
        scanTaskId: button.dataset.taskId,
      });
    });
  });

  document.querySelectorAll('.advisor-apply-draft').forEach((button) => {
    button.addEventListener('click', () => applyPreferenceDraft(button.dataset.draftId, button.dataset.scope));
  });

  document.querySelectorAll('.advisor-set-suggestion').forEach((button) => {
    button.addEventListener('click', () => updateSuggestionStatus(button.dataset.suggestionId, button.dataset.status));
  });
}

async function refreshQuickScans() {
  try {
    state.quickScans = await listScanHistory(QUICK_SCAN_LIMIT);
  } catch (err) {
    console.warn('[Advisor] Failed to load scan history:', err);
    state.quickScans = [];
  }
}

async function loadSession(sessionId, { silent = false } = {}) {
  if (!sessionId) return;
  if (!silent) {
    state.loading = true;
    renderPage();
  }
  try {
    const payload = await advisorSessionGet(sessionId);
    state.sessionData = payload;
    state.sessionId = payload?.session?.sessionId || sessionId;
    state.rootPath = String(payload?.session?.rootPath || state.rootPath || '').trim();
    setPersisted(PERSIST_KEYS.sessionId, state.sessionId);
    setPersisted(PERSIST_KEYS.rootPath, state.rootPath);
  } catch (err) {
    state.sessionData = null;
    state.sessionId = '';
    removePersisted(PERSIST_KEYS.sessionId);
    showToast(`${advisorText('加载顾问会话失败：', 'Failed to load advisor session: ')}${err?.message || err}`, 'error');
  } finally {
    state.loading = false;
    renderPage();
  }
}

async function startSession(override = {}) {
  const rootPath = String(override.rootPath || state.rootPath || '').trim();
  const scanTaskId = String(override.scanTaskId || '').trim();
  if (!rootPath) {
    showToast(advisorText('请先选择工作目录', 'Select a working folder first'), 'error');
    return;
  }

  state.loading = true;
  state.previewData = null;
  state.executionData = null;
  renderPage();
  try {
    const payload = await advisorSessionStart({
      rootPath,
      scanTaskId: scanTaskId || undefined,
      mode: state.mode,
      responseLanguage: getLang(),
    });
    state.sessionData = payload;
    state.sessionId = payload?.session?.sessionId || '';
    state.rootPath = rootPath;
    setPersisted(PERSIST_KEYS.sessionId, state.sessionId);
    setPersisted(PERSIST_KEYS.rootPath, state.rootPath);
    await refreshExecutionPreview({ showErrors: false });
    showToast(
      (payload?.suggestions?.length || 0)
        ? advisorText('顾问会话已准备好，并已生成初始建议', 'Advisor session is ready with initial suggestions')
        : advisorText('顾问会话已准备好，但当前还没有可用建议', 'Advisor session is ready, but no suggestions are available yet'),
      'success',
    );
  } catch (err) {
    showToast(`${advisorText('启动顾问会话失败：', 'Failed to start advisor session: ')}${err?.message || err}`, 'error');
  } finally {
    state.loading = false;
    renderPage();
  }
}

async function handleBrowse() {
  try {
    const picked = await browseFolder();
    if (picked?.cancelled || !picked?.path) return;
    state.rootPath = picked.path;
    setPersisted(PERSIST_KEYS.rootPath, state.rootPath);
    renderPage();
  } catch (err) {
    showToast(`${t('settings.toast_browse_failed')}${err?.message || err}`, 'error');
  }
}

async function handleSendMessage() {
  const sessionId = state.sessionId;
  const message = String(document.getElementById('advisor-message-input')?.value || '').trim();
  if (!sessionId) {
    showToast(advisorText('请先开始顾问会话', 'Start a session first'), 'error');
    return;
  }
  if (!message) {
    showToast(advisorText('请输入想告诉 AI 的偏好', 'Enter a preference or question first'), 'error');
    return;
  }

  state.sending = true;
  state.messageDraft = message;
  setPersisted(PERSIST_KEYS.messageDraft, state.messageDraft);
  renderPage();
  try {
    await advisorMessageSend({ sessionId, message });
    state.messageDraft = '';
    setPersisted(PERSIST_KEYS.messageDraft, '');
    await loadSession(sessionId, { silent: true });
    await refreshExecutionPreview({ showErrors: false });
    showToast(advisorText('顾问建议已更新', 'Advisor suggestions refreshed'), 'success');
  } catch (err) {
    showToast(`${advisorText('发送消息失败：', 'Failed to send message: ')}${err?.message || err}`, 'error');
  } finally {
    state.sending = false;
    renderPage();
  }
}

async function applyPreferenceDraft(draftId, scope) {
  if (!state.sessionId || !draftId) return;
  try {
    await advisorPreferenceApply({
      sessionId: state.sessionId,
      draftId,
      scope: scope === 'global' ? 'global' : 'session',
      enabled: true,
    });
    await loadSession(state.sessionId, { silent: true });
    await refreshExecutionPreview({ showErrors: false });
    showToast(
      scope === 'global'
        ? advisorText('偏好已保存为全局规则', 'Preference saved globally')
        : advisorText('偏好已保存到当前会话', 'Preference saved to this session'),
      'success',
    );
  } catch (err) {
    showToast(`${advisorText('应用偏好失败：', 'Failed to apply preference: ')}${err?.message || err}`, 'error');
  } finally {
    renderPage();
  }
}

async function updateSuggestionStatus(suggestionId, status) {
  if (!state.sessionId || !suggestionId || !status) return;
  try {
    await advisorSuggestionUpdate({
      sessionId: state.sessionId,
      suggestionId,
      status,
    });
    await loadSession(state.sessionId, { silent: true });
    await refreshExecutionPreview({ showErrors: false });
  } catch (err) {
    showToast(`${advisorText('更新建议状态失败：', 'Failed to update suggestion state: ')}${err?.message || err}`, 'error');
  } finally {
    renderPage();
  }
}

async function handlePreview() {
  await refreshExecutionPreview({ announce: true });
}

async function handleConfirmExecute() {
  if (state.sessionId && !state.previewData?.previewId) {
    await refreshExecutionPreview({ showErrors: false });
  }
  if (!state.sessionId || !state.previewData?.previewId) {
    showToast(advisorText('请先采纳建议，系统会自动生成执行预览', 'Accept suggestions first. The preview will be generated automatically.'), 'error');
    return;
  }
  const canExecute = Number(state.previewData?.summary?.canExecute || 0);
  if (!canExecute) {
    showToast(advisorText('当前预览里没有可执行项', 'There are no executable items in the preview'), 'error');
    return;
  }
  if (!confirm(advisorText(`确认执行这 ${canExecute} 条建议吗？删除项会进入系统回收站。`, `Execute these ${canExecute} suggestions? Delete items will be sent to the recycle bin.`))) {
    return;
  }

  state.confirming = true;
  renderPage();
  try {
    state.executionData = await advisorExecuteConfirm({
      sessionId: state.sessionId,
      previewId: state.previewData.previewId,
    });
    await loadSession(state.sessionId, { silent: true });
    showToast(advisorText('已执行确认通过的建议', 'Accepted suggestions have been executed'), 'success');
  } catch (err) {
    showToast(`${advisorText('执行失败：', 'Execution failed: ')}${err?.message || err}`, 'error');
  } finally {
    state.confirming = false;
    renderPage();
  }
}

async function handleRollback() {
  const jobId = state.executionData?.jobId || state.sessionData?.lastExecution?.jobId;
  if (!jobId) {
    showToast(advisorText('没有可回滚的执行记录', 'No execution record to roll back'), 'error');
    return;
  }
  if (!confirm(advisorText('回滚会撤销已移动或已归档的项目，回收站项不会自动恢复。继续吗？', 'Rollback will undo moved or archived items. Recycled items are not restored automatically. Continue?'))) {
    return;
  }

  state.rollingBack = true;
  renderPage();
  try {
    const rollback = await advisorExecutionRollback(jobId);
    state.executionData = {
      ...(state.executionData || state.sessionData?.lastExecution || {}),
      ...rollback,
    };
    await loadSession(state.sessionId, { silent: true });
    showToast(advisorText('已完成可回滚项的恢复', 'Rollback completed for reversible items'), 'success');
  } catch (err) {
    showToast(`${advisorText('回滚失败：', 'Rollback failed: ')}${err?.message || err}`, 'error');
  } finally {
    state.rollingBack = false;
    renderPage();
  }
}

export async function renderAdvisor(container) {
  pageContainer = container;
  state = createInitialState();
  await refreshQuickScans();
  renderPage();

  const handoff = getPendingHandoff();
  if (handoff?.rootPath) {
    await startSession(handoff);
    return;
  }

  if (state.sessionId) {
    await loadSession(state.sessionId);
    return;
  }

  if (state.rootPath) {
    await startSession();
  }
}
