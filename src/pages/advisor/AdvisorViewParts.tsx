import { useMemo, useState } from 'react';
import type { AdvisorCard, AdvisorSessionData, AdvisorCardAction, JsonRecord, OrganizeResultRow, OrganizeSnapshot, TimelineTurn, TreeNode } from '../../types';
import { text } from '../../utils/i18n';
import {
  formatDateTime,
  getCurrentSnapshot,
  getOrganizeProgress,
  getOrganizeStatusLabel,
  getStageLabel,
  isOrganizeFinished,
  isOrganizeRunning,
  summaryModeHint,
  summaryModeLabel,
  type AdvisorWorkflowState,
} from './model';
import { SUMMARY_MODES } from './constants';
import {
  buildOrganizeBrowserTree,
  findOrganizeBrowserFolder,
  type OrganizeBrowserFile,
  type OrganizeBrowserFolder,
} from './organizeBrowser';

export function TreeList({ childrenNodes, limit = 12 }: { childrenNodes: TreeNode[]; limit?: number }) {
  if (!childrenNodes.length) return null;
  return (
    <ul className="advisor-tree-list">
      {childrenNodes.slice(0, limit).map((node, index) => (
        <TreeNodeView key={`${node.nodeId || node.id || node.name || 'node'}-${index}`} node={node} />
      ))}
    </ul>
  );
}

function TreeNodeView({ node }: { node: TreeNode }) {
  const children = Array.isArray(node.children) ? node.children : [];
  return (
    <li>
      <span>{node.name || '-'}</span>
      <span className="advisor-tree-count">{node.itemCount || 0}</span>
      {children.length ? <TreeList childrenNodes={children} /> : null}
    </li>
  );
}

function summarizeCard(card: AdvisorCard): string {
  if (card.cardType === 'tree') {
    const count = Number((card.body?.stats as JsonRecord | undefined)?.itemCount || 0);
    return text(`当前树覆盖 ${count} 个项目。`, `Tree covers ${count} items.`);
  }
  if (card.cardType === 'plan_preview') {
    const summary = (card.body?.summary || {}) as JsonRecord;
    return text(
      `共 ${summary.total || 0} 项，可执行 ${summary.canExecute || 0} 项。`,
      `${summary.total || 0} items, ${summary.canExecute || 0} executable.`,
    );
  }
  if (card.cardType === 'execution_result') {
    const result = (card.body?.result || {}) as JsonRecord;
    const summary = (result.summary || {}) as JsonRecord;
    return text(`总计 ${summary.total ?? '-'}，失败 ${summary.failed || 0}。`, `Total ${summary.total ?? '-'}, failed ${summary.failed || 0}.`);
  }
  return String(card.body?.summary || card.body?.message || '').trim();
}

function PlanEntries({ entries }: { entries: unknown[] }) {
  return (
    <div className="advisor-entry-list">
      {entries.slice(0, 10).map((raw, index) => {
        const entry = (raw || {}) as JsonRecord;
        return (
          <div className="advisor-entry-row" key={index}>
            <div className="advisor-entry-copy">
              <div>{String(entry.name || entry.sourcePath || '-')}</div>
              <div className="form-hint">{String(entry.sourcePath || '-')}</div>
            </div>
            <span className={`badge ${entry.canExecute ? 'badge-success' : 'badge-warning'}`}>
              {String(entry.action || '-')}
            </span>
          </div>
        );
      })}
    </div>
  );
}

