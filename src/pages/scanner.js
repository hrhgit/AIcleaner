/**
 * src/pages/scanner.js
 * Scanner page with merged settings + scan controls.
 */
import {
  browseFolder,
  connectScanStream,
  getActiveScan,
  getProviderModels,
  getScanResult,
  getPrivilegeStatus,
  getSettings,
  requestElevation,
  saveSettings,
  startScan,
  stopScan,
} from '../utils/api.js';
import { handleElevationTransition } from '../utils/elevation.js';
import { formatSize } from '../utils/storage.js';
import * as storage from '../utils/storage.js';
import { showToast } from '../main.js';
import { getLang, t } from '../utils/i18n.js';
import { ensureRequiredCredentialsConfigured, getProviderCredentialPresence } from '../utils/secret-ui.js';

let activeTaskId = null;
let latestTaskId = null;
let activeEventSource = null;
let logEntries = [];
let nextLogEntryId = 0;
let currentSettings = null;
let renderVersion = 0;
const expandedDetailLogIds = new Set();
const SCANNER_FORM_DRAFT_KEY = 'wipeout.scanner.global.form.v1';
const SCANNER_FORM_DRAFT_VERSION = 1;
const SCAN_LOG_COLLAPSED_KEY = 'wipeout.scanner.log.collapsed.v1';
const SCAN_RECORD_GROUP_COLLAPSED_KEY = 'wipeout.scanner.record-group.collapsed.v1';
const SCAN_LOG_CACHE_KEY = 'wipeout.scanner.global.log.v1';
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
let scanProviderApiKeyMap = {};
const scanRemoteModelsCache = new Map();
let scanModelsRequestToken = 0;
let scannerProviderSettingsUpdatedHandler = null;

function clampNumber(value, min, max, fallback) {
  const n = Number(value);
  if (!Number.isFinite(n)) return fallback;
  if (n < min) return min;
  if (n > max) return max;
  return n;
}

function resolveSearchApi(settings) {
  const source = settings?.searchApi && typeof settings.searchApi === 'object'
    ? settings.searchApi
    : {};
  const scopes = source?.scopes && typeof source.scopes === 'object'
    ? source.scopes
    : {};

  const scanEnabled = typeof scopes.scan === 'boolean'
    ? scopes.scan
    : !!settings?.enableWebSearch;
  const classifyEnabled = typeof scopes.classify === 'boolean'
    ? scopes.classify
    : (typeof scopes.organizer === 'boolean'
      ? scopes.organizer
      : (settings?.enableWebSearchClassify != null
        ? !!settings.enableWebSearchClassify
        : (settings?.enableWebSearchOrganizer != null
          ? !!settings.enableWebSearchOrganizer
          : scanEnabled)));
  const enabled = typeof source.enabled === 'boolean'
    ? source.enabled
    : (scanEnabled || classifyEnabled);
  const apiKey = String(source.apiKey || settings?.tavilyApiKey || '').trim();

  return {
    enabled: !!enabled,
    apiKey,
    scopes: {
      scan: !!scanEnabled,
      classify: !!classifyEnabled,
      organizer: !!classifyEnabled,
    },
  };
}

function collectScannerForm() {
  const scanPath = String(document.getElementById('scan-path')?.value || '').trim();
  const targetSize = clampNumber(
    document.getElementById('target-size-input')?.value ?? document.getElementById('target-size')?.value,
    0.1,
    100,
    1
  );
  const maxDepth = Math.floor(clampNumber(
    document.getElementById('max-depth-input')?.value ?? document.getElementById('max-depth')?.value,
    1,
    10,
    5
  ));
  const scanWebSearchEnabled = !!document.getElementById('scan-enable-web-search')?.checked;
  const scanProviderEndpoint = String(document.getElementById('scan-provider')?.value || '').trim();
  const scanModel = String(document.getElementById('scan-model')?.value || '').trim();

  return {
    scanPath,
    targetSizeGB: targetSize,
    maxDepth,
    scanWebSearchEnabled,
    scanProviderEndpoint,
    scanModel,
  };
}

function normalizeScannerFormDraft(raw = {}) {
  return {
    scanPath: String(raw?.scanPath || '').trim(),
    targetSizeGB: clampNumber(raw?.targetSizeGB, 0.1, 100, 1),
    maxDepth: Math.floor(clampNumber(raw?.maxDepth, 1, 10, 5)),
    scanWebSearchEnabled: !!raw?.scanWebSearchEnabled,
    scanProviderEndpoint: String(raw?.scanProviderEndpoint || '').trim(),
    scanModel: String(raw?.scanModel || '').trim(),
  };
}

function extractScannerFormFromSettings(settings = {}) {
  const searchApi = resolveSearchApi(settings);
  const scanWebSearchEnabled = typeof settings?.scanWebSearchEnabled === 'boolean'
    ? settings.scanWebSearchEnabled
    : !!searchApi.scopes.scan;

  return normalizeScannerFormDraft({
    scanPath: settings?.scanPath,
    targetSizeGB: settings?.targetSizeGB,
    maxDepth: settings?.maxDepth,
    scanWebSearchEnabled,
    scanProviderEndpoint: settings?.apiEndpoint || settings?.defaultProviderEndpoint || '',
    scanModel: settings?.model || '',
  });
}

function readScannerFormDraft() {
  const raw = storage.get(SCANNER_FORM_DRAFT_KEY, null);
  if (!raw || typeof raw !== 'object') {
    return { dirty: false, data: null };
  }
  if (Number(raw.version) !== SCANNER_FORM_DRAFT_VERSION) {
    return { dirty: false, data: null };
  }
  if (!raw.data || typeof raw.data !== 'object') {
    return { dirty: false, data: null };
  }

  return {
    dirty: !!raw.dirty,
    data: normalizeScannerFormDraft(raw.data),
  };
}

function writeScannerFormDraft(form, { dirty = true } = {}) {
  storage.set(SCANNER_FORM_DRAFT_KEY, {
    version: SCANNER_FORM_DRAFT_VERSION,
    dirty: !!dirty,
    updatedAt: Date.now(),
    data: normalizeScannerFormDraft(form),
  });
}

function persistScannerFormDraft({ dirty = true } = {}) {
  writeScannerFormDraft(collectScannerForm(), { dirty });
}

function normalizePersistedLogEntries(raw) {
  if (!Array.isArray(raw)) return [];
  return raw
    .filter((entry) => entry && typeof entry === 'object')
    .map((entry) => ({
      id: Number.isFinite(Number(entry.id)) ? Number(entry.id) : undefined,
      kind: entry.kind === 'detail' ? 'detail' : 'simple',
      type: String(entry.type || 'scanning'),
      text: String(entry.text || ''),
      summary: String(entry.summary || ''),
      detailHtml: String(entry.detailHtml || ''),
      time: String(entry.time || ''),
    }))
    .slice(-200);
}

function readPersistedScanLog() {
  const raw = storage.get(SCAN_LOG_CACHE_KEY, null);
  if (!raw || typeof raw !== 'object') return { entries: [], nextId: 0 };
  const entries = normalizePersistedLogEntries(raw.entries);
  const maxId = entries.reduce((acc, entry) => Math.max(acc, Number.isFinite(entry.id) ? entry.id : -1), -1);
  return {
    entries,
    nextId: Math.max(Number(raw.nextId) || 0, maxId + 1),
  };
}

