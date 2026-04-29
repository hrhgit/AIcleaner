export const PERSIST_KEYS = {
    rootPath: 'wipeout.advisor.workflow.root_path.v1',
    summaryStrategy: 'wipeout.advisor.workflow.summary_strategy.v1',
    useWebSearch: 'wipeout.advisor.workflow.use_web_search.v1',
    lastTaskId: 'wipeout.advisor.workflow.last_task_id.v1',
    lastSnapshot: 'wipeout.advisor.workflow.last_snapshot.v1',
};

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

export const DEFAULT_BATCH_SIZE = 20;
export const DEFAULT_SUMMARY_MODE = 'filename_only';
export const SUMMARY_MODES = ['filename_only', 'local_summary', 'agent_summary'];
