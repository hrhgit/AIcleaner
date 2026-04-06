/**
 * src/pages/scanner.js
 * Scanner page with merged settings + scan controls.
 */
import {
  browseFolder,
  findLatestScanForPath,
  getProviderModels,
  getPrivilegeStatus,
  getSettings,
  openFileLocation,
  requestElevation,
  saveSettings,
} from '../utils/api.js';
import { handleElevationTransition } from '../utils/elevation.js';
import { getErrorMessage } from '../utils/errors.js';
import { formatSize } from '../utils/storage.js';
import * as storage from '../utils/storage.js';
import { showToast } from '../main.js';
import { getLang, t } from '../utils/i18n.js';
import {
  ensureRequiredCredentialsConfigured,
  getProviderCredentialPresence,
  getSearchCredentialPresence,
} from '../utils/secret-ui.js';
import { scanTaskController } from '../utils/scan-task-controller.js';

let activeTaskId = null;
let latestTaskId = null;
let logEntries = [];
let currentSettings = null;
let scannerIgnoredPaths = [];
let scannerWhitelistExpanded = false;
let renderVersion = 0;
let scannerTaskUnsubscribe = null;
const expandedDetailLogIds = new Set();
const collapsedDetailLogIds = new Set();
let renderedLogKeys = [];
const SCANNER_FORM_DRAFT_KEY = 'wipeout.scanner.global.form.v2';
const SCANNER_FORM_DRAFT_VERSION = 2;
const SCAN_LOG_COLLAPSED_KEY = 'wipeout.scanner.log.collapsed.v1';
const SCAN_RECORD_GROUP_COLLAPSED_KEY = 'wipeout.scanner.record-group.collapsed.v1';
const TARGET_SIZE_MIN_GB = 0.1;
const TARGET_SIZE_MAX_GB = 20;
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
let latestBaselineSnapshot = null;
let baselineLookupToken = 0;

function clampNumber(value, min, max, fallback) {
  const n = Number(value);
  if (!Number.isFinite(n)) return fallback;
  if (n < min) return min;
  if (n > max) return max;
  return n;
}

