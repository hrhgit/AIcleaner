import type { AdvisorSessionData, OrganizeSnapshot, StreamHandle, SummaryMode, TimelineTurn } from '../../types';
import { getLang, text } from '../../utils/i18n';
import { readPersisted, removePersisted, writePersisted } from '../../utils/storage';
import {
  ADVISOR_PERSIST_KEYS,
  DEFAULT_SUMMARY_MODE,
  SUMMARY_MODES,
  WORKFLOW_PERSIST_KEYS,
} from './constants';

export type AdvisorWorkflowState = {
  rootPath: string;
  summaryStrategy: SummaryMode;
  useWebSearch: boolean;
  sessionId: string;
  messageDraft: string;
  sessionData: AdvisorSessionData | null;
  organizeTaskId: string;
  organizeSnapshot: OrganizeSnapshot | null;
  organizeStream: StreamHandle | null;
  loading: boolean;
  sending: boolean;
  acting: boolean;
  organizeStarting: boolean;
  organizeStopping: boolean;
  syncingSearch: boolean;
};

export type AdvisorAction =
  | { type: 'patch'; patch: Partial<AdvisorWorkflowState> }
  | { type: 'setRootPath'; rootPath: string }
  | { type: 'setSummaryStrategy'; summaryStrategy: string }
  | { type: 'setUseWebSearch'; useWebSearch: boolean }
  | { type: 'setSession'; sessionData: AdvisorSessionData | null; sessionId?: string }
  | { type: 'setMessageDraft'; messageDraft: string }
  | { type: 'appendPendingMessage'; message: string }
  | { type: 'markPendingAssistantFailed'; message: string }
  | { type: 'setOrganizeSnapshot'; snapshot: OrganizeSnapshot | null }
  | { type: 'setOrganizeTaskId'; taskId: string }
  | { type: 'setOrganizeStream'; stream: StreamHandle | null }
  | { type: 'clearSession' };

export function sanitizeSnapshot(snapshot: unknown): OrganizeSnapshot | null {
  return snapshot && typeof snapshot === 'object' ? snapshot as OrganizeSnapshot : null;
}

function resolveInitialRootPath(): string {
  const persisted = String(readPersisted(ADVISOR_PERSIST_KEYS.rootPath, '') || '').trim();
  if (persisted) return persisted;
  return String(readPersisted(WORKFLOW_PERSIST_KEYS.rootPath, '') || '').trim();
}

function resolveInitialSummaryStrategy(): SummaryMode {
  const persisted = String(readPersisted(WORKFLOW_PERSIST_KEYS.summaryStrategy, DEFAULT_SUMMARY_MODE) || '').trim();
  return SUMMARY_MODES.includes(persisted as SummaryMode) ? persisted as SummaryMode : DEFAULT_SUMMARY_MODE;
}

export function createInitialAdvisorState(): AdvisorWorkflowState {
  return {
    rootPath: resolveInitialRootPath(),
    summaryStrategy: resolveInitialSummaryStrategy(),
    useWebSearch: !!readPersisted(WORKFLOW_PERSIST_KEYS.useWebSearch, false),
    sessionId: readPersisted(ADVISOR_PERSIST_KEYS.sessionId, ''),
    messageDraft: readPersisted(ADVISOR_PERSIST_KEYS.messageDraft, ''),
    sessionData: null,
    organizeTaskId: String(readPersisted(WORKFLOW_PERSIST_KEYS.lastTaskId, '') || ''),
    organizeSnapshot: null,
    organizeStream: null,
    loading: false,
    sending: false,
    acting: false,
    organizeStarting: false,
    organizeStopping: false,
    syncingSearch: false,
  };
}

function createLocalTimelineTurn(role: 'user' | 'assistant', textValue: string, extra: Partial<TimelineTurn> = {}): TimelineTurn {
  const createdAt = new Date().toISOString();
  return {
    turnId: `local-${role}-${createdAt}-${Math.random().toString(36).slice(2, 8)}`,
    role,
    text: textValue,
    createdAt,
    cards: [],
    localPending: true,
    ...extra,
  };
}

