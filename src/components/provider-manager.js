import {
  getEditableSecrets,
  getProviderModels,
  getSecretStatus,
  getSettings,
  saveSecrets,
  saveSettings,
  setupSecretVault,
  unlockSecretVault,
} from '../utils/api.js';
import { showToast } from '../main.js';
import {
  getCachedSecretStatus,
  lockSecretVaultInUi,
  refreshSecretStatus,
  registerSecretStatusChangeHandler,
  resetSecretVaultInUi,
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
  secretStatus: {
    initialized: false,
    unlocked: false,
    needsMigration: false,
    providerHasApiKey: {},
    searchApiHasKey: false,
  },
  editableSecrets: {
    providerSecrets: {},
    searchApiKey: '',
  },
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
let initialized = false;
let vaultActionPending = false;
const VAULT_SLOW_HINT_DELAY_MS = 1800;

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
    apiKey: String(source?.apiKey || settings?.tavilyApiKey || '').trim(),
    scopes: {
      scan: !!scanEnabled,
      classify: !!classifyEnabled,
      organizer: !!organizerEnabled,
    },
  };
}

function setSavingState(isSaving) {
  if (!saveBtn) return;
  saveBtn.disabled = isSaving || !state.secretStatus?.unlocked;
  saveBtn.textContent = isSaving ? t('provider_modal.saving') : t('provider_modal.save');
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
  const secretStatus = state.secretStatus || {};
  if (saveBtn) saveBtn.disabled = !secretStatus.unlocked;
  if (!secretStatus.unlocked) {
    const actionLabel = secretStatus.initialized
      ? t('provider_modal.unlock')
      : t('provider_modal.setup');
    const pendingLabel = secretStatus.initialized
      ? t('provider_modal.unlocking')
      : t('provider_modal.setting_up');
    const passwordLabel = t('secret.password_label');
    const confirmLabel = t('secret.password_confirm_label');
    const migrationHint = secretStatus.needsMigration
      ? `<div class="form-hint" style="color: var(--accent-warning); margin-top: 8px;">${escapeHtml(t('provider_modal.needs_migration'))}</div>`
      : '';
    const missingVaultHint = !secretStatus.initialized
      ? `<div class="form-hint" style="color: var(--accent-warning); margin-top: 8px;">${escapeHtml(t('provider_modal.setup_missing_vault'))}</div>`
      : '';
    listEl.innerHTML = `
      <div class="provider-row provider-search-row">
        <div class="provider-row-head">
          <div>
            <div class="provider-name">${escapeHtml(t('provider_modal.locked'))}</div>
            <div class="provider-endpoint mono">Stronghold</div>
          </div>
        </div>
        <div class="form-hint">${escapeHtml(t('provider_modal.locked_hint'))}</div>
        ${migrationHint}
        ${missingVaultHint}
        <div class="provider-grid" style="margin-top: 16px;">
          <div class="form-group">
            <label class="form-label">${escapeHtml(passwordLabel)}</label>
            <input id="provider-vault-password" class="form-input" type="password" autocomplete="${secretStatus.initialized ? 'current-password' : 'new-password'}" />
          </div>
          ${secretStatus.initialized ? '' : `
          <div class="form-group">
            <label class="form-label">${escapeHtml(confirmLabel)}</label>
            <input id="provider-vault-password-confirm" class="form-input" type="password" autocomplete="new-password" />
          </div>
          `}
        </div>
        <div id="provider-vault-status" class="form-hint" style="color: var(--accent-info); margin-top: 10px;" hidden></div>
        <div id="provider-vault-error" class="form-hint" style="color: #fca5a5; margin-top: -4px; margin-bottom: 8px;" hidden></div>
        <div class="flex items-center gap-8" style="margin-top: 12px;">
          <button id="provider-vault-open-btn" class="btn btn-primary" type="button" data-label-id="${secretStatus.initialized ? 'unlock' : 'setup'}" data-pending-label="${escapeHtml(pendingLabel)}">${escapeHtml(actionLabel)}</button>
          <button id="provider-vault-reset-btn" class="btn btn-secondary" type="button">${escapeHtml(t('provider_modal.reset'))}</button>
        </div>
      </div>
    `;
    listEl.querySelector('#provider-vault-open-btn')?.addEventListener('click', async () => {
      if (vaultActionPending) return;
      const passwordInput = listEl.querySelector('#provider-vault-password');
      const confirmInput = listEl.querySelector('#provider-vault-password-confirm');
      const statusEl = listEl.querySelector('#provider-vault-status');
      const errorEl = listEl.querySelector('#provider-vault-error');
      const actionBtn = listEl.querySelector('#provider-vault-open-btn');
      const resetBtn = listEl.querySelector('#provider-vault-reset-btn');
      const password = String(passwordInput?.value || '').trim();
      const confirmPassword = String(confirmInput?.value || '').trim();
      let slowHintTimer = null;
      const setError = (message) => {
        if (!errorEl) return;
        errorEl.textContent = message;
        errorEl.hidden = !message;
      };
      const setStatus = (message) => {
        if (!statusEl) return;
        statusEl.textContent = message;
        statusEl.hidden = !message;
      };
      try {
        if (!password) {
          setError(t('secret.password_required'));
          passwordInput?.focus();
          return;
        }
        if (!secretStatus.initialized && password !== confirmPassword) {
          setError(t('secret.password_mismatch'));
          confirmInput?.focus();
          return;
        }
        setError('');
        setStatus('');
        vaultActionPending = true;
        if (actionBtn) {
          actionBtn.disabled = true;
          actionBtn.textContent = actionBtn.getAttribute('data-pending-label') || pendingLabel;
        }
        if (resetBtn) resetBtn.disabled = true;
        if (passwordInput) passwordInput.disabled = true;
        if (confirmInput) confirmInput.disabled = true;
        slowHintTimer = window.setTimeout(() => {
          setStatus(t('provider_modal.slow_hint'));
        }, VAULT_SLOW_HINT_DELAY_MS);
        if (secretStatus.initialized) {
          const result = await unlockSecretVault(password);
          state.secretStatus = result?.secretStatus || await getSecretStatus();
          showToast(result?.migrated ? t('secret.migrated') : t('secret.unlock_done'), 'success');
        } else {
          const result = await setupSecretVault(password);
          state.secretStatus = result?.secretStatus || await getSecretStatus();
          showToast(result?.migrated ? t('secret.migrated') : t('secret.setup_done'), 'success');
        }
        await refreshSecretStatus();
        await refreshModalData();
        focusFirstProviderApiInput();
      } catch (err) {
        const message = getErrorMessage(err);
        console.error('Provider vault open/setup failed:', err);
        setError(message);
        setStatus('');
        showToast(message, 'error');
      } finally {
        vaultActionPending = false;
        if (slowHintTimer != null) window.clearTimeout(slowHintTimer);
        const resetLabel = secretStatus.initialized
          ? t('provider_modal.unlock')
          : t('provider_modal.setup');
        if (actionBtn) {
          actionBtn.disabled = false;
          actionBtn.textContent = resetLabel;
        }
        if (resetBtn) resetBtn.disabled = false;
        if (passwordInput) passwordInput.disabled = false;
        if (confirmInput) confirmInput.disabled = false;
      }
    });
    listEl.querySelector('#provider-vault-reset-btn')?.addEventListener('click', async () => {
      try {
        await resetSecretVaultInUi();
        await refreshModalData();
        window.dispatchEvent(new CustomEvent('provider-settings-updated', { detail: await getSettings() }));
      } catch (err) {
        showToast(getErrorMessage(err), 'error');
      }
    });
    return;
  }

  const providerRowsHtml = state.providers.map((provider) => {
    const endpointKey = encodeURIComponent(provider.endpoint);
    const models = fallbackModelsByEndpoint(provider.endpoint);
    const hasSelected = models.some((item) => item.value === provider.model);
    const mergedModels = hasSelected || !provider.model
      ? models
      : [...models, { value: provider.model, label: provider.model }];
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
              placeholder="${escapeHtml(t('provider_modal.api_key_placeholder'))}"
              value="${escapeHtml(state.editableSecrets?.providerSecrets?.[provider.endpoint] || '')}"
            />
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
            placeholder="tvly-xxxxxxxxxxxxxxx"
            value="${escapeHtml(state.editableSecrets?.searchApiKey || '')}"
          />
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
    <div class="flex items-center justify-between" style="margin-bottom: 12px;">
      <div class="form-hint">${escapeHtml(t('provider_modal.locked_hint'))}</div>
      <div class="flex items-center gap-8">
        <button id="provider-vault-lock-btn" class="btn btn-ghost" type="button">${escapeHtml(t('provider_modal.lock'))}</button>
        <button id="provider-vault-reset-btn" class="btn btn-secondary" type="button">${escapeHtml(t('provider_modal.reset'))}</button>
      </div>
    </div>
    ${providerRowsHtml}${searchApiRowHtml}
  `;

  listEl.querySelector('#provider-vault-lock-btn')?.addEventListener('click', async () => {
    try {
      await lockSecretVaultInUi();
      await refreshModalData();
      } catch (err) {
        showToast(getErrorMessage(err), 'error');
      }
    });
    listEl.querySelector('#provider-vault-reset-btn')?.addEventListener('click', async () => {
    try {
      await resetSecretVaultInUi();
      await refreshModalData();
      window.dispatchEvent(new CustomEvent('provider-settings-updated', { detail: await getSettings() }));
      } catch (err) {
        showToast(getErrorMessage(err), 'error');
      }
    });

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
      state.editableSecrets.providerSecrets[endpoint] = nextValue;
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
    state.editableSecrets.searchApiKey = nextValue;
  });

  // Auto-fetch available models for each provider row independently.
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
  const cacheKey = `${endpoint}|${providerState.apiKey}`;

  const token = (requestTokens.get(endpoint) || 0) + 1;
  requestTokens.set(endpoint, token);

  modelSelect.disabled = true;
  modelSelect.innerHTML = `<option value="">${escapeHtml(t('provider_modal.loading'))}</option>`;

  let models = [];
  try {
    if (providerState.apiKey) {
      if (!forceRefresh && remoteModelsCache.has(cacheKey)) {
        models = remoteModelsCache.get(cacheKey);
      } else {
        const resp = await getProviderModels(endpoint, providerState.apiKey);
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
  modalEl.classList.remove('open');
  modalEl.setAttribute('aria-hidden', 'true');
  document.body.style.overflow = '';
}

async function refreshModalData() {
  if (!listEl) return;
  listEl.innerHTML = `<div class="form-hint">${escapeHtml(t('provider_modal.loading'))}</div>`;
  const settings = await getSettings();
  const normalized = normalizeProviders(settings);
  state.providers = normalized.providers;
  state.defaultProviderEndpoint = normalized.defaultProviderEndpoint;
  state.searchApi = normalizeSearchApi(settings);
  state.secretStatus = settings?.secretStatus || getCachedSecretStatus() || state.secretStatus;
  state.editableSecrets = { providerSecrets: {}, searchApiKey: '' };
  if (state.secretStatus?.unlocked) {
    try {
      const editable = await getEditableSecrets();
      state.editableSecrets = {
        providerSecrets: { ...(editable?.providerSecrets || {}) },
        searchApiKey: String(editable?.searchApiKey || ''),
      };
    } catch {
      state.secretStatus = { ...state.secretStatus, unlocked: false };
    }
  }
  renderProviderRows();
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

function collectSecretsFromDOM() {
  const providerSecrets = {};
  const rows = listEl?.querySelectorAll('.provider-row') || [];
  for (const row of rows) {
    const endpoint = decodeURIComponent(String(row.getAttribute('data-endpoint') || ''));
    if (!endpoint) continue;
    providerSecrets[endpoint] = String(row.querySelector('.provider-api-key')?.value || '').trim();
  }
  return {
    providerSecrets,
    searchApiKey: String(listEl?.querySelector('#provider-tavily-api-key')?.value || state.editableSecrets.searchApiKey || '').trim(),
  };
}

async function handleOpenModal() {
  try {
    await refreshModalData();
    openModal();
    if (state.secretStatus?.unlocked) {
      focusFirstProviderApiInput();
    } else {
      const passwordInput = listEl?.querySelector('#provider-vault-password');
      setTimeout(() => passwordInput?.focus(), 0);
    }
  } catch (err) {
    showToast(`${t('provider_modal.failed')}${getErrorMessage(err)}`, 'error');
  }
}

async function handleSave() {
  try {
    setSavingState(true);
    const payload = collectPayloadFromDOM();
    await saveSettings(payload);
    if (state.secretStatus?.unlocked) {
      const secretResult = await saveSecrets(collectSecretsFromDOM());
      state.secretStatus = secretResult?.secretStatus || state.secretStatus;
    }
    state.defaultProviderEndpoint = payload.defaultProviderEndpoint;
    const latestSettings = await getSettings();
    showToast(t('provider_modal.saved'), 'success');
    window.dispatchEvent(new CustomEvent('provider-settings-updated', { detail: latestSettings }));
    closeModal();
  } catch (err) {
    showToast(`${t('provider_modal.failed')}${getErrorMessage(err)}`, 'error');
  } finally {
    setSavingState(false);
  }
}

function bindStaticEvents() {
  openBtn?.addEventListener('click', handleOpenModal);
  closeBtn?.addEventListener('click', closeModal);
  cancelBtn?.addEventListener('click', closeModal);
  saveBtn?.addEventListener('click', handleSave);

  modalEl?.addEventListener('click', (event) => {
    const closeTarget = event.target?.getAttribute('data-modal-close');
    if (closeTarget === 'true') closeModal();
  });

  document.addEventListener('keydown', (event) => {
    if (event.key === 'Escape' && modalEl?.classList.contains('open')) {
      closeModal();
    }
  });

  registerLangChangeHandler(() => {
    if (modalEl?.classList.contains('open')) {
      renderProviderRows();
    }
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

  bindStaticEvents();
  registerSecretStatusChangeHandler(async (event) => {
    state.secretStatus = event?.detail || state.secretStatus;
    if (modalEl?.classList.contains('open')) {
      try {
        await refreshModalData();
      } catch (err) {
        console.warn('Failed to refresh provider modal after secret status change:', err);
      }
    }
  });
}
