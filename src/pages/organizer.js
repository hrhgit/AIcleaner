import {
  applyOrganize,
  browseFolder,
  connectOrganizeStream,
  getProviderModels,
  getOrganizeCapability,
  getOrganizeResult,
  getSettings,
  rollbackOrganize,
  startOrganize,
  stopOrganize,
  suggestOrganizeCategories,
} from '../utils/api.js';
import { showToast } from '../main.js';
import { t } from '../utils/i18n.js';

const PERSIST_KEYS = {
  rootPath: 'wipeout.organizer.global.root_path.v1',
  recursive: 'wipeout.organizer.global.recursive.v1',
  mode: 'wipeout.organizer.global.mode.v1',
  allowNewCategories: 'wipeout.organizer.global.allow_new_categories.v1',
  categories: 'wipeout.organizer.global.categories.v1',
  exclusions: 'wipeout.organizer.global.exclusions.v1',
  parallelism: 'wipeout.organizer.global.parallelism.v1',
  modelRouting: 'wipeout.organizer.global.model_routing.v1',
  modelSelection: 'wipeout.organizer.global.model_selection.v1',
  lastJobId: 'wipeout.organizer.global.last_job_id.v1',
};

const DEFAULT_CATEGORIES = [
  '工作学习',
  '财务票据',
  '媒体素材',
  '开发项目',
  '安装与压缩',
  '临时下载',
  '其他待定',
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
  return String(providerApiKeyMap?.[String(endpoint || '').trim()] || '').trim();
}

function readModelRoutingFromDOM() {
  const textEndpoint = document.getElementById(PROVIDER_SELECT_IDS.text)?.value?.trim() || '';
  const imageEndpoint = document.getElementById(PROVIDER_SELECT_IDS.image)?.value?.trim() || '';
  const videoEndpoint = document.getElementById(PROVIDER_SELECT_IDS.video)?.value?.trim() || '';
  const audioEndpoint = document.getElementById(PROVIDER_SELECT_IDS.audio)?.value?.trim() || '';

  return {
    text: {
      endpoint: textEndpoint,
      apiKey: getApiKeyForEndpoint(textEndpoint),
      model: document.getElementById(MODEL_SELECT_IDS.text)?.value?.trim() || '',
    },
    image: {
      endpoint: imageEndpoint,
      apiKey: getApiKeyForEndpoint(imageEndpoint),
      model: document.getElementById(MODEL_SELECT_IDS.image)?.value?.trim() || '',
    },
    video: {
      endpoint: videoEndpoint,
      apiKey: getApiKeyForEndpoint(videoEndpoint),
      model: document.getElementById(MODEL_SELECT_IDS.video)?.value?.trim() || '',
    },
    audio: {
      endpoint: audioEndpoint,
      apiKey: getApiKeyForEndpoint(audioEndpoint),
      model: document.getElementById(MODEL_SELECT_IDS.audio)?.value?.trim() || '',
    },
  };
}

function collectForm() {
  const rootPath = document.getElementById('org-root-path')?.value?.trim() || '';
  const recursive = !!document.getElementById('org-recursive')?.checked;
  const mode = document.getElementById('org-mode')?.value || 'fast';
  const allowNewCategories = !!document.getElementById('org-allow-new-categories')?.checked;
  const categories = parseListInput(document.getElementById('org-categories')?.value || '');
  const excludedPatterns = parseListInput(document.getElementById('org-exclusions')?.value || '');
  const parallelism = Number(document.getElementById('org-parallelism')?.value || 5);
  const modelRouting = readModelRoutingFromDOM();

  return {
    rootPath,
    recursive,
    mode,
    allowNewCategories,
    categories: categories.length ? categories : [...DEFAULT_CATEGORIES],
    excludedPatterns: excludedPatterns.length ? excludedPatterns : [...DEFAULT_EXCLUSIONS],
    parallelism: Number.isFinite(parallelism) ? Math.max(1, Math.min(20, Math.floor(parallelism))) : 5,
    modelRouting,
  };
}

