/**
 * src/pages/scanner.js
 * Scanner page with merged settings + scan controls.
 */
import {
  analyzeScanFolder,
  browseFolder,
  connectScanStream,
  getActiveScan,
  getProviderModels,
  getPrivilegeStatus,
  getScanTree,
  getSettings,
  requestElevation,
  saveSettings,
  startScan,
  stopScan,
} from '../utils/api.js';
import { formatSize } from '../utils/storage.js';
import * as storage from '../utils/storage.js';
import { showToast } from '../main.js';
import { t } from '../utils/i18n.js';

let activeTaskId = null;
let latestTaskId = null;
let activeEventSource = null;
let logEntries = [];
let currentSettings = null;
let candidateFolders = [];
const SCANNER_FORM_DRAFT_KEY = 'wipeout.scanner.global.form.v1';
const SCANNER_FORM_DRAFT_VERSION = 1;
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
  return String(scanProviderApiKeyMap?.[String(endpoint || '').trim()] || '').trim();
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

  const apiKey = getApiKeyForScanEndpoint(endpoint);
  const cacheKey = `${endpoint}|${apiKey}`;
  let models = [];
  try {
    if (endpoint) {
      if (scanRemoteModelsCache.has(cacheKey)) {
        models = scanRemoteModelsCache.get(cacheKey);
      } else {
        const resp = await getProviderModels(endpoint, apiKey);
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

  scanProviderApiKeyMap = {};
  if (settings?.providerConfigs && typeof settings.providerConfigs === 'object') {
    for (const [endpoint, config] of Object.entries(settings.providerConfigs)) {
      scanProviderApiKeyMap[String(endpoint).trim()] = String(config?.apiKey || '').trim();
    }
  }
  if (settings?.apiEndpoint && !scanProviderApiKeyMap[settings.apiEndpoint]) {
    scanProviderApiKeyMap[settings.apiEndpoint] = String(settings?.apiKey || '').trim();
  }

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
  if (remoteSearch.apiKey) {
    mergedSearchApi.apiKey = remoteSearch.apiKey;
  }

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
    const selectedApiKey = String(
      selectedConfig.apiKey
      || scanProviderApiKeyMap[selectedEndpoint]
      || (selectedEndpoint === currentSettings?.apiEndpoint ? currentSettings?.apiKey : '')
      || ''
    ).trim();
    providerConfigs[selectedEndpoint] = {
      ...selectedConfig,
      endpoint: selectedEndpoint,
      name: String(selectedConfig.name || getProviderLabel(selectedEndpoint)),
      apiKey: selectedApiKey,
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
    if (existingSearchApi.apiKey) {
      searchApi.apiKey = existingSearchApi.apiKey;
    }

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
      apiKey: selectedApiKey,
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
  const statusEl = document.getElementById('stat-status');
  if (!statusEl) return;

  const statusMap = {
    scanning: t('scanner.scanning'),
    analyzing: t('scanner.analyzing'),
    idle: t('scanner.not_set'),
    done: t('scanner.completed').split('!')[0],
    stopped: t('scanner.stopped'),
    error: t('toast.error'),
  };
  statusEl.textContent = statusMap[status] || status || t('scanner.not_set');
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

  sizeSlider?.addEventListener('input', () => {
    if (sizeInput) sizeInput.value = clampNumber(sizeSlider.value, 0.1, 100, 1).toFixed(1);
    persistScannerFormDraft();
  });
  sizeInput?.addEventListener('input', () => {
    const val = clampNumber(sizeInput.value, 0.1, 100, 1);
    if (sizeSlider) sizeSlider.value = String(val);
    persistScannerFormDraft();
  });
  sizeInput?.addEventListener('blur', () => {
    const val = clampNumber(sizeInput.value, 0.1, 100, 1);
    sizeInput.value = val.toFixed(1);
    if (sizeSlider) sizeSlider.value = String(val);
    persistScannerFormDraft();
  });

  depthSlider?.addEventListener('input', () => {
    if (depthInput) depthInput.value = String(Math.floor(clampNumber(depthSlider.value, 1, 10, 5)));
    persistScannerFormDraft();
  });
  depthInput?.addEventListener('input', () => {
    const val = Math.floor(clampNumber(depthInput.value, 1, 10, 5));
    if (depthSlider) depthSlider.value = String(val);
    persistScannerFormDraft();
  });
  depthInput?.addEventListener('blur', () => {
    const val = Math.floor(clampNumber(depthInput.value, 1, 10, 5));
    depthInput.value = String(val);
    if (depthSlider) depthSlider.value = String(val);
    persistScannerFormDraft();
  });

  document.getElementById('scan-path')?.addEventListener('input', (event) => {
    updatePathDisplay(event.target?.value || '');
    persistScannerFormDraft();
  });

  document.getElementById('scan-enable-web-search')?.addEventListener('change', () => {
    persistScannerFormDraft();
  });
  document.getElementById('scan-provider')?.addEventListener('change', async () => {
    await initScanModelSelectors();
    persistScannerFormDraft();
  });
  document.getElementById('scan-model')?.addEventListener('change', () => {
    persistScannerFormDraft();
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
        persistScannerFormDraft();
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
      await requestElevation();
      showToast(t('settings.elevation_uac_prompt'), 'info');
      if (adminStatusEl) {
        adminStatusEl.textContent = t('settings.elevation_restarting');
        adminStatusEl.style.color = 'var(--accent-info)';
      }
    } catch (err) {
      showToast(t('settings.elevation_failed') + err.message, 'error');
      await refreshPrivilegeStatus();
    }
  });
}

function showManualAnalyzePanel(show) {
  const panel = document.getElementById('manual-analyze-card');
  if (panel) panel.style.display = show ? '' : 'none';
}

function setManualAnalyzeBusy(busy) {
  const analyzeBtn = document.getElementById('manual-analyze-btn');
  const refreshBtn = document.getElementById('manual-refresh-btn');
  const startBtn = document.getElementById('start-btn');
  const stopBtn = document.getElementById('stop-btn');

  if (analyzeBtn) {
    analyzeBtn.disabled = !!busy;
    analyzeBtn.innerHTML = busy
      ? `<span class="spinner"></span> ${t('scanner.manual_analyzing')}`
      : t('scanner.manual_analyze_btn');
  }
  if (refreshBtn) {
    refreshBtn.disabled = !!busy;
  }
  if (startBtn) {
    startBtn.disabled = !!busy;
  }
  if (stopBtn && busy) {
    stopBtn.disabled = true;
  } else if (stopBtn) {
    stopBtn.disabled = false;
  }
}

function renderFolderCandidates() {
  const listEl = document.getElementById('manual-candidate-list');
  if (!listEl) return;

  if (!candidateFolders.length) {
    listEl.innerHTML = `<div class="form-hint">${t('scanner.manual_candidates_empty')}</div>`;
    return;
  }

  listEl.innerHTML = candidateFolders
    .map((item) => `
      <div style="display:flex; align-items:center; justify-content:space-between; gap:8px; padding:8px 0; border-bottom:1px solid rgba(255,255,255,0.06);">
        <div style="min-width:0;">
          <div style="font-weight:600; font-size:0.85rem; white-space:nowrap; overflow:hidden; text-overflow:ellipsis;">${escHtml(item.name)}</div>
          <div class="form-hint mono" style="white-space:nowrap; overflow:hidden; text-overflow:ellipsis;">${escHtml(item.path)}</div>
        </div>
        <div style="display:flex; align-items:center; gap:8px; flex-shrink:0;">
          <span class="badge badge-info">${formatSize(item.size || 0)}</span>
          <button class="btn btn-secondary manual-candidate-analyze-btn" data-path="${escHtml(item.path)}">${t('scanner.manual_analyze_btn')}</button>
        </div>
      </div>
    `)
    .join('');

  document.querySelectorAll('.manual-candidate-analyze-btn').forEach((btn) => {
    btn.addEventListener('click', async () => {
      const folderPath = String(btn.dataset.path || '').trim();
      if (!folderPath) return;
      const input = document.getElementById('manual-folder-path');
      if (input) input.value = folderPath;
      await handleManualAnalyze(folderPath);
    });
  });
}

async function refreshFolderCandidates(pathOverride = '') {
  if (!latestTaskId) return;

  const hintEl = document.getElementById('manual-candidate-hint');
  if (hintEl) hintEl.textContent = t('scanner.manual_candidates_loading');

  try {
    const lastScan = storage.get('lastScan', null);
    const rootPath = String(pathOverride || lastScan?.targetPath || lastScan?.currentPath || '').trim();
    if (!rootPath) {
      candidateFolders = [];
      renderFolderCandidates();
      if (hintEl) hintEl.textContent = t('scanner.manual_candidates_empty');
      return;
    }

    const tree = await getScanTree(latestTaskId, {
      path: rootPath,
      dirsOnly: true,
      limit: 50,
    });
    candidateFolders = Array.isArray(tree?.children) ? tree.children : [];
    renderFolderCandidates();
    if (hintEl) hintEl.textContent = t('scanner.manual_candidates_hint');
  } catch (err) {
    candidateFolders = [];
    renderFolderCandidates();
    if (hintEl) hintEl.textContent = t('scanner.manual_candidates_failed') + err.message;
  }
}

async function handleManualAnalyze(folderPathArg = '') {
  if (!latestTaskId) {
    showToast(t('scanner.manual_no_task'), 'error');
    return;
  }

  const input = document.getElementById('manual-folder-path');
  const folderPath = String(folderPathArg || input?.value || '').trim();
  if (!folderPath) {
    showToast(t('scanner.manual_path_required'), 'error');
    return;
  }

  setManualAnalyzeBusy(true);
  addLog('analyzing', `${t('scanner.manual_start')} ${folderPath}`);

  try {
    const response = await analyzeScanFolder(latestTaskId, folderPath);
    const snap = response?.snapshot || null;
    if (!snap) {
      throw new Error('Empty snapshot');
    }

    updateStats(snap);
    setScanStatus(snap.status);
    storage.set('lastScan', snap);
    storage.set('scanResults', snap.deletable || []);
    addLog('found', t('scanner.manual_done').replace('{count}', snap.deletableCount || 0));
    showToast(t('scanner.manual_done').replace('{count}', snap.deletableCount || 0), 'success');
    await refreshFolderCandidates();
  } catch (err) {
    addLog('analyzing', `${t('toast.error')}: ${err.message}`);
    showToast(t('scanner.manual_failed') + err.message, 'error');
  } finally {
    setManualAnalyzeBusy(false);
  }
}

export async function renderScanner(container) {
  const lastScan = storage.get('lastScan', null);
  const draftState = readScannerFormDraft();
  latestTaskId = storage.get('lastScanTaskId', null) || lastScan?.id || null;
  candidateFolders = [];

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
        <div class="form-hint">${t('settings.api_key_managed_hint')}</div>
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
        <button id="clear-log-btn" class="btn btn-ghost" style="padding: 6px 12px; font-size: 0.75rem;">Clear</button>
      </div>
      <div class="scan-activity" id="scan-activity-hidden" style="display:none;">
        <div class="scan-log" id="scan-log"></div>
      </div>
      <div id="scan-empty" class="empty-state" style="padding:30px;">
        <div class="empty-state-icon">...</div>
        <div class="empty-state-text">${t('scanner.prepare')}</div>
        <div class="empty-state-hint">${t('scanner.not_set')}</div>
      </div>
    </div>

    <div id="manual-analyze-card" class="card animate-in mb-24" style="animation-delay: 0.23s; display:none;">
      <div class="card-header">
        <h2 class="card-title">${t('scanner.manual_title')}</h2>
        <button id="manual-refresh-btn" class="btn btn-ghost" style="padding: 6px 12px; font-size: 0.75rem;">${t('scanner.manual_refresh')}</button>
      </div>
      <div class="form-group">
        <label class="form-label">${t('scanner.manual_path_label')}</label>
        <div style="display:flex; gap:8px;">
          <input id="manual-folder-path" class="form-input mono" placeholder="C:\\path\\to\\folder" />
          <button id="manual-analyze-btn" class="btn btn-primary">${t('scanner.manual_analyze_btn')}</button>
        </div>
        <div id="manual-candidate-hint" class="form-hint">${t('scanner.manual_candidates_hint')}</div>
      </div>
      <div id="manual-candidate-list"></div>
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
  }
  await refreshPrivilegeStatus();

  try {
    const remoteSettings = await getSettings();
    currentSettings = mergeSettingsWithDraft(remoteSettings, draftState);
    applySettingsToForm(currentSettings);
    await initScanProviderFields(currentSettings);
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

  if (lastScan) {
    updateStats(lastScan);
    setScanStatus(lastScan.status);
    if (['done', 'analyzing'].includes(lastScan.status)) {
      showManualAnalyzePanel(true);
      refreshFolderCandidates();
    }
  }

  document.getElementById('start-btn')?.addEventListener('click', handleStart);
  document.getElementById('stop-btn')?.addEventListener('click', handleStop);
  document.getElementById('clear-log-btn')?.addEventListener('click', () => {
    logEntries = [];
    const logEl = document.getElementById('scan-log');
    if (logEl) logEl.innerHTML = '';
  });
  document.getElementById('manual-refresh-btn')?.addEventListener('click', () => refreshFolderCandidates());
  document.getElementById('manual-analyze-btn')?.addEventListener('click', () => handleManualAnalyze());

  if (activeTaskId) {
    restoreActiveState();
    return;
  }

  try {
    const activeTasks = await getActiveScan();
    if (activeTasks.length > 0) {
      const task = activeTasks[0];
      activeTaskId = task.taskId;
      latestTaskId = task.taskId;
      storage.set('lastScanTaskId', latestTaskId);
      updateStats(task);
      setScanStatus(task.status);
      storage.set('lastScan', task);
      if (task.status === 'done') {
        showManualAnalyzePanel(true);
        refreshFolderCandidates();
      }
      restoreActiveState();
    }
  } catch {
    // ignore
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
    log.innerHTML = '';
    for (const entry of logEntries) {
      const el = document.createElement('div');
      el.className = `scan-log-entry ${entry.type}`;
      el.innerHTML = `
        <span class="log-icon">${entry.type === 'found' ? '+' : entry.type === 'analyzing' ? '*' : '-'}</span>
        <span style="color: var(--text-muted); margin-right: 6px;">[${entry.time}]</span>
        <span>${entry.text}</span>
      `;
      log.appendChild(el);
    }
    log.scrollTop = log.scrollHeight;
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

  showManualAnalyzePanel(lastScan?.status === 'done');
  if (lastScan?.status === 'done') {
    refreshFolderCandidates();
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

    if (startBtn) {
      startBtn.disabled = true;
      startBtn.innerHTML = `<span class="spinner"></span> ${t('scanner.prepare')}`;
    }

    const result = await startScan({
      targetPath: form.scanPath,
      targetSizeGB: form.targetSizeGB,
      maxDepth: form.maxDepth,
    });

    activeTaskId = result.taskId;
    latestTaskId = result.taskId;
    storage.set('lastScanTaskId', latestTaskId);
    showManualAnalyzePanel(false);
    candidateFolders = [];
    renderFolderCandidates();

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
    showManualAnalyzePanel(false);
    addLog('analyzing', `${t('scanner.analyzing')}: ${data.currentPath}`);
  } else if (data.status === 'scanning') {
    showManualAnalyzePanel(false);
    addLog('scanning', `${t('scanner.scanning')}: ${data.currentPath}`);
  } else if (data.status === 'done') {
    showManualAnalyzePanel(true);
  }
}

function handleFound(item) {
  addLog('found', `${t('results.safe_to_clean')}: ${item.name} (${formatSize(item.size)}) - ${item.reason}`);
}

function handleDone(data) {
  if (data?.id) {
    latestTaskId = data.id;
    storage.set('lastScanTaskId', latestTaskId);
  }

  updateStats(data);
  setScanStatus('done');
  storage.set('lastScan', data);
  storage.set('scanResults', data.deletable);
  resetButtons();
  showManualAnalyzePanel(true);
  refreshFolderCandidates();

  const fill = document.getElementById('progress-fill');
  const label = document.getElementById('progress-pct');
  if (fill) fill.style.width = '100%';
  if (label) label.textContent = '100.0%';

  addLog('found', t('scanner.completed').replace('{count}', data.deletableCount));
  showToast(`${t('scanner.completed').replace('{count}', data.deletableCount)} ${t('scanner.manual_ready_hint')}`, 'success');
}

function handleError(err) {
  resetButtons();
  setScanStatus('error');
  showManualAnalyzePanel(false);
  addLog('analyzing', `${t('toast.error')}: ${err.message || t('toast.error')}`);
  showToast(t('toast.error'), 'error');
}

function handleStopped(data) {
  if (data?.id) {
    latestTaskId = data.id;
    storage.set('lastScanTaskId', latestTaskId);
  }
  updateStats(data);
  setScanStatus('stopped');
  storage.set('lastScan', data);
  storage.set('scanResults', data.deletable);
  resetButtons();
  showManualAnalyzePanel(false);
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
  const log = document.getElementById('scan-log');
  if (!log) return;

  const entry = document.createElement('div');
  entry.className = `scan-log-entry ${type}`;
  const time = new Date().toLocaleTimeString('zh-CN', { hour12: false });
  const iconMap = { found: '+', analyzing: '*', agent_call: '>', agent_response: '<' };

  entry.innerHTML = `
    <span class="log-icon">${iconMap[type] || '-'}</span>
    <span style="color: var(--text-muted); margin-right: 6px;">[${time}]</span>
    <span>${text}</span>
  `;
  log.appendChild(entry);
  log.scrollTop = log.scrollHeight;

  logEntries.push({ type, text, time });
  while (log.children.length > 200) {
    log.removeChild(log.firstChild);
  }
}

function addDetailLog(type, summary, detailHtml) {
  const log = document.getElementById('scan-log');
  if (!log) return;

  const wrapper = document.createElement('div');
  wrapper.className = `scan-log-entry ${type}`;
  const time = new Date().toLocaleTimeString('zh-CN', { hour12: false });
  const iconMap = { agent_call: '>', agent_response: '<' };

  wrapper.innerHTML = `
    <span class="log-icon">${iconMap[type] || '*'}</span>
    <div style="flex: 1; min-width: 0;">
      <div class="log-detail-header" style="cursor: pointer; user-select: none; display: flex; align-items: center; gap: 6px;">
        <span style="color: var(--text-muted); margin-right: 4px;">[${time}]</span>
        <span class="log-detail-arrow" style="transition: transform 0.2s; display: inline-block; font-size: 0.65rem;">></span>
        <span>${summary}</span>
      </div>
      <div class="log-detail-body" style="display: none; margin-top: 8px; padding: 10px 12px; background: rgba(0,0,0,0.35); border-radius: 6px; border: 1px solid rgba(255,255,255,0.06); font-size: 0.72rem; line-height: 1.7; word-break: break-all; white-space: pre-wrap; max-height: 600px; overflow-y: auto; color: var(--text-secondary);">
        ${detailHtml}
      </div>
    </div>
  `;

  const header = wrapper.querySelector('.log-detail-header');
  const body = wrapper.querySelector('.log-detail-body');
  const arrow = wrapper.querySelector('.log-detail-arrow');
  header?.addEventListener('click', () => {
    if (!body || !arrow) return;
    const open = body.style.display !== 'none';
    body.style.display = open ? 'none' : 'block';
    arrow.style.transform = open ? 'rotate(0deg)' : 'rotate(90deg)';
  });

  log.appendChild(wrapper);
  log.scrollTop = log.scrollHeight;

  logEntries.push({ type, text: summary, time });
  while (log.children.length > 200) {
    log.removeChild(log.firstChild);
  }
}

function handleAgentCall(data) {
  const entryList = (data.entries || [])
    .map((e) => `- [${e.type}] ${e.name} (${formatSize(e.size)})`)
    .join('\n');

  const detailHtml = `
    <div style="margin-bottom: 8px;"><strong>Path:</strong> ${escHtml(data.dirPath)}</div>
    <div style="margin-bottom: 8px;"><strong>Batch #${data.batchIndex}</strong> (${data.batchSize} items)</div>
    <div style="margin-bottom: 4px;"><strong>Entries</strong></div>
    <div style="padding-left: 8px; border-left: 2px solid rgba(6, 182, 212, 0.3);">${escHtml(entryList)}</div>
  `;

  addDetailLog('agent_call', `LLM call - batch #${data.batchIndex}, ${data.batchSize} items`, detailHtml);
}

function handleAgentResponse(data) {
  const elapsed = Number(data.elapsed || 0) / 1000;
  const classLabels = { safe_to_delete: 'safe_to_delete', suspicious: 'suspicious', keep: 'keep' };
  const classStr = Object.entries(data.classifications || {})
    .map(([k, v]) => `${classLabels[k] || k}: ${v}`)
    .join(', ');

  let detailSections = '';
  detailSections += `<div style="margin-bottom: 10px;">
    <strong>Model:</strong> ${escHtml(data.model)} | <strong>Elapsed:</strong> ${elapsed.toFixed(1)}s | <strong>Token:</strong> ${(data.tokenUsage?.total || 0).toLocaleString()}
  </div>`;

  if (classStr) {
    detailSections += `<div style="margin-bottom: 10px;"><strong>Classification:</strong> ${classStr}</div>`;
  }

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
    `${statusIcon} LLM response - ${elapsed.toFixed(1)}s, ${data.resultsCount} items (${classStr || 'none'})`,
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