function AdvisorCardView({
  card,
  acting,
  onAction,
}: {
  card: AdvisorCard;
  acting: boolean;
  onAction: (cardId: string, action: string) => void;
}) {
  const actions = Array.isArray(card.actions) ? card.actions : [];
  const entries = Array.isArray(card.body?.entries) ? card.body.entries : [];
  const treeChildren = Array.isArray(((card.body?.tree || {}) as { children?: TreeNode[] }).children)
    ? ((card.body?.tree || {}) as { children?: TreeNode[] }).children || []
    : [];

  return (
    <article className={`advisor-card advisor-card-${card.cardType || 'generic'}`}>
      <div className="advisor-card-head">
        <div className="advisor-card-title-group">
          <div className="card-title">{card.title || '-'}</div>
          <div className="form-hint">{formatDateTime(card.createdAt)}</div>
        </div>
        <span className="badge badge-info">{card.status || 'ready'}</span>
      </div>

      {card.cardType === 'tree' ? (
        <>
          <div className="advisor-card-copy">{summarizeCard(card)}</div>
          <div className="advisor-tree-shell"><TreeList childrenNodes={treeChildren} /></div>
        </>
      ) : null}
      {card.cardType === 'plan_preview' ? (
        <>
          <div className="advisor-card-copy">{summarizeCard(card)}</div>
          <PlanEntries entries={entries} />
        </>
      ) : null}
      {['preference_draft', 'reclassification_result'].includes(card.cardType || '') ? (
        <>
          <div className="advisor-card-copy">{String(card.body?.summary || summarizeCard(card))}</div>
          {card.body?.updatedTreeText ? <div className="form-hint">{String(card.body.updatedTreeText)}</div> : null}
        </>
      ) : null}
      {card.cardType === 'execution_result' ? <div className="advisor-card-copy">{summarizeCard(card)}</div> : null}
      {!['tree', 'plan_preview', 'preference_draft', 'reclassification_result', 'execution_result'].includes(card.cardType || '') ? (
        <div className="advisor-card-copy">{summarizeCard(card)}</div>
      ) : null}

      {actions.length ? (
        <div className="advisor-inline-actions">
          {actions.map((action: AdvisorCardAction, index) => (
            <button
              key={`${action.action || 'action'}-${index}`}
              className={`btn ${action.variant === 'primary' ? 'btn-primary' : 'btn-secondary'} advisor-card-action`}
              type="button"
              disabled={acting}
              onClick={() => onAction(card.cardId || '', action.action || '')}
            >
              {action.label || action.action || 'Action'}
            </button>
          ))}
        </div>
      ) : null}
    </article>
  );
}

function formatJsonBlock(value: unknown): string {
  if (value === null || value === undefined) return '-';
  if (typeof value === 'string') return value.trim() || '-';
  try {
    return JSON.stringify(value, null, 2);
  } catch {
    return String(value);
  }
}

function summarizeUsage(usage: unknown): string {
  if (!usage || typeof usage !== 'object') return '';
  const row = usage as JsonRecord;
  return text(
    `Token: ${row.total ?? '-'} / 输入 ${row.prompt ?? '-'} / 输出 ${row.completion ?? '-'}`,
    `Tokens: ${row.total ?? '-'} / prompt ${row.prompt ?? '-'} / completion ${row.completion ?? '-'}`,
  );
}

function ToolCallDetails({ turn }: { turn: TimelineTurn }) {
  const steps = Array.isArray(turn.agentTrace?.steps) ? turn.agentTrace.steps : [];
  const rows = steps.flatMap((step) => {
    const calls = Array.isArray(step.toolCalls) ? step.toolCalls : [];
    const results = Array.isArray(step.toolResults) ? step.toolResults : [];
    return calls.map((call) => {
      const result = results.find((row) => row.id === call.id) || results.find((row) => row.name === call.name) || null;
      return {
        step: step.step,
        route: step.route,
        usage: step.usage,
        assistantText: step.assistantText,
        id: call.id,
        name: call.name,
        arguments: call.arguments,
        status: result?.status || text('无结果', 'no result'),
        payload: result?.payload,
      };
    });
  });
  if (!rows.length) return null;
  const title = text(`工具调用明细 ${rows.length} 次`, `${rows.length} tool call${rows.length > 1 ? 's' : ''}`);
  return (
    <details className="advisor-tool-details">
      <summary>
        <span>{title}</span>
        <span className="advisor-tool-summary-hint">{text('默认折叠', 'Collapsed by default')}</span>
      </summary>
      <div className="advisor-tool-list">
        {rows.map((row, index) => {
          const status = String(row.status || '').toLowerCase();
          const statusClass = status === 'ok' ? 'ok' : status === 'error' ? 'error' : 'neutral';
          const route = row.route && typeof row.route === 'object'
            ? [row.route.model, row.route.endpoint].filter(Boolean).join(' / ')
            : '';
          const usage = summarizeUsage(row.usage);
          return (
            <section className="advisor-tool-item" key={`${row.id || row.name || 'tool'}-${index}`}>
              <div className="advisor-tool-item-head">
                <div className="advisor-tool-title">
                  <span className="advisor-tool-step">#{index + 1}</span>
                  <span>{row.name || text('未知工具', 'Unknown tool')}</span>
                </div>
                <span className={`advisor-tool-status advisor-tool-status-${statusClass}`}>{row.status || '-'}</span>
              </div>
              <div className="advisor-tool-meta">
                <span>{text('步骤', 'Step')}: {row.step ?? '-'}</span>
                <span>{text('调用 ID', 'Call ID')}: {row.id || '-'}</span>
                {route ? <span>{route}</span> : null}
                {usage ? <span>{usage}</span> : null}
              </div>
              {String(row.assistantText || '').trim() ? (
                <div className="advisor-tool-field">
                  <div className="advisor-tool-field-label">{text('模型文本', 'Assistant text')}</div>
                  <pre>{row.assistantText}</pre>
                </div>
              ) : null}
              <div className="advisor-tool-grid">
                <div className="advisor-tool-field">
                  <div className="advisor-tool-field-label">{text('参数', 'Arguments')}</div>
                  <pre>{formatJsonBlock(row.arguments)}</pre>
                </div>
                <div className="advisor-tool-field">
                  <div className="advisor-tool-field-label">{text('结果', 'Result')}</div>
                  <pre>{formatJsonBlock(row.payload)}</pre>
                </div>
              </div>
            </section>
          );
        })}
      </div>
    </details>
  );
}