function persistScanLog() {
  storage.set(SCAN_LOG_CACHE_KEY, {
    entries: logEntries,
    nextId: nextLogEntryId,
    updatedAt: Date.now(),
  });
}

function isScanLogCollapsed() {
  return !!storage.get(SCAN_LOG_COLLAPSED_KEY, true);
}

function setScanLogCollapsed(collapsed) {
  storage.set(SCAN_LOG_COLLAPSED_KEY, !!collapsed);
}

function refreshScanLogPanel() {
  const collapsed = isScanLogCollapsed();
  const panel = document.getElementById('scan-log-panel');
  const toggleBtn = document.getElementById('toggle-log-btn');
  if (panel) {
    panel.classList.toggle('is-collapsed', collapsed);
  }
  if (toggleBtn) {
    toggleBtn.textContent = collapsed ? t('scanner.log_expand') : t('scanner.log_collapse');
    toggleBtn.setAttribute('aria-expanded', collapsed ? 'false' : 'true');
  }
}

function isScanRecordGroupCollapsed() {
  return !!storage.get(SCAN_RECORD_GROUP_COLLAPSED_KEY, true);
}

function setScanRecordGroupCollapsed(collapsed) {
  storage.set(SCAN_RECORD_GROUP_COLLAPSED_KEY, !!collapsed);
}

function setDetailLogExpanded(entryId, expanded) {
  if (entryId == null) return;
  if (expanded) {
    expandedDetailLogIds.add(entryId);
  } else {
    expandedDetailLogIds.delete(entryId);
  }
}

function isLogPinnedToBottom(log) {
  if (!log) return true;
  return (log.scrollHeight - log.scrollTop - log.clientHeight) <= 24;
}

function isGroupedScanLogType(type) {
  return ['scanning', 'analyzing', 'found'].includes(type);
}

function getLogIcon(type) {
  if (type === 'found') return '+';
  if (type === 'analyzing') return '*';
  if (type === 'agent_call') return '>';
  if (type === 'agent_response') return '<';
  return '-';
}

function createSimpleLogEntryElement(entry) {
  const el = document.createElement('div');
  el.className = `scan-log-entry ${entry.type}`;
  el.innerHTML = `
    <span class="log-icon">${getLogIcon(entry.type)}</span>
    <span style="color: var(--text-muted); margin-right: 6px;">[${entry.time}]</span>
    <span>${entry.text}</span>
  `;
  return el;
}

