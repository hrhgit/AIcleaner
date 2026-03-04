/**
 * src/pages/settings.js
 * 设置页面：API 提供商/模型、扫描、联网搜索、权限
 */
import {
  browseFolder,
  getPrivilegeStatus,
  getProviderModels,
  getSettings,
  requestElevation,
  saveSettings,
} from '../utils/api.js';
import { showToast } from '../main.js';
import { t } from '../utils/i18n.js';

const PROVIDER_MODELS = {
  'https://api.openai.com/v1': [
    { value: 'gpt-4o-mini', label: 'gpt-4o-mini (推荐)' },
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

function normalizeModels(models) {
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

function renderModelOptions(models, selectedValue) {
  const modelSelect = document.getElementById('api-model');
  if (!modelSelect) return;
  modelSelect.innerHTML = models.map((m) => `<option value="${m.value}">${m.label}</option>`).join('');

  if (selectedValue) {
    const exists = Array.from(modelSelect.options).some((opt) => opt.value === selectedValue);
    if (!exists) modelSelect.add(new Option(selectedValue, selectedValue));
    modelSelect.value = selectedValue;
  } else if (modelSelect.options.length > 0) {
    modelSelect.value = modelSelect.options[0].value;
  }
}

export async function renderSettings(container) {
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
      <h1 class="page-title">${t('settings.title')}</h1>
      <p class="page-subtitle">${t('settings.subtitle')}</p>
    </div>

    <div class="card animate-in mb-24" style="animation-delay: 0.05s">
      <div class="card-header">
        <h2 class="card-title">${t('settings.api_config')}</h2>
        <span class="badge badge-info">${t('settings.llm_engine')}</span>
      </div>

      <div class="grid-2">
        <div class="form-group">
          <label class="form-label">${t('settings.provider')}</label>
          <select id="api-endpoint" class="form-input">
            <option value="https://api.deepseek.com">DeepSeek</option>
            <option value="https://api.openai.com/v1">OpenAI</option>
            <option value="https://generativelanguage.googleapis.com/v1beta/openai/">Google Gemini</option>
            <option value="https://dashscope.aliyuncs.com/compatible-mode/v1">通义千问 (阿里云)</option>
            <option value="https://open.bigmodel.cn/api/paas/v4">智谱 GLM</option>
            <option value="https://api.moonshot.cn/v1">Kimi (Moonshot)</option>
          </select>
          <div class="form-hint">${t('settings.provider_hint')}</div>
        </div>
        <div class="form-group">
          <label class="form-label">${t('settings.model')}</label>
          <select id="api-model" class="form-input"></select>
          <div class="form-hint">${t('settings.model_hint')}</div>
        </div>
      </div>
      <div class="form-hint">${t('settings.api_key_managed_hint')}</div>

      <div class="form-group" style="margin-top: 16px; padding-top: 12px; border-top: 1px dashed var(--border);">
        <div style="display:flex; align-items:center; gap:10px; margin-bottom: 8px;">
          <label class="form-label" style="margin:0;">${t('settings.search_config')}</label>
          <span class="badge badge-warning">${t('settings.expert_feature')}</span>
        </div>
        <div class="form-hint" style="margin-bottom: 12px;">${t('settings.search_hint')}</div>

        <div class="grid-2">
          <div class="form-group" style="margin-bottom: 0;">
            <label style="display:flex; align-items:center; gap:10px; cursor:pointer; margin-bottom: 8px;">
              <input type="checkbox" id="enable-web-search" class="toggle-checkbox" style="width: 20px; height: 20px;" />
              <span class="form-label" style="margin:0;">${t('settings.enable_search_scan')}</span>
            </label>
            <label style="display:flex; align-items:center; gap:10px; cursor:pointer;">
              <input type="checkbox" id="enable-organizer-web-search" class="toggle-checkbox" style="width: 20px; height: 20px;" />
              <span class="form-label" style="margin:0;">${t('settings.enable_search_organizer')}</span>
            </label>
          </div>
        </div>
      </div>
    </div>

    <div class="card animate-in mb-24" style="animation-delay: 0.1s">
      <div class="card-header">
        <h2 class="card-title">${t('settings.scan_config')}</h2>
        <span class="badge badge-secondary">${t('settings.scan_params')}</span>
      </div>

      <div class="form-group">
        <label class="form-label">${t('settings.scan_path')}</label>
        <div style="display: flex; gap: 8px; align-items: center;">
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
          <div style="display: flex; align-items: center; gap: 8px;">
            <input type="number" id="target-size-input" class="form-input no-spin" style="width: 80px; height: 32px; padding: 4px 8px; text-align: center;" min="0.1" max="100" step="0.1" value="1" />
            <span class="range-value" style="min-width: unset;">GB</span>
          </div>
        </div>
        <div class="form-hint">${t('settings.target_size_hint')}</div>
      </div>

      <div class="form-group">
        <label class="form-label">${t('settings.max_depth')}</label>
        <div class="range-container">
          <input type="range" id="max-depth" class="range-slider" min="1" max="10" step="1" value="5" />
          <div style="display: flex; align-items: center; gap: 8px;">
            <input type="number" id="max-depth-input" class="form-input no-spin" style="width: 80px; height: 32px; padding: 4px 8px; text-align: center;" min="1" max="10" step="1" value="5" />
            <span class="range-value" style="min-width: unset;">${t('settings.depth_unit')}</span>
          </div>
        </div>
        <div class="form-hint">${t('settings.max_depth_hint')}</div>
      </div>
    </div>

    <div class="card animate-in mb-24" style="animation-delay: 0.15s">
      <div class="card-header">
        <h2 class="card-title">${t('settings.privilege_config')}</h2>
        <span class="badge badge-warning">${t('settings.privilege_required')}</span>
      </div>
      <div class="form-group" style="display: flex; align-items: center; justify-content: space-between; gap: 12px; margin-bottom: 10px;">
        <span id="admin-status" class="form-hint" style="margin-top: 0;"></span>
        <button type="button" id="request-elevation-btn" class="btn btn-secondary">
          ${t('settings.request_elevation')}
        </button>
      </div>
      <div class="form-hint">${t('settings.privilege_hint')}</div>
    </div>

    <div class="flex items-center justify-between animate-in" style="animation-delay: 0.2s">
      <span id="save-status" class="form-hint"></span>
      <button id="save-btn" class="btn btn-primary btn-lg">${t('settings.save')}</button>
    </div>
  `;

  const remoteModelsCache = new Map();
  const providerApiKeyMap = {};
  let searchApiSettings = {
    provider: 'tavily',
    enabled: false,
    apiKey: '',
    scopes: { scan: false, organizer: false },
  };
  let modelsRequestToken = 0;

  const endpointSelect = document.getElementById('api-endpoint');

  async function updateModelsDropdown(selectedValue) {
    const endpoint = String(endpointSelect?.value || '').trim();
    const apiKey = String(providerApiKeyMap[endpoint] || '').trim();
    const modelSelect = document.getElementById('api-model');
    const requestToken = ++modelsRequestToken;

    modelSelect.disabled = true;
    modelSelect.innerHTML = `<option value="">Loading models...</option>`;

    let models = [];
    const cacheKey = `${endpoint}|${apiKey}`;
    try {
      if (endpoint) {
        if (remoteModelsCache.has(cacheKey)) {
          models = remoteModelsCache.get(cacheKey);
        } else {
          const resp = await getProviderModels(endpoint, apiKey);
          models = normalizeModels(resp?.models || []);
          remoteModelsCache.set(cacheKey, models);
        }
      }
    } catch {
      models = [];
    }

    if (!models.length) {
      models = normalizeModels(PROVIDER_MODELS[endpoint] || PROVIDER_MODELS['https://api.deepseek.com']);
    }
    if (!models.length) {
      models = [{ value: 'gpt-4o-mini', label: 'gpt-4o-mini' }];
    }

    if (requestToken !== modelsRequestToken) return;
    renderModelOptions(models, selectedValue);
    modelSelect.disabled = false;
  }

  endpointSelect?.addEventListener('change', () => {
    updateModelsDropdown();
  });

  try {
    const settings = await getSettings();
    if (settings?.providerConfigs && typeof settings.providerConfigs === 'object') {
      for (const [endpoint, config] of Object.entries(settings.providerConfigs)) {
        providerApiKeyMap[String(endpoint).trim()] = String(config?.apiKey || '').trim();
      }
    }
    if (settings?.apiEndpoint && !providerApiKeyMap[settings.apiEndpoint]) {
      providerApiKeyMap[settings.apiEndpoint] = String(settings?.apiKey || '').trim();
    }
    await fillForm(settings);
  } catch (err) {
    console.warn('Failed to load settings:', err);
  }

  const adminStatusEl = document.getElementById('admin-status');
  const elevationBtn = document.getElementById('request-elevation-btn');

  async function refreshPrivilegeStatus() {
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

  elevationBtn?.addEventListener('click', async () => {
    if (!confirm(t('settings.elevation_confirm'))) return;
    elevationBtn.disabled = true;
    elevationBtn.innerHTML = `<span class="spinner"></span> ${t('settings.requesting_elevation')}`;

    try {
      await requestElevation();
      showToast(t('settings.elevation_uac_prompt'), 'info');
      adminStatusEl.textContent = t('settings.elevation_restarting');
      adminStatusEl.style.color = 'var(--accent-info)';
    } catch (err) {
      showToast(t('settings.elevation_failed') + err.message, 'error');
      elevationBtn.disabled = false;
      elevationBtn.textContent = t('settings.request_elevation');
      await refreshPrivilegeStatus();
    }
  });

  await refreshPrivilegeStatus();

  const sizeSlider = document.getElementById('target-size');
  const sizeInput = document.getElementById('target-size-input');
  sizeSlider.addEventListener('input', () => {
    sizeInput.value = parseFloat(sizeSlider.value).toFixed(1);
  });
  sizeInput.addEventListener('input', () => {
    const val = parseFloat(sizeInput.value);
    if (!Number.isNaN(val)) sizeSlider.value = String(val);
  });
  sizeInput.addEventListener('blur', () => {
    let val = parseFloat(sizeInput.value);
    if (Number.isNaN(val) || val < 0.1) val = 0.1;
    if (val > 100) val = 100;
    sizeInput.value = val.toFixed(1);
    sizeSlider.value = String(val);
  });

  const depthSlider = document.getElementById('max-depth');
  const depthInput = document.getElementById('max-depth-input');
  depthSlider.addEventListener('input', () => {
    depthInput.value = String(parseInt(depthSlider.value, 10));
  });
  depthInput.addEventListener('input', () => {
    const val = parseInt(depthInput.value, 10);
    if (!Number.isNaN(val)) depthSlider.value = String(val);
  });
  depthInput.addEventListener('blur', () => {
    let val = parseInt(depthInput.value, 10);
    if (Number.isNaN(val) || val < 1) val = 1;
    if (val > 10) val = 10;
    depthInput.value = String(val);
    depthSlider.value = String(val);
  });

  document.getElementById('browse-folder-btn')?.addEventListener('click', async () => {
    const btn = document.getElementById('browse-folder-btn');
    btn.disabled = true;
    btn.textContent = t('settings.browsing');
    try {
      const result = await browseFolder();
      if (!result.cancelled && result.path) {
        document.getElementById('scan-path').value = result.path;
        showToast(t('settings.toast_path_selected') + result.path, 'success');
      }
    } catch (err) {
      showToast(t('settings.toast_browse_failed') + err.message, 'error');
    } finally {
      btn.disabled = false;
      btn.textContent = t('settings.browse');
    }
  });

  document.getElementById('save-btn')?.addEventListener('click', async () => {
    const btn = document.getElementById('save-btn');
    const status = document.getElementById('save-status');
    btn.disabled = true;
    btn.innerHTML = `<span class="spinner"></span> ${t('settings.saving')}`;

    try {
      await saveSettings(collectForm(providerApiKeyMap, searchApiSettings));
      showToast(t('settings.toast_saved'), 'success');
      status.textContent = t('settings.saved');
      status.style.color = 'var(--accent-success)';
    } catch (err) {
      showToast(t('settings.toast_save_failed') + err.message, 'error');
      status.textContent = t('settings.save_failed');
      status.style.color = 'var(--accent-danger)';
    } finally {
      btn.disabled = false;
      btn.textContent = t('settings.save');
    }
  });

  async function fillForm(s) {
    const el = (id) => document.getElementById(id);
    if (s.apiEndpoint) {
      const endpointEl = el('api-endpoint');
      const exists = Array.from(endpointEl.options).some((opt) => opt.value === s.apiEndpoint);
      if (!exists) endpointEl.add(new Option(s.apiEndpoint, s.apiEndpoint));
      endpointEl.value = s.apiEndpoint;
    }
    await updateModelsDropdown(String(s.model || ''));

    if (s.scanPath) el('scan-path').value = s.scanPath;
    if (s.targetSizeGB != null) {
      el('target-size').value = s.targetSizeGB;
      el('target-size-input').value = parseFloat(s.targetSizeGB).toFixed(1);
    }
    if (s.maxDepth != null) {
      el('max-depth').value = s.maxDepth;
      el('max-depth-input').value = s.maxDepth;
    }
    const searchApiEnabled = s?.searchApi?.enabled != null ? !!s.searchApi.enabled : true;
    const scanWebSearchEnabled = typeof s?.searchApi?.scopes?.scan === 'boolean'
      ? (searchApiEnabled && !!s.searchApi.scopes.scan)
      : !!s.enableWebSearch;
    const organizerWebSearchEnabled = typeof s?.searchApi?.scopes?.organizer === 'boolean'
      ? (searchApiEnabled && !!s.searchApi.scopes.organizer)
      : (s.enableWebSearchOrganizer != null ? !!s.enableWebSearchOrganizer : scanWebSearchEnabled);
    el('enable-web-search').checked = scanWebSearchEnabled;
    el('enable-organizer-web-search').checked = organizerWebSearchEnabled;
    searchApiSettings = {
      provider: 'tavily',
      enabled: searchApiEnabled,
      apiKey: String(s?.searchApi?.apiKey || s?.tavilyApiKey || '').trim(),
      scopes: {
        scan: !!scanWebSearchEnabled,
        organizer: !!organizerWebSearchEnabled,
      },
    };
  }
}

function collectForm(providerApiKeyMap, searchApiSettings = {}) {
  const endpoint = document.getElementById('api-endpoint').value.trim();
  const model = document.getElementById('api-model').value.trim() || 'deepseek-chat';
  const targetSizeInputVal = document.getElementById('target-size-input')?.value;
  const targetSizeVal = targetSizeInputVal || document.getElementById('target-size').value;
  const maxDepthInputVal = document.getElementById('max-depth-input')?.value;
  const maxDepthVal = maxDepthInputVal || document.getElementById('max-depth').value;

  const scanWebSearchEnabled = !!document.getElementById('enable-web-search').checked;
  const organizerWebSearchEnabled = !!document.getElementById('enable-organizer-web-search').checked;
  const tavilyApiKey = String(searchApiSettings?.apiKey || '').trim();
  const searchApi = {
    provider: 'tavily',
    enabled: scanWebSearchEnabled || organizerWebSearchEnabled,
    scopes: {
      scan: scanWebSearchEnabled,
      organizer: organizerWebSearchEnabled,
    },
  };
  if (tavilyApiKey) {
    searchApi.apiKey = tavilyApiKey;
  }

  return {
    apiEndpoint: endpoint,
    apiKey: String(providerApiKeyMap?.[endpoint] || '').trim(),
    model,
    scanPath: document.getElementById('scan-path').value.trim(),
    targetSizeGB: parseFloat(targetSizeVal),
    maxDepth: parseInt(maxDepthVal, 10),
    enableWebSearch: scanWebSearchEnabled,
    enableWebSearchOrganizer: organizerWebSearchEnabled,
    searchApi,
  };
}
