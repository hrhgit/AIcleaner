import { useMemo, useState } from 'react';
import type { AdvisorCard, AdvisorSessionData, AdvisorCardAction, JsonRecord, OrganizeResultRow, OrganizeViewSnapshot, TimelineTurn, TreeNode } from '../../types';
import { text } from '../../utils/i18n';
import {
  formatDateTime,
  getCurrentSnapshot,
  getOrganizeProgress,
  getOrganizeProgressDetail,
  getOrganizeProgressStage,
  getOrganizeStatusLabel,
  getStageLabel,
  hasDeterminateOrganizeProgress,
  isOrganizeFinished,
  isOrganizeRunning,
  summaryModeHint,
  summaryModeLabel,
  type AdvisorWorkflowState,
} from './model';
import { SUMMARY_MODES } from './constants';
import {
  buildOrganizeBrowserTree,
  buildOrganizeBrowserTreeFromTreeNodes,
  type OrganizeBrowserFile,
  type OrganizeBrowserFolder,
} from './organizeBrowser';

function summarizeCard(card: AdvisorCard): string {
  if (card.cardType === 'execution_result') {
    const result = (card.body?.result || {}) as JsonRecord;
    const summary = (result.summary || {}) as JsonRecord;
    return text(`总计 ${summary.total ?? '-'}，失败 ${summary.failed || 0}。`, `Total ${summary.total ?? '-'}, failed ${summary.failed || 0}.`);
  }
  return String(card.body?.summary || card.body?.message || '').trim();
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
  if (card.cardType === 'tree') {
    return null;
  }
  const actions = Array.isArray(card.actions) ? card.actions : [];

  return (
    <article className={`advisor-card advisor-card-${card.cardType || 'generic'}`}>
      <div className="advisor-card-head">
        <div className="advisor-card-title-group">
          <div className="card-title">{card.title || '-'}</div>
          <div className="form-hint">{formatDateTime(card.createdAt)}</div>
        </div>
        <span className="badge badge-info">{card.status || 'ready'}</span>
      </div>

      {['preference_draft', 'reclassification_result'].includes(card.cardType || '') ? (
        <>
          <div className="advisor-card-copy">{String(card.body?.summary || summarizeCard(card))}</div>
          {card.body?.updatedTreeText ? <div className="form-hint">{String(card.body.updatedTreeText)}</div> : null}
        </>
      ) : null}
      {card.cardType === 'execution_result' ? <div className="advisor-card-copy">{summarizeCard(card)}</div> : null}
      {!['tree', 'preference_draft', 'reclassification_result', 'execution_result'].includes(card.cardType || '') ? (
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

function formatDurationMs(value: unknown): string {
  const ms = Number(value);
  if (!Number.isFinite(ms) || ms < 0) return '-';
  if (ms < 1000) return `${Math.round(ms)} ms`;
  const seconds = ms / 1000;
  if (seconds < 60) return `${seconds.toFixed(seconds < 10 ? 1 : 0)} s`;
  const minutes = Math.floor(seconds / 60);
  const rest = Math.round(seconds % 60);
  return `${minutes}m ${rest}s`;
}

function metricRecord(value: unknown): JsonRecord | null {
  return value && typeof value === 'object' ? value as JsonRecord : null;
}

function formatTokenUsage(value: unknown): string {
  const row = metricRecord(value);
  if (!row) return '-';
  return text(
    `${row.total ?? '-'} / 输入 ${row.prompt ?? '-'} / 输出 ${row.completion ?? '-'}`,
    `${row.total ?? '-'} / prompt ${row.prompt ?? '-'} / completion ${row.completion ?? '-'}`,
  );
}

const ORGANIZE_METRIC_STAGES = [
  { key: 'summaryPreparation', label: () => text('摘要准备', 'Summary') },
  { key: 'initialTree', label: () => text('初始树', 'Initial Tree') },
  { key: 'classification', label: () => text('分类', 'Classification') },
  { key: 'buildTreeShape', label: () => text('合并树', 'Merge Tree') },
  { key: 'adjust', label: () => text('调整', 'Adjust') },
];

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
        durationMs: result?.durationMs,
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
                {row.durationMs !== undefined ? <span>{text('耗时', 'Duration')}: {formatDurationMs(row.durationMs)}</span> : null}
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

function FolderRow({ folder, depth = 0 }: { folder: OrganizeBrowserFolder; depth?: number }) {
  const [expanded, setExpanded] = useState(false);
  const hasChildren = folder.folders.length > 0 || folder.files.length > 0;
  const hasSamples = folder.sampleItems.length > 0;
  return (
    <div className="advisor-browser-folder-group">
      <div className="advisor-browser-row advisor-browser-folder-row">
        <button
          className={`advisor-browser-folder-nav ${expanded ? 'expanded' : ''}`}
          type="button"
          disabled={!hasChildren}
          aria-expanded={expanded}
          onClick={() => setExpanded((value) => !value)}
        >
          <span className="advisor-browser-icon advisor-browser-icon-folder" aria-hidden="true" />
          <span className="advisor-browser-row-copy">
            <span className="advisor-browser-row-title">{folder.name || '-'}</span>
            <span className="advisor-browser-row-meta">{countLabel(folder.fileCount)}</span>
          </span>
          <span className="advisor-browser-chevron" aria-hidden="true" />
        </button>
        {hasSamples ? (
          <button
            className="advisor-browser-info-btn"
            type="button"
            aria-expanded={expanded}
            aria-label={text('查看分类详情', 'View classification details')}
            onClick={(e) => { e.stopPropagation(); setExpanded((value) => !value); }}
          >
            {text('详情', 'Info')}
          </button>
        ) : null}
      </div>
      {expanded && hasChildren ? (
        <div className="advisor-browser-folder-children" style={{ paddingLeft: `${depth > 0 ? 14 : 18}px` }}>
          {folder.folders.map((child) => (
            <FolderRow key={child.id} folder={child} depth={depth + 1} />
          ))}
          {folder.files.map((file) => (
            <FileRow key={file.id} file={file} />
          ))}
        </div>
      ) : null}
      {expanded && hasSamples ? (
        <div className="advisor-browser-folder-detail">
          <div className="advisor-browser-detail-header">
            {text(`样本文件 (共 ${folder.fileCount} 项)`, `Sample files (${folder.fileCount} total)`)}
          </div>
          <ul className="advisor-browser-sample-list">
            {folder.sampleItems.map((item, index) => (
              <li key={index} className="advisor-browser-sample-item">
                <span className="advisor-browser-sample-name">{item.name}</span>
                {item.reason ? <span className="advisor-browser-sample-reason">{item.reason}</span> : null}
              </li>
            ))}
          </ul>
        </div>
      ) : null}
    </div>
  );
}

function turnUsedTool(turn: TimelineTurn, toolName: string): boolean {
  const steps = Array.isArray(turn.agentTrace?.steps) ? turn.agentTrace.steps : [];
  return steps.some((step) => {
    const calls = Array.isArray(step.toolCalls) ? step.toolCalls : [];
    const results = Array.isArray(step.toolResults) ? step.toolResults : [];
    return [...calls, ...results].some((tool) => String(tool?.name || '').trim() === toolName);
  });
}

function DerivedTreePanel({ tree }: { tree: TreeNode | undefined }) {
  const childrenNodes = Array.isArray(tree?.children) ? tree.children : [];
  const browserTree = useMemo(() => buildOrganizeBrowserTreeFromTreeNodes(childrenNodes), [childrenNodes]);
  return (
    <div className="advisor-turn-tree-panel">
      <div className="advisor-turn-tree-head">
        <div className="card-title">{text('当前分类树', 'Current Tree')}</div>
        <div className="form-hint">{text('本轮已更新分类树，下面按归类结果的文件夹形式展示。', 'This turn updated the category tree, shown below in the organize-result folder style.')}</div>
      </div>
      {childrenNodes.length ? (
        <FolderBrowserShell root={browserTree} className="advisor-tree-folder-shell" emptyText={text('当前没有可展示的分类树。', 'No category tree is available right now.')} />
      ) : <div className="form-hint">{text('当前没有可展示的分类树。', 'No category tree is available right now.')}</div>}
    </div>
  );
}

function FileRow({ file }: { file: OrganizeBrowserFile }) {
  const error = file.classificationError.trim();
  const reason = file.reason.trim();
  const [showReason, setShowReason] = useState(false);
  return (
    <div className={`advisor-browser-row advisor-browser-file-row ${error ? 'advisor-browser-file-error' : ''}`}>
      <span className="advisor-browser-icon advisor-browser-icon-file" aria-hidden="true" />
      <span className="advisor-browser-row-copy">
        <span className="advisor-browser-row-title">{file.name || '-'}</span>
        <span className="advisor-browser-path">{file.path || '-'}</span>
        {error ? <span className="advisor-browser-error-text">{error}</span> : null}
        {showReason && reason ? <span className="advisor-browser-reason">{reason}</span> : null}
      </span>
      <span className="advisor-browser-file-actions">
        {reason ? (
          <button
            className="advisor-browser-reason-btn"
            type="button"
            aria-expanded={showReason}
            onClick={() => setShowReason(!showReason)}
          >
            {text('原因', 'Reason')}
          </button>
        ) : null}
        <span className="advisor-browser-type">{file.itemType || 'file'}</span>
      </span>
    </div>
  );
}

function FolderBrowserShell({
  root,
  className = '',
  emptyText,
}: {
  root: OrganizeBrowserFolder;
  className?: string;
  emptyText: string;
}) {
  const empty = root.folders.length === 0 && root.files.length === 0;

  return (
    <section className={`advisor-browser-shell ${className}`.trim()} aria-label={text('归类结果浏览', 'Organize result browser')}>
      <div className="advisor-browser-list">
        {root.folders.map((folder) => (
          <FolderRow key={folder.id} folder={folder} />
        ))}
        {root.files.map((file) => (
          <FileRow key={file.id} file={file} />
        ))}
        {empty ? <div className="advisor-browser-empty">{emptyText}</div> : null}
      </div>
    </section>
  );
}

function OrganizeResultBrowser({ rows }: { rows: OrganizeResultRow[] }) {
  const root = useMemo(() => buildOrganizeBrowserTree(rows), [rows]);

  return <FolderBrowserShell root={root} emptyText={text('当前层级没有文件。', 'No files in this level.')} />;
}

const ORGANIZE_STAGE_FLOW = [
  { stage: 'collecting', label: () => text('收集', 'Collect') },
  { stage: 'summary', label: () => text('摘要', 'Summary') },
  { stage: 'initial_tree', label: () => text('建树', 'Tree') },
  { stage: 'classification', label: () => text('分类', 'Classify') },
  { stage: 'build_tree_shape', label: () => text('合并树', 'Merge Tree') },
  { stage: 'fill_items', label: () => text('填入', 'Fill') },
  { stage: 'adjust', label: () => text('调整', 'Adjust') },
  { stage: 'finalize', label: () => text('结果', 'Finalize') },
  { stage: 'completed', label: () => text('完成', 'Done') },
];

function OrganizeStageStrip({ stage }: { stage: string }) {
  const activeIndex = ORGANIZE_STAGE_FLOW.findIndex((item) => item.stage === stage);
  const terminal = ['completed', 'error', 'stopped'].includes(stage);
  return (
    <div className={`advisor-organize-stage-strip advisor-organize-stage-${stage || 'idle'}`}>
      {ORGANIZE_STAGE_FLOW.map((item, index) => {
        const stateClass = terminal && stage !== 'completed'
          ? 'pending'
          : activeIndex < 0
            ? 'pending'
            : index < activeIndex
              ? 'done'
              : index === activeIndex
                ? 'active'
                : 'pending';
        return (
          <div className={`advisor-organize-stage-step ${stateClass}`} key={item.stage}>
            <span className="advisor-organize-stage-dot" aria-hidden="true" />
            <span>{item.label()}</span>
          </div>
        );
      })}
    </div>
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
  const derivedTree = state.sessionData?.derivedTree as TreeNode | undefined;
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
              {(turn.cards || []).filter((card) => card.cardType !== 'tree').map((card, cardIndex) => (
                <AdvisorCardView
                  key={card.cardId || `${turn.turnId || index}-${cardIndex}`}
                  card={card}
                  acting={state.acting}
                  onAction={onCardAction}
                />
              ))}
            </div>
            {index === timeline.length - 1 && turn.role === 'assistant' && turnUsedTool(turn, 'apply_reclassification') ? (
              <DerivedTreePanel tree={derivedTree} />
            ) : null}
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
  snapshot: OrganizeViewSnapshot | null;
  state: AdvisorWorkflowState;
}) {
  if (!snapshot) return <div className="advisor-organize-summary" />;
  const totalFiles = Number(snapshot.totalFiles || 0);
  const error = String(snapshot.error || '').trim();
  const stage = getOrganizeProgressStage(snapshot);
  const stageDetail = getOrganizeProgressDetail(snapshot);
  const progressValue = getOrganizeProgress(snapshot);
  const determinate = hasDeterminateOrganizeProgress(snapshot);
  const treeChildren = Array.isArray(snapshot.tree?.children) ? snapshot.tree.children : [];
  const treeBrowserRoot = buildOrganizeBrowserTreeFromTreeNodes(treeChildren);
  const resultRows = Array.isArray(snapshot.displayResults) ? snapshot.displayResults : [];
  const timingMs = metricRecord(snapshot.timingMs);
  const tokenUsage = snapshot.tokenUsage;
  const tokenUsageByStage = metricRecord(snapshot.tokenUsageByStage);
  return (
    <section className="advisor-organize-summary">
      <div className="advisor-organize-summary-head">
        <div className="advisor-hero-stat">
          <span className="advisor-hero-stat-label">{text('归类状态', 'Organize Status')}</span>
          <strong>{getOrganizeStatusLabel(state, snapshot)}</strong>
        </div>
        <div className="advisor-organize-summary-meta">
          <span className="badge badge-info">{text('摘要模式', 'Summary')}: {snapshot.summaryStrategy || state.summaryStrategy}</span>
          <span className={`badge ${snapshot.webSearchEnabled ? 'badge-success' : (snapshot.useWebSearch ? 'badge-warning' : 'badge-info')}`}>
            {snapshot.webSearchEnabled ? text('联网可用', 'Web Search Ready') : (snapshot.useWebSearch ? text('联网未就绪', 'Web Search Unavailable') : text('联网关闭', 'Web Search Off'))}
          </span>
        </div>
      </div>
      <div className="advisor-organize-state-card" role="status" aria-live="polite">
        <div className="advisor-organize-state-copy">
          <div className="advisor-organize-state-label">{getOrganizeStatusLabel(state, snapshot)}</div>
          {stageDetail ? <div className="advisor-organize-state-detail">{stageDetail}</div> : null}
        </div>
        <div className="advisor-organize-state-meter">
          <div className={`advisor-organize-progress-track ${determinate ? '' : 'indeterminate'}`}>
            {determinate ? (
              <div className="advisor-organize-progress-fill" style={{ width: `${progressValue}%` }} />
            ) : (
              <div className="advisor-organize-progress-fill advisor-organize-progress-pulse" />
            )}
          </div>
          {determinate ? <span className="advisor-organize-progress-percent">{progressValue}%</span> : null}
        </div>
        <OrganizeStageStrip stage={stage} />
      </div>
      <div className="advisor-organize-stats">
        <div className="advisor-context-chip">{text('文件数', 'Files')}: {totalFiles}</div>
        <div className="advisor-context-chip">{text('当前阶段', 'Stage')}: {getOrganizeStatusLabel(state, snapshot)}</div>
        <div className="advisor-context-chip">{text('总耗时', 'Total Duration')}: {formatDurationMs(snapshot.durationMs ?? timingMs?.total)}</div>
        <div className="advisor-context-chip">{text('总 Token', 'Total Tokens')}: {formatTokenUsage(tokenUsage)}</div>
        <div className="advisor-context-chip">{text('任务 ID', 'Task ID')}: {snapshot.id || '-'}</div>
      </div>
      {(timingMs || tokenUsageByStage) ? (
        <div className="advisor-organize-metrics">
          {ORGANIZE_METRIC_STAGES.map((item) => {
            const stageUsage = tokenUsageByStage?.[item.key];
            const stageDuration = timingMs?.[item.key];
            if (stageUsage === undefined && stageDuration === undefined) return null;
            return (
              <div className="advisor-context-chip advisor-organize-metric-chip" key={item.key}>
                <span>{item.label()}</span>
                <span>{text('耗时', 'Duration')}: {formatDurationMs(stageDuration)}</span>
                <span>{text('Token', 'Tokens')}: {formatTokenUsage(stageUsage)}</span>
              </div>
            );
          })}
        </div>
      ) : null}
      {error ? <div className="form-hint">{text('错误: ', 'Error: ')}{error}</div> : null}
      {resultRows.length ? (
        <OrganizeResultBrowser rows={resultRows} />
      ) : treeChildren.length ? (
        <div className="advisor-browser-readonly">
          <div className="form-hint">{text('当前只有分类树，文件列表会在结果明细返回后显示。', 'Only the category tree is available now. Files appear after result details load.')}</div>
          <FolderBrowserShell root={treeBrowserRoot} className="advisor-tree-folder-shell" emptyText={text('当前没有可展示的分类树。', 'No category tree is available right now.')} />
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