export function advisorReducer(state: AdvisorWorkflowState, action: AdvisorAction): AdvisorWorkflowState {
  switch (action.type) {
    case 'patch':
      return { ...state, ...action.patch };
    case 'setRootPath': {
      const rootPath = String(action.rootPath || '').trim();
      writePersisted(ADVISOR_PERSIST_KEYS.rootPath, rootPath);
      writePersisted(WORKFLOW_PERSIST_KEYS.rootPath, rootPath);
      return { ...state, rootPath };
    }
    case 'setSummaryStrategy': {
      const summaryStrategy = SUMMARY_MODES.includes(action.summaryStrategy as SummaryMode)
        ? action.summaryStrategy as SummaryMode
        : DEFAULT_SUMMARY_MODE;
      writePersisted(WORKFLOW_PERSIST_KEYS.summaryStrategy, summaryStrategy);
      return { ...state, summaryStrategy };
    }
    case 'setUseWebSearch':
      writePersisted(WORKFLOW_PERSIST_KEYS.useWebSearch, action.useWebSearch);
      return applyLocalWebSearchState({ ...state, useWebSearch: action.useWebSearch });
    case 'setSession': {
      const sessionId = String(action.sessionId || action.sessionData?.sessionId || '');
      if (sessionId) writePersisted(ADVISOR_PERSIST_KEYS.sessionId, sessionId);
      return { ...state, sessionData: action.sessionData, sessionId };
    }
    case 'clearSession':
      removePersisted(ADVISOR_PERSIST_KEYS.sessionId);
      return { ...state, sessionData: null, sessionId: '' };
    case 'setMessageDraft':
      writePersisted(ADVISOR_PERSIST_KEYS.messageDraft, action.messageDraft);
      return { ...state, messageDraft: action.messageDraft };
    case 'appendPendingMessage': {
      const currentData = state.sessionData || { sessionId: state.sessionId, timeline: [] };
      const timeline = Array.isArray(currentData.timeline) ? currentData.timeline : [];
      return {
        ...state,
        sessionData: {
          ...currentData,
          sessionId: currentData.sessionId || state.sessionId,
          timeline: [
            ...timeline,
            createLocalTimelineTurn('user', action.message),
            createLocalTimelineTurn('assistant', '', { loading: true }),
          ],
        },
      };
    }
    case 'markPendingAssistantFailed':
      if (!state.sessionData) return state;
      return {
        ...state,
        sessionData: {
          ...state.sessionData,
          timeline: (state.sessionData.timeline || []).map((turn) => (turn.loading
            ? { ...turn, loading: false, failed: true, text: action.message }
            : turn)),
        },
      };
    case 'setOrganizeSnapshot': {
      const organizeSnapshot = sanitizeSnapshot(action.snapshot);
      if (organizeSnapshot) writePersisted(WORKFLOW_PERSIST_KEYS.lastSnapshot, organizeSnapshot);
      else removePersisted(WORKFLOW_PERSIST_KEYS.lastSnapshot);
      return { ...state, organizeSnapshot };
    }
    case 'setOrganizeTaskId': {
      const organizeTaskId = String(action.taskId || '').trim();
      if (organizeTaskId) writePersisted(WORKFLOW_PERSIST_KEYS.lastTaskId, organizeTaskId);
      else removePersisted(WORKFLOW_PERSIST_KEYS.lastTaskId);
      return { ...state, organizeTaskId };
    }
    case 'setOrganizeStream':
      return { ...state, organizeStream: action.stream };
    default:
      return state;
  }
}

function normalizePath(value: string): string {
  return String(value || '').trim().replace(/[\\/]+/g, '/').toLowerCase();
}

export function getCurrentSnapshot(state: AdvisorWorkflowState): OrganizeSnapshot | null {
  const snapshot = sanitizeSnapshot(state.organizeSnapshot);
  if (!snapshot) return null;
  const rootPath = String(state.rootPath || '').trim();
  const snapshotRoot = String(snapshot.rootPath || snapshot.root_path || '').trim();
  if (!rootPath || !snapshotRoot) return snapshot;
  return normalizePath(rootPath) === normalizePath(snapshotRoot) ? snapshot : null;
}

export function getWorkflowStage(state: AdvisorWorkflowState): string {
  return state.sessionData?.session?.workflowStage || state.sessionData?.workflowStage || 'understand';
}

export function getStageLabel(state: AdvisorWorkflowState): string {
  const stage = getWorkflowStage(state);
  if (stage === 'execute_ready') return text('可执行', 'Ready to Execute');
  if (stage === 'preview_ready') return text('可预览', 'Ready to Preview');
  return text('理解中', 'Understanding');
}

export function getOrganizeStatus(snapshot: OrganizeSnapshot | null): string {
  return String(snapshot?.status || '').trim().toLowerCase();
}

export function getOrganizeProgressStage(snapshot: OrganizeSnapshot | null): string {
  const stage = String(snapshot?.progress?.stage || '').trim().toLowerCase();
  const status = getOrganizeStatus(snapshot);
  if (stage && !(stage === 'idle' && status && status !== 'idle')) return stage;
  if (status === 'classifying') {
    const totalBatches = Number(snapshot?.totalBatches || snapshot?.total_batches || 0);
    const processedBatches = Number(snapshot?.processedBatches || snapshot?.processed_batches || 0);
    return totalBatches > 0 && processedBatches < totalBatches ? 'summary' : 'classification';
  }
  if (status === 'done') return 'completed';
  return status || 'idle';
}

export function isOrganizeRunning(snapshot: OrganizeSnapshot | null): boolean {
  return ['idle', 'collecting', 'classifying', 'stopping', 'moving'].includes(getOrganizeStatus(snapshot));
}

export function isOrganizeFinished(snapshot: OrganizeSnapshot | null): boolean {
  return ['completed', 'done'].includes(getOrganizeStatus(snapshot));
}