function AdvisorLoading() {
  return (
    <div className="advisor-loading-reply" role="status" aria-live="polite">
      <span className="advisor-loading-dots" aria-hidden="true"><span /><span /><span /></span>
      <span>{text('正在生成回复', 'Generating reply')}</span>
    </div>
  );
}

function countLabel(count: number): string {
  return text(`${count} 项`, `${count} item${count === 1 ? '' : 's'}`);
}

function FolderRow({ folder, onOpen }: { folder: OrganizeBrowserFolder; onOpen: (path: string[]) => void }) {
  return (
    <button className="advisor-browser-row advisor-browser-folder-row" type="button" onClick={() => onOpen(folder.path)}>
      <span className="advisor-browser-icon advisor-browser-icon-folder" aria-hidden="true" />
      <span className="advisor-browser-row-copy">
        <span className="advisor-browser-row-title">{folder.name || '-'}</span>
        <span className="advisor-browser-row-meta">{countLabel(folder.fileCount)}</span>
      </span>
      <span className="advisor-browser-chevron" aria-hidden="true" />
    </button>
  );
}

function FileRow({ file }: { file: OrganizeBrowserFile }) {
  const error = file.classificationError.trim();
  return (
    <div className={`advisor-browser-row advisor-browser-file-row ${error ? 'advisor-browser-file-error' : ''}`}>
      <span className="advisor-browser-icon advisor-browser-icon-file" aria-hidden="true" />
      <span className="advisor-browser-row-copy">
        <span className="advisor-browser-row-title">{file.name || '-'}</span>
        <span className="advisor-browser-path">{file.path || '-'}</span>
        {error ? <span className="advisor-browser-error-text">{error}</span> : null}
      </span>
      <span className="advisor-browser-type">{file.itemType || 'file'}</span>
    </div>
  );
}

