export const PERSIST_KEYS = {
    rootPath: 'wipeout.organizer.global.root_path.v2',
    exclusions: 'wipeout.organizer.global.exclusions.v2',
    batchSize: 'wipeout.organizer.global.batch_size.v2',
    summaryStrategy: 'wipeout.organizer.global.summary_strategy.v1',
    maxClusterDepth: 'wipeout.organizer.global.max_cluster_depth.v2',
    useWebSearch: 'wipeout.organizer.global.use_web_search.v2',
    modelRouting: 'wipeout.organizer.global.model_routing.v2',
    lastJobId: 'wipeout.organizer.global.last_job_id.v2',
    lastTaskId: 'wipeout.organizer.global.last_task_id.v2',
    lastSnapshot: 'wipeout.organizer.global.last_snapshot.v2',
    lastApplyManifest: 'wipeout.organizer.global.last_apply_manifest.v2',
    logEntries: 'wipeout.organizer.global.log_entries.v1',
    logCollapsed: 'wipeout.organizer.global.log_collapsed.v1',
    logRecordGroupCollapsed: 'wipeout.organizer.global.log_record_group_collapsed.v1',
    logTaskId: 'wipeout.organizer.global.log_task_id.v1',
    runtimeCacheVersion: 'wipeout.organizer.global.runtime_cache_version.v1',
};

const ORGANIZER_RUNTIME_CACHE_VERSION = 2;
const RUNTIME_CACHE_KEYS = [
    PERSIST_KEYS.lastJobId,
    PERSIST_KEYS.lastTaskId,
    PERSIST_KEYS.lastSnapshot,
    PERSIST_KEYS.lastApplyManifest,
    PERSIST_KEYS.logEntries,
    PERSIST_KEYS.logTaskId,
];

const LEGACY_PERSIST_KEYS = [
    'wipeout.organizer.global.root_path.v1',
    'wipeout.organizer.global.exclusions.v1',
    'wipeout.organizer.global.batch_size.v1',
    'wipeout.organizer.global.max_cluster_depth.v1',
    'wipeout.organizer.global.use_web_search.v1',
    'wipeout.organizer.global.model_routing.v1',
    'wipeout.organizer.global.last_job_id.v1',
    'wipeout.organizer.global.last_task_id.v1',
    'wipeout.organizer.global.last_snapshot.v1',
    'wipeout.organizer.global.last_apply_manifest.v1',
    'wipeout.organizer.global.recursive.v1',
    'wipeout.organizer.global.model_selection.v1',
    'wipeout.organizer.global.mode.v1',
    'wipeout.organizer.global.allow_new_categories.v1',
    'wipeout.organizer.global.categories.v1',
    'wipeout.organizer.global.parallelism.v1',
];

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

export function getPersisted(key, fallback) {
    try {
        const raw = localStorage.getItem(key);
        if (!raw) return fallback;
        return JSON.parse(raw);
    } catch {
        return fallback;
    }
}

export function setPersisted(key, value) {
    try {
        localStorage.setItem(key, JSON.stringify(value));
    } catch {
        // ignore quota errors
    }
}

export function removePersisted(key) {
    try {
        localStorage.removeItem(key);
    } catch {
        // ignore storage errors
    }
}

export function cleanupLegacyPersistedState() {
    for (const key of LEGACY_PERSIST_KEYS) {
        removePersisted(key);
    }
}

export function invalidateOrganizerRuntimeCacheIfNeeded() {
    const currentVersion = Number(getPersisted(PERSIST_KEYS.runtimeCacheVersion, 0) || 0);
    if (currentVersion === ORGANIZER_RUNTIME_CACHE_VERSION) {
        return;
    }
    for (const key of RUNTIME_CACHE_KEYS) {
        removePersisted(key);
    }
    setPersisted(PERSIST_KEYS.runtimeCacheVersion, ORGANIZER_RUNTIME_CACHE_VERSION);
}

export function setPersistedApplyManifest(manifest) {
    if (manifest && typeof manifest === 'object') {
        setPersisted(PERSIST_KEYS.lastApplyManifest, manifest);
        return;
    }
    removePersisted(PERSIST_KEYS.lastApplyManifest);
}

export function getPersistedApplyManifest() {
    const manifest = getPersisted(PERSIST_KEYS.lastApplyManifest, null);
    return manifest && typeof manifest === 'object' ? manifest : null;
}

export function persistForm(data) {
    setPersisted(PERSIST_KEYS.rootPath, data.rootPath);
    setPersisted(PERSIST_KEYS.exclusions, data.excludedPatterns);
    setPersisted(PERSIST_KEYS.batchSize, data.batchSize);
    setPersisted(PERSIST_KEYS.summaryStrategy, data.summaryStrategy || DEFAULT_SUMMARY_MODE);
    setPersisted(PERSIST_KEYS.maxClusterDepth, data.maxClusterDepth);
    setPersisted(PERSIST_KEYS.useWebSearch, data.useWebSearch);
    setPersisted(PERSIST_KEYS.modelRouting, data.modelRouting || {});
}

export function restoreDefaults() {
    const modelRouting = getPersisted(PERSIST_KEYS.modelRouting, null);
    return {
        rootPath: getPersisted(PERSIST_KEYS.rootPath, ''),
        excludedPatterns: getPersisted(PERSIST_KEYS.exclusions, DEFAULT_EXCLUSIONS),
        batchSize: getPersisted(PERSIST_KEYS.batchSize, DEFAULT_BATCH_SIZE),
        summaryStrategy: getPersisted(PERSIST_KEYS.summaryStrategy, DEFAULT_SUMMARY_MODE),
        maxClusterDepth: getPersisted(PERSIST_KEYS.maxClusterDepth, null),
        useWebSearch: getPersisted(PERSIST_KEYS.useWebSearch, null),
        modelRouting: modelRouting || {},
    };
}

export function isOrganizerLogCollapsed() {
    return !!getPersisted(PERSIST_KEYS.logCollapsed, true);
}

export function setOrganizerLogCollapsed(collapsed) {
    setPersisted(PERSIST_KEYS.logCollapsed, !!collapsed);
}

export function isOrganizerRecordGroupCollapsed() {
    return !!getPersisted(PERSIST_KEYS.logRecordGroupCollapsed, true);
}

export function setOrganizerRecordGroupCollapsed(collapsed) {
    setPersisted(PERSIST_KEYS.logRecordGroupCollapsed, !!collapsed);
}
