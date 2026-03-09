import {
  getCredentials,
  getProviderModels,
  getSettings,
  saveCredentials,
  saveSettings,
} from '../utils/api.js';
import { showToast } from '../main.js';
import {
  refreshCredentialsStatus,
  registerCredentialsStatusChangeHandler,
} from '../utils/secret-ui.js';
import { registerLangChangeHandler, t } from '../utils/i18n.js';

const PROVIDER_PRESETS = [
  { name: 'DeepSeek', endpoint: 'https://api.deepseek.com' },
  { name: 'OpenAI', endpoint: 'https://api.openai.com/v1' },
  { name: 'Google Gemini', endpoint: 'https://generativelanguage.googleapis.com/v1beta/openai/' },
  { name: 'Qwen', endpoint: 'https://dashscope.aliyuncs.com/compatible-mode/v1' },
  { name: 'GLM', endpoint: 'https://open.bigmodel.cn/api/paas/v4' },
  { name: 'Moonshot', endpoint: 'https://api.moonshot.cn/v1' },
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

const remoteModelsCache = new Map();
const requestTokens = new Map();
const state = {
  providers: [],
  defaultProviderEndpoint: '',
  credentialsStatus: {
    providerHasApiKey: {},
    searchApiHasKey: false,
  },
  editableCredentials: {
    providerSecrets: {},
    searchApiKey: '',
  },
  dirtyCredentials: {
    providerSecrets: {},
    searchApiKey: false,
  },
  initialModalSnapshot: null,
  searchApi: {
    provider: 'tavily',
    enabled: false,
    apiKey: '',
    scopes: {
      scan: false,
      classify: false,
      organizer: false,
    },
  },
};

let modalEl;
let listEl;
let openBtn;
let closeBtn;
let cancelBtn;
let saveBtn;
let closeConfirmEl;
let closeConfirmCancelBtn;
let closeConfirmSaveBtn;
let initialized = false;

function escapeHtml(value) {
  return String(value || '')
    .replaceAll('&', '&amp;')
    .replaceAll('<', '&lt;')
    .replaceAll('>', '&gt;')
    .replaceAll('"', '&quot;')
    .replaceAll("'", '&#39;');
}

function getErrorMessage(err) {
  if (typeof err === 'string') return err;
  if (err && typeof err.message === 'string' && err.message.trim()) return err.message;
  try {
    return JSON.stringify(err);
  } catch {
    return String(err || 'Unknown error');
  }
}

function normalizeModels(models) {
  const seen = new Set();
  const normalized = [];
  for (const item of models || []) {
    const value = String(item?.value || '').trim();
    if (!value || seen.has(value)) continue;
    seen.add(value);
    normalized.push({
      value,
      label: String(item?.label || value),
    });
  }
  return normalized;
}

function fallbackModelsByEndpoint(endpoint) {
  return normalizeModels(PROVIDER_MODELS[String(endpoint || '').trim()] || [{ value: 'gpt-4o-mini', label: 'gpt-4o-mini' }]);
}

function defaultModelByEndpoint(endpoint) {
  return fallbackModelsByEndpoint(endpoint)[0]?.value || 'gpt-4o-mini';
}

function normalizeProviders(settings) {
  const merged = [];
  const byEndpoint = settings?.providerConfigs && typeof settings.providerConfigs === 'object'
    ? settings.providerConfigs
    : {};

  const presetSet = new Set();
  for (const preset of PROVIDER_PRESETS) {
    presetSet.add(preset.endpoint);
    const config = byEndpoint[preset.endpoint] || {};
    const isActive = settings?.defaultProviderEndpoint === preset.endpoint || settings?.apiEndpoint === preset.endpoint;
    merged.push({
      name: String(config?.name || preset.name),
      endpoint: preset.endpoint,
      apiKey: '',
      model: String(config?.model || (isActive ? settings?.model || '' : '') || defaultModelByEndpoint(preset.endpoint)),
    });
  }

  for (const [key, rawConfig] of Object.entries(byEndpoint)) {
    const endpoint = String(rawConfig?.endpoint || key || '').trim();
    if (!endpoint || presetSet.has(endpoint)) continue;
    merged.push({
      name: String(rawConfig?.name || endpoint),
      endpoint,
      apiKey: '',
      model: String(rawConfig?.model || defaultModelByEndpoint(endpoint)),
    });
  }

  if (!merged.length) {
    merged.push({
      name: 'OpenAI',
      endpoint: 'https://api.openai.com/v1',
      apiKey: '',
      model: String(settings?.model || 'gpt-4o-mini'),
    });
  }

  let defaultProviderEndpoint = String(settings?.defaultProviderEndpoint || settings?.apiEndpoint || '').trim();
  if (!merged.some((item) => item.endpoint === defaultProviderEndpoint)) {
    defaultProviderEndpoint = merged[0].endpoint;
  }

  return { providers: merged, defaultProviderEndpoint };
}

function normalizeSearchApi(settings) {
  const source = settings?.searchApi && typeof settings.searchApi === 'object'
    ? settings.searchApi
    : {};
  const scopesSource = source?.scopes && typeof source.scopes === 'object'
    ? source.scopes
    : {};

  const scanEnabled = typeof scopesSource.scan === 'boolean'
    ? scopesSource.scan
    : !!settings?.enableWebSearch;
  const classifyEnabled = typeof scopesSource.classify === 'boolean'
    ? scopesSource.classify
    : (typeof scopesSource.organizer === 'boolean'
      ? scopesSource.organizer
      : (settings?.enableWebSearchClassify != null
        ? !!settings.enableWebSearchClassify
        : (settings?.enableWebSearchOrganizer != null
          ? !!settings.enableWebSearchOrganizer
          : scanEnabled)));
  const organizerEnabled = typeof scopesSource.organizer === 'boolean'
    ? scopesSource.organizer
    : classifyEnabled;
  const enabled = source?.enabled != null
    ? !!source.enabled
    : (scanEnabled || classifyEnabled || organizerEnabled);

  return {
    provider: 'tavily',
    enabled,
    apiKey: '',
    scopes: {
      scan: !!scanEnabled,
      classify: !!classifyEnabled,
      organizer: !!organizerEnabled,
    },
  };
}

function setSavingState(isSaving) {
  if (!saveBtn) return;
  saveBtn.disabled = isSaving;
  saveBtn.textContent = isSaving ? t('provider_modal.saving') : t('provider_modal.save');
}

function hasStoredProviderSecret(endpoint) {
  return !!state.credentialsStatus?.providerHasApiKey?.[String(endpoint || '').trim()];
}

function hasStoredSearchApiSecret() {
  return !!state.credentialsStatus?.searchApiHasKey;
}

function renderModelOptions(selectEl, models, selected) {
  if (!selectEl) return;
  selectEl.innerHTML = '';
  for (const item of models) {
    selectEl.add(new Option(String(item.label || item.value), String(item.value)));
  }
  const selectedValue = String(selected || '').trim();
  if (selectedValue) {
    const exists = Array.from(selectEl.options).some((opt) => opt.value === selectedValue);
    if (!exists) selectEl.add(new Option(selectedValue, selectedValue));
    selectEl.value = selectedValue;
  } else if (selectEl.options.length > 0) {
    selectEl.value = selectEl.options[0].value;
  }
}

function renderProviderRows() {
  if (!listEl) return;

  const providerRowsHtml = state.providers.map((provider) => {
    const endpointKey = encodeURIComponent(provider.endpoint);
    const models = fallbackModelsByEndpoint(provider.endpoint);
    const hasSelected = models.some((item) => item.value === provider.model);
    const mergedModels = hasSelected || !provider.model
      ? models
      : [...models, { value: provider.model, label: provider.model }];
    const secretLoaded = !!state.editableCredentials?.providerSecrets?.[provider.endpoint];
    const secretStored = hasStoredProviderSecret(provider.endpoint);
    const secretUnavailable = secretStored && !secretLoaded;
    const modelOptions = mergedModels.map((item) => {
      const selected = item.value === provider.model ? 'selected' : '';
      return `<option value="${escapeHtml(item.value)}" ${selected}>${escapeHtml(item.label)}</option>`;
    }).join('');

    return `
      <div class="provider-row" data-endpoint="${endpointKey}">
        <div class="provider-row-head">
          <label class="provider-default-toggle">
            <input type="radio" name="provider-default" value="${escapeHtml(provider.endpoint)}" ${provider.endpoint === state.defaultProviderEndpoint ? 'checked' : ''} />
            <span>${t('provider_modal.default')}</span>
          </label>
          <div>
            <div class="provider-name">${escapeHtml(provider.name)}</div>
            <div class="provider-endpoint mono">${escapeHtml(provider.endpoint)}</div>
          </div>
        </div>
        <div class="provider-grid">
          <div class="form-group">
            <label class="form-label">${t('provider_modal.api_key')}</label>
            <input
              type="password"
              class="form-input provider-api-key"
              placeholder="${escapeHtml(secretUnavailable ? t('provider_modal.api_key_saved_placeholder') : t('provider_modal.api_key_placeholder'))}"
              value="${escapeHtml(state.editableCredentials?.providerSecrets?.[provider.endpoint] || '')}"
            />
            ${secretUnavailable ? `<div class="form-hint">${escapeHtml(t('provider_modal.api_key_saved_hint'))}</div>` : ''}
          </div>
          <div class="form-group">
            <label class="form-label">${t('provider_modal.model')}</label>
            <div class="provider-model-line">
              <select class="form-input provider-model">${modelOptions}</select>
              <button type="button" class="btn btn-ghost provider-refresh-btn">${t('provider_modal.refresh')}</button>
            </div>
          </div>
        </div>
      </div>
    `;
  }).join('');

  const searchApiRowHtml = `
    <div class="provider-row provider-search-row">
      <div class="provider-row-head">
        <div>
          <div class="provider-name">${t('provider_modal.search_api_title')}</div>
          <div class="provider-endpoint mono">Tavily</div>
        </div>
      </div>
      <div class="provider-grid">
        <div class="form-group">
          <label class="form-label">${t('provider_modal.search_api_key')}</label>
          <input
            id="provider-tavily-api-key"
            type="password"
            class="form-input"
            placeholder="${escapeHtml(hasStoredSearchApiSecret() && !state.editableCredentials?.searchApiKey ? t('provider_modal.api_key_saved_placeholder') : 'tvly-xxxxxxxxxxxxxxx')}"
            value="${escapeHtml(state.editableCredentials?.searchApiKey || '')}"
          />
          ${hasStoredSearchApiSecret() && !state.editableCredentials?.searchApiKey ? `<div class="form-hint">${escapeHtml(t('provider_modal.api_key_saved_hint'))}</div>` : ''}
          <div class="form-hint">
            <a href="https://tavily.com/" target="_blank" style="color: var(--accent-info); text-decoration: underline;">
              ${escapeHtml(t('provider_modal.search_api_hint'))}
            </a>
          </div>
        </div>
      </div>
    </div>
  `;

  listEl.innerHTML = `
    <div class="form-hint" style="margin-bottom: 12px;">${escapeHtml(t('provider_modal.credentials_hint'))}</div>
    ${providerRowsHtml}${searchApiRowHtml}
  `;

  for (const row of listEl.querySelectorAll('.provider-row')) {
    const endpoint = decodeURIComponent(String(row.getAttribute('data-endpoint') || ''));
    const apiKeyInput = row.querySelector('.provider-api-key');
    const modelSelect = row.querySelector('.provider-model');
    const defaultRadio = row.querySelector('input[name="provider-default"]');
    const refreshBtn = row.querySelector('.provider-refresh-btn');

    apiKeyInput?.addEventListener('input', () => {
      const target = state.providers.find((p) => p.endpoint === endpoint);
      const nextValue = String(apiKeyInput.value || '').trim();
      if (target) target.apiKey = nextValue;
      state.editableCredentials.providerSecrets[endpoint] = nextValue;
      state.dirtyCredentials.providerSecrets[endpoint] = true;
    });

    apiKeyInput?.addEventListener('blur', () => {
      loadModelsForProvider(endpoint, true);
    });

    modelSelect?.addEventListener('change', () => {
      const target = state.providers.find((p) => p.endpoint === endpoint);
      if (target) target.model = String(modelSelect.value || '').trim();
    });

    defaultRadio?.addEventListener('change', () => {
      if (defaultRadio.checked) state.defaultProviderEndpoint = endpoint;
    });

    refreshBtn?.addEventListener('click', () => {
      loadModelsForProvider(endpoint, true);
    });
  }

  const tavilyApiKeyInput = listEl.querySelector('#provider-tavily-api-key');
  tavilyApiKeyInput?.addEventListener('input', () => {
    const nextValue = String(tavilyApiKeyInput.value || '').trim();
    state.searchApi.apiKey = nextValue;
    state.editableCredentials.searchApiKey = nextValue;
    state.dirtyCredentials.searchApiKey = true;
  });

  for (const provider of state.providers) {
    loadModelsForProvider(provider.endpoint, false);
  }
}

async function loadModelsForProvider(endpoint, forceRefresh = false) {
  const row = listEl?.querySelector(`.provider-row[data-endpoint="${encodeURIComponent(endpoint)}"]`);
  if (!row) return;

  const apiKeyInput = row.querySelector('.provider-api-key');
  const modelSelect = row.querySelector('.provider-model');
  const providerState = state.providers.find((item) => item.endpoint === endpoint);
  if (!modelSelect || !providerState) return;

  providerState.apiKey = String(apiKeyInput?.value || '').trim();
  const selectedModel = String(modelSelect.value || providerState.model || '').trim();
  const storedCredential = hasStoredProviderSecret(endpoint);
  const cacheKey = `${endpoint}|${providerState.apiKey || (storedCredential ? '__stored__' : '')}`;
  const token = (requestTokens.get(endpoint) || 0) + 1;
  requestTokens.set(endpoint, token);

  modelSelect.disabled = true;
  modelSelect.innerHTML = `<option value="">${escapeHtml(t('provider_modal.loading'))}</option>`;

  let models = [];
  try {
    if (providerState.apiKey || storedCredential) {
      if (!forceRefresh && remoteModelsCache.has(cacheKey)) {
        models = remoteModelsCache.get(cacheKey);
      } else {
        const resp = providerState.apiKey
          ? await getProviderModels(endpoint, providerState.apiKey)
          : await getProviderModels(endpoint);
        models = normalizeModels(resp?.models || []);
        remoteModelsCache.set(cacheKey, models);
      }
    }
  } catch {
    models = [];
  }

  if (!models.length) models = fallbackModelsByEndpoint(endpoint);
  if (!models.length) models = [{ value: 'gpt-4o-mini', label: 'gpt-4o-mini' }];
  if (requestTokens.get(endpoint) !== token) return;

  renderModelOptions(modelSelect, models, selectedModel || providerState.model);
  providerState.model = String(modelSelect.value || providerState.model || defaultModelByEndpoint(endpoint)).trim();
  modelSelect.disabled = false;
}

function openModal() {
  if (!modalEl) return;
  modalEl.classList.add('open');
  modalEl.setAttribute('aria-hidden', 'false');
  document.body.style.overflow = 'hidden';
}

function focusFirstProviderApiInput() {
  const firstInput = listEl?.querySelector('.provider-api-key');
  if (!firstInput) return;
  setTimeout(() => {
    firstInput.focus();
    firstInput.select?.();
  }, 0);
}

function closeModal() {
  if (!modalEl) return;
  closeCloseConfirm();
  modalEl.classList.remove('open');
  modalEl.setAttribute('aria-hidden', 'true');
  document.body.style.overflow = '';
}

function openCloseConfirm() {
  if (!closeConfirmEl) return;
  closeConfirmEl.hidden = false;
  closeConfirmSaveBtn?.focus();
}

function closeCloseConfirm() {
  if (!closeConfirmEl) return;
  closeConfirmEl.hidden = true;
}

function collectEditableCredentialsFromDOM() {
  const providerSecrets = {};
  const rows = listEl?.querySelectorAll('.provider-row') || [];
  for (const row of rows) {
    const endpoint = decodeURIComponent(String(row.getAttribute('data-endpoint') || ''));
    if (!endpoint) continue;
    providerSecrets[endpoint] = String(row.querySelector('.provider-api-key')?.value || '').trim();
  }
  return {
    providerSecrets,
    searchApiKey: String(listEl?.querySelector('#provider-tavily-api-key')?.value || state.editableCredentials.searchApiKey || '').trim(),
  };
}

function buildModalSnapshot() {
  const settings = listEl ? collectPayloadFromDOM() : {
    providerConfigs: Object.fromEntries(
      state.providers.map((provider) => [provider.endpoint, {
        name: String(provider.name || provider.endpoint),
        endpoint: provider.endpoint,
        model: String(provider.model || defaultModelByEndpoint(provider.endpoint)),
      }]),
    ),
    defaultProviderEndpoint: state.defaultProviderEndpoint,
    apiEndpoint: state.defaultProviderEndpoint,
    model: String(
      state.providers.find((provider) => provider.endpoint === state.defaultProviderEndpoint)?.model
      || defaultModelByEndpoint(state.defaultProviderEndpoint),
    ),
    searchApi: {
      provider: 'tavily',
      enabled: !!(
        state.searchApi?.enabled
        || state.searchApi?.scopes?.scan
        || state.searchApi?.scopes?.classify
        || state.searchApi?.scopes?.organizer
      ),
      scopes: {
        scan: !!state.searchApi?.scopes?.scan,
        classify: !!(state.searchApi?.scopes?.classify || state.searchApi?.scopes?.organizer),
        organizer: !!(state.searchApi?.scopes?.organizer || state.searchApi?.scopes?.classify),
      },
    },
    enableWebSearch: !!state.searchApi?.scopes?.scan,
    enableWebSearchClassify: !!(state.searchApi?.scopes?.classify || state.searchApi?.scopes?.organizer),
    enableWebSearchOrganizer: !!(state.searchApi?.scopes?.organizer || state.searchApi?.scopes?.classify),
  };
  const credentials = listEl ? collectEditableCredentialsFromDOM() : {
    providerSecrets: { ...(state.editableCredentials?.providerSecrets || {}) },
    searchApiKey: String(state.editableCredentials?.searchApiKey || ''),
  };
  return JSON.stringify({ settings, credentials });
}

function hasUnsavedChanges() {
  return buildModalSnapshot() !== state.initialModalSnapshot;
}

async function refreshModalData() {
  if (!listEl) return;
  listEl.innerHTML = `<div class="form-hint">${escapeHtml(t('provider_modal.loading'))}</div>`;
  const settings = await getSettings();
  const normalized = normalizeProviders(settings);
  state.providers = normalized.providers;
  state.defaultProviderEndpoint = normalized.defaultProviderEndpoint;
  state.searchApi = normalizeSearchApi(settings);
  state.credentialsStatus = settings?.credentialsStatus || state.credentialsStatus;
  const editable = await getCredentials();
  state.editableCredentials = {
    providerSecrets: { ...(editable?.providerSecrets || {}) },
    searchApiKey: String(editable?.searchApiKey || ''),
  };
  state.dirtyCredentials = {
    providerSecrets: {},
    searchApiKey: false,
  };
  renderProviderRows();
  state.initialModalSnapshot = buildModalSnapshot();
}

function collectPayloadFromDOM() {
  const providerConfigs = {};
  const rows = listEl?.querySelectorAll('.provider-row') || [];

  for (const row of rows) {
    const endpoint = decodeURIComponent(String(row.getAttribute('data-endpoint') || ''));
    if (!endpoint) continue;
    const fromState = state.providers.find((item) => item.endpoint === endpoint);
    const model = String(row.querySelector('.provider-model')?.value || '').trim() || defaultModelByEndpoint(endpoint);
    providerConfigs[endpoint] = {
      name: String(fromState?.name || endpoint),
      endpoint,
      model,
    };
  }

  const checkedDefault = listEl?.querySelector('input[name="provider-default"]:checked');
  const defaultProviderEndpoint = String(checkedDefault?.value || state.defaultProviderEndpoint || '').trim()
    || Object.keys(providerConfigs)[0]
    || PROVIDER_PRESETS[0].endpoint;
  const activeConfig = providerConfigs[defaultProviderEndpoint] || {
    endpoint: defaultProviderEndpoint,
    model: defaultModelByEndpoint(defaultProviderEndpoint),
  };

  const searchApi = {
    provider: 'tavily',
    enabled: !!(
      state.searchApi?.enabled
      || state.searchApi?.scopes?.scan
      || state.searchApi?.scopes?.classify
      || state.searchApi?.scopes?.organizer
    ),
    scopes: {
      scan: !!state.searchApi?.scopes?.scan,
      classify: !!(state.searchApi?.scopes?.classify || state.searchApi?.scopes?.organizer),
      organizer: !!(state.searchApi?.scopes?.organizer || state.searchApi?.scopes?.classify),
    },
  };

  return {
    providerConfigs,
    defaultProviderEndpoint,
    apiEndpoint: defaultProviderEndpoint,
    model: String(activeConfig.model || defaultModelByEndpoint(defaultProviderEndpoint)),
    searchApi,
    enableWebSearch: searchApi.enabled && searchApi.scopes.scan,
    enableWebSearchClassify: searchApi.enabled && searchApi.scopes.classify,
    enableWebSearchOrganizer: searchApi.enabled && searchApi.scopes.organizer,
  };
}

function collectCredentialsFromDOM() {
  const providerSecrets = {};
  const fullCredentials = collectEditableCredentialsFromDOM();
  for (const [endpoint, value] of Object.entries(fullCredentials.providerSecrets || {})) {
    if (state.dirtyCredentials?.providerSecrets?.[endpoint]) {
      providerSecrets[endpoint] = value;
    }
  }
  const payload = {};
  if (Object.keys(providerSecrets).length > 0) {
    payload.providerSecrets = providerSecrets;
  }
  if (state.dirtyCredentials?.searchApiKey) {
    payload.searchApiKey = fullCredentials.searchApiKey;
  }
  return payload;
}

async function handleOpenModal() {
  try {
    await refreshModalData();
    closeCloseConfirm();
    openModal();
    focusFirstProviderApiInput();
  } catch (err) {
    showToast(`${t('provider_modal.failed')}${getErrorMessage(err)}`, 'error');
  }
}

async function handleSave() {
  try {
    setSavingState(true);
    closeCloseConfirm();
    const payload = collectPayloadFromDOM();
    await saveSettings(payload);
    const credentialResult = await saveCredentials(collectCredentialsFromDOM());
    state.credentialsStatus = credentialResult?.credentialsStatus || state.credentialsStatus;
    const latestSettings = await getSettings();
    await refreshCredentialsStatus();
    state.initialModalSnapshot = buildModalSnapshot();
    showToast(t('provider_modal.saved'), 'success');
    window.dispatchEvent(new CustomEvent('provider-settings-updated', { detail: latestSettings }));
    closeModal();
  } catch (err) {
    showToast(`${t('provider_modal.failed')}${getErrorMessage(err)}`, 'error');
  } finally {
    setSavingState(false);
  }
}

async function requestCloseModal() {
  if (saveBtn?.disabled) return;
  if (!modalEl?.classList.contains('open')) {
    closeModal();
    return;
  }
  if (!hasUnsavedChanges()) {
    closeModal();
    return;
  }
  openCloseConfirm();
}

function bindStaticEvents() {
  openBtn?.addEventListener('click', handleOpenModal);
  closeBtn?.addEventListener('click', requestCloseModal);
  cancelBtn?.addEventListener('click', requestCloseModal);
  saveBtn?.addEventListener('click', handleSave);
  closeConfirmCancelBtn?.addEventListener('click', closeCloseConfirm);
  closeConfirmSaveBtn?.addEventListener('click', handleSave);

  modalEl?.addEventListener('click', (event) => {
    if (!closeConfirmEl?.hidden && closeConfirmEl?.contains(event.target)) return;
    const closeTarget = event.target?.getAttribute('data-modal-close');
    if (closeTarget === 'true') requestCloseModal();
  });

  document.addEventListener('keydown', (event) => {
    if (event.key === 'Escape' && modalEl?.classList.contains('open')) {
      requestCloseModal();
    }
  });

  registerLangChangeHandler(() => {
    if (modalEl?.classList.contains('open')) {
      renderProviderRows();
    }
  });

  window.addEventListener('open-provider-manager-requested', () => {
    handleOpenModal();
  });
}

export function initProviderManager() {
  if (initialized) return;
  initialized = true;

  modalEl = document.getElementById('provider-modal');
  listEl = document.getElementById('provider-list');
  openBtn = document.getElementById('provider-manager-btn');
  closeBtn = document.getElementById('provider-modal-close');
  cancelBtn = document.getElementById('provider-modal-cancel');
  saveBtn = document.getElementById('provider-modal-save');
  closeConfirmEl = document.getElementById('provider-close-confirm');
  closeConfirmCancelBtn = document.getElementById('provider-close-confirm-cancel');
  closeConfirmSaveBtn = document.getElementById('provider-close-confirm-save');

  bindStaticEvents();
  registerCredentialsStatusChangeHandler(async (event) => {
    state.credentialsStatus = event?.detail || state.credentialsStatus;
    if (modalEl?.classList.contains('open')) {
      try {
        await refreshModalData();
      } catch (err) {
        console.warn('Failed to refresh provider modal after credentials change:', err);
      }
    }
  });
}