function OrganizeResultBrowser({ rows }: { rows: OrganizeResultRow[] }) {
  const root = useMemo(() => buildOrganizeBrowserTree(rows), [rows]);
  const [activePath, setActivePath] = useState<string[]>([]);
  const current = findOrganizeBrowserFolder(root, activePath) || root;
  const crumbs = [
    { label: text('根目录', 'Root'), path: [] as string[] },
    ...current.path.map((segment, index) => ({
      label: segment,
      path: current.path.slice(0, index + 1),
    })),
  ];
  const parentPath = current.path.slice(0, Math.max(0, current.path.length - 1));
  const empty = current.folders.length === 0 && current.files.length === 0;

  return (
    <section className="advisor-browser-shell" aria-label={text('归类结果浏览', 'Organize result browser')}>
      <div className="advisor-browser-toolbar">
        <button className="btn btn-secondary advisor-browser-back" type="button" disabled={!current.path.length} onClick={() => setActivePath(parentPath)}>
          {text('返回上级', 'Back')}
        </button>
        <nav className="advisor-browser-breadcrumbs" aria-label={text('当前位置', 'Current location')}>
          {crumbs.map((crumb, index) => (
            <button
              className={`advisor-browser-crumb ${index === crumbs.length - 1 ? 'active' : ''}`}
              type="button"
              key={crumb.path.join('/') || 'root'}
              onClick={() => setActivePath(crumb.path)}
            >
              {crumb.label}
            </button>
          ))}
        </nav>
      </div>
      <div className="advisor-browser-list">
        {current.folders.map((folder) => (
          <FolderRow key={folder.id} folder={folder} onOpen={setActivePath} />
        ))}
        {current.files.map((file) => (
          <FileRow key={file.id} file={file} />
        ))}
        {empty ? <div className="advisor-browser-empty">{text('当前层级没有文件。', 'No files in this level.')}</div> : null}
      </div>
    </section>
  );
}

export function AdvisorTimeline({
  state,
  onCardAction,
}: {
  state: AdvisorWorkflowState;
  onCardAction: (cardId: string, action: string) => void;
}) {
  const timeline = Array.isArray(state.sessionData?.timeline) ? state.sessionData.timeline : [];
  const snapshot = getCurrentSnapshot(state);
  if (!timeline.length) {
    const finished = isOrganizeFinished(snapshot);
    return (
      <div className="card advisor-empty-panel">
        <div className="empty-state advisor-empty-compact">
          <div className="empty-state-text">{finished ? text('归类结果已就绪', 'Organize results ready') : text('暂无会话内容', 'No session content')}</div>
        </div>
      </div>
    );
  }
  return (
    <>
      {timeline.map((turn, index) => (
        <section
          key={turn.turnId || index}
          className={`advisor-message-section advisor-message-section-${turn.role || 'assistant'} ${turn.loading ? 'advisor-message-section-loading' : ''} ${turn.failed ? 'advisor-message-section-failed' : ''}`}
        >
          <div className="advisor-message-rail" aria-hidden="true"><span className="advisor-message-node" /></div>
          <div className="advisor-message-stack">
            {(turn.text || '').trim() || turn.loading ? (
              <article className={`advisor-message-bubble ${turn.loading ? 'advisor-message-bubble-loading' : ''} ${turn.failed ? 'advisor-message-bubble-failed' : ''}`}>
                <div className="advisor-message-meta">
                  <span className="advisor-message-role">{turn.role === 'user' ? text('你', 'You') : text('顾问', 'Advisor')}</span>
                  <span className="advisor-message-time">{formatDateTime(turn.createdAt)}</span>
                </div>
                {turn.loading ? <AdvisorLoading /> : <div className="advisor-message-text">{turn.text || ''}</div>}
              </article>
            ) : (
              <div className="advisor-message-meta advisor-message-meta-inline">
                <span className="advisor-message-role">{turn.role === 'user' ? text('你', 'You') : text('顾问', 'Advisor')}</span>
                <span className="advisor-message-time">{formatDateTime(turn.createdAt)}</span>
              </div>
            )}
            <div className="advisor-turn-cards">
              {(turn.cards || []).map((card, cardIndex) => (
                <AdvisorCardView
                  key={card.cardId || `${turn.turnId || index}-${cardIndex}`}
                  card={card}
                  acting={state.acting}
                  onAction={onCardAction}
                />
              ))}
            </div>
            <ToolCallDetails turn={turn} />
          </div>
        </section>
      ))}
    </>
  );
}