function normalizePathKey(value) {
  let normalized = String(value || '').trim().replace(/\//g, '\\').toLowerCase();
  while (normalized.length > 3 && normalized.endsWith('\\')) {
    normalized = normalized.slice(0, -1);
  }
  return normalized;
}

function isSystemPrunedRootPath(path) {
  return normalizePathKey(path) === 'c:\\';
}

function escapeHtml(value) {
  const div = document.createElement('div');
  div.textContent = String(value ?? '');
  return div.innerHTML;
}

function resolveSearchApi(settings) {
  const source = settings?.searchApi && typeof settings.searchApi === 'object'
    ? settings.searchApi
    : {};
  const scopes = source?.scopes && typeof source.scopes === 'object'
    ? source.scopes
    : {};

  return {
    enabled: !!source.enabled,
    apiKey: String(source.apiKey || '').trim(),
    scopes: {
      scan: !!scopes.scan,
      classify: !!scopes.classify,
      organizer: !!scopes.organizer,
    },
  };
}

function normalizeIgnoredPaths(paths) {
  const seen = new Set();
  const normalized = [];
  for (const raw of Array.isArray(paths) ? paths : []) {
    const path = String(raw || '').trim();
    const key = path.replace(/\//g, '\\').toLowerCase();
    if (!key || seen.has(key)) continue;
    seen.add(key);
    normalized.push(path);
  }
  return normalized;
}

function removeIgnoredPath(existingPaths, targetPath) {
  const targetKey = String(targetPath || '').trim().replace(/\//g, '\\').toLowerCase();
  return normalizeIgnoredPaths(existingPaths).filter((entry) => String(entry || '').trim().replace(/\//g, '\\').toLowerCase() !== targetKey);
}

function setScannerWhitelistButtonBusy(btn, busy) {
  if (!btn) return;
  btn.disabled = !!busy;
  btn.classList.toggle('is-busy', !!busy);
}

function syncScannerIgnoredPaths(settings = {}) {
  scannerIgnoredPaths = normalizeIgnoredPaths(settings?.scanIgnorePaths);
}

function renderScannerWhitelistPanel() {
  const panel = document.getElementById('scanner-whitelist-panel');
  const listEl = document.getElementById('scanner-whitelist-list');
  const emptyEl = document.getElementById('scanner-whitelist-empty');
  const countEl = document.getElementById('scanner-whitelist-count');
  const toggleBtn = document.getElementById('scanner-whitelist-toggle-btn');
  if (!panel || !listEl || !emptyEl || !countEl || !toggleBtn) return;

  const items = [...scannerIgnoredPaths].sort((a, b) => String(a).localeCompare(String(b), undefined, { sensitivity: 'base' }));
  countEl.textContent = String(items.length);
  toggleBtn.textContent = scannerWhitelistExpanded ? t('scanner.whitelist_hide') : t('scanner.whitelist_show');
  panel.hidden = !scannerWhitelistExpanded;

  if (!scannerWhitelistExpanded) {
    return;
  }

  if (!items.length) {
    listEl.innerHTML = '';
    emptyEl.style.display = '';
    return;
  }

  emptyEl.style.display = 'none';
  listEl.innerHTML = items.map((path) => `
    <div class="results-whitelist-item">
      <div class="results-whitelist-path-wrap">
        <div class="results-whitelist-path-label">${t('scanner.whitelist_path_label')}</div>
        <div class="results-whitelist-path" title="${escapeHtml(path)}">${escapeHtml(path)}</div>
      </div>
      <div class="results-whitelist-actions">
        <button class="btn btn-ghost scanner-whitelist-open-btn" type="button" data-path="${escapeHtml(path)}">
          ${t('results.open_folder')}
        </button>
        <button class="btn btn-secondary scanner-whitelist-remove-btn" type="button" data-path="${escapeHtml(path)}">
          ${t('scanner.whitelist_remove')}
        </button>
      </div>
    </div>
  `).join('');

  document.querySelectorAll('.scanner-whitelist-open-btn').forEach((btn) => {
    btn.addEventListener('click', async () => {
      try {
        setScannerWhitelistButtonBusy(btn, true);
        const res = await openFileLocation(btn.dataset.path);
        if (!res.success) {
          showToast(t('results.toast_open_failed') + res.error, 'error');
        }
      } catch (err) {
        showToast(t('results.toast_open_failed') + err.message, 'error');
      } finally {
        setScannerWhitelistButtonBusy(btn, false);
      }
    });
  });

  document.querySelectorAll('.scanner-whitelist-remove-btn').forEach((btn) => {
    btn.addEventListener('click', async () => {
      const path = String(btn.dataset.path || '').trim();
      if (!path) return;
      try {
        setScannerWhitelistButtonBusy(btn, true);
        await removePathFromScannerWhitelist(path);
      } finally {
        setScannerWhitelistButtonBusy(btn, false);
      }
    });
  });
}

function setScannerWhitelistExpanded(expanded) {
  scannerWhitelistExpanded = !!expanded;
  renderScannerWhitelistPanel();
}

async function removePathFromScannerWhitelist(path) {
  const nextIgnoredPaths = removeIgnoredPath(scannerIgnoredPaths, path);
  if (nextIgnoredPaths.length === scannerIgnoredPaths.length) {
    showToast(t('scanner.whitelist_removed'), 'info');
    return;
  }

  try {
    const result = await saveSettings({ scanIgnorePaths: nextIgnoredPaths });
    currentSettings = result?.settings || { ...(currentSettings || {}), scanIgnorePaths: nextIgnoredPaths };
    syncScannerIgnoredPaths(currentSettings);
    renderScannerWhitelistPanel();
    showToast(t('scanner.whitelist_removed'), 'success');
  } catch (err) {
    showToast(t('scanner.whitelist_remove_failed') + err.message, 'error');
  }
}

function collectScannerForm() {
  const scanPath = String(document.getElementById('scan-path')?.value || '').trim();
  const targetSize = clampNumber(
    document.getElementById('target-size-input')?.value ?? document.getElementById('target-size')?.value,
    TARGET_SIZE_MIN_GB,
    TARGET_SIZE_MAX_GB,
    1
  );
  const maxDepthUnlimited = !!document.getElementById('max-depth-unlimited')?.checked;
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
    maxDepthUnlimited,
    scanWebSearchEnabled,
    scanProviderEndpoint,
    scanModel,
  };
}

function normalizeScannerFormDraft(raw = {}) {
  return {
    scanPath: String(raw?.scanPath || '').trim(),
    targetSizeGB: clampNumber(raw?.targetSizeGB, TARGET_SIZE_MIN_GB, TARGET_SIZE_MAX_GB, 1),
    maxDepth: Math.floor(clampNumber(raw?.maxDepth, 1, 10, 5)),
    maxDepthUnlimited: !!raw?.maxDepthUnlimited,
    scanWebSearchEnabled: !!raw?.scanWebSearchEnabled,
    scanProviderEndpoint: String(raw?.scanProviderEndpoint || '').trim(),
    scanModel: String(raw?.scanModel || '').trim(),
  };
}

function extractScannerFormFromSettings(settings = {}) {
  const searchApi = resolveSearchApi(settings);
  const defaultEndpoint = String(settings?.defaultProviderEndpoint || '').trim();
  const defaultModel = String(settings?.providerConfigs?.[defaultEndpoint]?.model || '').trim();

  return normalizeScannerFormDraft({
    scanPath: settings?.scanPath,
    targetSizeGB: settings?.targetSizeGB,
    maxDepth: settings?.maxDepth,
    maxDepthUnlimited: settings?.maxDepthUnlimited,
    scanWebSearchEnabled: !!searchApi.scopes.scan,
    scanProviderEndpoint: defaultEndpoint,
    scanModel: defaultModel,
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

function replaceLogEntries(nextEntries = [], { persist = true } = {}) {
  scanTaskController.replaceLogEntries(nextEntries, { persist });
  logEntries = scanTaskController.getState().logEntries;
  expandedDetailLogIds.clear();
  collapsedDetailLogIds.clear();
  syncRenderedLogEntries({ force: true });
  refreshScanLogPanel();
}

function isScanLogCollapsed() {
  return !!storage.get(SCAN_LOG_COLLAPSED_KEY, false);
}

function setScanLogCollapsed(collapsed) {
  storage.set(SCAN_LOG_COLLAPSED_KEY, !!collapsed);
}

function expandScanLogPreview() {
  if (!isScanLogCollapsed() || !logEntries.length) return;
  setScanLogCollapsed(false);
  refreshScanLogPanel();
}

function refreshScanLogPanel() {
  const collapsed = isScanLogCollapsed();
  const panel = document.getElementById('scan-log-panel');
  const toggleBtn = document.getElementById('toggle-log-btn');
  const hint = document.getElementById('scan-log-collapsed-hint');
  const hasPreview = logEntries.length > 0;
  if (panel) {
    panel.classList.toggle('is-collapsed', collapsed);
    panel.classList.toggle('is-clickable-preview', collapsed && hasPreview);
    panel.setAttribute('aria-expanded', collapsed ? 'false' : 'true');
    if (collapsed && hasPreview) {
      panel.setAttribute('role', 'button');
      panel.setAttribute('tabindex', '0');
      panel.setAttribute('aria-label', t('scanner.log_preview_hint'));
    } else {
      panel.removeAttribute('role');
      panel.removeAttribute('tabindex');
      panel.removeAttribute('aria-label');
    }
  }
  if (toggleBtn) {
    toggleBtn.textContent = collapsed ? t('scanner.log_expand') : t('scanner.log_collapse');
    toggleBtn.setAttribute('aria-expanded', collapsed ? 'false' : 'true');
  }
  if (hint) {
    hint.style.display = collapsed && hasPreview ? '' : 'none';
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
    collapsedDetailLogIds.delete(entryId);
  } else {
    expandedDetailLogIds.delete(entryId);
    collapsedDetailLogIds.add(entryId);
  }
}

function isDetailLogExpanded(entry) {
  if (!entry || entry.id == null) return false;
  if (expandedDetailLogIds.has(entry.id)) return true;
  return !!entry.expandByDefault && !collapsedDetailLogIds.has(entry.id);
}

function isLogPinnedToBottom(log) {
  if (!log) return true;
  return (log.scrollHeight - log.scrollTop - log.clientHeight) <= 24;
}

function isGroupedScanLogType(type) {
  return ['scanning', 'analyzing', 'found'].includes(type);
}

function getLogIcon(type) {
  if (type === 'found') return '✅';
  if (type === 'analyzing') return '🧠';
  if (type === 'agent_call') return '⚡';
  if (type === 'agent_response') return '🤖';
  if (type === 'scanning') return '🔍';
  return '•';
}

function createSimpleLogEntryElement(entry) {
  const el = document.createElement('div');
  el.className = `scan-log-entry ${entry.type}`;
  el.innerHTML = `
    <span class="log-icon">${getLogIcon(entry.type)}</span>
    <span class="log-time" style="color: var(--text-muted); margin-right: 6px;">[${entry.time}]</span>
    <span class="log-text">${entry.text}</span>
  `;
  return el;
}

function createDetailLogEntryElement(entry) {
  const expanded = isDetailLogExpanded(entry);
  const wrapper = document.createElement('div');
  wrapper.className = `scan-log-entry ${entry.type}`;
  wrapper.innerHTML = `
    <span class="log-icon">${getLogIcon(entry.type)}</span>
    <div class="log-content">
      <div class="log-detail-header" style="cursor: pointer; user-select: none; display: flex; align-items: center; gap: 6px;">
        <span class="log-time" style="color: var(--text-muted); margin-right: 4px;">[${entry.time}]</span>
        <span class="log-detail-arrow" style="transition: transform 0.2s; display: inline-block; font-size: 0.65rem; transform: ${expanded ? 'rotate(90deg)' : 'rotate(0deg)'};">▶</span>
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
      <span class="scan-log-group-arrow">▶</span>
      <span>${t('scanner.scan_records')} (${entries.length})</span>
    </div>
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
  });

  wrapper.appendChild(header);
  wrapper.appendChild(body);
  return wrapper;
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

function getLogEntryKey(entry) {
  return JSON.stringify([
    entry?.id ?? '',
    entry?.kind ?? 'simple',
    entry?.type ?? '',
    entry?.time ?? '',
    entry?.text ?? '',
    entry?.summary ?? '',
    entry?.detailHtml ?? '',
    entry?.expandByDefault ?? false,
  ]);
}

function areLogKeysEqual(left, right) {
  if (left.length !== right.length) return false;
  for (let i = 0; i < left.length; i += 1) {
    if (left[i] !== right[i]) return false;
  }
  return true;
}

function syncRenderedLogEntries({ force = false } = {}) {
  const log = document.getElementById('scan-log');
  if (!log) return;

  const nextKeys = logEntries.map((entry) => getLogEntryKey(entry));
  if (!force && areLogKeysEqual(renderedLogKeys, nextKeys)) {
    return;
  }

  const canAppendOnly = !force
    && renderedLogKeys.length > 0
    && nextKeys.length === renderedLogKeys.length + 1
    && areLogKeysEqual(renderedLogKeys, nextKeys.slice(0, -1));

  if (canAppendOnly) {
    appendLogEntry(logEntries[logEntries.length - 1]);
    renderedLogKeys = nextKeys;
    return;
  }

  if (!nextKeys.length) {
    log.innerHTML = '';
    renderedLogKeys = [];
    return;
  }

  renderLogEntries();
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
  renderedLogKeys = logEntries.map((entry) => getLogEntryKey(entry));
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
  const endpoint = String(form.scanProviderEndpoint || 'https://api.deepseek.com').trim();
  const model = String(form.scanModel || PROVIDER_MODELS[endpoint]?.[0]?.value || 'deepseek-chat').trim();
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
    maxDepthUnlimited: draft.maxDepthUnlimited,
    providerConfigs: {
      ...(settings?.providerConfigs && typeof settings.providerConfigs === 'object' ? settings.providerConfigs : {}),
      ...(draft.scanProviderEndpoint
        ? {
          [draft.scanProviderEndpoint]: {
            ...(settings?.providerConfigs?.[draft.scanProviderEndpoint] || {}),
            endpoint: draft.scanProviderEndpoint,
            name: String(settings?.providerConfigs?.[draft.scanProviderEndpoint]?.name || getProviderLabel(draft.scanProviderEndpoint)),
            model: draft.scanModel || settings?.providerConfigs?.[draft.scanProviderEndpoint]?.model || PROVIDER_MODELS[draft.scanProviderEndpoint]?.[0]?.value || 'deepseek-chat',
          },
        }
        : {}),
    },
    defaultProviderEndpoint: draft.scanProviderEndpoint || settings?.defaultProviderEndpoint,
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
  const maxDepthUnlimitedToggle = document.getElementById('max-depth-unlimited');
  const scanToggle = document.getElementById('scan-enable-web-search');
  const providerSelect = document.getElementById('scan-provider');
  const modelSelect = document.getElementById('scan-model');

  if (scanPathInput) scanPathInput.value = form.scanPath;
  if (targetSizeSlider) targetSizeSlider.value = String(form.targetSizeGB);
  if (targetSizeInput) targetSizeInput.value = form.targetSizeGB.toFixed(1);
  if (maxDepthSlider) maxDepthSlider.value = String(form.maxDepth);
  if (maxDepthInput) maxDepthInput.value = String(form.maxDepth);
  if (maxDepthUnlimitedToggle) maxDepthUnlimitedToggle.checked = !!form.maxDepthUnlimited;
  updateScannerMaxDepthControls(!!form.maxDepthUnlimited);
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
  refreshLatestBaselineForPath(form.scanPath);
}

function updateScannerMaxDepthControls(unlimited) {
  const maxDepthSlider = document.getElementById('max-depth');
  const maxDepthInput = document.getElementById('max-depth-input');
  const maxDepthHint = document.getElementById('max-depth-hint');
  const disabled = !!unlimited;
  if (maxDepthSlider) maxDepthSlider.disabled = disabled;
  if (maxDepthInput) maxDepthInput.disabled = disabled;
  if (maxDepthHint) {
    maxDepthHint.textContent = disabled
      ? t('settings.max_depth_unlimited_hint')
      : t('settings.max_depth_hint');
  }
}

function updatePathDisplay(pathValue) {
  const pathDisplay = document.getElementById('scan-path-display');
  if (!pathDisplay) return;
  const path = String(pathValue || '').trim();
  if (path) {
    pathDisplay.textContent = `${t('settings.scan_path')}: ${path}`;
    pathDisplay.title = pathDisplay.textContent;
  } else {
    pathDisplay.textContent = t('scanner.path_not_configured');
    pathDisplay.title = pathDisplay.textContent;
  }
}

function resetScanPreview({ clearLog = true } = {}) {
  updateStats({ scannedCount: 0, totalCleanable: 0, tokenUsage: { total: 0 } });
  setScanStatus('idle');

  const fill = document.getElementById('progress-fill');
  const label = document.getElementById('progress-pct');
  const pathEl = document.getElementById('breadcrumb-path');
  const depthEl = document.getElementById('breadcrumb-depth');
  const activityEl = document.getElementById('scan-activity-hidden');
  const emptyEl = document.getElementById('scan-empty');
  const breadcrumb = document.getElementById('scan-breadcrumb');

  if (fill) fill.style.width = '0%';
  if (label) label.textContent = '0.0%';
  if (pathEl) {
    pathEl.textContent = '...';
    pathEl.title = '';
  }
  if (depthEl) depthEl.textContent = t('scanner.not_set');
  if (activityEl) activityEl.style.display = 'none';
  if (emptyEl) emptyEl.style.display = '';
  if (breadcrumb) breadcrumb.style.display = 'none';

  latestTaskId = null;

  if (clearLog) {
    replaceLogEntries([], { persist: false });
  } else {
    syncRenderedLogEntries({ force: true });
  }
}

function previewSnapshotForPath(snapshot) {
  if (!snapshot?.id) {
    resetScanPreview({ clearLog: true });
    return;
  }

  latestTaskId = snapshot.id;
  applySnapshotToUI(snapshot, { persist: false });

  const cachedLastScan = storage.get('lastScan', null);
  const shouldKeepCurrentLog = String(cachedLastScan?.id || '') === String(snapshot.id || '');
  if (!shouldKeepCurrentLog) {
    replaceLogEntries([], { persist: false });
  } else {
    refreshScanLogPanel();
    syncRenderedLogEntries({ force: true });
  }
}

function refreshScanActionHint() {
  const hintEl = document.getElementById('scan-incremental-hint');
  const pruneHintEl = document.getElementById('scan-system-prune-hint');
  const startBtn = document.getElementById('start-btn');
  const baseline = latestBaselineSnapshot;
  const scanPath = String(document.getElementById('scan-path')?.value || '').trim();
  if (hintEl) {
    if (baseline?.id) {
      hintEl.textContent = t('scanner.incremental_hint', {
        depth: baseline.maxScannedDepth ?? baseline.configuredMaxDepth ?? 0,
        size: formatSize(baseline.totalCleanable || 0),
      });
      hintEl.style.display = 'block';
    } else {
      hintEl.textContent = '';
      hintEl.style.display = 'none';
    }
  }
  if (pruneHintEl) {
    if (isSystemPrunedRootPath(scanPath)) {
      pruneHintEl.textContent = t('scanner.system_prune_hint');
      pruneHintEl.style.display = 'block';
    } else {
      pruneHintEl.textContent = '';
      pruneHintEl.style.display = 'none';
    }
  }
  if (startBtn && !activeTaskId) {
    startBtn.textContent = baseline?.id ? t('scanner.rescan') : t('scanner.start');
  }
}

async function refreshLatestBaselineForPath(pathValue, { syncPreview = true } = {}) {
  const path = String(pathValue || '').trim();
  const token = ++baselineLookupToken;
  if (!path) {
    latestBaselineSnapshot = null;
    refreshScanActionHint();
    if (syncPreview && !activeTaskId) {
      resetButtons();
      resetScanPreview({ clearLog: true });
    }
    return;
  }
  try {
    const snapshot = await findLatestScanForPath(path);
    if (token !== baselineLookupToken) return;
    latestBaselineSnapshot = snapshot && typeof snapshot === 'object' && snapshot.id ? snapshot : null;
  } catch (err) {
    if (token !== baselineLookupToken) return;
    latestBaselineSnapshot = null;
    console.warn('Failed to resolve latest scan baseline:', err);
  }
  refreshScanActionHint();
  if (!syncPreview || activeTaskId || token !== baselineLookupToken) return;
  resetButtons();
  if (latestBaselineSnapshot?.id) {
    previewSnapshotForPath(latestBaselineSnapshot);
  } else {
    resetScanPreview({ clearLog: true });
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
        || 'https://api.deepseek.com'
      ).trim();
      const selectedModel = String(
        form.scanModel
        || currentSettings?.providerConfigs?.[selectedEndpoint]?.model
        || PROVIDER_MODELS[selectedEndpoint]?.[0]?.value
        || 'deepseek-chat'
      ).trim();
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
      maxDepthUnlimited: !!form.maxDepthUnlimited,
      providerConfigs,
      defaultProviderEndpoint: selectedEndpoint,
      searchApi,
    };

    const result = await saveSettings(payload);
    currentSettings = result?.settings || await getSettings();
    syncScannerIgnoredPaths(currentSettings);
    applySettingsToForm(currentSettings);
    await initScanProviderFields(currentSettings);
    renderScannerWhitelistPanel();
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
  const depthUnlimitedToggle = document.getElementById('max-depth-unlimited');
  const handleFormMutation = () => {
    persistScannerFormDraft();
  };

  sizeSlider?.addEventListener('input', () => {
    if (sizeInput) sizeInput.value = clampNumber(sizeSlider.value, TARGET_SIZE_MIN_GB, TARGET_SIZE_MAX_GB, 1).toFixed(1);
    handleFormMutation();
  });
  sizeInput?.addEventListener('input', () => {
    const val = clampNumber(sizeInput.value, TARGET_SIZE_MIN_GB, TARGET_SIZE_MAX_GB, 1);
    if (sizeSlider) sizeSlider.value = String(val);
    handleFormMutation();
  });
  sizeInput?.addEventListener('blur', () => {
    const val = clampNumber(sizeInput.value, TARGET_SIZE_MIN_GB, TARGET_SIZE_MAX_GB, 1);
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
  depthUnlimitedToggle?.addEventListener('change', () => {
    updateScannerMaxDepthControls(depthUnlimitedToggle.checked);
    handleFormMutation();
  });

  document.getElementById('scan-path')?.addEventListener('input', (event) => {
    updatePathDisplay(event.target?.value || '');
    handleFormMutation();
    refreshLatestBaselineForPath(event.target?.value || '');
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
  document.getElementById('scanner-whitelist-toggle-btn')?.addEventListener('click', () => {
    setScannerWhitelistExpanded(!scannerWhitelistExpanded);
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
        refreshLatestBaselineForPath(result.path);
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

function applySnapshotToUI(data, { persist = true } = {}) {
  if (!data) return;

  updateStats(data);
  setScanStatus(data.status);
  if (persist) {
    storage.set('lastScan', data);
  }

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
  if (pathEl) {
    pathEl.textContent = data.currentPath || '...';
    pathEl.title = data.currentPath || '';
  }
  if (depthEl) depthEl.textContent = `Depth ${data.currentDepth || 0}`;

  const scanPct = data.totalEntries > 0
    ? Math.min(100, (data.processedEntries || 0) / data.totalEntries * 100)
    : (data.status === 'done' ? 100 : 0);
  if (fill) fill.style.width = `${scanPct}%`;
  if (label) label.textContent = `${scanPct.toFixed(1)}%`;
}

function syncTaskViewState(state = scanTaskController.getState()) {
  activeTaskId = state?.activeTaskId || null;
  latestTaskId = state?.latestTaskId || null;
  logEntries = Array.isArray(state?.logEntries) ? state.logEntries : [];
}

function applyTaskControllerState(state = scanTaskController.getState()) {
  syncTaskViewState(state);
  refreshScanLogPanel();
  syncRenderedLogEntries();

  if (activeTaskId) {
    restoreActiveState(state?.snapshot || null);
    return;
  }

  if (!activeTaskId) {
    const startBtn = document.getElementById('start-btn');
    const stopBtn = document.getElementById('stop-btn');
    if (startBtn) {
      startBtn.style.display = '';
      startBtn.disabled = false;
      startBtn.innerHTML = latestBaselineSnapshot?.id ? t('scanner.rescan') : t('scanner.start');
    }
    if (stopBtn) stopBtn.style.display = 'none';
  }
}

function handleTaskControllerEvent(event) {
  if (!event || typeof event !== 'object') return;

  if (event.kind === 'state') {
    applyTaskControllerState(event.state);
    return;
  }

  applyTaskControllerState(event.state);

  if (event.kind === 'done') {
    const doneText = event.doneText || t('scanner.completed', { count: event.data?.deletableCount ?? 0 });
    if (event.permissionText) {
      showToast(`${doneText} ${event.permissionText}`, 'info');
    } else {
      showToast(doneText, 'success');
    }
    refreshLatestBaselineForPath(event.data?.targetPath || collectScannerForm().scanPath);
    window.location.hash = '#/results';
    return;
  }

  if (event.kind === 'error') {
    showToast(`${t('scanner.toast_failed_detail')}${getErrorMessage(event.error || event.message)}`, 'error');
    return;
  }

  if (event.kind === 'stopped') {
    showToast(t('scanner.stopped'), 'info');
    refreshLatestBaselineForPath(event.data?.targetPath || collectScannerForm().scanPath);
  }
}

async function restoreActiveTaskById(taskId) {
  try {
    return await scanTaskController.restoreTaskById(taskId);
  } catch (err) {
    console.warn('Failed to restore active scan by task id:', err);
    return false;
  }
}

export async function renderScanner(container) {
  const expectedRenderVersion = ++renderVersion;
  const isStale = () => expectedRenderVersion !== renderVersion || !container.isConnected;
  const draftState = readScannerFormDraft();
  const preferredTaskId = String(storage.get('lastScanTaskId', null) || '').trim();
  scannerIgnoredPaths = [];
  scannerWhitelistExpanded = false;
  renderedLogKeys = [];
  syncTaskViewState(scanTaskController.getState());

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
          <input type="text" id="scan-path" class="form-input" style="flex: 1; min-width: 0;" placeholder="C:\\Users\\YourName\\Downloads" />
          <button type="button" id="browse-folder-btn" class="btn btn-secondary" style="white-space: nowrap; flex-shrink: 0;">
            ${t('settings.browse')}
          </button>
        </div>
        <div class="form-hint">${t('settings.browse_hint')}</div>
        <div id="scan-incremental-hint" class="form-hint" style="display:none; color: var(--accent-info); margin-top: 6px;"></div>
        <div id="scan-system-prune-hint" class="form-hint" style="display:none; color: var(--text-secondary); margin-top: 6px;"></div>
      </div>

      <div class="form-group">
        <label class="form-label">${t('settings.target_size')}</label>
        <div class="range-container">
          <input type="range" id="target-size" class="range-slider" min="0.1" max="20" step="0.1" value="1" />
          <div style="display:flex; align-items:center; gap:8px;">
          <input type="number" id="target-size-input" class="form-input no-spin" style="width:80px; height:32px; padding:4px 8px; text-align:center;" min="0.1" max="20" step="0.1" value="1" />
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
        <label style="display:flex; align-items:center; gap:10px; cursor:pointer; margin-top:10px;">
          <input type="checkbox" id="max-depth-unlimited" class="toggle-checkbox" style="width: 18px; height: 18px;" />
          <span>${t('settings.max_depth_unlimited')}</span>
        </label>
        <div id="max-depth-hint" class="form-hint">${t('settings.max_depth_hint')}</div>
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
        <div>
          <h2 class="card-title">${t('scanner.whitelist_title')}</h2>
          <div class="form-hint">${t('scanner.whitelist_hint')}</div>
        </div>
        <div class="scanner-whitelist-toolbar">
          <span class="badge badge-info">${t('scanner.whitelist_count')} <span id="scanner-whitelist-count">0</span></span>
          <button id="scanner-whitelist-toggle-btn" class="btn btn-ghost" type="button">${t('scanner.whitelist_show')}</button>
        </div>
      </div>
      <div id="scanner-whitelist-panel" class="scanner-whitelist-panel" hidden>
        <div id="scanner-whitelist-list" class="results-whitelist-list"></div>
        <div id="scanner-whitelist-empty" class="empty-state results-whitelist-empty" style="padding: 24px;">
          <div class="empty-state-text">${t('scanner.whitelist_empty')}</div>
          <div class="empty-state-hint">${t('scanner.whitelist_empty_hint')}</div>
        </div>
      </div>
    </div>

    <div class="card animate-in mb-24" style="animation-delay: 0.1s">
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

    <div class="stats-grid animate-in" style="animation-delay: 0.15s">
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

    <div class="card animate-in mb-24" style="animation-delay: 0.2s">
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

    <div class="card animate-in mb-24" style="animation-delay: 0.25s">
      <div class="card-header">
        <h2 class="card-title">${t('scanner.activity_log')}</h2>
        <div style="display:flex; gap:8px; align-items:center;">
          <button id="toggle-log-btn" class="btn btn-ghost" style="padding: 6px 12px; font-size: 0.75rem;">${t('scanner.log_expand')}</button>
          <button id="clear-log-btn" class="btn btn-ghost" style="padding: 6px 12px; font-size: 0.75rem;">${t('scanner.log_clear')}</button>
        </div>
      </div>
      <div id="scan-log-panel" class="scan-log-panel">
        <div class="scan-activity" id="scan-activity-hidden" style="display:none;">
          <div class="scan-log" id="scan-log"></div>
          <div id="scan-log-collapsed-hint" class="scan-log-collapsed-hint" style="display:none;">${t('scanner.log_preview_hint')}</div>
        </div>
        <div id="scan-empty" class="empty-state" style="padding:30px;">
          <div class="empty-state-icon">🧭</div>
          <div class="empty-state-text">${t('scanner.prepare')}</div>
          <div class="empty-state-hint">${t('scanner.not_set')}</div>
        </div>
      </div>
    </div>

    <div class="flex items-center justify-between animate-in" style="animation-delay: 0.3s; gap: 16px; flex-wrap: wrap;">
      <div class="path-inline-display"><span id="scan-path-display" class="form-hint mono"></span></div>
      <div class="flex gap-16" style="flex-shrink: 0;">
        <button id="stop-btn" class="btn btn-danger" style="display: none;">${t('scanner.stop')}</button>
        <button id="start-btn" class="btn btn-primary btn-lg">${t('scanner.start')}</button>
      </div>
    </div>
  `;

  bindSettingsEvents();
  renderScannerWhitelistPanel();
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
    syncScannerIgnoredPaths(currentSettings);
    applySettingsToForm(currentSettings);
    await initScanProviderFields(currentSettings);
    renderScannerWhitelistPanel();
    if (isStale()) return;
    if (!draftState.dirty) {
      writeScannerFormDraft(extractScannerFormFromSettings(currentSettings), { dirty: false });
    }
  } catch (err) {
    currentSettings = null;
    syncScannerIgnoredPaths(null);
    renderScannerWhitelistPanel();
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
      syncScannerIgnoredPaths(currentSettings);
      syncScanProviderApiKeyMap(currentSettings);
      const selectedEndpoint = String(document.getElementById('scan-provider')?.value || '').trim();
      const selectedModel = String(document.getElementById('scan-model')?.value || '').trim();
      await initScanModelSelectors({ endpoint: selectedEndpoint, model: selectedModel });
      refreshScanApiConfigHint(selectedEndpoint);
      renderScannerWhitelistPanel();
    } catch (err) {
      console.warn('Failed to refresh scanner provider settings:', err);
    }
  };
  window.addEventListener('provider-settings-updated', scannerProviderSettingsUpdatedHandler);

  if (scannerTaskUnsubscribe) {
    scannerTaskUnsubscribe();
  }
  scannerTaskUnsubscribe = scanTaskController.subscribe((event) => {
    if (isStale()) return;
    handleTaskControllerEvent(event);
  });

  document.getElementById('start-btn')?.addEventListener('click', handleStart);
  document.getElementById('stop-btn')?.addEventListener('click', handleStop);
  document.getElementById('toggle-log-btn')?.addEventListener('click', () => {
    setScanLogCollapsed(!isScanLogCollapsed());
    refreshScanLogPanel();
  });
  document.getElementById('scan-log-panel')?.addEventListener('click', (event) => {
    if (!isScanLogCollapsed() || !logEntries.length) return;
    const target = event.target;
    if (target instanceof Element && target.closest('button, a, input, textarea, select, label')) {
      return;
    }
    expandScanLogPreview();
  });
  document.getElementById('scan-log-panel')?.addEventListener('keydown', (event) => {
    if (!isScanLogCollapsed() || !logEntries.length) return;
    if (event.key !== 'Enter' && event.key !== ' ') return;
    event.preventDefault();
    expandScanLogPreview();
  });
  document.getElementById('clear-log-btn')?.addEventListener('click', () => {
    replaceLogEntries([], { persist: true });
  });
  applyTaskControllerState(scanTaskController.getState());

  if (activeTaskId) {
    restoreActiveState(scanTaskController.getState().snapshot);
    return;
  }

  try {
    const restored = await scanTaskController.restoreAnyActiveTask(preferredTaskId);
    if (isStale()) return;
    if (restored) {
      return;
    }
  } catch {
    // ignore
  }

  if (await restoreActiveTaskById(preferredTaskId)) {
    if (isStale()) return;
    return;
  }

  await refreshLatestBaselineForPath(collectScannerForm().scanPath, { syncPreview: true });
}

function restoreActiveState(initialSnapshot = null) {
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

  const cachedSnapshot = initialSnapshot?.id
    ? initialSnapshot
    : storage.get('lastScan', null);
  const lastScan = String(cachedSnapshot?.id || '').trim() === String(activeTaskId || '').trim()
    ? cachedSnapshot
    : null;
  if (lastScan) {
    updateStats(lastScan);
    setScanStatus(lastScan.status);

    const pathEl = document.getElementById('breadcrumb-path');
    const depthEl = document.getElementById('breadcrumb-depth');
    if (pathEl) {
      pathEl.textContent = lastScan.currentPath || '...';
      pathEl.title = lastScan.currentPath || '';
    }
    if (depthEl) depthEl.textContent = `Depth ${lastScan.currentDepth || 0}`;

    const scanPct = lastScan.totalEntries > 0
      ? Math.min(100, (lastScan.processedEntries || 0) / lastScan.totalEntries * 100)
      : 0;
    const fill = document.getElementById('progress-fill');
    const label = document.getElementById('progress-pct');
    if (fill) fill.style.width = `${scanPct}%`;
    if (label) label.textContent = `${scanPct.toFixed(1)}%`;
  }
}

async function handleStart() {
  const startBtn = document.getElementById('start-btn');

  try {
    await persistScannerSettings({ showSuccessToast: false });
    const form = collectScannerForm();

    if (!form.scanPath) {
      showToast(t('scanner.path_not_configured'), 'error');
      return;
    }
    const selectedEndpoint = String(form.scanProviderEndpoint || currentSettings?.defaultProviderEndpoint || '').trim();
    if (!getApiKeyForScanEndpoint(selectedEndpoint)) {
      try {
        await ensureRequiredCredentialsConfigured({
          providerEndpoints: [selectedEndpoint],
          requireSearchApi: !!form.scanWebSearchEnabled,
          reasonText: t('scanner.api_key_required'),
        });
      } catch (err) {
        showToast(err.message, 'error');
        return;
      }
    }
    if (form.scanWebSearchEnabled && !getSearchCredentialPresence(currentSettings || {})) {
      try {
        await ensureRequiredCredentialsConfigured({
          providerEndpoints: [selectedEndpoint],
          requireSearchApi: true,
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

    await scanTaskController.startTask({
      targetPath: form.scanPath,
      targetSizeGB: form.targetSizeGB,
      baselineTaskId: latestBaselineSnapshot?.id || null,
      maxDepth: form.maxDepthUnlimited ? null : form.maxDepth,
      scanMode: 'full_rescan_incremental',
      autoAnalyze: true,
      useWebSearch: !!form.scanWebSearchEnabled,
      responseLanguage: getLang(),
    });
  } catch (err) {
    showToast(t('scanner.toast_start_failed') + getErrorMessage(err), 'error');
    if (startBtn) {
      startBtn.disabled = false;
      startBtn.innerHTML = t('scanner.start');
    }
  }
}

async function handleStop() {
  if (!activeTaskId) return;
  try {
    await scanTaskController.stopTask();
  } catch (err) {
    showToast(t('scanner.toast_stop_failed') + getErrorMessage(err), 'error');
  }
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

  refreshScanActionHint();
}

function updateStats(data) {
  const el = (id) => document.getElementById(id);
  if (el('stat-scanned')) el('stat-scanned').textContent = data.scannedCount || 0;
  if (el('stat-cleanable')) el('stat-cleanable').textContent = formatSize(data.totalCleanable || 0);
  if (el('stat-tokens')) el('stat-tokens').textContent = (data.tokenUsage?.total || 0).toLocaleString();
}