function persistForm(data) {
  setPersisted(PERSIST_KEYS.rootPath, data.rootPath);
  setPersisted(PERSIST_KEYS.recursive, data.recursive);
  setPersisted(PERSIST_KEYS.mode, data.mode);
  setPersisted(PERSIST_KEYS.allowNewCategories, data.allowNewCategories);
  setPersisted(PERSIST_KEYS.categories, data.categories);
  setPersisted(PERSIST_KEYS.exclusions, data.excludedPatterns);
  setPersisted(PERSIST_KEYS.parallelism, data.parallelism);
  setPersisted(PERSIST_KEYS.modelRouting, data.modelRouting || {});
}

function restoreDefaults() {
  const legacyModelSelection = getPersisted(PERSIST_KEYS.modelSelection, {});
  const modelRouting = getPersisted(PERSIST_KEYS.modelRouting, null);

  const fallbackRouting = {
    text: { endpoint: '', apiKey: '', model: legacyModelSelection?.text || '' },
    image: { endpoint: '', apiKey: '', model: legacyModelSelection?.image || '' },
    video: { endpoint: '', apiKey: '', model: legacyModelSelection?.video || '' },
    audio: { endpoint: '', apiKey: '', model: legacyModelSelection?.audio || '' },
  };

  return {
    rootPath: getPersisted(PERSIST_KEYS.rootPath, ''),
    recursive: getPersisted(PERSIST_KEYS.recursive, true),
    mode: getPersisted(PERSIST_KEYS.mode, 'fast'),
    allowNewCategories: getPersisted(PERSIST_KEYS.allowNewCategories, true),
    categories: getPersisted(PERSIST_KEYS.categories, DEFAULT_CATEGORIES),
    excludedPatterns: getPersisted(PERSIST_KEYS.exclusions, DEFAULT_EXCLUSIONS),
    parallelism: getPersisted(PERSIST_KEYS.parallelism, 5),
    modelRouting: modelRouting || fallbackRouting,
  };
}

function escapeHtml(value) {
  const div = document.createElement('div');
  div.textContent = String(value ?? '');
  return div.innerHTML;
}

