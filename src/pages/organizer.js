import {
  applyOrganize,
  browseFolder,
  connectOrganizeStream,
  getProviderModels,
  getOrganizeCapability,
  getOrganizeResult,
  getSettings,
  saveSettings,
  rollbackOrganize,
  startOrganize,
  stopOrganize,
} from '../utils/api.js';
import { showToast } from '../main.js';
import { getLang, t } from '../utils/i18n.js';
import {
  ensureRequiredCredentialsConfigured,
  getProviderCredentialPresence,
  getSearchCredentialPresence,
} from '../utils/secret-ui.js';

const PERSIST_KEYS = {
  rootPath: 'wipeout.organizer.global.root_path.v2',
  exclusions: 'wipeout.organizer.global.exclusions.v2',
  batchSize: 'wipeout.organizer.global.batch_size.v2',
  summaryMode: 'wipeout.organizer.global.summary_mode.v1',
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

const DEFAULT_EXCLUSIONS = [
  '.git',
  'node_modules',
  'dist',
  'build',
  'out',
  'Windows',
  'Program Files',
  'Program Files (x86)',
];

const DEFAULT_BATCH_SIZE = 20;
const DEFAULT_SUMMARY_MODE = 'filename_only';
const SUMMARY_MODES = ['filename_only', 'local_summary', 'agent_summary'];

const MOVE_RESULT_TEXT = {
  zh: {
    title: '移动结果',
    moved: '已移动',
    skipped: '已跳过',
    failed: '失败',
    total: '总数',
    status: '状态',
    reason: '原因',
    empty: '暂无最近一次移动结果',
    clean: '本次移动没有失败或跳过项',
    reasonSkipped: '源路径与目标路径相同，无需重复移动',
    statusMoved: '已移动',
    statusSkipped: '已跳过',
    statusFailed: '失败',
  },
  en: {
    title: 'Move Results',
    moved: 'Moved',
    skipped: 'Skipped',
    failed: 'Failed',
    total: 'Total',
    status: 'Status',
    reason: 'Reason',
    empty: 'No recent move result yet',
    clean: 'This move had no failed or skipped items',
    reasonSkipped: 'Source and target are identical, so the move was skipped',
    statusMoved: 'Moved',
    statusSkipped: 'Skipped',
    statusFailed: 'Failed',
  },
};

const MODEL_SELECT_IDS = {
  text: 'org-model-text',
  image: 'org-model-image',
  video: 'org-model-video',
  audio: 'org-model-audio',
};

const PROVIDER_SELECT_IDS = {
  text: 'org-provider-text',
  image: 'org-provider-image',
  video: 'org-provider-video',
  audio: 'org-provider-audio',
};

const COPY_TEXT_ROUTE_BTN_ID = 'org-copy-text-route-btn';

const PROVIDER_OPTIONS = [
  { value: 'https://api.deepseek.com', label: 'DeepSeek' },
  { value: 'https://api.openai.com/v1', label: 'OpenAI' },
  { value: 'https://generativelanguage.googleapis.com/v1beta/openai/', label: 'Google Gemini' },
  { value: 'https://dashscope.aliyuncs.com/compatible-mode/v1', label: 'Qwen (DashScope)' },
  { value: 'https://open.bigmodel.cn/api/paas/v4', label: 'GLM (BigModel)' },
  { value: 'https://api.moonshot.cn/v1', label: 'Kimi (Moonshot)' },
];

const PROVIDER_MODELS = {
  'https://api.openai.com/v1': [
    { value: 'gpt-4o-mini', label: 'gpt-4o-mini' },
    { value: 'gpt-4o', label: 'gpt-4o' },
    { value: 'gpt-3.5-turbo', label: 'gpt-3.5-turbo' },
  ],
  'https://api.deepseek.com': [
    { value: 'deepseek-chat', label: 'deepseek-chat' },
    { value: 'deepseek-reasoner', label: 'deepseek-reasoner' },
  ],
  'https://dashscope.aliyuncs.com/compatible-mode/v1': [
    { value: 'qwen-plus', label: 'qwen-plus' },
    { value: 'qwen-turbo', label: 'qwen-turbo' },
    { value: 'qwen-max', label: 'qwen-max' },
  ],
  'https://open.bigmodel.cn/api/paas/v4': [
    { value: 'glm-4-flash', label: 'glm-4-flash' },
    { value: 'glm-4', label: 'glm-4' },
  ],
  'https://api.moonshot.cn/v1': [
    { value: 'moonshot-v1-8k', label: 'moonshot-v1-8k' },
    { value: 'moonshot-v1-32k', label: 'moonshot-v1-32k' },
  ],
  'https://generativelanguage.googleapis.com/v1beta/openai/': [
    { value: 'gemini-2.5-flash', label: 'gemini-2.5-flash' },
    { value: 'gemini-2.5-pro', label: 'gemini-2.5-pro' },
    { value: 'gemini-2.0-flash', label: 'gemini-2.0-flash' },
    { value: 'gemini-1.5-pro', label: 'gemini-1.5-pro' },
  ],
};

let activeTaskId = null;
let activeEventSource = null;
let latestSnapshot = null;
let latestCapability = null;
let providerApiKeyMap = {};
const remoteModelsCache = new Map();
const modelsRequestToken = { text: 0, image: 0, video: 0, audio: 0 };
let organizerProviderSettingsUpdatedHandler = null;
let renderVersion = 0;
let organizerLogEntries = [];
const expandedOrganizerDetailLogIds = new Set();
const loggedOrganizerRawBatchKeys = new Set();
const loggedOrganizerSummaryKeys = new Set();
let organizerLogTaskId = null;
let organizerLogProgressBaseline = null;

function getPersisted(key, fallback) {
  try {
    const raw = localStorage.getItem(key);
    if (!raw) return fallback;
    return JSON.parse(raw);
  } catch {
    return fallback;
  }
}

function setPersisted(key, value) {
  try {
    localStorage.setItem(key, JSON.stringify(value));
  } catch {
    // ignore quota errors
  }
}

function removePersisted(key) {
  try {
    localStorage.removeItem(key);
  } catch {
    // ignore storage errors
  }
}

function cleanupLegacyPersistedState() {
  for (const key of LEGACY_PERSIST_KEYS) {
    removePersisted(key);
  }
}

function invalidateOrganizerRuntimeCacheIfNeeded() {
  const currentVersion = Number(getPersisted(PERSIST_KEYS.runtimeCacheVersion, 0) || 0);
  if (currentVersion === ORGANIZER_RUNTIME_CACHE_VERSION) {
    return;
  }
  for (const key of RUNTIME_CACHE_KEYS) {
    removePersisted(key);
  }
  setPersisted(PERSIST_KEYS.runtimeCacheVersion, ORGANIZER_RUNTIME_CACHE_VERSION);
}

function getErrorMessage(err) {
  if (typeof err === 'string' && err.trim()) {
    return err.trim();
  }
  if (err && typeof err === 'object') {
    if (typeof err.message === 'string' && err.message.trim()) {
      return err.message.trim();
    }
    if (typeof err.error === 'string' && err.error.trim()) {
      return err.error.trim();
    }
  }
  const text = String(err ?? '').trim();
  return text && text !== '[object Object]' ? text : 'unknown error';
}

function getMoveResultText(key) {
  const lang = getLang() === 'en' ? 'en' : 'zh';
  return MOVE_RESULT_TEXT[lang]?.[key] || MOVE_RESULT_TEXT.zh[key] || key;
}

function setPersistedApplyManifest(manifest) {
  if (manifest && typeof manifest === 'object') {
    setPersisted(PERSIST_KEYS.lastApplyManifest, manifest);
    return;
  }
  removePersisted(PERSIST_KEYS.lastApplyManifest);
}

function getPersistedApplyManifest() {
  const manifest = getPersisted(PERSIST_KEYS.lastApplyManifest, null);
  return manifest && typeof manifest === 'object' ? manifest : null;
}

function parseListInput(text) {
  return String(text || '')
    .split(/[\n,]/)
    .map((x) => x.trim())
    .filter(Boolean)
    .filter((x, idx, arr) => arr.indexOf(x) === idx);
}

function normalizeRemoteModels(models) {
  const seen = new Set();
  const normalized = [];
  for (const item of models || []) {
    if (!item?.value) continue;
    const value = String(item.value).trim();
    if (!value || seen.has(value)) continue;
    seen.add(value);
    normalized.push({ value, label: String(item.label || value) });
  }
  return normalized;
}

function ensureProviderOptionExists(select, endpoint) {
  if (!select || !endpoint) return;
  const exists = Array.from(select.options).some((opt) => opt.value === endpoint);
  if (!exists) {
    select.add(new Option(endpoint, endpoint));
  }
}

function getApiKeyForEndpoint(endpoint) {
  return !!providerApiKeyMap?.[String(endpoint || '').trim()];
}

function syncProviderApiKeys(settings = {}) {
  providerApiKeyMap = {};
  if (settings?.providerConfigs && typeof settings.providerConfigs === 'object') {
    for (const [endpoint] of Object.entries(settings.providerConfigs)) {
      providerApiKeyMap[String(endpoint).trim()] = getProviderCredentialPresence(settings, endpoint);
    }
  }
}

function refreshOrganizerApiConfigHint() {
  const hintEl = document.getElementById('org-api-config-hint');
  if (!hintEl) return;
  const selectedEndpoints = Object.values(PROVIDER_SELECT_IDS)
    .map((id) => String(document.getElementById(id)?.value || '').trim())
    .filter(Boolean);
  const uniqueEndpoints = Array.from(new Set(selectedEndpoints));
  const hasMissingConfig = uniqueEndpoints.some((endpoint) => !getApiKeyForEndpoint(endpoint));
  hintEl.hidden = !hasMissingConfig;
  hintEl.style.display = hasMissingConfig ? 'flex' : 'none';
}

function readModelRoutingFromDOM() {
  const textEndpoint = document.getElementById(PROVIDER_SELECT_IDS.text)?.value?.trim() || '';
  const imageEndpoint = document.getElementById(PROVIDER_SELECT_IDS.image)?.value?.trim() || '';
  const videoEndpoint = document.getElementById(PROVIDER_SELECT_IDS.video)?.value?.trim() || '';
  const audioEndpoint = document.getElementById(PROVIDER_SELECT_IDS.audio)?.value?.trim() || '';

  return {
    text: {
      endpoint: textEndpoint,
      model: document.getElementById(MODEL_SELECT_IDS.text)?.value?.trim() || '',
    },
    image: {
      endpoint: imageEndpoint,
      model: document.getElementById(MODEL_SELECT_IDS.image)?.value?.trim() || '',
    },
    video: {
      endpoint: videoEndpoint,
      model: document.getElementById(MODEL_SELECT_IDS.video)?.value?.trim() || '',
    },
    audio: {
      endpoint: audioEndpoint,
      model: document.getElementById(MODEL_SELECT_IDS.audio)?.value?.trim() || '',
    },
  };
}

function collectForm() {
  const rootPath = document.getElementById('org-root-path')?.value?.trim() || '';
  const recursive = true;
  const excludedPatterns = parseListInput(document.getElementById('org-exclusions')?.value || '');
  const batchSizeRaw = Number(document.getElementById('org-batch-size')?.value || DEFAULT_BATCH_SIZE);
  const maxClusterDepthInput = String(document.getElementById('org-max-cluster-depth')?.value || '').trim();
  const parsedMaxClusterDepth = maxClusterDepthInput ? Number(maxClusterDepthInput) : null;
  const useWebSearch = !!document.getElementById('org-enable-web-search')?.checked;
  const summaryModeRaw = String(document.getElementById('org-summary-mode')?.value || '').trim();
  const summaryMode = SUMMARY_MODES.includes(summaryModeRaw) ? summaryModeRaw : DEFAULT_SUMMARY_MODE;
  const modelRouting = readModelRoutingFromDOM();
  const batchSize = Number.isFinite(batchSizeRaw)
    ? Math.max(1, Math.min(200, Math.floor(batchSizeRaw)))
    : DEFAULT_BATCH_SIZE;
  const maxClusterDepth = Number.isFinite(parsedMaxClusterDepth) && parsedMaxClusterDepth > 0
    ? Math.floor(parsedMaxClusterDepth)
    : null;

  return {
    rootPath,
    recursive,
    excludedPatterns: excludedPatterns.length ? excludedPatterns : [...DEFAULT_EXCLUSIONS],
    batchSize,
    summaryMode,
    maxClusterDepth,
    useWebSearch,
    modelRouting,
    responseLanguage: getLang(),
  };
}

function persistForm(data) {
  setPersisted(PERSIST_KEYS.rootPath, data.rootPath);
  setPersisted(PERSIST_KEYS.exclusions, data.excludedPatterns);
  setPersisted(PERSIST_KEYS.batchSize, data.batchSize);
  setPersisted(PERSIST_KEYS.summaryMode, data.summaryMode || DEFAULT_SUMMARY_MODE);
  setPersisted(PERSIST_KEYS.maxClusterDepth, data.maxClusterDepth);
  setPersisted(PERSIST_KEYS.useWebSearch, data.useWebSearch);
  setPersisted(PERSIST_KEYS.modelRouting, data.modelRouting || {});
}

function restoreDefaults() {
  const modelRouting = getPersisted(PERSIST_KEYS.modelRouting, null);
  return {
    rootPath: getPersisted(PERSIST_KEYS.rootPath, ''),
    excludedPatterns: getPersisted(PERSIST_KEYS.exclusions, DEFAULT_EXCLUSIONS),
    batchSize: getPersisted(PERSIST_KEYS.batchSize, DEFAULT_BATCH_SIZE),
    summaryMode: getPersisted(PERSIST_KEYS.summaryMode, DEFAULT_SUMMARY_MODE),
    maxClusterDepth: getPersisted(PERSIST_KEYS.maxClusterDepth, null),
    useWebSearch: getPersisted(PERSIST_KEYS.useWebSearch, null),
    modelRouting: modelRouting || {},
  };
}

function resolveSearchApi(settings) {
  const source = settings?.searchApi && typeof settings.searchApi === 'object'
    ? settings.searchApi
    : {};
  const scopes = source?.scopes && typeof source.scopes === 'object'
    ? source.scopes
    : {};

  return {
    provider: 'tavily',
    enabled: !!source.enabled,
    apiKey: String(source.apiKey || '').trim(),
    scopes: {
      scan: !!scopes.scan,
      classify: !!scopes.classify,
      organizer: !!scopes.organizer,
    },
  };
}

async function syncClassifyWebSearchToSettings(isEnabled) {
  const settings = await getSettings();
  const currentSearchApi = resolveSearchApi(settings);
  const nextSearchApi = {
    provider: 'tavily',
    enabled: !!(currentSearchApi.scopes.scan || isEnabled),
    scopes: {
      scan: !!currentSearchApi.scopes.scan,
      classify: !!isEnabled,
      organizer: !!isEnabled,
    },
  };

  await saveSettings({
    searchApi: nextSearchApi,
  });
}

async function syncSummaryModeTikaToSettings(summaryMode, settings = null) {
  if (!['local_summary', 'agent_summary'].includes(String(summaryMode || '').trim())) {
    return settings;
  }

  const currentSettings = settings || await getSettings();
  const currentTika = currentSettings?.contentExtraction?.tika && typeof currentSettings.contentExtraction.tika === 'object'
    ? currentSettings.contentExtraction.tika
    : {};

  const nextEnabled = true;
  const nextAutoStart = true;
  const currentEnabled = !!currentTika.enabled;
  const currentAutoStart = !!currentTika.autoStart;

  if (currentEnabled === nextEnabled && currentAutoStart === nextAutoStart) {
    return currentSettings;
  }

  await saveSettings({
    contentExtraction: {
      tika: {
        enabled: nextEnabled,
        autoStart: nextAutoStart,
        url: String(currentTika.url || '').trim() || 'http://127.0.0.1:9998',
        jarPath: String(currentTika.jarPath || '').trim(),
      },
    },
  });

  return getSettings();
}

function escapeHtml(value) {
  const div = document.createElement('div');
  div.textContent = String(value ?? '');
  return div.innerHTML;
}

function organizerText(zh, en) {
  return getLang() === 'en' ? en : zh;
}

function getProviderLabel(endpoint) {
  return PROVIDER_OPTIONS.find((item) => item.value === endpoint)?.label || String(endpoint || '').trim() || 'N/A';
}

function getOrganizerCategoryLabel(data = {}) {
  if (String(data.reason || '').trim() === 'classification_error' || String(data.classificationError || '').trim()) {
    return organizerText('分类错误', 'Classification Error');
  }
  if (Array.isArray(data.categoryPath) && data.categoryPath.length > 0) {
    return data.categoryPath.map((segment) => String(segment || '').trim()).filter(Boolean).join(' / ');
  }
  return String(data.category || '').trim() || t('organizer.uncategorized');
}

function getOrganizerLogTime() {
  return new Date().toLocaleTimeString([], { hour12: false });
}

function getOrganizerSummaryModeLabel(mode) {
  if (mode === 'local_summary') return t('organizer.summary_mode_local_summary');
  if (mode === 'agent_summary') return t('organizer.summary_mode_agent_summary');
  return t('organizer.summary_mode_filename_only');
}

function getOrganizerSummaryModeDescription(mode) {
  if (mode === 'local_summary') return t('organizer.summary_mode_local_summary_hint');
  if (mode === 'agent_summary') return t('organizer.summary_mode_agent_summary_hint');
  return t('organizer.summary_mode_filename_only_hint');
}

function getOrganizerSummaryText(row = {}) {
  const summary = String(row.summary || '').trim();
  if (summary) return summary;
  const summarySource = String(row.summarySource || '').trim();
  if (summarySource === 'filename_only') return t('organizer.summary_placeholder_filename_only');
  if (summarySource === 'agent_fallback_local') return t('organizer.summary_placeholder_agent_fallback');
  return t('organizer.summary_placeholder_empty');
}

function buildOrganizerLogEntry({
  type = 'scanning',
  kind = 'simple',
  summary = '',
  text = '',
  detail = '',
  batchKey = '',
  summaryKey = '',
  taskId = organizerLogTaskId || activeTaskId || latestSnapshot?.id || null,
} = {}) {
  const detailText = String(detail || '').trim();
  return {
    id: `${Date.now()}-${Math.random().toString(16).slice(2, 10)}`,
    type,
    kind,
    time: getOrganizerLogTime(),
    taskId: taskId ? String(taskId) : null,
    batchKey: String(batchKey || '').trim() || null,
    summaryKey: String(summaryKey || '').trim() || null,
    summary: String(summary || text || '').trim(),
    text: String(text || summary || '').trim(),
    detailHtml: detailText ? escapeHtml(detailText).replace(/\n/g, '<br>') : '',
  };
}

function normalizeOrganizerLogStringList(value) {
  return Array.isArray(value)
    ? value.map((item) => String(item || '').trim()).filter(Boolean)
    : [];
}

function getOrganizerSummarySourceLabel(source) {
  if (source === 'agent_summary') return organizerText('Agent 摘要', 'Agent Summary');
  if (source === 'agent_fallback_local') return organizerText('Agent 失败后回退本地摘要', 'Agent fallback to local summary');
  if (source === 'local_summary') return organizerText('本地摘要', 'Local Summary');
  if (source === 'filename_only') return organizerText('仅文件名', 'Filename Only');
  return String(source || '').trim() || '-';
}

function buildOrganizerLocalSummaryDetail(row, { category = '-', route = '-' } = {}) {
  if (!row || String(row.summaryMode || '').trim() === 'filename_only') return '';
  const extraction = row?.localExtraction && typeof row.localExtraction === 'object' ? row.localExtraction : null;
  const extractionKeywords = normalizeOrganizerLogStringList(extraction?.keywords);
  const extractionWarnings = normalizeOrganizerLogStringList(extraction?.warnings);
  const extractionMetadata = normalizeOrganizerLogStringList(extraction?.metadata);
  const summaryKeywords = normalizeOrganizerLogStringList(row?.summaryKeywords);
  const summaryWarnings = normalizeOrganizerLogStringList(row?.warnings ?? row?.summaryWarnings);
  const excerpt = String(extraction?.excerpt || '').trim();
  const summary = String(row?.summary || '').trim();
  const title = String(extraction?.title || '').trim();
  const parser = String(extraction?.parser || '').trim();
  const confidence = String(row?.summaryConfidence || '').trim();

  if (
    !extraction
    && !summary
    && !summaryWarnings.length
    && !summaryKeywords.length
  ) {
    return '';
  }

  const detail = [
    `${organizerText('文件', 'Item')}: ${String(row?.name || row?.path || '-').trim() || '-'}`,
    `${organizerText('路径', 'Path')}: ${String(row?.path || '-').trim() || '-'}`,
    `${organizerText('分类', 'Category')}: ${category}`,
    `${organizerText('模型', 'Model')}: ${route}`,
    `${organizerText('输入模式', 'Summary Mode')}: ${getOrganizerSummaryModeLabel(row?.summaryMode || DEFAULT_SUMMARY_MODE)}`,
    `${organizerText('摘要来源', 'Summary Source')}: ${getOrganizerSummarySourceLabel(row?.summarySource)}`,
    parser ? `${organizerText('提取器', 'Extractor')}: ${parser}` : '',
    title ? `${organizerText('标题', 'Title')}: ${title}` : '',
    confidence ? `${organizerText('置信度', 'Confidence')}: ${confidence}` : '',
    extractionKeywords.length ? `${organizerText('提取关键词', 'Extraction Keywords')}: ${extractionKeywords.join(', ')}` : '',
    summaryKeywords.length ? `${organizerText('摘要关键词', 'Summary Keywords')}: ${summaryKeywords.join(', ')}` : '',
    extractionWarnings.length ? `${organizerText('提取告警', 'Extraction Warnings')}: ${extractionWarnings.join(' | ')}` : '',
    summaryWarnings.length ? `${organizerText('摘要告警', 'Summary Warnings')}: ${summaryWarnings.join(' | ')}` : '',
    extractionMetadata.length ? `\n${organizerText('提取元数据', 'Extraction Metadata')}:\n${extractionMetadata.join('\n')}` : '',
    excerpt ? `\n${organizerText('本地提取摘录', 'Local Extraction Excerpt')}:\n${excerpt}` : '',
    summary ? `\n${organizerText('最终摘要', 'Final Summary')}:\n${summary}` : '',
  ].filter(Boolean).join('\n');

  return detail.trim();
}

function syncOrganizerRawBatchKeys() {
  loggedOrganizerRawBatchKeys.clear();
  loggedOrganizerSummaryKeys.clear();
  for (const entry of organizerLogEntries) {
    const batchKey = String(entry?.batchKey || '').trim();
    if (batchKey) loggedOrganizerRawBatchKeys.add(batchKey);
    const summaryKey = String(entry?.summaryKey || '').trim();
    if (summaryKey) loggedOrganizerSummaryKeys.add(summaryKey);
  }
}

function buildOrganizerSummaryLogKey(row, taskId = organizerLogTaskId || activeTaskId || latestSnapshot?.id || null) {
  const resolvedTaskId = String(taskId || row?.taskId || '').trim();
  const pathKey = String(row?.path || row?.relativePath || row?.name || '').trim();
  if (!resolvedTaskId || !pathKey) return '';
  return `${resolvedTaskId}::summary::${pathKey}`;
}

function isGroupedOrganizerLogType(type) {
  return ['scanning', 'analyzing', 'found'].includes(type);
}

function getOrganizerLogIcon(type) {
  if (type === 'found') return '+';
  if (type === 'analyzing') return '*';
  if (type === 'agent_call') return '>';
  if (type === 'agent_response') return '<';
  if (type === 'error') return '!';
  return '-';
}

function setOrganizerDetailLogExpanded(entryId, expanded) {
  if (entryId == null) return;
  if (expanded) {
    expandedOrganizerDetailLogIds.add(entryId);
  } else {
    expandedOrganizerDetailLogIds.delete(entryId);
  }
}

function isOrganizerLogPinnedToBottom(log) {
  if (!log) return true;
  return (log.scrollHeight - log.scrollTop - log.clientHeight) <= 24;
}

function isOrganizerLogCollapsed() {
  return !!getPersisted(PERSIST_KEYS.logCollapsed, true);
}

function setOrganizerLogCollapsed(collapsed) {
  setPersisted(PERSIST_KEYS.logCollapsed, !!collapsed);
}

function isOrganizerRecordGroupCollapsed() {
  return !!getPersisted(PERSIST_KEYS.logRecordGroupCollapsed, true);
}

function setOrganizerRecordGroupCollapsed(collapsed) {
  setPersisted(PERSIST_KEYS.logRecordGroupCollapsed, !!collapsed);
}

function refreshOrganizerLogPanel() {
  const collapsed = isOrganizerLogCollapsed();
  const panel = document.getElementById('org-log-panel');
  const toggleBtn = document.getElementById('org-toggle-log-btn');
  const hint = document.getElementById('org-log-collapsed-hint');
  const hasPreview = organizerLogEntries.length > 0;
  if (panel) {
    panel.classList.toggle('is-collapsed', collapsed);
    panel.classList.toggle('is-clickable-preview', collapsed && hasPreview);
    panel.setAttribute('aria-expanded', collapsed ? 'false' : 'true');
    if (collapsed && hasPreview) {
      panel.setAttribute('role', 'button');
      panel.setAttribute('tabindex', '0');
      panel.setAttribute('aria-label', t('organizer.log_preview_hint'));
    } else {
      panel.removeAttribute('role');
      panel.removeAttribute('tabindex');
      panel.removeAttribute('aria-label');
    }
  }
  if (toggleBtn) {
    toggleBtn.textContent = collapsed ? t('organizer.log_expand') : t('organizer.log_collapse');
    toggleBtn.setAttribute('aria-expanded', collapsed ? 'false' : 'true');
  }
  if (hint) {
    hint.style.display = collapsed && hasPreview ? '' : 'none';
  }
}

function createOrganizerSimpleLogEntryElement(entry) {
  const el = document.createElement('div');
  el.className = `scan-log-entry ${entry.type}`;
  el.innerHTML = `
    <span class="log-icon">${getOrganizerLogIcon(entry.type)}</span>
    <span class="log-time" style="color: var(--text-muted); margin-right: 6px;">[${entry.time}]</span>
    <span class="log-text">${entry.text}</span>
  `;
  return el;
}

function createOrganizerDetailLogEntryElement(entry) {
  const expanded = expandedOrganizerDetailLogIds.has(entry.id);
  const wrapper = document.createElement('div');
  wrapper.className = `scan-log-entry ${entry.type}`;
  wrapper.innerHTML = `
    <span class="log-icon">${getOrganizerLogIcon(entry.type)}</span>
    <div class="log-content">
      <div class="log-detail-header" style="cursor: pointer; user-select: none; display: flex; align-items: center; gap: 6px;">
        <span class="log-time" style="color: var(--text-muted); margin-right: 4px;">[${entry.time}]</span>
        <span class="log-detail-arrow" style="transition: transform 0.2s; display: inline-block; font-size: 0.65rem; transform: ${expanded ? 'rotate(90deg)' : 'rotate(0deg)'};">></span>
        <span class="log-summary">${entry.summary}</span>
      </div>
      <div class="log-detail-body" style="display: ${expanded ? 'block' : 'none'}; margin-top: 8px; padding: 10px 12px; background: rgba(0,0,0,0.35); border-radius: 6px; border: 1px solid rgba(255,255,255,0.06); font-size: 0.72rem; line-height: 1.7; word-break: break-all; white-space: pre-wrap; max-height: 600px; overflow-y: auto; color: var(--text-secondary);">
        ${entry.detailHtml}
      </div>
    </div>
  `;

  const header = wrapper.querySelector('.log-detail-header');
  const body = wrapper.querySelector('.log-detail-body');
  const arrow = wrapper.querySelector('.log-detail-arrow');
  header?.addEventListener('click', () => {
    if (!body || !arrow) return;
    const nextOpen = body.style.display === 'none';
    setOrganizerDetailLogExpanded(entry.id, nextOpen);
    body.style.display = nextOpen ? 'block' : 'none';
    arrow.style.transform = nextOpen ? 'rotate(90deg)' : 'rotate(0deg)';
  });

  return wrapper;
}

function createOrganizerRecordGroupElement(entries) {
  const wrapper = document.createElement('div');
  wrapper.className = `scan-log-group${isOrganizerRecordGroupCollapsed() ? ' is-collapsed' : ''}`;

  const header = document.createElement('div');
  header.className = 'scan-log-group-header';
  header.innerHTML = `
    <div class="scan-log-group-title">
      <span class="scan-log-group-arrow">></span>
      <span>${t('organizer.log_records')} (${entries.length})</span>
    </div>
  `;

  const body = document.createElement('div');
  body.className = 'scan-log-group-body';
  for (const entry of entries) {
    body.appendChild(createOrganizerSimpleLogEntryElement(entry));
  }

  header.addEventListener('click', () => {
    const nextCollapsed = !wrapper.classList.contains('is-collapsed');
    setOrganizerRecordGroupCollapsed(nextCollapsed);
    wrapper.classList.toggle('is-collapsed', nextCollapsed);
  });

  wrapper.appendChild(header);
  wrapper.appendChild(body);
  return wrapper;
}

function applyOrganizerLogScrollPosition(log, { shouldStickToBottom, insertMode, previousScrollTop, previousScrollHeight }) {
  if (shouldStickToBottom) {
    log.scrollTop = log.scrollHeight;
    return;
  }

  if (insertMode === 'top') {
    const heightDelta = Math.max(0, log.scrollHeight - previousScrollHeight);
    log.scrollTop = previousScrollTop + heightDelta;
    return;
  }

  const maxScrollTop = Math.max(0, log.scrollHeight - log.clientHeight);
  log.scrollTop = Math.min(previousScrollTop, maxScrollTop);
}

function updateOrganizerRecordGroupHeader(wrapper, count) {
  if (!wrapper) return;
  const titleText = wrapper.querySelector('.scan-log-group-title span:last-child');
  if (titleText) {
    titleText.textContent = `${t('organizer.log_records')} (${count})`;
  }
}

function ensureOrganizerRecordGroup(log) {
  if (!log) return null;
  let group = log.querySelector('.scan-log-group');
  if (group) return group;

  group = createOrganizerRecordGroupElement([]);
  log.prepend(group);
  return group;
}

function renderOrganizerLogEntries(insertMode = 'reset') {
  const log = document.getElementById('org-log');
  if (!log) return;
  const shouldStickToBottom = isOrganizerLogPinnedToBottom(log);
  const previousScrollTop = log.scrollTop;
  const previousScrollHeight = log.scrollHeight;

  log.innerHTML = '';
  const groupedEntries = organizerLogEntries.filter((entry) => isGroupedOrganizerLogType(entry.type));
  const detailEntries = organizerLogEntries.filter((entry) => !isGroupedOrganizerLogType(entry.type));

  if (groupedEntries.length > 0) {
    log.appendChild(createOrganizerRecordGroupElement(groupedEntries));
  }

  for (const entry of detailEntries) {
    if (entry.kind === 'detail') {
      log.appendChild(createOrganizerDetailLogEntryElement(entry));
    } else {
      log.appendChild(createOrganizerSimpleLogEntryElement(entry));
    }
  }

  applyOrganizerLogScrollPosition(log, {
    shouldStickToBottom,
    insertMode,
    previousScrollTop,
    previousScrollHeight,
  });
}

function replaceOrganizerLogEntries(nextEntries = [], { persist = true, taskId = organizerLogTaskId || activeTaskId || latestSnapshot?.id || null } = {}) {
  organizerLogEntries = Array.isArray(nextEntries) ? nextEntries : [];
  organizerLogTaskId = taskId ? String(taskId) : null;
  expandedOrganizerDetailLogIds.clear();
  syncOrganizerRawBatchKeys();
  if (persist) {
    setPersisted(PERSIST_KEYS.logEntries, organizerLogEntries);
    if (organizerLogTaskId) {
      setPersisted(PERSIST_KEYS.logTaskId, organizerLogTaskId);
    } else {
      removePersisted(PERSIST_KEYS.logTaskId);
    }
  }

  const logEl = document.getElementById('org-log');
  if (logEl) {
    if (organizerLogEntries.length > 0) {
      renderOrganizerLogEntries();
    } else {
      logEl.innerHTML = '';
    }
  }
  refreshOrganizerLogPanel();
}

function appendOrganizerLogEntry(entry, { persist = true } = {}) {
  if (!entry) return;
  organizerLogEntries = [...organizerLogEntries, entry];
  organizerLogTaskId = entry.taskId ? String(entry.taskId) : (organizerLogTaskId || null);
  if (entry.batchKey) {
    loggedOrganizerRawBatchKeys.add(String(entry.batchKey));
  }
  if (entry.summaryKey) {
    loggedOrganizerSummaryKeys.add(String(entry.summaryKey));
  }
  if (persist) {
    setPersisted(PERSIST_KEYS.logEntries, organizerLogEntries);
    if (organizerLogTaskId) {
      setPersisted(PERSIST_KEYS.logTaskId, organizerLogTaskId);
    }
  }

  const log = document.getElementById('org-log');
  if (!log) {
    refreshOrganizerLogPanel();
    return;
  }

  const shouldStickToBottom = isOrganizerLogPinnedToBottom(log);
  const previousScrollTop = log.scrollTop;
  const previousScrollHeight = log.scrollHeight;
  const insertMode = isGroupedOrganizerLogType(entry.type) ? 'top' : 'bottom';

  if (isGroupedOrganizerLogType(entry.type)) {
    const group = ensureOrganizerRecordGroup(log);
    const body = group?.querySelector('.scan-log-group-body');
    if (body) {
      body.appendChild(createOrganizerSimpleLogEntryElement(entry));
    }
    updateOrganizerRecordGroupHeader(group, organizerLogEntries.filter((item) => isGroupedOrganizerLogType(item.type)).length);
  } else if (entry.kind === 'detail') {
    log.appendChild(createOrganizerDetailLogEntryElement(entry));
  } else {
    log.appendChild(createOrganizerSimpleLogEntryElement(entry));
  }

  applyOrganizerLogScrollPosition(log, {
    shouldStickToBottom,
    insertMode,
    previousScrollTop,
    previousScrollHeight,
  });
  refreshOrganizerLogPanel();
}

function bindOrganizerLogPanelEvents() {
  document.getElementById('org-toggle-log-btn')?.addEventListener('click', () => {
    setOrganizerLogCollapsed(!isOrganizerLogCollapsed());
    refreshOrganizerLogPanel();
  });
  document.getElementById('org-log-panel')?.addEventListener('click', (event) => {
    if (!isOrganizerLogCollapsed() || !organizerLogEntries.length) return;
    const target = event.target;
    if (target instanceof Element && target.closest('button, a, input, textarea, select, label')) {
      return;
    }
    setOrganizerLogCollapsed(false);
    refreshOrganizerLogPanel();
  });
  document.getElementById('org-log-panel')?.addEventListener('keydown', (event) => {
    if (!isOrganizerLogCollapsed() || !organizerLogEntries.length) return;
    if (event.key !== 'Enter' && event.key !== ' ') return;
    event.preventDefault();
    setOrganizerLogCollapsed(false);
    refreshOrganizerLogPanel();
  });
  document.getElementById('org-clear-log-btn')?.addEventListener('click', () => {
    replaceOrganizerLogEntries([], { persist: true, taskId: organizerLogTaskId });
  });
}

function syncOrganizerLogProgressBaseline(snapshot) {
  if (!snapshot?.id) {
    organizerLogProgressBaseline = null;
    return;
  }
  organizerLogProgressBaseline = {
    taskId: String(snapshot.id),
    status: String(snapshot.status || 'idle'),
    processedFiles: Number(snapshot.processedFiles || 0),
    processedBatches: Number(snapshot.processedBatches || 0),
    totalBatches: Number(snapshot.totalBatches || 0),
    tokenTotal: Number(snapshot.tokenUsage?.total || 0),
  };
}

function restoreOrganizerLogState(snapshot = null) {
  const rawEntries = getPersisted(PERSIST_KEYS.logEntries, []);
  organizerLogEntries = Array.isArray(rawEntries) ? rawEntries : [];
  organizerLogTaskId = String(getPersisted(PERSIST_KEYS.logTaskId, '') || '').trim() || null;
  expandedOrganizerDetailLogIds.clear();
  syncOrganizerRawBatchKeys();
  syncOrganizerLogProgressBaseline(snapshot);
}

function buildOrganizerBatchRawOutputEntries(results = [], taskId = organizerLogTaskId) {
  const grouped = new Map();
  for (const row of Array.isArray(results) ? results : []) {
    const batchIndex = Number(row?.batchIndex || 0);
    const rawOutput = String(row?.modelRawOutput || '').trim();
    const classificationError = String(row?.classificationError || '').trim();
    if (!batchIndex || (!rawOutput && !classificationError)) continue;
    if (!grouped.has(batchIndex)) {
      grouped.set(batchIndex, {
        batchIndex,
        taskId: String(row?.taskId || taskId || '').trim() || null,
        route: formatOrganizerRouteLabel(row?.provider, row?.model),
        rawOutput,
        classificationError,
        names: [],
      });
    }
    const item = grouped.get(batchIndex);
    const name = String(row?.name || row?.path || '').trim();
    if (name) item.names.push(name);
    if (!item.rawOutput && rawOutput) item.rawOutput = rawOutput;
    if (!item.classificationError && classificationError) item.classificationError = classificationError;
  }

  return Array.from(grouped.values())
    .sort((a, b) => a.batchIndex - b.batchIndex)
    .map((item) => {
      const detail = [
        `${organizerText('批次', 'Batch')}: ${item.batchIndex}`,
        `${organizerText('模型', 'Model')}: ${item.route}`,
        item.names.length ? `${organizerText('文件', 'Items')}: ${item.names.join(', ')}` : '',
        item.classificationError ? `${organizerText('分类错误', 'Classification Error')}: ${item.classificationError}` : '',
        '',
        item.rawOutput || organizerText('模型没有返回可记录的 HTTP 原始响应。', 'The model did not return any recordable HTTP raw response.'),
      ].filter(Boolean).join('\n');
      return buildOrganizerLogEntry({
        type: item.classificationError ? 'error' : 'agent_response',
        kind: 'detail',
        summary: organizerText(`批次 ${item.batchIndex} HTTP 原始响应`, `Batch ${item.batchIndex} HTTP raw response`),
        detail,
        batchKey: `${item.taskId || ''}::${item.batchIndex}`,
        taskId: item.taskId,
      });
    });
}

function ensureOrganizerLogsForSnapshot(snapshot) {
  const snapshotTaskId = String(snapshot?.id || '').trim();
  if (!snapshotTaskId) return;
  if (organizerLogEntries.length > 0 && organizerLogTaskId === snapshotTaskId) return;

  const summary = organizerText('已恢复最近一次归类任务', 'Restored the most recent organize task');
  const detail = [
    `${organizerText('状态', 'Status')}: ${String(snapshot.status || 'idle')}`,
    `${organizerText('目录', 'Root')}: ${snapshot.rootPath || '-'}`,
    `${organizerText('文件', 'Files')}: ${Number(snapshot.processedFiles || 0)}/${Number(snapshot.totalFiles || 0)}`,
    `${organizerText('批次', 'Batches')}: ${Number(snapshot.processedBatches || 0)}/${Number(snapshot.totalBatches || 0)}`,
    `${organizerText('Token', 'Token')}: ${Number(snapshot.tokenUsage?.total || 0).toLocaleString()}`,
  ].join('\n');
  replaceOrganizerLogEntries([
    buildOrganizerLogEntry({
      type: 'agent_response',
      kind: 'detail',
      summary,
      detail,
      taskId: snapshotTaskId,
    }),
    ...buildOrganizerBatchRawOutputEntries(snapshot?.results, snapshotTaskId),
  ], { persist: true, taskId: snapshotTaskId });
}

function formatOrganizerRouteLabel(endpoint, model) {
  const providerLabel = getProviderLabel(endpoint);
  return `${providerLabel}/${String(model || '').trim() || '-'}`;
}

function recordOrganizerStartLog(form, taskId, capability) {
  const textRoute = form?.modelRouting?.text || {};
  const selectedProviders = capability?.selectedProviders || latestCapability?.selectedProviders || {};
  const selectedModels = capability?.selectedModels || latestCapability?.selectedModels || {};
  const detail = [
    `${organizerText('目录', 'Root')}: ${form.rootPath || '-'}`,
    `${organizerText('批大小', 'Batch Size')}: ${Number(form.batchSize || DEFAULT_BATCH_SIZE)}`,
    `${organizerText('聚类深度', 'Cluster Depth')}: ${form.maxClusterDepth == null ? organizerText('不限', 'Unlimited') : Number(form.maxClusterDepth)}`,
    `${organizerText('输入模式', 'Summary Mode')}: ${getOrganizerSummaryModeLabel(form.summaryMode)}`,
    `${organizerText('联网搜索', 'Web Search')}: ${form.useWebSearch ? organizerText('开启', 'Enabled') : organizerText('关闭', 'Disabled')}`,
    `${organizerText('文本路由', 'Text Route')}: ${formatOrganizerRouteLabel(textRoute.endpoint || selectedProviders.text, textRoute.model || selectedModels.text)}`,
  ].join('\n');

  replaceOrganizerLogEntries([], { persist: true, taskId });
  appendOrganizerLogEntry(buildOrganizerLogEntry({
    type: 'agent_call',
    kind: 'detail',
    summary: organizerText('开始归类任务', 'Organize task started'),
    detail,
    taskId,
  }));
}

function recordOrganizerProgressLog(snapshot) {
  if (!snapshot?.id) return;
  const previous = organizerLogProgressBaseline;
  const current = {
    taskId: String(snapshot.id),
    status: String(snapshot.status || 'idle'),
    processedFiles: Number(snapshot.processedFiles || 0),
    processedBatches: Number(snapshot.processedBatches || 0),
    totalBatches: Number(snapshot.totalBatches || 0),
    tokenTotal: Number(snapshot.tokenUsage?.total || 0),
  };

  if (!previous || previous.taskId !== current.taskId) {
    appendOrganizerLogEntry(buildOrganizerLogEntry({
      type: current.status === 'classifying' ? 'analyzing' : 'scanning',
      text: organizerText(
        `任务状态: ${current.status} | 文件 ${current.processedFiles}/${Number(snapshot.totalFiles || 0)} | 批次 ${current.processedBatches}/${current.totalBatches} | Token ${current.tokenTotal.toLocaleString()}`,
        `Task status: ${current.status} | Files ${current.processedFiles}/${Number(snapshot.totalFiles || 0)} | Batches ${current.processedBatches}/${current.totalBatches} | Token ${current.tokenTotal.toLocaleString()}`
      ),
      taskId: current.taskId,
    }));
    return;
  }

  if (previous.status !== current.status) {
    appendOrganizerLogEntry(buildOrganizerLogEntry({
      type: current.status === 'classifying' ? 'analyzing' : 'scanning',
      text: organizerText(
        `阶段切换为 ${current.status}`,
        `Stage changed to ${current.status}`
      ),
      taskId: current.taskId,
    }));
  }

  if (previous.processedBatches !== current.processedBatches || previous.tokenTotal !== current.tokenTotal) {
    appendOrganizerLogEntry(buildOrganizerLogEntry({
      type: current.status === 'classifying' ? 'analyzing' : 'scanning',
      text: organizerText(
        `批次 ${current.processedBatches}/${current.totalBatches} | 已处理 ${current.processedFiles}/${Number(snapshot.totalFiles || 0)} | Token ${current.tokenTotal.toLocaleString()}`,
        `Batches ${current.processedBatches}/${current.totalBatches} | Processed ${current.processedFiles}/${Number(snapshot.totalFiles || 0)} | Token ${current.tokenTotal.toLocaleString()}`
      ),
      taskId: current.taskId,
    }));
  }
}

function recordOrganizerFileDoneLog(row) {
  if (!row) return;
  const taskId = String(row.taskId || organizerLogTaskId || activeTaskId || '').trim() || null;
  const batchIndex = Number(row.batchIndex || 0);
  const batchKey = batchIndex ? `${taskId || ''}::${batchIndex}` : '';
  const summaryKey = buildOrganizerSummaryLogKey(row, taskId);
  const category = getOrganizerCategoryLabel(row);
  const route = formatOrganizerRouteLabel(row.provider, row.model);
  const degradedText = row.degraded ? ` | ${organizerText('降级', 'Degraded')}` : '';
  const classificationError = String(row.classificationError || '').trim();
  const isClassificationErrorRow =
    String(row.reason || '').trim() === 'classification_error' || !!classificationError;
  const isFallbackBatchRow =
    String(row.reason || '').trim() === 'fallback_uncategorized'
    && (!!classificationError || (batchKey && loggedOrganizerRawBatchKeys.has(batchKey)));
  if (batchKey && !loggedOrganizerRawBatchKeys.has(batchKey)) {
    const rawOutput = String(row.modelRawOutput || '').trim();
    if (rawOutput || classificationError) {
      const detail = [
        `${organizerText('批次', 'Batch')}: ${batchIndex}`,
        `${organizerText('模型', 'Model')}: ${route}`,
        classificationError ? `${organizerText('分类错误', 'Classification Error')}: ${classificationError}` : '',
        classificationError ? organizerText('该批次未拿到最终分类结果，下面显示的是中间输出或可记录的原始响应。', 'This batch did not produce a final classification result. The content below is intermediate output or the raw response we managed to record.') : '',
        '',
        rawOutput || organizerText('模型没有返回可记录的 HTTP 原始响应。', 'The model did not return any recordable HTTP raw response.'),
      ].filter(Boolean).join('\n');
      appendOrganizerLogEntry(buildOrganizerLogEntry({
        type: classificationError ? 'error' : 'agent_response',
        kind: 'detail',
        summary: organizerText(`批次 ${batchIndex} HTTP 原始响应`, `Batch ${batchIndex} HTTP raw response`),
        detail,
        batchKey,
        taskId,
      }));
    }
  }
  const localSummaryDetail = buildOrganizerLocalSummaryDetail(row, { category, route });
  if (localSummaryDetail && (!summaryKey || !loggedOrganizerSummaryKeys.has(summaryKey))) {
    appendOrganizerLogEntry(buildOrganizerLogEntry({
      type: 'agent_response',
      kind: 'detail',
      summary: organizerText(
        `本地摘要 | ${row.name || row.path || '-'}`,
        `Local summary | ${row.name || row.path || '-'}`,
      ),
      detail: localSummaryDetail,
      taskId,
      summaryKey,
    }));
  }
  if (isFallbackBatchRow) {
    return;
  }
  appendOrganizerLogEntry(buildOrganizerLogEntry({
    type: isClassificationErrorRow ? 'error' : 'found',
    text: organizerText(
      `${row.name || row.path || '-'} -> ${category} | ${route}${degradedText}`,
      `${row.name || row.path || '-'} -> ${category} | ${route}${degradedText}`
    ),
    taskId,
  }));
}

function recordOrganizerTerminalLog(snapshot, kind, message, detail) {
  appendOrganizerLogEntry(buildOrganizerLogEntry({
    type: kind === 'error' ? 'error' : 'agent_response',
    kind: 'detail',
    summary: message,
    detail,
    taskId: snapshot?.id || organizerLogTaskId || activeTaskId || null,
  }));
}

function recordOrganizerSummaryReadyLog(row) {
  if (!row) return;
  const taskId = String(row.taskId || organizerLogTaskId || activeTaskId || '').trim() || null;
  const route = formatOrganizerRouteLabel(row.provider, row.model);
  const summaryKey = buildOrganizerSummaryLogKey(row, taskId);
  if (summaryKey && loggedOrganizerSummaryKeys.has(summaryKey)) {
    return;
  }
  const detail = buildOrganizerLocalSummaryDetail(row, {
    category: organizerText('待分类', 'Pending Classification'),
    route,
  });
  if (!detail) {
    return;
  }
  appendOrganizerLogEntry(buildOrganizerLogEntry({
    type: 'agent_response',
    kind: 'detail',
    summary: organizerText(
      `分类前摘要 | ${row.name || row.path || '-'}`,
      `Pre-classification summary | ${row.name || row.path || '-'}`,
    ),
    detail,
    taskId,
    summaryKey,
  }));
}

function setStatusText(snapshot) {
  const el = document.getElementById('org-status');
  if (!el) return;
  if (!snapshot) {
    el.textContent = t('organizer.status_idle');
    updateStatusDecor(null);
    return;
  }

  const statusMap = {
    idle: t('organizer.status_idle'),
    scanning: t('organizer.status_scanning'),
    classifying: t('organizer.status_classifying'),
    stopping: t('organizer.status_stopping'),
    stopped: t('organizer.status_stopped'),
    completed: t('organizer.status_completed'),
    moving: t('organizer.status_moving'),
    done: t('organizer.status_done'),
    error: t('organizer.status_error'),
  };

  el.textContent = statusMap[snapshot.status] || snapshot.status;
  updateStatusDecor(snapshot);
}

function getSelectionsFromDOM() {
  const selectedProviders = {};
  const selectedModels = {};
  for (const modality of Object.keys(PROVIDER_SELECT_IDS)) {
    selectedProviders[modality] = String(document.getElementById(PROVIDER_SELECT_IDS[modality])?.value || '').trim();
    selectedModels[modality] = String(document.getElementById(MODEL_SELECT_IDS[modality])?.value || '').trim();
  }
  const hasSelection = Object.values(selectedProviders).some(Boolean) && Object.values(selectedModels).some(Boolean);
  return { selectedProviders, selectedModels, hasSelection };
}

function supportsMultimodalBySelection(selectedProviders, selectedModels) {
  const endpoint = String(selectedProviders?.image || '').toLowerCase();
  const model = String(selectedModels?.image || '').toLowerCase();
  const value = `${endpoint}|${model}`;
  return [
    'gpt-4o',
    'gpt-4.1',
    'gemini',
    'qwen-vl',
    'qvq',
    'glm-4v',
    'claude',
  ].some((token) => value.includes(token));
}

function renderCapability(snapshot) {
  const modelEl = document.getElementById('org-model-name');
  const mmEl = document.getElementById('org-mm-badge');
  if (!modelEl || !mmEl) return;

  const domSelections = getSelectionsFromDOM();
  const useDomSelections = !snapshot && domSelections.hasSelection;
  const selectedModels = snapshot?.selectedModels
    || (useDomSelections ? domSelections.selectedModels : latestCapability?.selectedModels);
  const selectedProviders = snapshot?.selectedProviders
    || (useDomSelections ? domSelections.selectedProviders : latestCapability?.selectedProviders);
  const fallbackModel = selectedModels?.text || '-';
  const labelByEndpoint = new Map(PROVIDER_OPTIONS.map((item) => [item.value, item.label]));
  const renderCell = (modality) => {
    const endpoint = selectedProviders?.[modality] || '';
    const providerLabel = labelByEndpoint.get(endpoint) || endpoint || 'N/A';
    const modelName = selectedModels?.[modality] || fallbackModel;
    return `${providerLabel}/${modelName}`;
  };
  const model = [
    `${t('organizer.model_text')}:${renderCell('text')}`,
    `${t('organizer.model_image')}:${renderCell('image')}`,
    `${t('organizer.model_video')}:${renderCell('video')}`,
    `${t('organizer.model_audio')}:${renderCell('audio')}`,
  ].join(' | ');
  const supports = typeof snapshot?.supportsMultimodal === 'boolean'
    ? snapshot.supportsMultimodal
    : useDomSelections
      ? supportsMultimodalBySelection(selectedProviders, selectedModels)
      : latestCapability?.supportsMultimodal;

  modelEl.textContent = model;

  mmEl.classList.remove('badge-success', 'badge-warning', 'badge-danger');
  if (supports === true) {
    mmEl.textContent = t('organizer.multimodal_supported');
    mmEl.classList.add('badge-success');
  } else if (supports === false) {
    mmEl.textContent = t('organizer.multimodal_not_supported');
    mmEl.classList.add('badge-warning');
  } else {
    mmEl.textContent = t('organizer.multimodal_unknown');
    mmEl.classList.add('badge-danger');
  }
}

function renderModelSelectOptions(select, models, selectedValue) {
  if (!select) return;
  select.innerHTML = '';
  for (const model of models) {
    select.add(new Option(String(model.label || model.value), String(model.value)));
  }
  if (selectedValue) {
    const exists = Array.from(select.options).some((opt) => opt.value === selectedValue);
    if (!exists) {
      select.add(new Option(selectedValue, selectedValue));
    }
    select.value = selectedValue;
  } else if (select.options.length > 0) {
    select.value = select.options[0].value;
  }
}

async function initModelSelectors(defaultSelection = {}) {
  const modality = defaultSelection?.modality;
  if (!modality || !MODEL_SELECT_IDS[modality]) return;

  const providerSelect = document.getElementById(PROVIDER_SELECT_IDS[modality]);
  const modelSelect = document.getElementById(MODEL_SELECT_IDS[modality]);
  if (!providerSelect || !modelSelect) return;

  const endpoint = String(providerSelect.value || '').trim();
  const selectedModel = String(defaultSelection?.model || modelSelect.value || '').trim();
  const requestToken = ++modelsRequestToken[modality];

  modelSelect.disabled = true;
  modelSelect.innerHTML = `<option value="">${t('organizer.model_loading')}</option>`;

  const cacheKey = endpoint;
  let models = [];
  try {
    if (endpoint && getApiKeyForEndpoint(endpoint)) {
      if (remoteModelsCache.has(cacheKey)) {
        models = remoteModelsCache.get(cacheKey);
      } else {
        const resp = await getProviderModels(endpoint);
        models = normalizeRemoteModels(resp?.models || []);
        remoteModelsCache.set(cacheKey, models);
      }
    }
  } catch {
    models = [];
  }

  if (!models.length) {
    models = normalizeRemoteModels(PROVIDER_MODELS[endpoint] || PROVIDER_MODELS['https://api.deepseek.com']);
  }
  if (!models.length) {
    models = [{ value: 'gpt-4o-mini', label: 'gpt-4o-mini' }];
  }

  if (requestToken !== modelsRequestToken[modality]) return;
  renderModelSelectOptions(modelSelect, models, selectedModel || models[0]?.value);
  modelSelect.disabled = false;
}

async function initModelRoutingFields(defaultRouting = {}) {
  let settings = null;
  try {
    settings = await getSettings();
  } catch {
    settings = null;
  }

  syncProviderApiKeys(settings);

  const baseEndpoint = String(settings?.defaultProviderEndpoint || 'https://api.deepseek.com').trim();
  const baseModel = String(
    settings?.providerConfigs?.[baseEndpoint]?.model
    || PROVIDER_MODELS[baseEndpoint]?.[0]?.value
    || 'deepseek-chat'
  ).trim();

  for (const option of PROVIDER_OPTIONS) {
    for (const modality of Object.keys(PROVIDER_SELECT_IDS)) {
      const select = document.getElementById(PROVIDER_SELECT_IDS[modality]);
      if (!select) continue;
      const exists = Array.from(select.options).some((x) => x.value === option.value);
      if (!exists) select.add(new Option(option.label, option.value));
    }
  }

  for (const modality of Object.keys(PROVIDER_SELECT_IDS)) {
    const providerSelect = document.getElementById(PROVIDER_SELECT_IDS[modality]);
    const route = defaultRouting?.[modality] || {};
    const endpoint = String(route.endpoint || baseEndpoint).trim();
    const model = String(route.model || baseModel).trim();

    ensureProviderOptionExists(providerSelect, endpoint);
    if (providerSelect) providerSelect.value = endpoint;

    await initModelSelectors({ modality, model });
  }

  refreshOrganizerApiConfigHint();

  return settings;
}

async function applyTextRouteToOtherModalities() {
  const routing = readModelRoutingFromDOM();
  const textRoute = routing?.text || {};
  const targetModalities = ['image', 'video', 'audio'];

  for (const modality of targetModalities) {
    const providerSelect = document.getElementById(PROVIDER_SELECT_IDS[modality]);
    const endpoint = String(textRoute.endpoint || '').trim();

    ensureProviderOptionExists(providerSelect, endpoint);
    if (providerSelect && endpoint) {
      providerSelect.value = endpoint;
    }
  }

  for (const modality of targetModalities) {
    await initModelSelectors({ modality, model: String(textRoute.model || '').trim() });
  }

  persistForm(collectForm());
  refreshOrganizerApiConfigHint();
}

function renderPreview(snapshot) {
  const groupsEl = document.getElementById('org-preview-groups');
  const empty = document.getElementById('org-preview-empty');
  if (!groupsEl || !empty) return;

  const preview = snapshot?.preview || [];
  const resultsMap = new Map((snapshot?.results || []).map((x) => [x.path, x]));

  if (!preview.length) {
    groupsEl.innerHTML = '';
    empty.style.display = '';
    return;
  }

  empty.style.display = 'none';

  const groups = new Map();
  for (const item of preview) {
    const categoryPath = Array.isArray(item.categoryPath)
      ? item.categoryPath.map((segment) => String(segment || '').trim()).filter(Boolean)
      : [];
    const key = categoryPath.length
      ? categoryPath.join(' / ')
      : String(item.category || t('organizer.uncategorized')).trim() || t('organizer.uncategorized');
    if (!groups.has(key)) {
      groups.set(key, []);
    }
    groups.get(key).push(item);
  }

  const sortedGroups = Array.from(groups.entries()).sort(([a], [b]) => String(a).localeCompare(String(b), 'zh-Hans-CN'));
  groupsEl.innerHTML = sortedGroups.map(([categoryPathLabel, items], groupIdx) => {
    const categoryTrail = categoryPathLabel
      .split(' / ')
      .map((segment) => `<span class="organizer-category-node">${escapeHtml(segment)}</span>`)
      .join('<span class="organizer-category-separator"></span>');
    const rows = items.map((item, rowIdx) => {
      const row = resultsMap.get(item.sourcePath);
      const summaryText = getOrganizerSummaryText(row);
      const summarySource = String(row?.summarySource || '').trim();
      const degraded = row?.degraded
        ? `<div style="margin-top:6px;"><span class="badge badge-warning">${t('organizer.degraded')}</span></div>`
        : '';
      const summaryBadge = summarySource
        ? `<div style="margin-top:6px;"><span class="badge badge-info">${escapeHtml(getOrganizerSummaryModeLabel(row?.summaryMode || DEFAULT_SUMMARY_MODE))}</span></div>`
        : '';

      return `
        <tr style="animation: slideUp 0.2s var(--ease-out) ${Math.min(rowIdx * 0.01, 0.2)}s both;">
          <td>
            <div class="file-name" title="${escapeHtml(row?.name || '')}">${escapeHtml(row?.name || '')}</div>
            <div class="file-path" title="${escapeHtml(item.sourcePath)}">${escapeHtml(item.sourcePath)}</div>
            <div class="file-purpose" title="${escapeHtml(summaryText)}">${escapeHtml(summaryText)}</div>
            ${summaryBadge}
            ${degraded}
          </td>
          <td><div class="file-path" title="${escapeHtml(item.targetPath)}">${escapeHtml(item.targetPath)}</div></td>
        </tr>
      `;
    }).join('');

    return `
      <section class="preview-group" style="animation: slideUp 0.2s var(--ease-out) ${Math.min(groupIdx * 0.03, 0.4)}s both;">
        <div class="preview-group-header">
          <div class="organizer-category-path">${categoryTrail}</div>
          <span class="preview-group-count">${items.length}</span>
        </div>
        <div class="preview-group-table-wrap">
          <table class="data-table preview-group-table">
            <thead>
              <tr>
                <th>${t('organizer.source')}</th>
                <th>${t('organizer.target')}</th>
              </tr>
            </thead>
            <tbody>${rows}</tbody>
          </table>
        </div>
      </section>
    `;
  }).join('');
}

function getMoveStatusLabel(status) {
  if (status === 'failed') return getMoveResultText('statusFailed');
  if (status === 'skipped') return getMoveResultText('statusSkipped');
  return getMoveResultText('statusMoved');
}

function getMoveStatusBadge(status) {
  if (status === 'failed') return 'badge-danger';
  if (status === 'skipped') return 'badge-warning';
  return 'badge-success';
}

function getActiveApplyManifest(snapshot = latestSnapshot) {
  const manifest = getPersistedApplyManifest();
  if (!manifest) return null;

  const manifestTaskId = String(manifest.taskId || '').trim();
  const manifestJobId = String(manifest.jobId || '').trim();
  const snapshotTaskId = String(snapshot?.id || activeTaskId || '').trim();
  const snapshotJobId = String(snapshot?.jobId || getPersisted(PERSIST_KEYS.lastJobId, null) || '').trim();

  if (snapshotTaskId && manifestTaskId && snapshotTaskId !== manifestTaskId) {
    return null;
  }
  if (snapshotJobId && manifestJobId && snapshotJobId !== manifestJobId) {
    return null;
  }
  return manifest;
}

function renderApplyResultPanel(snapshot = latestSnapshot) {
  const card = document.getElementById('org-move-result-card');
  const movedEl = document.getElementById('org-move-moved');
  const skippedEl = document.getElementById('org-move-skipped');
  const failedEl = document.getElementById('org-move-failed');
  const totalEl = document.getElementById('org-move-total');
  const tbody = document.getElementById('org-move-result-body');
  const empty = document.getElementById('org-move-result-empty');
  if (!card || !movedEl || !skippedEl || !failedEl || !totalEl || !tbody || !empty) return;

  const manifest = getActiveApplyManifest(snapshot);
  if (!manifest) {
    card.hidden = true;
    tbody.innerHTML = '';
    empty.style.display = '';
    return;
  }

  card.hidden = false;
  const summary = manifest.summary && typeof manifest.summary === 'object' ? manifest.summary : {};
  const moved = Number(summary.moved || 0);
  const skipped = Number(summary.skipped || 0);
  const failed = Number(summary.failed || 0);
  const total = Number(summary.total || 0);
  const issueEntries = (Array.isArray(manifest.entries) ? manifest.entries : [])
    .filter((entry) => String(entry?.status || '').trim() !== 'moved');

  movedEl.textContent = String(moved);
  skippedEl.textContent = String(skipped);
  failedEl.textContent = String(failed);
  totalEl.textContent = String(total);

  if (!issueEntries.length) {
    tbody.innerHTML = '';
    empty.querySelector('.empty-state-text').textContent = getMoveResultText('clean');
    empty.style.display = '';
    return;
  }

  empty.style.display = 'none';
  tbody.innerHTML = issueEntries.map((entry, idx) => {
    const status = String(entry?.status || '').trim() || 'failed';
    const reason = status === 'skipped'
      ? getMoveResultText('reasonSkipped')
      : (String(entry?.error || '').trim() || t('organizer.degraded_reason_unknown'));

    return `
      <tr style="animation: slideUp 0.2s var(--ease-out) ${Math.min(idx * 0.01, 0.3)}s both;">
        <td><span class="badge ${getMoveStatusBadge(status)}">${escapeHtml(getMoveStatusLabel(status))}</span></td>
        <td>
          <div class="file-path" title="${escapeHtml(entry?.sourcePath || '')}">${escapeHtml(entry?.sourcePath || '')}</div>
        </td>
        <td>
          <div class="file-path" title="${escapeHtml(entry?.targetPath || '')}">${escapeHtml(entry?.targetPath || '')}</div>
        </td>
        <td><div class="file-purpose">${escapeHtml(reason)}</div></td>
      </tr>
    `;
  }).join('');
}

function renderOrganizerResultsEmptyState(snapshot = latestSnapshot) {
  const emptyCard = document.getElementById('org-results-empty-card');
  const stack = document.getElementById('org-results-stack');
  if (!emptyCard || !stack) return;

  const hasSnapshot = !!snapshot?.id;
  emptyCard.hidden = hasSnapshot;
  stack.hidden = !hasSnapshot;
}

function updateStats(snapshot) {
  const total = snapshot?.totalFiles || 0;
  const done = snapshot?.processedFiles || 0;
  const token = snapshot?.tokenUsage?.total || 0;
  const degraded = (snapshot?.results || []).filter((x) => x.degraded).length;

  const totalEl = document.getElementById('org-total');
  const doneEl = document.getElementById('org-done');
  const tokenEl = document.getElementById('org-token');
  const degEl = document.getElementById('org-degraded');

  if (totalEl) totalEl.textContent = String(total);
  if (doneEl) doneEl.textContent = String(done);
  if (tokenEl) tokenEl.textContent = token.toLocaleString();
  if (degEl) degEl.textContent = String(degraded);

  const pct = total > 0 ? ((done / total) * 100).toFixed(1) : '0.0';
  const fill = document.getElementById('org-progress-fill');
  const pctEl = document.getElementById('org-progress-pct');
  if (fill) fill.style.width = `${pct}%`;
  if (pctEl) pctEl.textContent = `${pct}%`;
}

function updateButtons(snapshot) {
  const status = snapshot?.status || 'idle';
  const startBtn = document.getElementById('org-start-btn');
  const stopBtn = document.getElementById('org-stop-btn');
  const applyBtn = document.getElementById('org-apply-btn');
  const rollbackBtn = document.getElementById('org-rollback-btn');

  if (startBtn) {
    const running = status === 'scanning' || status === 'classifying' || status === 'stopping' || status === 'moving';
    startBtn.disabled = running;
  }

  if (stopBtn) {
    const stoppable = status === 'scanning' || status === 'classifying';
    stopBtn.disabled = !stoppable;
  }

  if (applyBtn) {
    const hasPreview = Array.isArray(snapshot?.preview) && snapshot.preview.length > 0;
    applyBtn.disabled = !(status === 'completed' || status === 'done') || !hasPreview;
  }

  if (rollbackBtn) {
    rollbackBtn.disabled = !getPersisted(PERSIST_KEYS.lastJobId, null);
  }
}

function syncBatchConfigInputs(snapshot) {
  if (!snapshot) return;

  const batchSizeInput = document.getElementById('org-batch-size');
  if (batchSizeInput && Number(snapshot.batchSize || 0) > 0) {
    const nextValue = String(snapshot.batchSize);
    if (String(batchSizeInput.value || '') !== nextValue) {
      batchSizeInput.value = nextValue;
    }
  }

  const depthInput = document.getElementById('org-max-cluster-depth');
  if (depthInput) {
    const nextValue = snapshot.maxClusterDepth == null ? '' : String(snapshot.maxClusterDepth);
    if (String(depthInput.value || '') !== nextValue) {
      depthInput.value = nextValue;
    }
  }

  const summaryModeSelect = document.getElementById('org-summary-mode');
  if (summaryModeSelect) {
    const nextValue = SUMMARY_MODES.includes(String(snapshot.summaryMode || '').trim())
      ? String(snapshot.summaryMode).trim()
      : DEFAULT_SUMMARY_MODE;
    if (String(summaryModeSelect.value || '') !== nextValue) {
      summaryModeSelect.value = nextValue;
    }
  }
}

function refreshView(snapshot) {
  latestSnapshot = snapshot || null;
  if (snapshot?.id) {
    activeTaskId = snapshot.id;
    setPersisted(PERSIST_KEYS.lastTaskId, snapshot.id);
    setPersisted(PERSIST_KEYS.lastSnapshot, snapshot);
  } else {
    activeTaskId = null;
  }
  if (snapshot?.jobId) {
    setPersisted(PERSIST_KEYS.lastJobId, snapshot.jobId);
  } else {
    removePersisted(PERSIST_KEYS.lastJobId);
  }
  syncBatchConfigInputs(snapshot);
  syncOrganizerLogProgressBaseline(snapshot);
  setStatusText(snapshot);
  updatePipeline(snapshot);
  renderCapability(snapshot);
  updateStats(snapshot);
  renderOrganizerResultsEmptyState(snapshot);
  renderPreview(snapshot);
  renderApplyResultPanel(snapshot);
  updateButtons(snapshot);
}

function closeActiveSSE() {
  if (activeEventSource) {
    activeEventSource.close();
    activeEventSource = null;
  }
}

function connectTaskStream(taskId) {
  closeActiveSSE();
  activeEventSource = connectOrganizeStream(taskId, {
    onProgress: (snap) => {
      recordOrganizerProgressLog(snap);
      refreshView(snap);
    },
    onSummaryReady: (row) => {
      recordOrganizerSummaryReadyLog(row);
    },
    onFileDone: (row) => {
      recordOrganizerFileDoneLog(row);
    },
    onDone: (snap) => {
      const detail = [
        `${organizerText('状态', 'Status')}: ${String(snap?.status || 'completed')}`,
        `${organizerText('文件', 'Files')}: ${Number(snap?.processedFiles || 0)}/${Number(snap?.totalFiles || 0)}`,
        `${organizerText('批次', 'Batches')}: ${Number(snap?.processedBatches || 0)}/${Number(snap?.totalBatches || 0)}`,
        `${organizerText('降级', 'Degraded')}: ${Number((snap?.results || []).filter((item) => item?.degraded).length)}`,
        `${organizerText('Token', 'Token')}: ${Number(snap?.tokenUsage?.total || 0).toLocaleString()}`,
      ].join('\n');
      recordOrganizerTerminalLog(
        snap,
        'done',
        organizerText('归类完成，结果已就绪', 'Organize task completed'),
        detail,
      );
      refreshView(snap);
      showToast(t('organizer.toast_classify_done'), 'success');
      window.location.hash = '#/organizer-results';
    },
    onStopped: (snap) => {
      const detail = [
        `${organizerText('状态', 'Status')}: ${String(snap?.status || 'stopped')}`,
        `${organizerText('文件', 'Files')}: ${Number(snap?.processedFiles || 0)}/${Number(snap?.totalFiles || 0)}`,
        `${organizerText('批次', 'Batches')}: ${Number(snap?.processedBatches || 0)}/${Number(snap?.totalBatches || 0)}`,
      ].join('\n');
      recordOrganizerTerminalLog(
        snap,
        'stopped',
        organizerText('归类任务已停止', 'Organize task stopped'),
        detail,
      );
      refreshView(snap);
    },
    onError: (err) => {
      const detail = getErrorMessage(err);
      const errSnapshot = err?.snapshot && typeof err.snapshot === 'object' ? err.snapshot : latestSnapshot;
      recordOrganizerTerminalLog(
        errSnapshot,
        'error',
        organizerText('归类任务失败', 'Organize task failed'),
        detail,
      );
      if (errSnapshot) {
        refreshView(errSnapshot);
      }
      showToast(`${t('organizer.toast_failed')}${getErrorMessage(err)}`, 'error');
    },
  });
}

function buildOptimisticRunningSnapshot(taskId, form, capability) {
  const selectedModels = capability?.selectedModels || latestCapability?.selectedModels || {};
  const selectedProviders = capability?.selectedProviders || latestCapability?.selectedProviders || {};
  return {
    id: taskId,
    status: 'scanning',
    error: null,
    rootPath: form.rootPath,
    recursive: true,
    excludedPatterns: Array.isArray(form.excludedPatterns) ? form.excludedPatterns : [],
    batchSize: Number(form.batchSize) || DEFAULT_BATCH_SIZE,
    summaryMode: form.summaryMode || DEFAULT_SUMMARY_MODE,
    maxClusterDepth: form.maxClusterDepth ?? null,
    useWebSearch: !!form.useWebSearch,
    webSearchEnabled: !!form.useWebSearch,
    selectedModel: selectedModels.text || '',
    selectedModels,
    selectedProviders,
    supportsMultimodal: typeof capability?.supportsMultimodal === 'boolean'
      ? capability.supportsMultimodal
      : latestCapability?.supportsMultimodal,
    tree: latestSnapshot?.tree || null,
    treeVersion: Number(latestSnapshot?.treeVersion || 0),
    totalFiles: 0,
    processedFiles: 0,
    totalBatches: 0,
    processedBatches: 0,
    tokenUsage: { prompt: 0, completion: 0, total: 0 },
    results: [],
    preview: [],
    createdAt: new Date().toISOString(),
    completedAt: null,
    jobId: null,
  };
}

async function handleBrowse() {
  const btn = document.getElementById('org-browse-btn');
  if (!btn) return;

  btn.disabled = true;
  btn.textContent = t('organizer.browsing');
  try {
    const result = await browseFolder();
    if (!result.cancelled && result.path) {
      const input = document.getElementById('org-root-path');
      if (input) input.value = result.path;
      const data = collectForm();
      persistForm(data);
    }
  } catch (err) {
    showToast(`${t('organizer.toast_failed')}${getErrorMessage(err)}`, 'error');
  } finally {
    btn.disabled = false;
    btn.textContent = t('settings.browse');
  }
}

async function handleStart() {
  const form = collectForm();
  if (!form.rootPath) {
    showToast(t('organizer.path_required'), 'error');
    return;
  }

  persistForm(form);

  const selectedEndpoints = Object.values(form.modelRouting || {})
    .map((item) => String(item?.endpoint || '').trim())
    .filter(Boolean);
  const missingProviderSecret = Array.from(new Set(selectedEndpoints)).some((endpoint) => !getApiKeyForEndpoint(endpoint));
  let settingsSnapshot = await getSettings().catch(() => null);
  try {
    settingsSnapshot = await syncSummaryModeTikaToSettings(form.summaryMode, settingsSnapshot);
  } catch (err) {
    console.warn('[Organizer] Failed to sync Tika setting for summary mode:', err);
  }
  const needsSearchSecret = !!form.useWebSearch && !getSearchCredentialPresence(settingsSnapshot || {});
  if (missingProviderSecret || needsSearchSecret) {
    try {
      await ensureRequiredCredentialsConfigured({
        providerEndpoints: Array.from(new Set(selectedEndpoints)),
        requireSearchApi: !!form.useWebSearch,
        reasonText: t('settings.api_key_managed_hint'),
      });
      settingsSnapshot = await getSettings();
      syncProviderApiKeys(settingsSnapshot);
      refreshOrganizerApiConfigHint();
    } catch (err) {
      showToast(getErrorMessage(err), 'error');
      return;
    }
    const stillMissingProviderSecret = Array.from(new Set(selectedEndpoints)).some((endpoint) => !getApiKeyForEndpoint(endpoint));
    const stillMissingSearchSecret = !!form.useWebSearch && !getSearchCredentialPresence(settingsSnapshot || {});
    if (stillMissingProviderSecret || stillMissingSearchSecret) {
      showToast(t('settings.api_key_managed_hint'), 'error');
      return;
    }
  }

  const btn = document.getElementById('org-start-btn');
  if (btn) {
    btn.disabled = true;
    btn.innerHTML = `<span class="spinner"></span> ${t('organizer.starting')}`;
  }

  try {
    const result = await startOrganize(form);
    activeTaskId = result.taskId;
    setPersistedApplyManifest(null);
    latestCapability = {
      selectedModels: result.selectedModels,
      selectedProviders: result.selectedProviders,
      supportsMultimodal: result.supportsMultimodal,
    };
    renderCapability();
    renderApplyResultPanel(null);
    setPersisted(PERSIST_KEYS.lastTaskId, activeTaskId);
    recordOrganizerStartLog(form, activeTaskId, result);
    refreshView(buildOptimisticRunningSnapshot(activeTaskId, form, result));
    connectTaskStream(activeTaskId);
    showToast(t('organizer.toast_started'), 'success');
  } catch (err) {
    showToast(`${t('organizer.toast_failed')}${getErrorMessage(err)}`, 'error');
  } finally {
    if (btn) {
      btn.disabled = false;
      btn.textContent = t('organizer.start');
    }
  }
}

async function handleApply() {
  if (!activeTaskId) {
    showToast(t('organizer.no_task'), 'error');
    return;
  }

  if (!confirm(t('organizer.confirm_apply'))) {
    return;
  }

  const btn = document.getElementById('org-apply-btn');
  if (btn) {
    btn.disabled = true;
    btn.innerHTML = `<span class="spinner"></span> ${t('organizer.applying')}`;
  }

  try {
    const result = await applyOrganize(activeTaskId);
    const jobId = result?.manifest?.jobId;
    if (jobId) {
      setPersisted(PERSIST_KEYS.lastJobId, jobId);
    }
    setPersistedApplyManifest(result?.manifest || null);

    const summary = result?.manifest?.summary;
    if (summary) {
      const moved = Number(summary.moved || 0);
      const skipped = Number(summary.skipped || 0);
      const failed = Number(summary.failed || 0);
      const total = Number(summary.total || 0);
      recordOrganizerTerminalLog(
        latestSnapshot,
        failed > 0 ? 'error' : 'done',
        organizerText('移动任务已完成', 'Move operation completed'),
        [
          `${getMoveResultText('moved')}: ${moved}`,
          `${getMoveResultText('skipped')}: ${skipped}`,
          `${getMoveResultText('failed')}: ${failed}`,
          `${getMoveResultText('total')}: ${total}`,
        ].join('\n'),
      );
      showToast(
        `${t('organizer.toast_apply_done')} (${getMoveResultText('moved')}: ${moved}，${getMoveResultText('skipped')}: ${skipped}，${getMoveResultText('failed')}: ${failed}，${getMoveResultText('total')}: ${total})`,
        failed > 0 ? 'error' : (skipped > 0 ? 'info' : 'success')
      );
    } else {
      recordOrganizerTerminalLog(
        latestSnapshot,
        'done',
        organizerText('移动任务已完成', 'Move operation completed'),
        organizerText('移动结果已更新。', 'Move results have been updated.'),
      );
      showToast(t('organizer.toast_apply_done'), 'success');
    }

    const snapshot = await getOrganizeResult(activeTaskId);
    refreshView(snapshot);
  } catch (err) {
    showToast(`${t('organizer.toast_failed')}${getErrorMessage(err)}`, 'error');
  } finally {
    if (btn) {
      btn.disabled = false;
      btn.textContent = t('organizer.apply_move');
    }
    updateButtons(latestSnapshot);
  }
}

async function handleStop() {
  if (!activeTaskId) {
    showToast(t('organizer.no_task'), 'error');
    return;
  }

  const btn = document.getElementById('org-stop-btn');
  if (btn) {
    btn.disabled = true;
    btn.innerHTML = `<span class="spinner"></span> ${t('organizer.stopping')}`;
  }

  try {
    await stopOrganize(activeTaskId);
    if (latestSnapshot?.id === activeTaskId) {
      refreshView({
        ...latestSnapshot,
        status: 'stopping',
      });
    }
    showToast(t('organizer.toast_stopped'), 'info');
  } catch (err) {
    showToast(`${t('organizer.toast_stop_failed')}${getErrorMessage(err)}`, 'error');
  } finally {
    if (btn) {
      btn.textContent = t('organizer.stop');
    }
    updateButtons(latestSnapshot);
  }
}

async function handleRollback() {
  const jobId = getPersisted(PERSIST_KEYS.lastJobId, null);
  if (!jobId) {
    showToast(t('organizer.no_rollback_job'), 'error');
    return;
  }

  if (!confirm(t('organizer.confirm_rollback'))) {
    return;
  }

  const btn = document.getElementById('org-rollback-btn');
  if (btn) {
    btn.disabled = true;
    btn.innerHTML = `<span class="spinner"></span> ${t('organizer.rolling_back')}`;
  }

  try {
    const result = await rollbackOrganize(jobId);
    const summary = result?.rollback?.summary;
    const persistedTaskId = activeTaskId || getPersisted(PERSIST_KEYS.lastTaskId, null);
    if (summary && Number(summary.failed || 0) === 0) {
      setPersistedApplyManifest(null);
      removePersisted(PERSIST_KEYS.lastJobId);
      renderApplyResultPanel(latestSnapshot);
    }
    if (persistedTaskId) {
      try {
        const snapshot = await getOrganizeResult(persistedTaskId);
        refreshView(snapshot);
      } catch (refreshErr) {
        console.warn('[Organizer] Failed to refresh snapshot after rollback:', refreshErr);
      }
    }
    if (summary) {
      recordOrganizerTerminalLog(
        latestSnapshot,
        summary.failed > 0 ? 'error' : 'done',
        organizerText('回滚任务已完成', 'Rollback completed'),
        [
          `${organizerText('已回滚', 'Rolled back')}: ${Number(summary.rolledBack || 0)}`,
          `${organizerText('总数', 'Total')}: ${Number(summary.total || 0)}`,
          `${organizerText('失败', 'Failed')}: ${Number(summary.failed || 0)}`,
        ].join('\n'),
      );
      showToast(
        `${t('organizer.toast_rollback_done')} (${summary.rolledBack}/${summary.total})`,
        summary.failed > 0 ? 'info' : 'success'
      );
    } else {
      recordOrganizerTerminalLog(
        latestSnapshot,
        'done',
        organizerText('回滚任务已完成', 'Rollback completed'),
        organizerText('最近一次移动任务已回滚。', 'The most recent move task has been rolled back.'),
      );
      showToast(t('organizer.toast_rollback_done'), 'success');
    }
  } catch (err) {
    showToast(`${t('organizer.toast_failed')}${getErrorMessage(err)}`, 'error');
  } finally {
    if (btn) {
      btn.disabled = false;
      btn.textContent = t('organizer.rollback');
    }
    updateButtons(latestSnapshot);
  }
}

async function handleCopyTextRoute() {
  const btn = document.getElementById(COPY_TEXT_ROUTE_BTN_ID);
  if (btn) {
    btn.disabled = true;
  }

  try {
    await applyTextRouteToOtherModalities();
    showToast(t('organizer.toast_route_copied'), 'success');
  } catch (err) {
    showToast(`${t('organizer.toast_failed')}${getErrorMessage(err)}`, 'error');
  } finally {
    if (btn) {
      btn.disabled = false;
    }
  }
}

function bindPersistenceListeners() {
  [
    'org-root-path',
    'org-enable-web-search',
    'org-exclusions',
    'org-batch-size',
    'org-summary-mode',
    'org-max-cluster-depth',
    PROVIDER_SELECT_IDS.text,
    PROVIDER_SELECT_IDS.image,
    PROVIDER_SELECT_IDS.video,
    PROVIDER_SELECT_IDS.audio,
    MODEL_SELECT_IDS.text,
    MODEL_SELECT_IDS.image,
    MODEL_SELECT_IDS.video,
    MODEL_SELECT_IDS.audio,
  ].forEach((id) => {
    const el = document.getElementById(id);
    if (!el) return;
    const eventName = [
      'org-enable-web-search',
      'org-summary-mode',
      PROVIDER_SELECT_IDS.text,
      PROVIDER_SELECT_IDS.image,
      PROVIDER_SELECT_IDS.video,
      PROVIDER_SELECT_IDS.audio,
      MODEL_SELECT_IDS.text,
      MODEL_SELECT_IDS.image,
      MODEL_SELECT_IDS.video,
      MODEL_SELECT_IDS.audio,
    ].includes(id)
      ? 'change'
      : 'input';
    el.addEventListener(eventName, () => {
      persistForm(collectForm());
      if (id === 'org-enable-web-search') {
        syncClassifyWebSearchToSettings(!!el.checked).catch((err) => {
          console.warn('[Organizer] Failed to sync classify web search setting:', err);
        });
      }
    });
  });
}

function bindModelRoutingListeners() {
  for (const modality of Object.keys(PROVIDER_SELECT_IDS)) {
    const providerSelect = document.getElementById(PROVIDER_SELECT_IDS[modality]);
    const modelSelect = document.getElementById(MODEL_SELECT_IDS[modality]);

    providerSelect?.addEventListener('change', async () => {
      await initModelSelectors({ modality });
      persistForm(collectForm());
      refreshOrganizerApiConfigHint();
      renderCapability();
    });

    modelSelect?.addEventListener('change', () => {
      persistForm(collectForm());
      renderCapability();
    });
  }
}

function stripDecorativePrefix(text) {
  return String(text || '').replace(/^[^\p{L}\p{N}]+/u, '').trim();
}

function renderPipelineStage(stepId, order, label) {
  return `
    <div class="organizer-stage" id="org-stage-${stepId}" data-state="pending">
      <span class="organizer-stage-index">${order}</span>
      <div class="organizer-stage-copy">
        <span class="organizer-stage-title">${escapeHtml(label)}</span>
      </div>
    </div>
  `;
}

function renderRoutingCard(modality) {
  const label = t(`organizer.model_${modality}`);
  return `
    <div class="organizer-route-card">
      <div class="organizer-route-card-header">
        <span class="organizer-route-chip">${escapeHtml(label)}</span>
      </div>
      <div class="provider-model-inline organizer-route-inputs">
        <select id="${PROVIDER_SELECT_IDS[modality]}" class="form-input"></select>
        <select id="${MODEL_SELECT_IDS[modality]}" class="form-input"></select>
      </div>
    </div>
  `;
}

function updateStatusDecor(snapshot) {
  const el = document.getElementById('org-status');
  if (!el) return;

  const status = String(snapshot?.status || 'idle').trim() || 'idle';
  el.dataset.status = status;
  el.classList.remove('badge-info', 'badge-warning', 'badge-success', 'badge-danger');

  const tone = {
    idle: 'badge-info',
    scanning: 'badge-warning',
    classifying: 'badge-warning',
    stopping: 'badge-warning',
    stopped: 'badge-info',
    completed: 'badge-success',
    moving: 'badge-warning',
    done: 'badge-success',
    error: 'badge-danger',
  }[status] || 'badge-info';

  el.classList.add(tone);
}

function updatePipeline(snapshot) {
  const stages = {
    scanning: document.getElementById('org-stage-scanning'),
    classifying: document.getElementById('org-stage-classifying'),
    preview: document.getElementById('org-stage-preview'),
    apply: document.getElementById('org-stage-apply'),
  };
  if (Object.values(stages).some((node) => !node)) return;

  const status = String(snapshot?.status || 'idle').trim() || 'idle';
  const hasPreview = Array.isArray(snapshot?.preview) && snapshot.preview.length > 0;
  const states = {
    scanning: 'pending',
    classifying: 'pending',
    preview: 'pending',
    apply: 'pending',
  };

  if (status === 'scanning') {
    states.scanning = 'active';
  } else if (status === 'classifying') {
    states.scanning = 'done';
    states.classifying = 'active';
  } else if (status === 'stopping') {
    if (Number(snapshot?.processedFiles || 0) > 0 || Number(snapshot?.totalFiles || 0) > 0) {
      states.scanning = 'done';
      states.classifying = 'active';
    } else {
      states.scanning = 'active';
    }
  } else if (status === 'completed') {
    states.scanning = 'done';
    states.classifying = 'done';
    states.preview = 'active';
  } else if (status === 'moving') {
    states.scanning = 'done';
    states.classifying = 'done';
    states.preview = 'done';
    states.apply = 'active';
  } else if (status === 'done') {
    states.scanning = 'done';
    states.classifying = 'done';
    states.preview = 'done';
    states.apply = 'done';
  } else if (status === 'stopped' || status === 'error') {
    if (hasPreview) {
      states.scanning = 'done';
      states.classifying = 'done';
      states.preview = 'active';
    } else if (Number(snapshot?.processedFiles || 0) > 0 || Number(snapshot?.totalFiles || 0) > 0) {
      states.scanning = 'done';
      states.classifying = 'active';
    } else {
      states.scanning = 'active';
    }
  }

  for (const [name, node] of Object.entries(stages)) {
    node.dataset.state = states[name];
  }
}

async function restoreOrganizerSnapshot(cachedSnapshot, isStale) {
  try {
    latestCapability = await getOrganizeCapability();
  } catch {
    latestCapability = null;
  }
  if (isStale()) return;
  renderCapability();

  const lastTaskId = getPersisted(PERSIST_KEYS.lastTaskId, null) || cachedSnapshot?.id || null;
  if (lastTaskId) {
    try {
      const snapshot = await getOrganizeResult(lastTaskId);
      if (isStale()) return;
      activeTaskId = lastTaskId;
      ensureOrganizerLogsForSnapshot(snapshot);
      refreshView(snapshot);

      if (['scanning', 'classifying', 'moving'].includes(snapshot.status)) {
        connectTaskStream(lastTaskId);
      }
      return;
    } catch {
      if (cachedSnapshot?.id === lastTaskId) {
        ensureOrganizerLogsForSnapshot(cachedSnapshot);
        refreshView(cachedSnapshot);
        return;
      }
      refreshView(null);
      return;
    }
  }

  if (!cachedSnapshot?.id) {
    refreshView(null);
  }
}

function renderOrganizerStatsGrid(animationDelay = '0.09s') {
  return `
    <div class="stats-grid organizer-stats-grid animate-in" style="animation-delay: ${animationDelay};">
      <div class="stat-card organizer-stat-card">
        <span class="stat-label">${t('organizer.total_files')}</span>
        <span class="stat-value" id="org-total">0</span>
      </div>
      <div class="stat-card organizer-stat-card">
        <span class="stat-label">${t('organizer.done_files')}</span>
        <span class="stat-value success" id="org-done">0</span>
      </div>
      <div class="stat-card organizer-stat-card">
        <span class="stat-label">Token</span>
        <span class="stat-value warning" id="org-token">0</span>
      </div>
      <div class="stat-card organizer-stat-card">
        <span class="stat-label">${t('organizer.degraded')}</span>
        <span class="stat-value danger" id="org-degraded">0</span>
      </div>
    </div>
  `;
}

function renderOrganizerPreviewPanel(animationDelay = '0.13s', subtitle = t('organizer.subtitle')) {
  return `
    <section class="card organizer-panel organizer-preview-panel animate-in" style="animation-delay: ${animationDelay}; padding: 0; overflow: hidden;">
      <div class="card-header organizer-panel-header">
        <div>
          <h2 class="card-title">${t('organizer.preview_title')}</h2>
          <div class="form-hint">${escapeHtml(subtitle)}</div>
        </div>
      </div>
      <div id="org-preview-groups" class="preview-groups organizer-preview-groups"></div>
      <div id="org-preview-empty" class="empty-state organizer-empty-state" style="padding: 32px;">
        <div class="organizer-empty-glyph" aria-hidden="true"></div>
        <div class="empty-state-text">${t('organizer.preview_empty')}</div>
      </div>
    </section>
  `;
}

function renderOrganizerMoveResultPanel(animationDelay = '0.17s') {
  return `
    <section id="org-move-result-card" class="card organizer-panel animate-in mt-24" style="animation-delay: ${animationDelay}; padding: 0; overflow: hidden;" hidden>
      <div class="card-header organizer-panel-header">
        <div>
          <h2 class="card-title">${escapeHtml(getMoveResultText('title'))}</h2>
          <div class="form-hint">${t('organizer.apply_move')}</div>
        </div>
      </div>
      <div class="stats-grid organizer-stats-grid organizer-stats-grid-compact" style="padding: 20px 20px 0;">
        <div class="stat-card organizer-stat-card">
          <span class="stat-label">${escapeHtml(getMoveResultText('moved'))}</span>
          <span id="org-move-moved" class="stat-value success">0</span>
        </div>
        <div class="stat-card organizer-stat-card">
          <span class="stat-label">${escapeHtml(getMoveResultText('skipped'))}</span>
          <span id="org-move-skipped" class="stat-value warning">0</span>
        </div>
        <div class="stat-card organizer-stat-card">
          <span class="stat-label">${escapeHtml(getMoveResultText('failed'))}</span>
          <span id="org-move-failed" class="stat-value danger">0</span>
        </div>
        <div class="stat-card organizer-stat-card">
          <span class="stat-label">${escapeHtml(getMoveResultText('total'))}</span>
          <span id="org-move-total" class="stat-value">0</span>
        </div>
      </div>
      <div class="organizer-table-wrap" style="padding: 20px 20px 0;">
        <table class="data-table">
          <thead>
            <tr>
              <th style="width: 10%;">${escapeHtml(getMoveResultText('status'))}</th>
              <th style="width: 32%;">${t('organizer.source')}</th>
              <th style="width: 32%;">${t('organizer.target')}</th>
              <th style="width: 26%;">${escapeHtml(getMoveResultText('reason'))}</th>
            </tr>
          </thead>
          <tbody id="org-move-result-body"></tbody>
        </table>
      </div>
      <div id="org-move-result-empty" class="empty-state organizer-empty-state" style="padding: 24px;">
        <div class="empty-state-text">${escapeHtml(getMoveResultText('empty'))}</div>
      </div>
    </section>
  `;
}

export async function renderOrganizer(container) {
  const expectedRenderVersion = ++renderVersion;
  const isStale = () => expectedRenderVersion !== renderVersion || !container.isConnected;
  cleanupLegacyPersistedState();
  invalidateOrganizerRuntimeCacheIfNeeded();
  const defaults = restoreDefaults();
  const cachedSnapshot = getPersisted(PERSIST_KEYS.lastSnapshot, null);
  restoreOrganizerLogState(cachedSnapshot);

  container.innerHTML = `
    <div class="organizer-shell">
      <section class="card organizer-hero animate-in" style="animation-delay: 0.03s;">
        <div class="organizer-hero-grid">
          <div class="organizer-hero-copy">
            <div class="page-header organizer-page-header">
              <div class="organizer-kicker">${t('organizer.config')}</div>
              <h1 class="page-title">${escapeHtml(stripDecorativePrefix(t('organizer.title')) || t('organizer.title'))}</h1>
              <p class="page-subtitle">${t('organizer.subtitle')}</p>
            </div>
            <div class="organizer-feature-pills">
              <span class="organizer-feature-pill">${t('organizer.activity_log')}</span>
              <span class="organizer-feature-pill">${t('organizer.preview_title')}</span>
              <span class="organizer-feature-pill">${t('organizer.multimodal')}</span>
            </div>
            <div class="organizer-pipeline">
              ${renderPipelineStage('scanning', '01', t('organizer.status_scanning'))}
              ${renderPipelineStage('classifying', '02', t('organizer.status_classifying'))}
              ${renderPipelineStage('preview', '03', t('organizer.preview_title'))}
              ${renderPipelineStage('apply', '04', t('organizer.apply_move'))}
            </div>
          </div>

          <div class="organizer-hero-side">
            <div class="organizer-status-card">
              <div class="organizer-status-row">
                <div class="organizer-status-stack">
                  <span id="org-status" class="badge badge-info">${t('organizer.status_idle')}</span>
                  <div class="organizer-status-caption">${t('organizer.progress')}</div>
                </div>
                <span id="org-progress-pct" class="organizer-progress-pct">0.0%</span>
              </div>
              <div class="progress-bar organizer-progress-bar">
                <div id="org-progress-fill" class="progress-fill" style="width:0%;"></div>
              </div>
              <div class="organizer-status-meta">
                <div>
                  <div class="form-label">${t('organizer.current_model')}</div>
                  <div id="org-model-name" class="form-hint mono organizer-model-summary">-</div>
                </div>
                <div>
                  <div class="form-label">${t('organizer.multimodal')}</div>
                  <span id="org-mm-badge" class="badge badge-danger">${t('organizer.multimodal_unknown')}</span>
                </div>
              </div>
              <div class="organizer-action-grid organizer-action-grid-compact">
                <button id="org-start-btn" class="btn btn-primary" type="button">${t('organizer.start')}</button>
                <button id="org-stop-btn" class="btn btn-danger" type="button" disabled>${t('organizer.stop')}</button>
              </div>
            </div>
          </div>
        </div>
      </section>

      <div class="organizer-main-grid">
        <div class="organizer-config-stack">
          <section class="card organizer-panel animate-in" style="animation-delay: 0.07s;">
            <div class="card-header organizer-section-header">
              <div>
                <h2 class="card-title">${t('organizer.config')}</h2>
                <div class="form-hint">${t('organizer.batch_cluster_hint')}</div>
              </div>
            </div>

            <div class="form-group">
              <label class="form-label">${t('organizer.root_path')}</label>
              <div class="organizer-path-row">
                <input id="org-root-path" class="form-input organizer-path-input" value="${escapeHtml(defaults.rootPath)}" placeholder="C:\\Users\\..." />
                <button id="org-browse-btn" class="btn btn-secondary organizer-browse-btn" type="button">${t('settings.browse')}</button>
              </div>
            </div>

            <div class="grid-2 organizer-metrics-grid">
              <div class="form-group organizer-metric-field">
                <label class="form-label">${t('organizer.batch_size')}</label>
                <input id="org-batch-size" type="number" min="1" max="200" class="form-input no-spin" value="${Number(defaults.batchSize) || DEFAULT_BATCH_SIZE}" />
                <div class="form-hint">${t('organizer.batch_size_hint')}</div>
              </div>
              <div class="form-group organizer-metric-field">
                <label class="form-label">${t('organizer.summary_mode')}</label>
                <select id="org-summary-mode" class="form-input">
                  <option value="filename_only" ${defaults.summaryMode === 'filename_only' ? 'selected' : ''}>${t('organizer.summary_mode_filename_only')}</option>
                  <option value="local_summary" ${defaults.summaryMode === 'local_summary' ? 'selected' : ''}>${t('organizer.summary_mode_local_summary')}</option>
                  <option value="agent_summary" ${defaults.summaryMode === 'agent_summary' ? 'selected' : ''}>${t('organizer.summary_mode_agent_summary')}</option>
                </select>
                <div class="form-hint" id="org-summary-mode-hint">${t(`organizer.summary_mode_${defaults.summaryMode || DEFAULT_SUMMARY_MODE}_hint`)}</div>
              </div>
              <div class="form-group organizer-metric-field">
                <label class="form-label">${t('organizer.max_cluster_depth')}</label>
                <input id="org-max-cluster-depth" type="number" min="1" class="form-input no-spin" value="${defaults.maxClusterDepth == null ? '' : Number(defaults.maxClusterDepth)}" placeholder="${t('organizer.max_cluster_depth_unlimited')}" />
                <div class="form-hint">${t('organizer.max_cluster_depth_hint')}</div>
              </div>
            </div>

            <div class="organizer-toggle-grid">
              <label class="organizer-toggle-card">
                <input id="org-enable-web-search" type="checkbox" ${defaults.useWebSearch === true ? 'checked' : ''} />
                <span class="organizer-toggle-copy">
                  <span class="organizer-toggle-title">${t('organizer.use_web_search')}</span>
                  <span class="organizer-toggle-hint">${t('organizer.use_web_search_hint')}</span>
                </span>
              </label>
            </div>
          </section>

          <section class="card organizer-panel animate-in" style="animation-delay: 0.11s;">
            <div class="card-header organizer-section-header">
              <div>
                <h2 class="card-title">${t('organizer.model_routing')}</h2>
                <div class="form-hint">${t('organizer.model_routing_hint')}</div>
              </div>
            </div>

            <div class="organizer-routing-grid">
              ${renderRoutingCard('text')}
              ${renderRoutingCard('image')}
              ${renderRoutingCard('video')}
              ${renderRoutingCard('audio')}
            </div>

            <div id="org-api-config-hint" class="form-hint api-config-hint">${t('settings.api_key_managed_hint')}</div>

            <div class="organizer-routing-footer">
              <button id="${COPY_TEXT_ROUTE_BTN_ID}" class="btn btn-secondary" type="button">${t('organizer.copy_text_route')}</button>
            </div>
          </section>

          <section class="card organizer-panel animate-in" style="animation-delay: 0.15s;">
            <div class="card-header organizer-section-header">
              <div>
                <h2 class="card-title">${t('organizer.exclusions')}</h2>
                <div class="form-hint">${t('organizer.exclusions_hint')}</div>
              </div>
            </div>

            <textarea id="org-exclusions" class="form-input organizer-exclusion-input" rows="8">${escapeHtml((defaults.excludedPatterns || []).join('\n'))}</textarea>
          </section>
        </div>

        <div class="organizer-results-stack">
          ${renderOrganizerStatsGrid('0.09s')}

          <section class="card organizer-panel animate-in mt-24" style="animation-delay: 0.13s;">
            <div class="card-header">
              <h2 class="card-title">${t('organizer.activity_log')}</h2>
              <div style="display:flex; gap:8px; align-items:center;">
                <button id="org-toggle-log-btn" class="btn btn-ghost" style="padding: 6px 12px; font-size: 0.75rem;">${t('organizer.log_expand')}</button>
                <button id="org-clear-log-btn" class="btn btn-ghost" style="padding: 6px 12px; font-size: 0.75rem;">Clear</button>
              </div>
            </div>
            <div id="org-log-panel" class="scan-log-panel">
              <div class="scan-activity">
                <div class="scan-log" id="org-log"></div>
                <div id="org-log-collapsed-hint" class="scan-log-collapsed-hint" style="display:none;">${t('organizer.log_preview_hint')}</div>
              </div>
            </div>
          </section>
        </div>
      </div>
    </div>
  `;

  document.getElementById('org-browse-btn')?.addEventListener('click', handleBrowse);
  document.getElementById('org-start-btn')?.addEventListener('click', handleStart);
  document.getElementById('org-stop-btn')?.addEventListener('click', handleStop);
  document.getElementById(COPY_TEXT_ROUTE_BTN_ID)?.addEventListener('click', handleCopyTextRoute);
  document.getElementById('org-summary-mode')?.addEventListener('change', () => {
    const select = document.getElementById('org-summary-mode');
    const hint = document.getElementById('org-summary-mode-hint');
    if (hint && select) {
      hint.textContent = getOrganizerSummaryModeDescription(select.value);
    }
    syncSummaryModeTikaToSettings(select?.value).catch((err) => {
      console.warn('[Organizer] Failed to enable Tika for summary mode:', err);
    });
  });
  bindOrganizerLogPanelEvents();
  if (organizerLogEntries.length > 0) {
    renderOrganizerLogEntries();
  }
  refreshOrganizerLogPanel();

  const runtimeSettings = await initModelRoutingFields(defaults.modelRouting);
  if (isStale()) return;
  if (defaults.useWebSearch == null) {
    const searchApi = resolveSearchApi(runtimeSettings);
    const organizerDefault = searchApi.enabled && (searchApi.scopes.classify || searchApi.scopes.organizer);
    const toggle = document.getElementById('org-enable-web-search');
    if (toggle) {
      toggle.checked = organizerDefault;
    }
    persistForm(collectForm());
  }
  bindModelRoutingListeners();
  bindPersistenceListeners();
  refreshOrganizerApiConfigHint();
  renderCapability();
  if (cachedSnapshot?.id) {
    ensureOrganizerLogsForSnapshot(cachedSnapshot);
    refreshView(cachedSnapshot);
  } else {
    refreshView(null);
  }

  if (organizerProviderSettingsUpdatedHandler) {
    window.removeEventListener('provider-settings-updated', organizerProviderSettingsUpdatedHandler);
  }
  organizerProviderSettingsUpdatedHandler = async (event) => {
    try {
      const settingsSnapshot = event?.detail && typeof event.detail === 'object'
        ? event.detail
        : await getSettings();
      syncProviderApiKeys(settingsSnapshot);
      const currentRouting = readModelRoutingFromDOM();
      for (const modality of Object.keys(PROVIDER_SELECT_IDS)) {
        await initModelSelectors({
          modality,
          model: String(currentRouting?.[modality]?.model || '').trim(),
        });
      }
      refreshOrganizerApiConfigHint();
      renderCapability();
    } catch (err) {
      console.warn('[Organizer] Failed to refresh provider settings:', err);
    }
  };
  window.addEventListener('provider-settings-updated', organizerProviderSettingsUpdatedHandler);

  await restoreOrganizerSnapshot(cachedSnapshot, isStale);
}

export async function renderOrganizerResults(container) {
  const expectedRenderVersion = ++renderVersion;
  const isStale = () => expectedRenderVersion !== renderVersion || !container.isConnected;
  cleanupLegacyPersistedState();
  invalidateOrganizerRuntimeCacheIfNeeded();
  const cachedSnapshot = getPersisted(PERSIST_KEYS.lastSnapshot, null);
  restoreOrganizerLogState(cachedSnapshot);
  if (organizerProviderSettingsUpdatedHandler) {
    window.removeEventListener('provider-settings-updated', organizerProviderSettingsUpdatedHandler);
    organizerProviderSettingsUpdatedHandler = null;
  }

  container.innerHTML = `
    <div class="page-header animate-in">
      <h1 class="page-title">${t('organizer.results_title')}</h1>
      <p class="page-subtitle">${t('organizer.results_subtitle')}</p>
    </div>

    <section id="org-results-empty-card" class="card animate-in mb-24" style="animation-delay: 0.05s;">
      <div class="empty-state organizer-empty-state" style="padding: 36px;">
        <div class="organizer-empty-glyph" aria-hidden="true"></div>
        <div class="empty-state-text">${t('organizer.results_empty')}</div>
        <div class="empty-state-hint">${t('organizer.results_empty_hint')}</div>
        <button id="org-results-go-btn" class="btn btn-primary" type="button">${t('organizer.go_organize')}</button>
      </div>
    </section>

    <div id="org-results-stack" class="organizer-results-page-stack" hidden>
      <section class="card organizer-panel animate-in mb-24" style="animation-delay: 0.08s;">
        <div class="card-header organizer-panel-header">
          <div>
            <h2 class="card-title">${t('organizer.results_title')}</h2>
            <div class="form-hint">${t('organizer.results_subtitle')}</div>
          </div>
          <div class="organizer-results-toolbar">
            <span id="org-status" class="badge badge-info">${t('organizer.status_idle')}</span>
            <button id="org-apply-btn" class="btn btn-success" type="button" disabled>${t('organizer.apply_move')}</button>
            <button id="org-rollback-btn" class="btn btn-secondary" type="button">${t('organizer.rollback')}</button>
          </div>
        </div>
        <div class="organizer-result-progress">
          <span id="org-progress-pct" class="organizer-progress-pct organizer-progress-pct-inline">0.0%</span>
          <div class="progress-bar organizer-progress-bar">
            <div id="org-progress-fill" class="progress-fill" style="width:0%;"></div>
          </div>
        </div>
      </section>

      ${renderOrganizerStatsGrid('0.1s')}
      ${renderOrganizerPreviewPanel('0.14s', t('organizer.results_subtitle'))}
      ${renderOrganizerMoveResultPanel('0.18s')}
    </div>
  `;

  document.getElementById('org-results-go-btn')?.addEventListener('click', () => {
    window.location.hash = '#/organizer';
  });
  document.getElementById('org-apply-btn')?.addEventListener('click', handleApply);
  document.getElementById('org-rollback-btn')?.addEventListener('click', handleRollback);

  if (cachedSnapshot?.id) {
    ensureOrganizerLogsForSnapshot(cachedSnapshot);
    refreshView(cachedSnapshot);
  } else {
    refreshView(null);
  }

  await restoreOrganizerSnapshot(cachedSnapshot, isStale);
}







