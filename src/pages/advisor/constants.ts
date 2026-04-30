import type { SummaryMode } from '../../types';

export const ADVISOR_PERSIST_KEYS = {
  rootPath: 'wipeout.advisor.global.root_path.v2',
  sessionId: 'wipeout.advisor.global.session_id.v2',
  messageDraft: 'wipeout.advisor.global.message_draft.v2',
} as const;

export const WORKFLOW_PERSIST_KEYS = {
  rootPath: 'wipeout.advisor.workflow.root_path.v1',
  summaryStrategy: 'wipeout.advisor.workflow.summary_strategy.v1',
  useWebSearch: 'wipeout.advisor.workflow.use_web_search.v1',
  lastTaskId: 'wipeout.advisor.workflow.last_task_id.v1',
  lastSnapshot: 'wipeout.advisor.workflow.last_snapshot.v1',
} as const;

export const DEFAULT_EXCLUSIONS = [
  '.git',
  'node_modules',
  'dist',
  'build',
  'out',
  'Windows',
  'Program Files',
  'Program Files (x86)',
];

export const DEFAULT_BATCH_SIZE = 60;
export const DEFAULT_SUMMARY_MODE: SummaryMode = 'filename_only';
export const SUMMARY_MODES: SummaryMode[] = ['filename_only', 'local_summary', 'agent_summary'];

export const PERSISTED_FIELDS = [
  { key: ADVISOR_PERSIST_KEYS.rootPath, kind: 'normal', field: 'rootPath' },
  { key: ADVISOR_PERSIST_KEYS.sessionId, kind: 'normal', field: 'sessionId' },
  { key: ADVISOR_PERSIST_KEYS.messageDraft, kind: 'normal', field: 'messageDraft' },
  { key: WORKFLOW_PERSIST_KEYS.summaryStrategy, kind: 'normal', field: 'summaryStrategy' },
  { key: WORKFLOW_PERSIST_KEYS.useWebSearch, kind: 'normal', field: 'useWebSearch' },
  { key: WORKFLOW_PERSIST_KEYS.lastTaskId, kind: 'normal', field: 'lastTaskId' },
] as const;

export const TRANSIENT_FIELDS = [
  'provider API keys',
  'Tavily API key',
  'loading flags',
  'active stream handles',
] as const;