function setStatusText(snapshot) {
  const el = document.getElementById('org-status');
  if (!el) return;
  if (!snapshot) {
    el.textContent = t('organizer.status_idle');
    return;
  }

  const statusMap = {
    idle: t('organizer.status_idle'),
    scanning: t('organizer.status_scanning'),
    classifying: t('organizer.status_classifying'),
    stopped: t('organizer.status_stopped'),
    completed: t('organizer.status_completed'),
    moving: t('organizer.status_moving'),
    done: t('organizer.status_done'),
    error: t('organizer.status_error'),
  };

  el.textContent = statusMap[snapshot.status] || snapshot.status;
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
  const fallbackModel = snapshot?.selectedModel
    || (useDomSelections ? domSelections.selectedModels?.text : latestCapability?.selectedModel)
    || '-';
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
  const apiKey = getApiKeyForEndpoint(endpoint);
  const selectedModel = String(defaultSelection?.model || modelSelect.value || '').trim();
  const requestToken = ++modelsRequestToken[modality];

  modelSelect.disabled = true;
  modelSelect.innerHTML = `<option value="">${t('organizer.model_loading')}</option>`;

  const cacheKey = `${endpoint}|${apiKey}`;
  let models = [];
  try {
    if (endpoint) {
      if (remoteModelsCache.has(cacheKey)) {
        models = remoteModelsCache.get(cacheKey);
      } else {
        const resp = await getProviderModels(endpoint, apiKey);
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

  providerApiKeyMap = {};
  if (settings?.providerConfigs && typeof settings.providerConfigs === 'object') {
    for (const [endpoint, config] of Object.entries(settings.providerConfigs)) {
      providerApiKeyMap[String(endpoint).trim()] = String(config?.apiKey || '').trim();
    }
  }
  if (settings?.apiEndpoint && !providerApiKeyMap[settings.apiEndpoint]) {
    providerApiKeyMap[settings.apiEndpoint] = String(settings?.apiKey || '').trim();
  }

  const baseEndpoint = String(settings?.apiEndpoint || 'https://api.deepseek.com').trim();
  const baseModel = String(settings?.model || 'deepseek-chat').trim();

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
}

function renderPreview(snapshot) {
  const tbody = document.getElementById('org-preview-body');
  const empty = document.getElementById('org-preview-empty');
  if (!tbody || !empty) return;

  const preview = snapshot?.preview || [];
  const resultsMap = new Map((snapshot?.results || []).map((x) => [x.path, x]));

  if (!preview.length) {
    tbody.innerHTML = '';
    empty.style.display = '';
    return;
  }

  empty.style.display = 'none';

  tbody.innerHTML = preview.map((item, idx) => {
    const row = resultsMap.get(item.sourcePath);
    const degraded = row?.degraded ? `<span class="badge badge-warning">${t('organizer.degraded')}</span>` : '';

    return `
      <tr style="animation: slideUp 0.2s var(--ease-out) ${Math.min(idx * 0.01, 0.3)}s both;">
        <td>
          <div class="file-name">${escapeHtml(row?.name || '')}</div>
          <div class="file-path">${escapeHtml(item.sourcePath)}</div>
        </td>
        <td><span class="badge badge-info">${escapeHtml(item.category)}</span>${degraded}</td>
        <td><div class="file-path">${escapeHtml(item.targetPath)}</div></td>
      </tr>
    `;
  }).join('');
}

function renderDegradedPanel(snapshot) {
  const tbody = document.getElementById('org-degraded-body');
  const empty = document.getElementById('org-degraded-empty');
  const count = document.getElementById('org-degraded-count');
  if (!tbody || !empty || !count) return;

  const rows = (snapshot?.results || []).filter((x) => x.degraded);
  count.textContent = String(rows.length);

  if (!rows.length) {
    tbody.innerHTML = '';
    empty.style.display = '';
    return;
  }

  empty.style.display = 'none';
  tbody.innerHTML = rows.map((row, idx) => {
    const reason = Array.isArray(row.warnings) && row.warnings.length
      ? row.warnings.join(' | ')
      : t('organizer.degraded_reason_unknown');

    return `
      <tr style="animation: slideUp 0.2s var(--ease-out) ${Math.min(idx * 0.01, 0.3)}s both;">
        <td>
          <div class="file-name">${escapeHtml(row.name || '')}</div>
          <div class="file-path">${escapeHtml(row.path || '')}</div>
        </td>
        <td><div class="file-purpose">${escapeHtml(reason)}</div></td>
      </tr>
    `;
  }).join('');
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
    const running = status === 'scanning' || status === 'classifying' || status === 'moving';
    startBtn.disabled = running;
  }

  if (stopBtn) {
    const stoppable = status === 'scanning' || status === 'classifying';
    stopBtn.disabled = !stoppable;
  }

  if (applyBtn) {
    applyBtn.disabled = !(status === 'completed' || status === 'done');
  }

  if (rollbackBtn) {
    rollbackBtn.disabled = !getPersisted(PERSIST_KEYS.lastJobId, null);
  }
}

function syncCategoryInput(snapshot) {
  const next = Array.isArray(snapshot?.categories) ? snapshot.categories : [];
  if (!next.length) return;

  const textArea = document.getElementById('org-categories');
  if (!textArea) return;

  const current = parseListInput(textArea.value || '');
  const unchanged =
    current.length === next.length &&
    current.every((value, index) => value === next[index]);

  if (unchanged) return;

  textArea.value = next.join('\n');
  const form = collectForm();
  persistForm({ ...form, categories: next });
}

function syncAllowNewCategoriesInput(snapshot) {
  if (!snapshot || typeof snapshot.allowNewCategories !== 'boolean') return;
  const checkbox = document.getElementById('org-allow-new-categories');
  if (!checkbox) return;
  if (checkbox.checked === snapshot.allowNewCategories) return;
  checkbox.checked = snapshot.allowNewCategories;
}

function refreshView(snapshot) {
  latestSnapshot = snapshot || null;
  syncAllowNewCategoriesInput(snapshot);
  syncCategoryInput(snapshot);
  setStatusText(snapshot);
  renderCapability(snapshot);
  updateStats(snapshot);
  renderPreview(snapshot);
  renderDegradedPanel(snapshot);
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
      refreshView(snap);
    },
    onFileDone: () => {
      // no-op, progress snapshot already contains latest aggregate
    },
    onDone: (snap) => {
      refreshView(snap);
      showToast(t('organizer.toast_classify_done'), 'success');
    },
    onStopped: (snap) => {
      refreshView(snap);
    },
    onError: (err) => {
      showToast(`${t('organizer.toast_failed')}${err?.message || ''}`, 'error');
    },
  });
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
    showToast(`${t('organizer.toast_failed')}${err.message}`, 'error');
  } finally {
    btn.disabled = false;
    btn.textContent = t('settings.browse');
  }
}