export function ContextSummary({
  sessionData,
  state,
  onToggle,
}: {
  sessionData: AdvisorSessionData | null;
  state: AdvisorWorkflowState;
  onToggle: () => void;
}) {
  if (!sessionData) return null;
  const contextBar = sessionData.contextBar || {};
  const collapsed = !!contextBar.collapsed;
  const rootPath = contextBar.rootPath || state.rootPath || '-';
  const modeLabel = contextBar.mode?.label || text('顾问模式：单智能体', 'Advisor Mode: Single Agent');
  const directorySummary = contextBar.directorySummary || {};
  const webSearch = contextBar.webSearch || {};
  const stageLabel = getStageLabel(state);
  return (
    <section className={`card advisor-context-summary ${collapsed ? 'collapsed' : ''}`}>
      <div className="advisor-context-head">
        <div>
          <div className="workflow-kicker workflow-kicker-subtle">{text('会话上下文', 'Session Context')}</div>
          <div className="card-title">{rootPath}</div>
        </div>
        <div className="advisor-context-actions">
          <span className="advisor-stage-chip advisor-stage-chip-muted">{stageLabel}</span>
          <button className="btn btn-ghost" type="button" disabled={state.acting} onClick={onToggle}>
            {collapsed ? text('展开', 'Expand') : text('折叠', 'Collapse')}
          </button>
        </div>
      </div>
      {collapsed ? <div className="form-hint">{modeLabel}</div> : (
        <div className="advisor-context-grid">
          <div className="advisor-context-chip">{modeLabel}</div>
          <div className="advisor-context-chip">{text('分类记录', 'Organize')}: {contextBar.organizeTaskId || '-'}</div>
          <div className="advisor-context-chip">{text('项目数', 'Items')}: {directorySummary.itemCount || 0}</div>
          <div className="advisor-context-chip">{text('可复用树', 'Reusable Tree')}: {directorySummary.treeAvailable ? text('是', 'Yes') : text('否', 'No')}</div>
          <div className="advisor-context-chip">
            {text('联网搜索', 'Web Search')}: {webSearch.webSearchEnabled ? text('可用', 'Available') : (webSearch.useWebSearch ? text('已开启但缺少密钥', 'Enabled but unavailable') : text('关闭', 'Off'))}
          </div>
        </div>
      )}
    </section>
  );
}

export function OrganizeSummary({
  snapshot,
  state,
}: {
  snapshot: OrganizeSnapshot | null;
  state: AdvisorWorkflowState;
}) {
  if (!snapshot) return <div className="advisor-organize-summary" />;
  const totalFiles = Number(snapshot.totalFiles || snapshot.total_files || 0);
  const processedFiles = Number(snapshot.processedFiles || snapshot.processed_files || 0);
  const error = String(snapshot.error || '').trim();
  const treeChildren = Array.isArray(snapshot.tree?.children) ? snapshot.tree.children : [];
  const resultRows = Array.isArray(snapshot.results) ? snapshot.results : [];
  return (
    <section className="advisor-organize-summary">
      <div className="advisor-organize-summary-head">
        <div className="advisor-hero-stat">
          <span className="advisor-hero-stat-label">{text('归类状态', 'Organize Status')}</span>
          <strong>{getOrganizeStatusLabel(state, snapshot)}</strong>
        </div>
        <div className="advisor-organize-summary-meta">
          <span className="badge badge-info">{text('摘要模式', 'Summary')}: {snapshot.summaryStrategy || snapshot.summary_strategy || state.summaryStrategy}</span>
          <span className={`badge ${snapshot.webSearchEnabled ? 'badge-success' : (snapshot.useWebSearch ? 'badge-warning' : 'badge-info')}`}>
            {snapshot.webSearchEnabled ? text('联网可用', 'Web Search Ready') : (snapshot.useWebSearch ? text('联网未就绪', 'Web Search Unavailable') : text('联网关闭', 'Web Search Off'))}
          </span>
        </div>
      </div>
      <div className="advisor-organize-stats">
        <div className="advisor-context-chip">{text('文件数', 'Files')}: {totalFiles}</div>
        <div className="advisor-context-chip">{text('已处理', 'Processed')}: {processedFiles}</div>
        <div className="advisor-context-chip">{text('任务 ID', 'Task ID')}: {snapshot.id || '-'}</div>
      </div>
      <div className="advisor-organize-progress">
        <div className="advisor-organize-progress-track">
          <div className="advisor-organize-progress-fill" style={{ width: `${getOrganizeProgress(snapshot)}%` }} />
        </div>
      </div>
      {error ? <div className="form-hint">{text('错误: ', 'Error: ')}{error}</div> : null}
      {resultRows.length ? (
        <OrganizeResultBrowser rows={resultRows} />
      ) : treeChildren.length ? (
        <div className="advisor-browser-readonly">
          <div className="form-hint">{text('当前只有分类树，文件列表会在结果明细返回后显示。', 'Only the category tree is available now. Files appear after result details load.')}</div>
          <div className="advisor-tree-shell"><TreeList childrenNodes={treeChildren} limit={18} /></div>
        </div>
      ) : null}
    </section>
  );
}

