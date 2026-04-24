import {
  getCredentials,
  getProviderModels,
  getSettings,
  saveCredentials,
  saveSettings,
} from '../utils/api.js';
import {
  refreshCredentialsStatus,
  registerCredentialsStatusChangeHandler,
} from '../utils/secret-ui.js';
import { registerLangChangeHandler, t } from '../utils/i18n.js';
import { escapeHtml } from '../utils/html.js';
import {
  DEFAULT_PROVIDER_ENDPOINT,
  defaultModelByEndpoint,
  fallbackModelsByEndpoint,
  normalizeRemoteModels,
  PROVIDER_OPTIONS,
} from '../utils/provider-registry.js';
import { normalizeProviders, normalizeSearchApi } from './provider-manager/normalizers.js';
import { showToast } from '../utils/toast.js';

const remoteModelsCache = new Map();
const requestTokens = new Map();
const state = {
  providers: [],
  defaultProviderEndpoint: '',
  activeTab: null, // Track currently selected provider endpoint or 'tavily'
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
let closeConfirmDiscardBtn;
let initialized = false;

function getErrorMessage(err) {
  if (typeof err === 'string') return err;
  if (err && typeof err.message === 'string' && err.message.trim()) return err.message;
  try {
    return JSON.stringify(err);
  } catch {
    return String(err || 'Unknown error');
  }
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

  if (!state.activeTab && state.providers.length > 0) {
    state.activeTab = state.defaultProviderEndpoint || state.providers[0].endpoint;
  }

  // Generate sidebar tabs
  const sidebarHtml = `
    <div class="provider-sidebar">
      <div class="provider-group-label">${t('provider_modal.llm_group') || '大模型 API'}</div>
      ${state.providers.map((provider) => {
        const hasKeyLocally = !!state.editableCredentials?.providerSecrets?.[provider.endpoint];
        const hasKeyStored = hasStoredProviderSecret(provider.endpoint);
        const isConfigured = hasKeyLocally || hasKeyStored;
        const badgeHtml = isConfigured ? '<div class="provider-status-badge"></div>' : '';
        return `
        <div class="provider-tab ${state.activeTab === provider.endpoint ? 'active' : ''}" data-tab="${escapeHtml(provider.endpoint)}">
          <div class="provider-tab-name">${escapeHtml(provider.name)}</div>
          ${badgeHtml}
        </div>
        `;
      }).join('')}
      
      <div class="provider-group-label">${t('provider_modal.search_group') || '聚合搜索引擎'}</div>
      ${(() => {
        const hasSearchLocally = !!state.editableCredentials?.searchApiKey;
        const hasSearchStored = hasStoredSearchApiSecret();
        const searchConfigured = hasSearchLocally || hasSearchStored;
        const searchBadge = searchConfigured ? '<div class="provider-status-badge"></div>' : '';
        return `
        <div class="provider-tab ${state.activeTab === 'tavily' ? 'active' : ''}" data-tab="tavily">
          <div class="provider-tab-name">${t('provider_modal.search_api_title')}</div>
          ${searchBadge}
        </div>
        `;
      })()}
    </div>
  `;

  // Generate main content
  let contentHtml = '';
  
  if (state.activeTab === 'tavily') {
    contentHtml = `
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
          </div>
        </div>
      </div>
    `;
  } else {
    const provider = state.providers.find(p => p.endpoint === state.activeTab);
    if (provider) {
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

      contentHtml = `
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
    }
  }

  listEl.innerHTML = `
    <div class="provider-layout">
      ${sidebarHtml}
      <div class="provider-content">
        ${contentHtml}
      </div>
    </div>
  `;

  // Bind tab switching events
  listEl.querySelectorAll('.provider-tab').forEach(tab => {
    tab.addEventListener('click', () => {
      const targetTab = tab.getAttribute('data-tab');
      if (state.activeTab !== targetTab) {
        state.activeTab = targetTab;
        renderProviderRows();
      }
    });
  });

  // Bind input events for active tab content
  const activeRow = listEl.querySelector('.provider-row');
  if (activeRow && state.activeTab !== 'tavily') {
    const endpoint = decodeURIComponent(String(activeRow.getAttribute('data-endpoint') || ''));
    const apiKeyInput = activeRow.querySelector('.provider-api-key');
    const modelSelect = activeRow.querySelector('.provider-model');
    const defaultRadio = activeRow.querySelector('input[name="provider-default"]');
    const refreshBtn = activeRow.querySelector('.provider-refresh-btn');

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
      // Re-render to update the radio button state across tabs if needed, 
      // though only one is visible at a time.
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

  // Automatically load models for the currently active tab if it's a provider
  if (state.activeTab && state.activeTab !== 'tavily') {
    const activeProvider = state.providers.find(p => p.endpoint === state.activeTab);
    if (activeProvider && !activeProvider.modelLoaded) {
       loadModelsForProvider(activeProvider.endpoint, false);
       activeProvider.modelLoaded = true; // prevent infinite loops
    }
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
        models = normalizeRemoteModels(resp?.models || []);
        remoteModelsCache.set(cacheKey, models);
      }
    }
  } catch {
    models = [];
  }

  if (!models.length) models = fallbackModelsByEndpoint(endpoint);
  if (!models.length) models = [{ value: defaultModelByEndpoint(endpoint), label: defaultModelByEndpoint(endpoint) }];
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
    searchApi: {
      provider: 'tavily',
      enabled: !!(
        state.searchApi?.enabled
        || state.searchApi?.scopes?.classify
        || state.searchApi?.scopes?.organizer
      ),
      scopes: {
        classify: !!(state.searchApi?.scopes?.classify || state.searchApi?.scopes?.organizer),
        organizer: !!(state.searchApi?.scopes?.organizer || state.searchApi?.scopes?.classify),
      },
    },
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
    || DEFAULT_PROVIDER_ENDPOINT;
  const activeConfig = providerConfigs[defaultProviderEndpoint] || {
    endpoint: defaultProviderEndpoint,
    model: defaultModelByEndpoint(defaultProviderEndpoint),
  };

  const searchApi = {
    provider: 'tavily',
    enabled: !!(
      state.searchApi?.enabled
      || state.searchApi?.scopes?.classify
      || state.searchApi?.scopes?.organizer
    ),
    scopes: {
      classify: !!(state.searchApi?.scopes?.classify || state.searchApi?.scopes?.organizer),
      organizer: !!(state.searchApi?.scopes?.organizer || state.searchApi?.scopes?.classify),
    },
  };

  return {
    providerConfigs,
    defaultProviderEndpoint,
    searchApi,
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
  closeConfirmDiscardBtn?.addEventListener('click', () => {
    closeCloseConfirm();
    closeModal();
  });

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
  closeConfirmDiscardBtn = document.getElementById('provider-close-confirm-discard');

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