async function handleSuggest() {
  const form = collectForm();
  if (!form.rootPath) {
    showToast(t('organizer.path_required'), 'error');
    return;
  }

  const btn = document.getElementById('org-suggest-btn');
  if (btn) {
    btn.disabled = true;
    btn.innerHTML = `<span class="spinner"></span> ${t('organizer.suggesting')}`;
  }

  try {
    const resp = await suggestOrganizeCategories({
      rootPath: form.rootPath,
      recursive: form.recursive,
      excludedPatterns: form.excludedPatterns,
      manualCategories: form.categories,
      modelRouting: form.modelRouting,
    });

    const categories = resp?.suggestedCategories || form.categories;
    const textArea = document.getElementById('org-categories');
    if (textArea) {
      textArea.value = categories.join('\n');
    }

    persistForm({ ...form, categories });
    showToast(t('organizer.toast_suggest_done'), 'success');
  } catch (err) {
    showToast(`${t('organizer.toast_failed')}${err.message}`, 'error');
  } finally {
    if (btn) {
      btn.disabled = false;
      btn.textContent = t('organizer.suggest_categories');
    }
  }
}

async function handleStart() {
  const form = collectForm();
  if (!form.rootPath) {
    showToast(t('organizer.path_required'), 'error');
    return;
  }

  if (form.mode === 'deep') {
    showToast(t('organizer.deep_warning'), 'info');
  }

  persistForm(form);

  const btn = document.getElementById('org-start-btn');
  if (btn) {
    btn.disabled = true;
    btn.innerHTML = `<span class="spinner"></span> ${t('organizer.starting')}`;
  }

  try {
    const result = await startOrganize(form);
    activeTaskId = result.taskId;
    latestCapability = {
      selectedModel: result.selectedModel,
      selectedModels: result.selectedModels,
      selectedProviders: result.selectedProviders,
      supportsMultimodal: result.supportsMultimodal,
    };
    renderCapability();
    setPersisted('wipeout.organizer.global.last_task_id.v1', activeTaskId);
    connectTaskStream(activeTaskId);
    showToast(t('organizer.toast_started'), 'success');
  } catch (err) {
    showToast(`${t('organizer.toast_failed')}${err.message}`, 'error');
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

    const summary = result?.manifest?.summary;
    if (summary) {
      showToast(
        t('organizer.toast_apply_done') + ` (${summary.moved}/${summary.total})`,
        summary.failed > 0 ? 'info' : 'success'
      );
    } else {
      showToast(t('organizer.toast_apply_done'), 'success');
    }

    const snapshot = await getOrganizeResult(activeTaskId);
    refreshView(snapshot);
  } catch (err) {
    showToast(`${t('organizer.toast_failed')}${err.message}`, 'error');
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
    showToast(t('organizer.toast_stopped'), 'info');
  } catch (err) {
    showToast(`${t('organizer.toast_stop_failed')}${err.message}`, 'error');
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
    if (summary) {
      showToast(
        `${t('organizer.toast_rollback_done')} (${summary.rolledBack}/${summary.total})`,
        summary.failed > 0 ? 'info' : 'success'
      );
    } else {
      showToast(t('organizer.toast_rollback_done'), 'success');
    }
  } catch (err) {
    showToast(`${t('organizer.toast_failed')}${err.message}`, 'error');
  } finally {
    if (btn) {
      btn.disabled = false;
      btn.textContent = t('organizer.rollback');
    }
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
    showToast(`${t('organizer.toast_failed')}${err.message}`, 'error');
  } finally {
    if (btn) {
      btn.disabled = false;
    }
  }
}