export function OrganizePanel({
  state,
  onRootPathChange,
  onBrowse,
  onStartOrganize,
  onStopOrganize,
  onStartSession,
  onSummaryChange,
  onWebSearchChange,
}: {
  state: AdvisorWorkflowState;
  onRootPathChange: (value: string) => void;
  onBrowse: () => void;
  onStartOrganize: () => void;
  onStopOrganize: () => void;
  onStartSession: () => void;
  onSummaryChange: (value: string) => void;
  onWebSearchChange: (value: boolean) => void;
}) {
  const snapshot = getCurrentSnapshot(state);
  const hasSession = !!state.sessionData;
  const sessionBtnLabel = hasSession
    ? text('重建会话', 'Restart Session')
    : isOrganizeFinished(snapshot)
      ? text('开始对话', 'Start Conversation')
      : text('直接开始会话', 'Start Conversation');

  return (
    <section className="card advisor-organize-panel">
      <div className="advisor-section-head">
        <div>
          <div className="workflow-kicker workflow-kicker-subtle">{text('前置归类', 'Organize First')}</div>
          <h2 className="card-title">{text('归类与顾问对话', 'Organize and Advisor')}</h2>
        </div>
        <div className="advisor-inline-actions advisor-organize-actions">
          <button className="btn btn-primary" type="button" disabled={state.organizeStarting || isOrganizeRunning(snapshot)} onClick={onStartOrganize}>
            {state.organizeStarting ? text('启动中...', 'Starting...') : text('开始归类', 'Start Organizing')}
          </button>
          <button className="btn btn-secondary" type="button" disabled={state.organizeStopping || !isOrganizeRunning(snapshot)} onClick={onStopOrganize}>
            {state.organizeStopping ? text('停止中...', 'Stopping...') : text('停止归类', 'Stop Organizing')}
          </button>
          <button className="btn btn-secondary" type="button" disabled={state.loading || state.organizeStarting} onClick={onStartSession}>
            {sessionBtnLabel}
          </button>
        </div>
      </div>
      <div className="advisor-organize-config-grid">
        <div className="advisor-source-field">
          <label className="form-label" htmlFor="advisor-root-path">{text('工作目录', 'Working Directory')}</label>
          <div className="advisor-path-actions">
            <input
              id="advisor-root-path"
              className="form-input advisor-input-path"
              type="text"
              value={state.rootPath}
              placeholder={text('选择目录', 'Choose a folder')}
              onChange={(event) => onRootPathChange(event.target.value)}
            />
            <button className="btn btn-secondary" type="button" onClick={onBrowse}>{text('浏览', 'Browse')}</button>
          </div>
        </div>
        <div className="advisor-organize-fields">
          <div className="form-group">
            <label className="form-label" htmlFor="advisor-summary-mode">{text('摘要模式', 'Summary Mode')}</label>
            <select
              id="advisor-summary-mode"
              className="form-input"
              value={state.summaryStrategy}
              onChange={(event) => onSummaryChange(event.target.value)}
            >
              {SUMMARY_MODES.map((mode) => <option key={mode} value={mode}>{summaryModeLabel(mode)}</option>)}
            </select>
            <div className="form-hint">{summaryModeHint(state.summaryStrategy)}</div>
          </div>
          <label className="advisor-organize-toggle">
            <input
              type="checkbox"
              checked={state.useWebSearch}
              disabled={state.syncingSearch}
              onChange={(event) => onWebSearchChange(event.target.checked)}
            />
            <span className="advisor-organize-toggle-copy">
              <span className="advisor-organize-toggle-title">{text('联网搜索', 'Web Search')}</span>
            </span>
          </label>
        </div>
      </div>
      <OrganizeSummary snapshot={snapshot} state={state} />
    </section>
  );
}