function createDetailLogEntryElement(entry) {
  const expanded = expandedDetailLogIds.has(entry.id);
  const wrapper = document.createElement('div');
  wrapper.className = `scan-log-entry ${entry.type}`;
  wrapper.innerHTML = `
    <span class="log-icon">${getLogIcon(entry.type)}</span>
    <div style="flex: 1; min-width: 0;">
      <div class="log-detail-header" style="cursor: pointer; user-select: none; display: flex; align-items: center; gap: 6px;">
        <span style="color: var(--text-muted); margin-right: 4px;">[${entry.time}]</span>
        <span class="log-detail-arrow" style="transition: transform 0.2s; display: inline-block; font-size: 0.65rem; transform: ${expanded ? 'rotate(90deg)' : 'rotate(0deg)'};">></span>
        <span>${entry.summary}</span>
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
    setDetailLogExpanded(entry.id, nextOpen);
    body.style.display = nextOpen ? 'block' : 'none';
    arrow.style.transform = nextOpen ? 'rotate(90deg)' : 'rotate(0deg)';
  });

  return wrapper;
}

function createScanRecordGroupElement(entries) {
  const wrapper = document.createElement('div');
  wrapper.className = `scan-log-group${isScanRecordGroupCollapsed() ? ' is-collapsed' : ''}`;

  const header = document.createElement('div');
  header.className = 'scan-log-group-header';
  header.innerHTML = `
    <div class="scan-log-group-title">
      <span class="scan-log-group-arrow">></span>
      <span>${t('scanner.scan_records')} (${entries.length})</span>
    </div>
    <span>${isScanRecordGroupCollapsed() ? t('scanner.log_expand') : t('scanner.log_collapse')}</span>
  `;

  const body = document.createElement('div');
  body.className = 'scan-log-group-body';
  for (const entry of entries) {
    body.appendChild(createSimpleLogEntryElement(entry));
  }

  header.addEventListener('click', () => {
    const nextCollapsed = !wrapper.classList.contains('is-collapsed');
    setScanRecordGroupCollapsed(nextCollapsed);
    wrapper.classList.toggle('is-collapsed', nextCollapsed);
    const label = header.lastElementChild;
    if (label) {
      label.textContent = nextCollapsed ? t('scanner.log_expand') : t('scanner.log_collapse');
    }
  });

  wrapper.appendChild(header);
  wrapper.appendChild(body);
  return wrapper;
}

function trimLogEntries() {
  let trimmed = false;
  while (logEntries.length > 200) {
    trimmed = true;
    const removed = logEntries.shift();
    if (removed?.kind === 'detail') {
      expandedDetailLogIds.delete(removed.id);
    }
  }
  if (trimmed) {
    persistScanLog();
  }
  return trimmed;
}

function applyLogScrollPosition(log, { shouldStickToBottom, insertMode, previousScrollTop, previousScrollHeight }) {
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

function updateScanRecordGroupHeader(wrapper, count) {
  if (!wrapper) return;
  const titleText = wrapper.querySelector('.scan-log-group-title span:last-child');
  if (titleText) {
    titleText.textContent = `${t('scanner.scan_records')} (${count})`;
  }
  const label = wrapper.querySelector('.scan-log-group-header > span:last-child');
  if (label) {
    label.textContent = wrapper.classList.contains('is-collapsed')
      ? t('scanner.log_expand')
      : t('scanner.log_collapse');
  }
}

function ensureScanRecordGroup(log) {
  if (!log) return null;
  let group = log.querySelector('.scan-log-group');
  if (group) return group;

  group = createScanRecordGroupElement([]);
  log.prepend(group);
  return group;
}

function appendLogEntry(entry) {
  const log = document.getElementById('scan-log');
  if (!log) return;
  const shouldStickToBottom = isLogPinnedToBottom(log);
  const previousScrollTop = log.scrollTop;
  const previousScrollHeight = log.scrollHeight;
  const insertMode = isGroupedScanLogType(entry.type) ? 'top' : 'bottom';

  if (isGroupedScanLogType(entry.type)) {
    const group = ensureScanRecordGroup(log);
    const body = group?.querySelector('.scan-log-group-body');
    if (body) {
      body.appendChild(createSimpleLogEntryElement(entry));
    }
    updateScanRecordGroupHeader(group, logEntries.filter((item) => isGroupedScanLogType(item.type)).length);
  } else if (entry.kind === 'detail') {
    log.appendChild(createDetailLogEntryElement(entry));
  } else {
    log.appendChild(createSimpleLogEntryElement(entry));
  }

  applyLogScrollPosition(log, { shouldStickToBottom, insertMode, previousScrollTop, previousScrollHeight });
}

function renderLogEntries(insertMode = 'reset') {
  const log = document.getElementById('scan-log');
  if (!log) return;
  const shouldStickToBottom = isLogPinnedToBottom(log);
  const previousScrollTop = log.scrollTop;
  const previousScrollHeight = log.scrollHeight;

  log.innerHTML = '';
  const groupedEntries = logEntries.filter((entry) => isGroupedScanLogType(entry.type));
  const detailEntries = logEntries.filter((entry) => !isGroupedScanLogType(entry.type));

  if (groupedEntries.length > 0) {
    log.appendChild(createScanRecordGroupElement(groupedEntries));
  }

  for (const entry of detailEntries) {
    if (entry.kind === 'detail') {
      log.appendChild(createDetailLogEntryElement(entry));
    } else {
      log.appendChild(createSimpleLogEntryElement(entry));
    }
  }

  applyLogScrollPosition(log, { shouldStickToBottom, insertMode, previousScrollTop, previousScrollHeight });
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

function ensureSelectOptionExists(select, value, label = null) {
  if (!select || !value) return;
  const exists = Array.from(select.options).some((opt) => opt.value === value);
  if (!exists) {
    select.add(new Option(String(label || value), String(value)));
  }
}

function getProviderLabel(endpoint) {
  return PROVIDER_OPTIONS.find((item) => item.value === endpoint)?.label || endpoint;
}

function getApiKeyForScanEndpoint(endpoint) {
  return !!scanProviderApiKeyMap?.[String(endpoint || '').trim()];
}

function syncScanProviderApiKeyMap(settings = {}) {
  scanProviderApiKeyMap = {};
  if (settings?.providerConfigs && typeof settings.providerConfigs === 'object') {
    for (const [endpoint] of Object.entries(settings.providerConfigs)) {
      scanProviderApiKeyMap[String(endpoint).trim()] = getProviderCredentialPresence(settings, endpoint);
    }
  }
  if (settings?.apiEndpoint && !scanProviderApiKeyMap[settings.apiEndpoint]) {
    scanProviderApiKeyMap[settings.apiEndpoint] = getProviderCredentialPresence(settings, settings.apiEndpoint);
  }
}

function refreshScanApiConfigHint(endpoint = document.getElementById('scan-provider')?.value) {
  const hintEl = document.getElementById('scan-api-config-hint');
  if (!hintEl) return;
  const isHidden = !!getApiKeyForScanEndpoint(endpoint);
  hintEl.hidden = isHidden;
  hintEl.style.display = isHidden ? 'none' : 'flex';
}

function renderScanModelOptions(select, models, selectedValue) {
  if (!select) return;
  select.innerHTML = '';
  for (const model of models) {
    select.add(new Option(String(model.label || model.value), String(model.value)));
  }
  const selected = String(selectedValue || '').trim();
  if (selected) {
    ensureSelectOptionExists(select, selected, selected);
    select.value = selected;
  } else if (select.options.length > 0) {
    select.value = select.options[0].value;
  }
}

async function initScanModelSelectors(defaultSelection = {}) {
  const providerSelect = document.getElementById('scan-provider');
  const modelSelect = document.getElementById('scan-model');
  if (!providerSelect || !modelSelect) return;

  const endpoint = String(defaultSelection?.endpoint || providerSelect.value || '').trim();
  if (endpoint) {
    ensureSelectOptionExists(providerSelect, endpoint, getProviderLabel(endpoint));
    providerSelect.value = endpoint;
  }

  const selectedModel = String(defaultSelection?.model || modelSelect.value || '').trim();
  const requestToken = ++scanModelsRequestToken;
  modelSelect.disabled = true;
  modelSelect.innerHTML = `<option value="">${t('organizer.model_loading')}</option>`;

  const cacheKey = endpoint;
  let models = [];
  try {
    if (endpoint && getApiKeyForScanEndpoint(endpoint)) {
      if (scanRemoteModelsCache.has(cacheKey)) {
        models = scanRemoteModelsCache.get(cacheKey);
      } else {
        const resp = await getProviderModels(endpoint);
        models = normalizeRemoteModels(resp?.models || []);
        scanRemoteModelsCache.set(cacheKey, models);
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

  if (requestToken !== scanModelsRequestToken) return;
  renderScanModelOptions(modelSelect, models, selectedModel || models[0]?.value);
  modelSelect.disabled = false;
}

async function initScanProviderFields(settings = {}) {
  const providerSelect = document.getElementById('scan-provider');
  const modelSelect = document.getElementById('scan-model');
  if (!providerSelect || !modelSelect) return;

  syncScanProviderApiKeyMap(settings);

  providerSelect.innerHTML = '';
  for (const item of PROVIDER_OPTIONS) {
    providerSelect.add(new Option(item.label, item.value));
  }

  const form = extractScannerFormFromSettings(settings);
  const endpoint = String(form.scanProviderEndpoint || settings?.apiEndpoint || 'https://api.deepseek.com').trim();
  const model = String(form.scanModel || settings?.model || 'deepseek-chat').trim();
  ensureSelectOptionExists(providerSelect, endpoint, getProviderLabel(endpoint));
  providerSelect.value = endpoint;
  await initScanModelSelectors({ endpoint, model });
  refreshScanApiConfigHint(endpoint);
}

function mergeSettingsWithDraft(settings = {}, draftState = { dirty: false, data: null }) {
  if (!draftState?.dirty || !draftState?.data) {
    return settings;
  }

  const remoteSearch = resolveSearchApi(settings);
  const draft = normalizeScannerFormDraft(draftState.data);
  const classifyEnabled = !!remoteSearch.scopes.classify;
  const mergedSearchApi = {
    provider: 'tavily',
    enabled: !!(draft.scanWebSearchEnabled || classifyEnabled),
    scopes: {
      scan: !!draft.scanWebSearchEnabled,
      classify: classifyEnabled,
      organizer: classifyEnabled,
    },
  };

  return {
    ...settings,
    scanPath: draft.scanPath,
    targetSizeGB: draft.targetSizeGB,
    maxDepth: draft.maxDepth,
    apiEndpoint: draft.scanProviderEndpoint || settings?.apiEndpoint,
    defaultProviderEndpoint: draft.scanProviderEndpoint || settings?.defaultProviderEndpoint,
    model: draft.scanModel || settings?.model,
    enableWebSearch: !!draft.scanWebSearchEnabled,
    enableWebSearchClassify: classifyEnabled,
    enableWebSearchOrganizer: classifyEnabled,
    searchApi: mergedSearchApi,
  };
}

function applySettingsToForm(settings = {}) {
  const form = extractScannerFormFromSettings(settings);

  const scanPathInput = document.getElementById('scan-path');
  const targetSizeSlider = document.getElementById('target-size');
  const targetSizeInput = document.getElementById('target-size-input');
  const maxDepthSlider = document.getElementById('max-depth');
  const maxDepthInput = document.getElementById('max-depth-input');
  const scanToggle = document.getElementById('scan-enable-web-search');
  const providerSelect = document.getElementById('scan-provider');
  const modelSelect = document.getElementById('scan-model');

  if (scanPathInput) scanPathInput.value = form.scanPath;
  if (targetSizeSlider) targetSizeSlider.value = String(form.targetSizeGB);
  if (targetSizeInput) targetSizeInput.value = form.targetSizeGB.toFixed(1);
  if (maxDepthSlider) maxDepthSlider.value = String(form.maxDepth);
  if (maxDepthInput) maxDepthInput.value = String(form.maxDepth);
  if (scanToggle) scanToggle.checked = !!form.scanWebSearchEnabled;
  if (providerSelect) {
    ensureSelectOptionExists(providerSelect, form.scanProviderEndpoint, getProviderLabel(form.scanProviderEndpoint));
    if (form.scanProviderEndpoint) providerSelect.value = form.scanProviderEndpoint;
  }
  if (modelSelect) {
    ensureSelectOptionExists(modelSelect, form.scanModel, form.scanModel);
    if (form.scanModel) modelSelect.value = form.scanModel;
  }

  updatePathDisplay(form.scanPath);
  refreshScanApiConfigHint(form.scanProviderEndpoint);
}

function updatePathDisplay(pathValue) {
  const pathDisplay = document.getElementById('scan-path-display');
  if (!pathDisplay) return;
  const path = String(pathValue || '').trim();
  if (path) {
    pathDisplay.textContent = `${t('settings.scan_path')}: ${path}`;
  } else {
    pathDisplay.textContent = t('scanner.path_not_configured');
  }
}

function setSaveStatus(text, colorToken = '--text-muted') {
  const statusEl = document.getElementById('scanner-save-status');
  if (!statusEl) return;
  statusEl.textContent = text;
  statusEl.style.color = `var(${colorToken})`;
}

async function persistScannerSettings({ showSuccessToast = true } = {}) {
  const saveBtn = document.getElementById('scanner-save-btn');
  const originalText = saveBtn?.textContent || t('settings.save');

  if (saveBtn) {
    saveBtn.disabled = true;
    saveBtn.innerHTML = `<span class="spinner"></span> ${t('settings.saving')}`;
  }
  setSaveStatus(t('settings.saving'));

  try {
    if (!currentSettings) {
      currentSettings = await getSettings();
    }

    const form = collectScannerForm();
    writeScannerFormDraft(form, { dirty: true });
    const existingSearchApi = resolveSearchApi(currentSettings);
    const classifyEnabled = !!existingSearchApi.scopes.classify;
    const selectedEndpoint = String(
      form.scanProviderEndpoint
      || currentSettings?.defaultProviderEndpoint
      || currentSettings?.apiEndpoint
      || 'https://api.deepseek.com'
    ).trim();
    const selectedModel = String(form.scanModel || currentSettings?.model || 'deepseek-chat').trim();
    const providerConfigs = currentSettings?.providerConfigs && typeof currentSettings.providerConfigs === 'object'
      ? { ...currentSettings.providerConfigs }
      : {};
    const selectedConfig = providerConfigs[selectedEndpoint] && typeof providerConfigs[selectedEndpoint] === 'object'
      ? { ...providerConfigs[selectedEndpoint] }
      : {};
    providerConfigs[selectedEndpoint] = {
      ...selectedConfig,
      endpoint: selectedEndpoint,
      name: String(selectedConfig.name || getProviderLabel(selectedEndpoint)),
      model: selectedModel,
    };

    const searchApi = {
      provider: 'tavily',
      enabled: !!(form.scanWebSearchEnabled || classifyEnabled),
      scopes: {
        scan: !!form.scanWebSearchEnabled,
        classify: classifyEnabled,
        organizer: classifyEnabled,
      },
    };

    const payload = {
      scanPath: form.scanPath,
      targetSizeGB: form.targetSizeGB,
      maxDepth: form.maxDepth,
      enableWebSearch: !!form.scanWebSearchEnabled,
      enableWebSearchClassify: classifyEnabled,
      enableWebSearchOrganizer: classifyEnabled,
      providerConfigs,
      defaultProviderEndpoint: selectedEndpoint,
      apiEndpoint: selectedEndpoint,
      model: selectedModel,
      searchApi,
    };

    const result = await saveSettings(payload);
    currentSettings = result?.settings || await getSettings();
    applySettingsToForm(currentSettings);
    await initScanProviderFields(currentSettings);
    writeScannerFormDraft(extractScannerFormFromSettings(currentSettings), { dirty: false });

    setSaveStatus(t('settings.saved'), '--accent-success');
    if (showSuccessToast) {
      showToast(t('settings.toast_saved'), 'success');
    }
    return currentSettings;
  } catch (err) {
    setSaveStatus(t('settings.save_failed'), '--accent-danger');
    showToast(t('settings.toast_save_failed') + err.message, 'error');
    throw err;
  } finally {
    if (saveBtn) {
      saveBtn.disabled = false;
      saveBtn.textContent = originalText;
    }
  }
}

function setScanStatus(status) {
  const statusLabel = getScanStatusLabel(status);
  const statusEl = document.getElementById('stat-status');
  if (!statusEl) return;
  statusEl.textContent = statusLabel;
}

function getScanStatusLabel(status) {
  const statusMap = {
    scanning: t('scanner.scanning'),
    analyzing: t('scanner.analyzing'),
    idle: t('scanner.not_set'),
    done: t('scanner.done'),
    stopped: t('scanner.stopped'),
    error: t('toast.error'),
  };
  return statusMap[status] || status || t('scanner.not_set');
}

async function refreshPrivilegeStatus() {
  const adminStatusEl = document.getElementById('admin-status');
  const elevationBtn = document.getElementById('request-elevation-btn');
  if (!adminStatusEl || !elevationBtn) return;

  adminStatusEl.textContent = t('settings.privilege_checking');
  adminStatusEl.style.color = 'var(--text-muted)';
  elevationBtn.disabled = true;

  try {
    const data = await getPrivilegeStatus();
    if (data.platform !== 'win32') {
      adminStatusEl.textContent = t('settings.admin_status_unsupported');
      adminStatusEl.style.color = 'var(--accent-warning)';
      elevationBtn.textContent = t('settings.request_elevation');
      return;
    }

    if (data.isAdmin) {
      adminStatusEl.textContent = t('settings.admin_status_on');
      adminStatusEl.style.color = 'var(--accent-success)';
      elevationBtn.textContent = t('settings.admin_already');
      return;
    }

    adminStatusEl.textContent = t('settings.admin_status_off');
    adminStatusEl.style.color = 'var(--accent-warning)';
    elevationBtn.disabled = false;
    elevationBtn.textContent = t('settings.request_elevation');
  } catch (err) {
    adminStatusEl.textContent = t('settings.privilege_check_failed') + err.message;
    adminStatusEl.style.color = 'var(--accent-danger)';
    elevationBtn.textContent = t('settings.request_elevation');
  }
}

function bindSettingsEvents() {
  const sizeSlider = document.getElementById('target-size');
  const sizeInput = document.getElementById('target-size-input');
  const depthSlider = document.getElementById('max-depth');
  const depthInput = document.getElementById('max-depth-input');
  const handleFormMutation = () => {
    persistScannerFormDraft();
  };

  sizeSlider?.addEventListener('input', () => {
    if (sizeInput) sizeInput.value = clampNumber(sizeSlider.value, 0.1, 100, 1).toFixed(1);
    handleFormMutation();
  });
  sizeInput?.addEventListener('input', () => {
    const val = clampNumber(sizeInput.value, 0.1, 100, 1);
    if (sizeSlider) sizeSlider.value = String(val);
    handleFormMutation();
  });
  sizeInput?.addEventListener('blur', () => {
    const val = clampNumber(sizeInput.value, 0.1, 100, 1);
    sizeInput.value = val.toFixed(1);
    if (sizeSlider) sizeSlider.value = String(val);
    handleFormMutation();
  });

  depthSlider?.addEventListener('input', () => {
    if (depthInput) depthInput.value = String(Math.floor(clampNumber(depthSlider.value, 1, 10, 5)));
    handleFormMutation();
  });
  depthInput?.addEventListener('input', () => {
    const val = Math.floor(clampNumber(depthInput.value, 1, 10, 5));
    if (depthSlider) depthSlider.value = String(val);
    handleFormMutation();
  });
  depthInput?.addEventListener('blur', () => {
    const val = Math.floor(clampNumber(depthInput.value, 1, 10, 5));
    depthInput.value = String(val);
    if (depthSlider) depthSlider.value = String(val);
    handleFormMutation();
  });

  document.getElementById('scan-path')?.addEventListener('input', (event) => {
    updatePathDisplay(event.target?.value || '');
    handleFormMutation();
  });

  document.getElementById('scan-enable-web-search')?.addEventListener('change', () => {
    handleFormMutation();
  });
  document.getElementById('scan-provider')?.addEventListener('change', async () => {
    await initScanModelSelectors();
    refreshScanApiConfigHint();
    persistScannerFormDraft();
  });
  document.getElementById('scan-model')?.addEventListener('change', () => {
    handleFormMutation();
  });

  document.getElementById('browse-folder-btn')?.addEventListener('click', async () => {
    const btn = document.getElementById('browse-folder-btn');
    if (btn) {
      btn.disabled = true;
      btn.textContent = t('settings.browsing');
    }
    try {
      const result = await browseFolder();
      if (!result.cancelled && result.path) {
        const input = document.getElementById('scan-path');
        if (input) input.value = result.path;
        updatePathDisplay(result.path);
        handleFormMutation();
        showToast(t('settings.toast_path_selected') + result.path, 'success');
      }
    } catch (err) {
      showToast(t('settings.toast_browse_failed') + err.message, 'error');
    } finally {
      if (btn) {
        btn.disabled = false;
        btn.textContent = t('settings.browse');
      }
    }
  });

  document.getElementById('scanner-save-btn')?.addEventListener('click', async () => {
    try {
      await persistScannerSettings({ showSuccessToast: true });
    } catch {
      // already handled in persistScannerSettings
    }
  });

  document.getElementById('request-elevation-btn')?.addEventListener('click', async () => {
    if (!confirm(t('settings.elevation_confirm'))) return;

    const elevationBtn = document.getElementById('request-elevation-btn');
    const adminStatusEl = document.getElementById('admin-status');
    if (elevationBtn) {
      elevationBtn.disabled = true;
      elevationBtn.innerHTML = `<span class="spinner"></span> ${t('settings.requesting_elevation')}`;
    }

    try {
      const result = await requestElevation();
      showToast(t('settings.elevation_uac_prompt'), 'info');
      if (adminStatusEl) {
        adminStatusEl.textContent = t('settings.elevation_restarting');
        adminStatusEl.style.color = 'var(--accent-info)';
      }
      if (result?.restarting) {
        handleElevationTransition({ showToast, t });
      }
    } catch (err) {
      showToast(t('settings.elevation_failed') + err.message, 'error');
      await refreshPrivilegeStatus();
    }
  });
}

function applySnapshotToUI(data) {
  if (!data) return;

  updateStats(data);
  setScanStatus(data.status);
  storage.set('lastScan', data);

  const pathEl = document.getElementById('breadcrumb-path');
  const depthEl = document.getElementById('breadcrumb-depth');
  const fill = document.getElementById('progress-fill');
  const label = document.getElementById('progress-pct');
  const activityEl = document.getElementById('scan-activity-hidden');
  const emptyEl = document.getElementById('scan-empty');
  const breadcrumb = document.getElementById('scan-breadcrumb');

  if (activityEl) activityEl.style.display = '';
  if (emptyEl) emptyEl.style.display = 'none';
  if (breadcrumb) breadcrumb.style.display = '';
  if (pathEl && data.currentPath) pathEl.textContent = data.currentPath;
  if (depthEl) depthEl.textContent = `Depth ${data.currentDepth || 0}`;

  const scanPct = data.totalEntries > 0
    ? Math.min(100, (data.processedEntries || 0) / data.totalEntries * 100)
    : (data.status === 'done' ? 100 : 0);
  if (fill) fill.style.width = `${scanPct}%`;
  if (label) label.textContent = `${scanPct.toFixed(1)}%`;
}

export async function renderScanner(container) {
  const expectedRenderVersion = ++renderVersion;
  const isStale = () => expectedRenderVersion !== renderVersion || !container.isConnected;
  const cachedLastScan = storage.get('lastScan', null);
  const draftState = readScannerFormDraft();
  const persistedLog = readPersistedScanLog();
  if (!logEntries.length && persistedLog.entries.length) {
    logEntries = persistedLog.entries;
    nextLogEntryId = persistedLog.nextId;
  }
  latestTaskId = storage.get('lastScanTaskId', null) || cachedLastScan?.id || null;

  container.innerHTML = `
    <style>
      #target-size-input::-webkit-inner-spin-button,
      #target-size-input::-webkit-outer-spin-button,
      #max-depth-input::-webkit-inner-spin-button,
      #max-depth-input::-webkit-outer-spin-button,
      .no-spin::-webkit-inner-spin-button,
      .no-spin::-webkit-outer-spin-button {
        -webkit-appearance: none !important;
        appearance: none !important;
        margin: 0 !important;
      }
      #target-size-input, #max-depth-input, .no-spin {
        -moz-appearance: textfield !important;
        appearance: textfield !important;
      }
    </style>

    <div class="page-header animate-in">
      <h1 class="page-title">${t('scanner.title')}</h1>
      <p class="page-subtitle">${t('scanner.subtitle')}</p>
    </div>

    <div class="card animate-in mb-24" style="animation-delay: 0.05s">
      <div class="card-header">
        <h2 class="card-title">${t('settings.scan_config')}</h2>
        <span class="badge badge-secondary">${t('settings.scan_params')}</span>
      </div>

      <div class="form-group">
        <label class="form-label">${t('settings.scan_path')}</label>
        <div style="display:flex; gap:8px; align-items:center;">
          <input type="text" id="scan-path" class="form-input" style="flex: 1;" placeholder="C:\\Users\\YourName\\Downloads" />
          <button type="button" id="browse-folder-btn" class="btn btn-secondary" style="white-space: nowrap; flex-shrink: 0;">
            ${t('settings.browse')}
          </button>
        </div>
        <div class="form-hint">${t('settings.browse_hint')}</div>
      </div>

      <div class="form-group">
        <label class="form-label">${t('settings.target_size')}</label>
        <div class="range-container">
          <input type="range" id="target-size" class="range-slider" min="0.1" max="100" step="0.1" value="1" />
          <div style="display:flex; align-items:center; gap:8px;">
            <input type="number" id="target-size-input" class="form-input no-spin" style="width:80px; height:32px; padding:4px 8px; text-align:center;" min="0.1" max="100" step="0.1" value="1" />
            <span class="range-value" style="min-width: unset;">GB</span>
          </div>
        </div>
        <div class="form-hint">${t('settings.target_size_hint')}</div>
      </div>

      <div class="form-group">
        <label class="form-label">${t('settings.max_depth')}</label>
        <div class="range-container">
          <input type="range" id="max-depth" class="range-slider" min="1" max="10" step="1" value="5" />
          <div style="display:flex; align-items:center; gap:8px;">
            <input type="number" id="max-depth-input" class="form-input no-spin" style="width:80px; height:32px; padding:4px 8px; text-align:center;" min="1" max="10" step="1" value="5" />
            <span class="range-value" style="min-width: unset;">${t('settings.depth_unit')}</span>
          </div>
        </div>
        <div class="form-hint">${t('settings.max_depth_hint')}</div>
      </div>

      <div class="form-group">
        <label class="form-label">${t('settings.model')}</label>
        <div class="provider-model-inline">
          <select id="scan-provider" class="form-input"></select>
          <select id="scan-model" class="form-input"></select>
        </div>
        <div class="form-hint">${t('settings.provider')} + ${t('settings.model')}</div>
        <div id="scan-api-config-hint" class="form-hint api-config-hint">${t('settings.api_key_managed_hint')}</div>
      </div>

      <div class="form-group" style="margin-bottom: 0;">
        <label style="display:flex; align-items:center; gap:10px; cursor:pointer;">
          <input type="checkbox" id="scan-enable-web-search" class="toggle-checkbox" style="width: 20px; height: 20px;" />
          <span class="form-label" style="margin:0;">${t('settings.enable_search_scan')}</span>
        </label>
        <div class="form-hint">${t('settings.search_hint')}</div>
      </div>

      <div class="flex items-center justify-between" style="margin-top: 16px;">
        <span id="scanner-save-status" class="form-hint"></span>
        <button id="scanner-save-btn" class="btn btn-primary">${t('settings.save')}</button>
      </div>
    </div>

    <div class="card animate-in mb-24" style="animation-delay: 0.08s">
      <div class="card-header">
        <h2 class="card-title">${t('settings.privilege_config')}</h2>
        <span class="badge badge-warning">${t('settings.privilege_required')}</span>
      </div>
      <div class="form-group" style="display:flex; align-items:center; justify-content:space-between; gap:12px; margin-bottom:10px;">
        <span id="admin-status" class="form-hint" style="margin-top:0;"></span>
        <button type="button" id="request-elevation-btn" class="btn btn-secondary">${t('settings.request_elevation')}</button>
      </div>
      <div class="form-hint">${t('settings.privilege_hint')}</div>
    </div>

    <div class="stats-grid animate-in" style="animation-delay: 0.1s">
      <div class="stat-card">
        <span class="stat-label">${t('scanner.progress_scan')}</span>
        <span class="stat-value accent" id="stat-status">${t('scanner.not_set')}</span>
      </div>
      <div class="stat-card">
        <span class="stat-label">${t('results.files_count')}</span>
        <span class="stat-value" id="stat-scanned">0</span>
      </div>
      <div class="stat-card">
        <span class="stat-label">${t('results.safe_to_clean')}</span>
        <span class="stat-value success" id="stat-cleanable">0 B</span>
      </div>
      <div class="stat-card">
        <span class="stat-label">Token</span>
        <span class="stat-value warning" id="stat-tokens">0</span>
      </div>
    </div>

    <div class="card animate-in mb-24" style="animation-delay: 0.15s">
      <div class="card-header">
        <h2 class="card-title">${t('scanner.progress_scan')}</h2>
        <div style="display:flex; gap:8px; align-items:center;">
          <span id="progress-pct" class="badge badge-info">0.0%</span>
        </div>
      </div>
      <div class="progress-bar mb-16">
        <div class="progress-fill" id="progress-fill" style="width:0%"></div>
      </div>
      <div id="scan-breadcrumb" class="scan-breadcrumb" style="display:none;">
        <span class="log-icon">...</span>
        <span id="breadcrumb-path" class="crumb">...</span>
        <span class="separator">|</span>
        <span id="breadcrumb-depth">${t('scanner.not_set')}</span>
      </div>
    </div>

    <div class="card animate-in mb-24" style="animation-delay: 0.2s">
      <div class="card-header">
        <h2 class="card-title">${t('scanner.activity_log')}</h2>
        <div style="display:flex; gap:8px; align-items:center;">
          <button id="toggle-log-btn" class="btn btn-ghost" style="padding: 6px 12px; font-size: 0.75rem;">${t('scanner.log_expand')}</button>
          <button id="clear-log-btn" class="btn btn-ghost" style="padding: 6px 12px; font-size: 0.75rem;">Clear</button>
        </div>
      </div>
      <div id="scan-log-panel" class="scan-log-panel">
        <div class="scan-activity" id="scan-activity-hidden" style="display:none;">
          <div class="scan-log" id="scan-log"></div>
        </div>
        <div id="scan-empty" class="empty-state" style="padding:30px;">
          <div class="empty-state-icon">...</div>
          <div class="empty-state-text">${t('scanner.prepare')}</div>
          <div class="empty-state-hint">${t('scanner.not_set')}</div>
        </div>
      </div>
    </div>

    <div class="flex items-center justify-between animate-in" style="animation-delay: 0.25s">
      <div><span id="scan-path-display" class="form-hint mono"></span></div>
      <div class="flex gap-16">
        <button id="stop-btn" class="btn btn-danger" style="display: none;">${t('scanner.stop')}</button>
        <button id="start-btn" class="btn btn-primary btn-lg">${t('scanner.start')}</button>
      </div>
    </div>
  `;

  bindSettingsEvents();
  if (draftState.data) {
    applySettingsToForm(draftState.data);
    await initScanProviderFields(draftState.data);
    if (isStale()) return;
  }
  await refreshPrivilegeStatus();
  if (isStale()) return;

  try {
    const remoteSettings = await getSettings();
    if (isStale()) return;
    currentSettings = mergeSettingsWithDraft(remoteSettings, draftState);
    applySettingsToForm(currentSettings);
    await initScanProviderFields(currentSettings);
    if (isStale()) return;
    if (!draftState.dirty) {
      writeScannerFormDraft(extractScannerFormFromSettings(currentSettings), { dirty: false });
    }
  } catch (err) {
    currentSettings = null;
    console.warn('Failed to load scanner settings:', err);
    if (!draftState.data) {
      updatePathDisplay('');
    }
  }

  if (scannerProviderSettingsUpdatedHandler) {
    window.removeEventListener('provider-settings-updated', scannerProviderSettingsUpdatedHandler);
  }
  scannerProviderSettingsUpdatedHandler = async (event) => {
    try {
      currentSettings = event?.detail && typeof event.detail === 'object'
        ? event.detail
        : await getSettings();
      syncScanProviderApiKeyMap(currentSettings);
      const selectedEndpoint = String(document.getElementById('scan-provider')?.value || '').trim();
      const selectedModel = String(document.getElementById('scan-model')?.value || '').trim();
      await initScanModelSelectors({ endpoint: selectedEndpoint, model: selectedModel });
      refreshScanApiConfigHint(selectedEndpoint);
    } catch (err) {
      console.warn('Failed to refresh scanner provider settings:', err);
    }
  };
  window.addEventListener('provider-settings-updated', scannerProviderSettingsUpdatedHandler);

  document.getElementById('start-btn')?.addEventListener('click', handleStart);
  document.getElementById('stop-btn')?.addEventListener('click', handleStop);
  document.getElementById('toggle-log-btn')?.addEventListener('click', () => {
    setScanLogCollapsed(!isScanLogCollapsed());
    refreshScanLogPanel();
  });
  document.getElementById('clear-log-btn')?.addEventListener('click', () => {
    logEntries = [];
    nextLogEntryId = 0;
    expandedDetailLogIds.clear();
    persistScanLog();
    const logEl = document.getElementById('scan-log');
    if (logEl) logEl.innerHTML = '';
  });
  refreshScanLogPanel();
  if (logEntries.length > 0) {
    renderLogEntries();
  }

  if (activeTaskId) {
    restoreActiveState();
    return;
  }

  try {
    const activeTasks = await getActiveScan();
    if (isStale()) return;
    if (activeTasks.length > 0) {
      const task = activeTasks[0];
      activeTaskId = task.taskId;
      latestTaskId = task.taskId;
      storage.set('lastScanTaskId', latestTaskId);
      applySnapshotToUI(task);
      restoreActiveState();
      return;
    }
  } catch {
    // ignore
  }

  if (!latestTaskId) return;

  try {
    const snapshot = await getScanResult(latestTaskId);
    if (isStale()) return;
    storage.set('lastScanTaskId', snapshot.id);
    applySnapshotToUI(snapshot);
    if (['idle', 'scanning', 'analyzing'].includes(snapshot.status)) {
      activeTaskId = snapshot.id;
      restoreActiveState();
      return;
    }
    resetButtons();
  } catch (err) {
    console.warn('Failed to restore latest scan snapshot:', err);
    if (isStale()) return;
    if (cachedLastScan?.id) {
      latestTaskId = cachedLastScan.id;
      applySnapshotToUI(cachedLastScan);
      resetButtons();
      return;
    }
    storage.remove('lastScanTaskId');
    storage.remove('lastScan');
  }
}

function restoreActiveState() {
  const startBtn = document.getElementById('start-btn');
  const stopBtn = document.getElementById('stop-btn');
  const activityEl = document.getElementById('scan-activity-hidden');
  const emptyEl = document.getElementById('scan-empty');
  const breadcrumb = document.getElementById('scan-breadcrumb');

  if (startBtn) startBtn.style.display = 'none';
  if (stopBtn) stopBtn.style.display = '';
  if (activityEl) activityEl.style.display = '';
  if (emptyEl) emptyEl.style.display = 'none';
  if (breadcrumb) breadcrumb.style.display = '';

  const log = document.getElementById('scan-log');
  if (log && logEntries.length > 0) {
    renderLogEntries();
  }

  const lastScan = storage.get('lastScan', null);
  if (lastScan) {
    updateStats(lastScan);
    setScanStatus(lastScan.status);

    const pathEl = document.getElementById('breadcrumb-path');
    const depthEl = document.getElementById('breadcrumb-depth');
    if (pathEl && lastScan.currentPath) pathEl.textContent = lastScan.currentPath;
    if (depthEl) depthEl.textContent = `Depth ${lastScan.currentDepth || 0}`;

    const scanPct = lastScan.totalEntries > 0
      ? Math.min(100, (lastScan.processedEntries || 0) / lastScan.totalEntries * 100)
      : 0;
    const fill = document.getElementById('progress-fill');
    const label = document.getElementById('progress-pct');
    if (fill) fill.style.width = `${scanPct}%`;
    if (label) label.textContent = `${scanPct.toFixed(1)}%`;
  }

  if (activeEventSource) {
    activeEventSource.close();
    activeEventSource = null;
  }
  activeEventSource = connectScanStream(activeTaskId, {
    onProgress: handleProgress,
    onFound: handleFound,
    onAgentCall: handleAgentCall,
    onAgentResponse: handleAgentResponse,
    onWarning: handleWarning,
    onDone: handleDone,
    onError: handleError,
    onStopped: handleStopped,
  });
}

async function handleStart() {
  const startBtn = document.getElementById('start-btn');
  const stopBtn = document.getElementById('stop-btn');

  try {
    await persistScannerSettings({ showSuccessToast: false });
    const form = collectScannerForm();

    if (!form.scanPath) {
      showToast(t('scanner.path_not_configured'), 'error');
      return;
    }
    const selectedEndpoint = String(form.scanProviderEndpoint || currentSettings?.apiEndpoint || '').trim();
    if (!getApiKeyForScanEndpoint(selectedEndpoint)) {
      try {
        await ensureRequiredCredentialsConfigured({
          providerEndpoints: [selectedEndpoint],
          reasonText: t('scanner.api_key_required'),
        });
      } catch (err) {
        showToast(err.message, 'error');
        return;
      }
    }

    if (startBtn) {
      startBtn.disabled = true;
      startBtn.innerHTML = `<span class="spinner"></span> ${t('scanner.prepare')}`;
    }

    const result = await startScan({
      targetPath: form.scanPath,
      targetSizeGB: form.targetSizeGB,
      maxDepth: form.maxDepth,
      autoAnalyze: true,
      responseLanguage: getLang(),
    });

    activeTaskId = result.taskId;
    latestTaskId = result.taskId;
    storage.set('lastScanTaskId', latestTaskId);

    const activityEl = document.getElementById('scan-activity-hidden');
    const emptyEl = document.getElementById('scan-empty');
    const breadcrumb = document.getElementById('scan-breadcrumb');
    if (activityEl) activityEl.style.display = '';
    if (emptyEl) emptyEl.style.display = 'none';
    if (breadcrumb) breadcrumb.style.display = '';

    if (startBtn) startBtn.style.display = 'none';
    if (stopBtn) stopBtn.style.display = '';

    addLog('scanning', `${t('scanner.log_start')} [${activeTaskId}]`);

    activeEventSource = connectScanStream(activeTaskId, {
      onProgress: handleProgress,
      onFound: handleFound,
      onAgentCall: handleAgentCall,
      onAgentResponse: handleAgentResponse,
      onWarning: handleWarning,
      onDone: handleDone,
      onError: handleError,
      onStopped: handleStopped,
    });
  } catch (err) {
    showToast(t('scanner.toast_start_failed') + err.message, 'error');
    if (startBtn) {
      startBtn.disabled = false;
      startBtn.innerHTML = t('scanner.start');
    }
  }
}

async function handleStop() {
  if (!activeTaskId) return;
  try {
    await stopScan(activeTaskId);
    showToast(t('scanner.stopped'), 'info');
  } catch (err) {
    showToast(t('scanner.toast_stop_failed') + err.message, 'error');
  }
}

function handleProgress(data) {
  if (data?.id) {
    latestTaskId = data.id;
    storage.set('lastScanTaskId', latestTaskId);
  }

  updateStats(data);
  setScanStatus(data.status);
  storage.set('lastScan', data);

  const pathEl = document.getElementById('breadcrumb-path');
  const depthEl = document.getElementById('breadcrumb-depth');
  if (pathEl && data.currentPath) pathEl.textContent = data.currentPath;
  if (depthEl) depthEl.textContent = `Depth ${data.currentDepth || 0}`;

  const scanPct = data.totalEntries > 0
    ? Math.min(100, (data.processedEntries || 0) / data.totalEntries * 100)
    : 0;
  const fill = document.getElementById('progress-fill');
  const label = document.getElementById('progress-pct');
  if (fill) fill.style.width = `${scanPct}%`;
  if (label) label.textContent = `${scanPct.toFixed(1)}%`;

  if (data.status === 'analyzing') {
    addLog('analyzing', `${t('scanner.analyzing')}: ${data.currentPath}`);
  } else if (data.status === 'scanning') {
    addLog('scanning', `${t('scanner.scanning')}: ${data.currentPath}`);
  }
}

function handleFound(item) {
  addLog('found', `${t('results.safe_to_clean')}: ${item.name} (${formatSize(item.size)}) - ${item.reason}`);
}

function handleWarning(info) {
  if (info?.type !== 'permission_denied') return;
  const path = String(info.path || '').trim();
  addLog('analyzing', `${t('scanner.permission_denied_skip')}${path || info.message || ''}`);
}

function handleDone(data) {
  if (data?.id) {
    latestTaskId = data.id;
    storage.set('lastScanTaskId', latestTaskId);
  }

  updateStats(data);
  setScanStatus('done');
  storage.set('lastScan', data);
  resetButtons();

  const fill = document.getElementById('progress-fill');
  const label = document.getElementById('progress-pct');
  if (fill) fill.style.width = '100%';
  if (label) label.textContent = '100.0%';

  const doneText = t('scanner.completed', { count: data.deletableCount ?? 0 });
  addLog('found', doneText);
  if (data.permissionDeniedCount > 0) {
    const permissionText = t('scanner.permission_denied_summary', { count: data.permissionDeniedCount ?? 0 });
    addLog('analyzing', permissionText);
    showToast(`${doneText} ${permissionText}`, 'info');
  } else {
    showToast(doneText, 'success');
  }
  window.location.hash = '#/results';
}

function handleError(err) {
  resetButtons();
  setScanStatus('error');
  const message = err?.message || t('toast.error');
  addLog('analyzing', `${t('scanner.toast_failed_detail')}${message}`);
  showToast(`${t('scanner.toast_failed_detail')}${message}`, 'error');
}

function handleStopped(data) {
  if (data?.id) {
    latestTaskId = data.id;
    storage.set('lastScanTaskId', latestTaskId);
  }
  updateStats(data);
  setScanStatus('stopped');
  storage.set('lastScan', data);
  resetButtons();
  addLog('scanning', t('scanner.stopped'));
}

function resetButtons() {
  const startBtn = document.getElementById('start-btn');
  const stopBtn = document.getElementById('stop-btn');
  if (startBtn) {
    startBtn.style.display = '';
    startBtn.disabled = false;
    startBtn.innerHTML = t('scanner.start');
  }
  if (stopBtn) stopBtn.style.display = 'none';

  activeTaskId = null;
  if (activeEventSource) {
    activeEventSource.close();
    activeEventSource = null;
  }
}

function updateStats(data) {
  const el = (id) => document.getElementById(id);
  if (el('stat-scanned')) el('stat-scanned').textContent = data.scannedCount || 0;
  if (el('stat-cleanable')) el('stat-cleanable').textContent = formatSize(data.totalCleanable || 0);
  if (el('stat-tokens')) el('stat-tokens').textContent = (data.tokenUsage?.total || 0).toLocaleString();
}

function addLog(type, text) {
  const time = new Date().toLocaleTimeString('zh-CN', { hour12: false });
  const entry = { kind: 'simple', type, text, time };
  logEntries.push(entry);
  persistScanLog();
  const trimmed = trimLogEntries();
  if (trimmed) {
    renderLogEntries(isGroupedScanLogType(type) ? 'top' : 'bottom');
    return;
  }
  appendLogEntry(entry);
}

function addDetailLog(type, summary, detailHtml) {
  const time = new Date().toLocaleTimeString('zh-CN', { hour12: false });
  const entry = { id: nextLogEntryId++, kind: 'detail', type, summary, detailHtml, time };
  logEntries.push(entry);
  persistScanLog();
  const trimmed = trimLogEntries();
  if (trimmed) {
    renderLogEntries(isGroupedScanLogType(type) ? 'top' : 'bottom');
    return;
  }
  appendLogEntry(entry);
}

function handleAgentCall(data) {
  const childDirList = (data.childDirectories || [])
    .map((entry) => `- ${entry.name} (${formatSize(entry.size)})`)
    .join('\n');

  let detailHtml = `
    <div style="margin-bottom: 8px;"><strong>Type:</strong> ${escHtml(data.nodeType)}</div>
    <div style="margin-bottom: 8px;"><strong>Path:</strong> ${escHtml(data.nodePath)}</div>
    <div style="margin-bottom: 8px;"><strong>Name:</strong> ${escHtml(data.nodeName)}</div>
    <div style="margin-bottom: 8px;"><strong>Size:</strong> ${escHtml(formatSize(data.nodeSize || 0))}</div>
  `;

  if (data.nodeType === 'directory') {
    detailHtml += `
      <div style="margin-bottom: 4px;"><strong>Direct Child Directories</strong></div>
      <div style="padding-left: 8px; border-left: 2px solid rgba(6, 182, 212, 0.3);">${escHtml(childDirList || '(none)')}</div>
    `;
  }

  addDetailLog('agent_call', `LLM call - ${data.nodeType}: ${data.nodeName}`, detailHtml);
}

function handleAgentResponse(data) {
  const elapsed = Number(data.elapsed || 0) / 1000;
  const classStr = String(data.classification || 'suspicious');
  const riskStr = String(data.risk || 'medium');
  const hasSubfolders = data.nodeType === 'directory'
    ? (data.hasPotentialDeletableSubfolders ? 'true' : 'false')
    : 'n/a';

  let detailSections = '';
  detailSections += `<div style="margin-bottom: 10px;">
    <strong>Type:</strong> ${escHtml(data.nodeType)} | <strong>Model:</strong> ${escHtml(data.model)} | <strong>Elapsed:</strong> ${elapsed.toFixed(1)}s | <strong>Token:</strong> ${(data.tokenUsage?.total || 0).toLocaleString()}
  </div>`;

  detailSections += `<div style="margin-bottom: 10px;"><strong>Path:</strong> ${escHtml(data.nodePath)}</div>`;
  detailSections += `<div style="margin-bottom: 10px;"><strong>Classification:</strong> ${escHtml(classStr)} | <strong>Risk:</strong> ${escHtml(riskStr)} | <strong>HasPotentialDeletableSubfolders:</strong> ${escHtml(hasSubfolders)}</div>`;

  if (data.error) {
    detailSections += `<div style="margin-bottom: 10px; color: var(--accent-danger);"><strong>Error:</strong> ${escHtml(data.error)}</div>`;
  }

  if (data.userPrompt) {
    detailSections += `<div style="margin-bottom: 10px;">
      <strong>Prompt:</strong>
      <div style="margin-top: 4px; padding: 8px; background: rgba(0,0,0,0.3); border-radius: 4px; max-height: 300px; overflow-y: auto;">${escHtml(data.userPrompt)}</div>
    </div>`;
  }

  if (data.reasoning) {
    detailSections += `<div style="margin-bottom: 10px;">
      <strong>Reasoning:</strong>
      <div style="margin-top: 4px; padding: 8px; background: rgba(245, 158, 11, 0.08); border: 1px solid rgba(245, 158, 11, 0.15); border-radius: 4px; max-height: 400px; overflow-y: auto;">${escHtml(data.reasoning)}</div>
    </div>`;
  }

  if (data.rawContent) {
    const raw = String(data.rawContent);
    const truncated = raw.length > 2000 ? `${raw.slice(0, 2000)}\n...` : raw;
    detailSections += `<div>
      <strong>Raw Response:</strong>
      <div style="margin-top: 4px; padding: 8px; background: rgba(0,0,0,0.3); border-radius: 4px; max-height: 400px; overflow-y: auto;">${escHtml(truncated)}</div>
    </div>`;
  }

  const statusIcon = data.error ? 'X' : 'OK';
  addDetailLog(
    'agent_response',
    `${statusIcon} LLM response - ${data.nodeType}: ${data.nodeName} (${classStr})`,
    detailSections
  );
}

function escHtml(str) {
  return String(str || '')
    .replace(/&/g, '&amp;')
    .replace(/</g, '&lt;')
    .replace(/>/g, '&gt;')
    .replace(/"/g, '&quot;')
    .replace(/\n/g, '<br>');
}