function bindPersistenceListeners() {
  [
    'org-root-path',
    'org-recursive',
    'org-mode',
    'org-allow-new-categories',
    'org-categories',
    'org-exclusions',
    'org-parallelism',
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
      'org-recursive',
      'org-allow-new-categories',
      'org-mode',
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
      renderCapability();
    });

    modelSelect?.addEventListener('change', () => {
      persistForm(collectForm());
      renderCapability();
    });
  }
}

export async function renderOrganizer(container) {
  const defaults = restoreDefaults();

  container.innerHTML = `
    <div class="page-header animate-in">
      <h1 class="page-title">${t('organizer.title')}</h1>
      <p class="page-subtitle">${t('organizer.subtitle')}</p>
    </div>

    <div class="card animate-in mb-24" style="animation-delay: 0.05s">
      <div class="card-header">
        <h2 class="card-title">${t('organizer.config')}</h2>
        <span class="badge badge-info" id="org-status">${t('organizer.status_idle')}</span>
      </div>

      <div class="form-group">
        <label class="form-label">${t('organizer.root_path')}</label>
        <div style="display:flex;gap:8px;align-items:center;">
          <input id="org-root-path" class="form-input" style="flex:1;" value="${escapeHtml(defaults.rootPath)}" placeholder="C:\\Users\\..." />
          <button id="org-browse-btn" class="btn btn-secondary" type="button">${t('settings.browse')}</button>
        </div>
      </div>

      <div class="grid-2">
        <div class="form-group">
          <label class="form-label">${t('organizer.scope')}</label>
          <label style="display:flex;align-items:center;gap:8px;">
            <input id="org-recursive" type="checkbox" ${defaults.recursive ? 'checked' : ''} />
            <span>${t('organizer.scope_recursive')}</span>
          </label>
        </div>
        <div class="form-group">
          <label class="form-label">${t('organizer.mode')}</label>
          <select id="org-mode" class="form-input">
            <option value="fast" ${defaults.mode === 'fast' ? 'selected' : ''}>${t('organizer.mode_fast')}</option>
            <option value="balanced" ${defaults.mode === 'balanced' ? 'selected' : ''}>${t('organizer.mode_balanced')}</option>
            <option value="deep" ${defaults.mode === 'deep' ? 'selected' : ''}>${t('organizer.mode_deep')}</option>
          </select>
        </div>
      </div>

      <div class="grid-2">
        <div class="form-group">
          <label class="form-label">${t('organizer.parallelism')}</label>
          <input id="org-parallelism" type="number" min="1" max="20" class="form-input no-spin" value="${Number(defaults.parallelism) || 5}" />
        </div>
        <div class="form-group">
          <label class="form-label">${t('organizer.cost_notice')}</label>
          <div class="form-hint" id="org-deep-warning">${t('organizer.deep_hint')}</div>
        </div>
      </div>

      <div class="form-group">
        <label class="form-label">${t('organizer.model_routing')}</label>
        <div class="grid-2">
          <div class="form-group">
            <label class="form-label">${t('organizer.model_text')}</label>
            <div class="provider-model-inline">
              <select id="${PROVIDER_SELECT_IDS.text}" class="form-input"></select>
              <select id="${MODEL_SELECT_IDS.text}" class="form-input"></select>
            </div>
            <div class="form-hint">${t('settings.provider')} + ${t('settings.model')}</div>
          </div>
          <div class="form-group">
            <label class="form-label">${t('organizer.model_image')}</label>
            <div class="provider-model-inline">
              <select id="${PROVIDER_SELECT_IDS.image}" class="form-input"></select>
              <select id="${MODEL_SELECT_IDS.image}" class="form-input"></select>
            </div>
            <div class="form-hint">${t('settings.provider')} + ${t('settings.model')}</div>
          </div>
          <div class="form-group">
            <label class="form-label">${t('organizer.model_video')}</label>
            <div class="provider-model-inline">
              <select id="${PROVIDER_SELECT_IDS.video}" class="form-input"></select>
              <select id="${MODEL_SELECT_IDS.video}" class="form-input"></select>
            </div>
            <div class="form-hint">${t('settings.provider')} + ${t('settings.model')}</div>
          </div>
          <div class="form-group">
            <label class="form-label">${t('organizer.model_audio')}</label>
            <div class="provider-model-inline">
              <select id="${PROVIDER_SELECT_IDS.audio}" class="form-input"></select>
              <select id="${MODEL_SELECT_IDS.audio}" class="form-input"></select>
            </div>
            <div class="form-hint">${t('settings.provider')} + ${t('settings.model')}</div>
          </div>
        </div>
        <div class="form-hint">${t('organizer.model_routing_hint')}</div>
        <div class="form-hint">${t('settings.api_key_managed_hint')}</div>
        <div class="flex items-center gap-8" style="margin-top:8px;">
          <button id="${COPY_TEXT_ROUTE_BTN_ID}" class="btn btn-secondary" type="button">${t('organizer.copy_text_route')}</button>
        </div>
      </div>

      <div class="grid-2">
        <div class="form-group">
          <label class="form-label">${t('organizer.current_model')}</label>
          <div id="org-model-name" class="form-hint mono">-</div>
        </div>
        <div class="form-group">
          <label class="form-label">${t('organizer.multimodal')}</label>
          <span id="org-mm-badge" class="badge badge-danger">${t('organizer.multimodal_unknown')}</span>
        </div>
      </div>

      <div class="grid-2">
        <div class="form-group">
          <label class="form-label">${t('organizer.categories')}</label>
          <textarea id="org-categories" class="form-input" rows="8">${escapeHtml((defaults.categories || []).join('\n'))}</textarea>
          <div class="form-hint">${t('organizer.categories_hint')}</div>
          <label style="display:flex;align-items:center;gap:8px;margin-top:8px;">
            <input id="org-allow-new-categories" type="checkbox" ${defaults.allowNewCategories ? 'checked' : ''} />
            <span>${t('organizer.allow_new_categories')}</span>
          </label>
          <div class="form-hint">${t('organizer.allow_new_categories_hint')}</div>
        </div>
        <div class="form-group">
          <label class="form-label">${t('organizer.exclusions')}</label>
          <textarea id="org-exclusions" class="form-input" rows="8">${escapeHtml((defaults.excludedPatterns || []).join('\n'))}</textarea>
          <div class="form-hint">${t('organizer.exclusions_hint')}</div>
        </div>
      </div>

      <div class="flex items-center gap-16">
        <button id="org-suggest-btn" class="btn btn-ghost" type="button">${t('organizer.suggest_categories')}</button>
        <button id="org-start-btn" class="btn btn-primary" type="button">${t('organizer.start')}</button>
        <button id="org-stop-btn" class="btn btn-danger" type="button" disabled>${t('organizer.stop')}</button>
        <button id="org-apply-btn" class="btn btn-success" type="button" disabled>${t('organizer.apply_move')}</button>
        <button id="org-rollback-btn" class="btn btn-secondary" type="button">${t('organizer.rollback')}</button>
      </div>
    </div>

    <div class="stats-grid animate-in" style="animation-delay: 0.1s">
      <div class="stat-card">
        <span class="stat-label">${t('organizer.total_files')}</span>
        <span class="stat-value" id="org-total">0</span>
      </div>
      <div class="stat-card">
        <span class="stat-label">${t('organizer.done_files')}</span>
        <span class="stat-value success" id="org-done">0</span>
      </div>
      <div class="stat-card">
        <span class="stat-label">Token</span>
        <span class="stat-value warning" id="org-token">0</span>
      </div>
      <div class="stat-card">
        <span class="stat-label">${t('organizer.degraded')}</span>
        <span class="stat-value danger" id="org-degraded">0</span>
      </div>
    </div>

    <div class="card animate-in mb-24" style="animation-delay:0.12s;">
      <div class="card-header">
        <h2 class="card-title">${t('organizer.progress')}</h2>
        <span id="org-progress-pct" class="badge badge-info">0.0%</span>
      </div>
      <div class="progress-bar">
        <div id="org-progress-fill" class="progress-fill" style="width:0%;"></div>
      </div>
    </div>

    <div class="card animate-in" style="animation-delay: 0.15s; padding: 0; overflow: hidden;">
      <div class="card-header" style="padding: 16px 20px; margin-bottom: 0; border-bottom: 1px solid var(--bg-glass-border);">
        <h2 class="card-title">${t('organizer.preview_title')}</h2>
      </div>
      <div style="overflow-x:auto;">
        <table class="data-table">
          <thead>
            <tr>
              <th>${t('organizer.source')}</th>
              <th style="width: 180px;">${t('organizer.category')}</th>
              <th>${t('organizer.target')}</th>
            </tr>
          </thead>
          <tbody id="org-preview-body"></tbody>
        </table>
      </div>
      <div id="org-preview-empty" class="empty-state" style="padding: 32px;">
        <div class="empty-state-icon">📁</div>
        <div class="empty-state-text">${t('organizer.preview_empty')}</div>
      </div>
    </div>

    <div class="card animate-in mt-24" style="animation-delay: 0.18s; padding: 0; overflow: hidden;">
      <div class="card-header" style="padding: 16px 20px; margin-bottom: 0; border-bottom: 1px solid var(--bg-glass-border);">
        <h2 class="card-title">${t('organizer.degraded_panel_title')}</h2>
        <span class="badge badge-warning">${t('organizer.degraded')}: <span id="org-degraded-count">0</span></span>
      </div>
      <div style="overflow-x:auto;">
        <table class="data-table">
          <thead>
            <tr>
              <th>${t('organizer.source')}</th>
              <th>${t('organizer.degraded_reason')}</th>
            </tr>
          </thead>
          <tbody id="org-degraded-body"></tbody>
        </table>
      </div>
      <div id="org-degraded-empty" class="empty-state" style="padding: 24px;">
        <div class="empty-state-text">${t('organizer.degraded_empty')}</div>
      </div>
    </div>
  `;

  document.getElementById('org-browse-btn')?.addEventListener('click', handleBrowse);
  document.getElementById('org-suggest-btn')?.addEventListener('click', handleSuggest);
  document.getElementById('org-start-btn')?.addEventListener('click', handleStart);
  document.getElementById('org-stop-btn')?.addEventListener('click', handleStop);
  document.getElementById('org-apply-btn')?.addEventListener('click', handleApply);
  document.getElementById('org-rollback-btn')?.addEventListener('click', handleRollback);
  document.getElementById(COPY_TEXT_ROUTE_BTN_ID)?.addEventListener('click', handleCopyTextRoute);

  await initModelRoutingFields(defaults.modelRouting);
  bindModelRoutingListeners();
  bindPersistenceListeners();
  renderCapability();

  try {
    latestCapability = await getOrganizeCapability();
  } catch {
    latestCapability = null;
  }
  renderCapability();

  // Try to reconnect to a running task.
  const lastTaskId = getPersisted('wipeout.organizer.global.last_task_id.v1', null);
  if (lastTaskId) {
    try {
      const snapshot = await getOrganizeResult(lastTaskId);
      activeTaskId = lastTaskId;
      refreshView(snapshot);

      if (['scanning', 'classifying', 'moving'].includes(snapshot.status)) {
        connectTaskStream(lastTaskId);
      }
    } catch {
      refreshView(null);
    }
  } else {
    refreshView(null);
  }
}