export function getOrganizeStatusLabel(state: AdvisorWorkflowState, snapshot: OrganizeSnapshot | null): string {
  const stage = getOrganizeProgressStage(snapshot);
  if (stage === 'collecting') return text('收集目录', 'Collecting Files');
  if (stage === 'summary') return text('准备摘要', 'Preparing Summaries');
  if (stage === 'initial_tree') return text('生成分类树', 'Building Category Tree');
  if (stage === 'classification') return text('分类批次', 'Classifying Batches');
  if (stage === 'reconcile') return text('合并校验', 'Reconciling Tree');
  if (stage === 'finalize') return text('生成结果', 'Finalizing Results');
  if (stage === 'moving') return text('执行移动', 'Applying Moves');
  if (stage === 'completed') return text('归类完成', 'Completed');
  if (stage === 'stopped') {
    return getOrganizeStatus(snapshot) === 'stopping' ? text('停止中', 'Stopping') : text('已停止', 'Stopped');
  }
  if (stage === 'error') return text('出错', 'Error');
  if (state.organizeStarting) return text('启动中', 'Starting');
  return String(snapshot?.progress?.label || '').trim() || text('待开始', 'Idle');
}

export function hasDeterminateOrganizeProgress(snapshot: OrganizeSnapshot | null): boolean {
  const progress = snapshot?.progress;
  const current = Number(progress?.current);
  const total = Number(progress?.total);
  return !progress?.indeterminate && Number.isFinite(current) && Number.isFinite(total) && total > 0;
}

export function getOrganizeProgress(snapshot: OrganizeSnapshot | null): number {
  if (hasDeterminateOrganizeProgress(snapshot)) {
    const current = Number(snapshot?.progress?.current || 0);
    const total = Number(snapshot?.progress?.total || 0);
    return Math.max(0, Math.min(100, Math.round((current / total) * 100)));
  }
  const total = Number(snapshot?.totalFiles || snapshot?.total_files || 0);
  const processed = Number(snapshot?.processedFiles || snapshot?.processed_files || 0);
  if (total <= 0) return isOrganizeFinished(snapshot) ? 100 : 0;
  return Math.max(0, Math.min(100, Math.round((processed / total) * 100)));
}

export function getOrganizeProgressUnitLabel(unit: string | undefined): string {
  if (unit === 'batches') return text('批次', 'Batches');
  if (unit === 'steps') return text('步骤', 'Steps');
  return text('文件', 'Files');
}

export function getOrganizeProgressDetail(snapshot: OrganizeSnapshot | null): string {
  if (!snapshot) return '';
  if (hasDeterminateOrganizeProgress(snapshot)) {
    const current = Number(snapshot.progress?.current || 0);
    const total = Number(snapshot.progress?.total || 0);
    return `${getOrganizeProgressUnitLabel(String(snapshot.progress?.unit || 'files'))} ${current}/${total}`;
  }
  return String(snapshot.progress?.detail || '').trim();
}

export function formatDateTime(value: string | undefined): string {
  if (!value) return '-';
  const date = new Date(value);
  if (Number.isNaN(date.getTime())) return String(value);
  return date.toLocaleString(getLang() === 'en' ? 'en-US' : 'zh-CN');
}

export function summaryModeLabel(mode: string): string {
  if (mode === 'agent_summary') return text('AI 摘要', 'Agent Summary');
  if (mode === 'local_summary') return text('本地摘要', 'Local Summary');
  return text('仅文件名', 'Filename Only');
}

export function summaryModeHint(mode: string): string {
  if (mode === 'agent_summary') {
    return text('先提取文本层，再调用摘要模型生成标准化摘要。', 'Extract text locally first, then call the summary model for normalized summaries.');
  }
  if (mode === 'local_summary') {
    return text('只做本地提取和模板摘要，不额外调用摘要模型。', 'Only use local extraction and template summaries, without extra summary model calls.');
  }
  return text('最低成本，只用文件名、路径和基础元信息归类。', 'Lowest-cost mode. Classify from filenames, paths, and metadata only.');
}

export function applyLocalWebSearchState(state: AdvisorWorkflowState): AdvisorWorkflowState {
  if (!state.sessionData) return state;
  const sessionData = {
    ...state.sessionData,
    useWebSearch: state.useWebSearch,
    webSearchEnabled: state.useWebSearch,
    session: state.sessionData.session
      ? {
          ...state.sessionData.session,
          useWebSearch: state.useWebSearch,
          webSearchEnabled: state.useWebSearch,
        }
      : state.sessionData.session,
    contextBar: {
      ...(state.sessionData.contextBar || {}),
      webSearch: {
        useWebSearch: state.useWebSearch,
        webSearchEnabled: state.useWebSearch,
        message: state.useWebSearch
          ? text('下一轮对话会按当前设置开放联网搜索。', 'Web search will be available on the next turn with the current setting.')
          : text('下一轮对话会关闭联网搜索。', 'Web search will be disabled on the next turn.'),
      },
    },
  };
  return { ...state, sessionData };
}
