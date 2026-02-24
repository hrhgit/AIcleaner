/**
 * src/pages/settings.js
 * 设置页面 — API 配置、${t('settings.scan_path')}、清理目标
 */
import { getSettings, saveSettings, browseFolder } from '../utils/api.js';
import { showToast } from '../main.js';
import { t } from '../utils/i18n.js';

const PROVIDER_MODELS = {
    "https://api.openai.com/v1": [
        { value: "gpt-4o-mini", label: "gpt-4o-mini (推荐，性价比高)" },
        { value: "gpt-4o", label: "gpt-4o (性能最强)" },
        { value: "gpt-3.5-turbo", label: "gpt-3.5-turbo" }
    ],
    "https://api.deepseek.com": [
        { value: "deepseek-chat", label: "deepseek-chat (DeepSeek V3)" },
        { value: "deepseek-reasoner", label: "deepseek-reasoner (DeepSeek R1)" }
    ],
    "https://dashscope.aliyuncs.com/compatible-mode/v1": [
        { value: "qwen-plus", label: "qwen-plus" },
        { value: "qwen-turbo", label: "qwen-turbo" },
        { value: "qwen-max", label: "qwen-max" }
    ],
    "https://open.bigmodel.cn/api/paas/v4": [
        { value: "glm-4-flash", label: "glm-4-flash (推荐，免费)" },
        { value: "glm-4", label: "glm-4" }
    ],
    "https://api.moonshot.cn/v1": [
        { value: "moonshot-v1-8k", label: "moonshot-v1-8k" },
        { value: "moonshot-v1-32k", label: "moonshot-v1-32k" }
    ],
    "https://generativelanguage.googleapis.com/v1beta/openai/": [
        { value: "gemini-2.5-flash", label: "gemini-2.5-flash (推荐)" },
        { value: "gemini-2.5-pro", label: "gemini-2.5-pro" },
        { value: "gemini-2.0-flash", label: "gemini-2.0-flash" },
        { value: "gemini-1.5-pro", label: "gemini-1.5-pro" }
    ]
};

export async function renderSettings(container) {
    container.innerHTML = `
    <style>
      /* Force remove spin buttons irrespective of global CSS or cache */
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

      <div class="form-group">
        <label class="form-label">${t('settings.provider')}</label>
        <select id="api-endpoint" class="form-input">
          <option value="https://api.deepseek.com">DeepSeek</option>
          <option value="https://api.openai.com/v1">OpenAI</option>
          <option value="https://generativelanguage.googleapis.com/v1beta/openai/">Google Gemini</option>
          <option value="https://dashscope.aliyuncs.com/compatible-mode/v1">通义千问 (阿里云)</option>
          <option value="https://open.bigmodel.cn/api/paas/v4">智谱 GLM</option>
          <option value="https://api.moonshot.cn/v1">Kimi (月之暗面)</option>
        </select>
        <div class="form-hint">${t('settings.provider_hint')}</div>
      </div>

      <div class="form-group">
        <label class="form-label">${t('settings.api_key')}</label>
        <input type="password" id="api-key" class="form-input"
               placeholder="${t('settings.api_key_placeholder')}" />
        <div class="form-hint">${t('settings.api_key_hint')}</div>
      </div>

      <div class="form-group">
        <label class="form-label">${t('settings.model')}</label>
        <select id="api-model" class="form-input">
          <!-- 这里将根据上面的服务商动态生成 -->
        </select>
        <div class="form-hint">${t('settings.model_hint')}</div>
      </div>
    </div>

    <div class="card animate-in mb-24" style="animation-delay: 0.1s">
      <div class="card-header">
        <h2 class="card-title">${t('settings.search_config')}</h2>
        <span class="badge badge-warning">${t('settings.expert_feature')}</span>
      </div>

      <div class="form-group" style="display: flex; align-items: center; gap: 12px;">
        <input type="checkbox" id="enable-web-search" class="toggle-checkbox" style="width: 20px; height: 20px;" />
        <label for="enable-web-search" class="form-label" style="margin-bottom: 0; cursor: pointer;">${t('settings.enable_search')}</label>
      </div>
      <div class="form-hint" style="margin-bottom: 16px;">${t('settings.search_hint')}</div>

      <div class="form-group" id="tavily-api-key-group" style="display: none; border-left: 2px solid var(--border); padding-left: 12px; margin-left: 8px;">
        <label class="form-label">${t('settings.tavily_key')}</label>
        <input type="password" id="tavily-api-key" class="form-input"
               placeholder="tvly-xxxxxxxxxxxxxxx" />
        <div class="form-hint"><a href="https://tavily.com/" target="_blank" style="color: var(--accent-info); text-decoration: underline;">${t('settings.tavily_hint')}</a></div>
      </div>
    </div>

    <div class="card animate-in mb-24" style="animation-delay: 0.15s">
      <div class="card-header">
        <h2 class="card-title">${t('settings.scan_config')}</h2>
        <span class="badge badge-secondary">${t('settings.scan_params')}</span>
      </div>

      <div class="form-group">
        <label class="form-label">扫描路径</label>
        <div style="display: flex; gap: 8px; align-items: center;">
          <input type="text" id="scan-path" class="form-input" style="flex: 1;"
                 placeholder="C:\\Users\\YourName\\Downloads" />
          <button type="button" id="browse-folder-btn" class="btn btn-secondary"
                  style="white-space: nowrap; flex-shrink: 0;"
                  title="打开文件夹选择对话框">
            📁 浏览
          </button>
        </div>
        <div class="form-hint">${t('settings.browse_hint')}</div>
      </div>

      <div class="form-group">
        <label class="form-label">${t('settings.target_size')}</label>
        <div class="range-container">
          <input type="range" id="target-size" class="range-slider"
                 min="0.1" max="100" step="0.1" value="1" />
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
          <input type="range" id="max-depth" class="range-slider"
                 min="1" max="10" step="1" value="5" />
          <div style="display: flex; align-items: center; gap: 8px;">
            <input type="number" id="max-depth-input" class="form-input no-spin" style="width: 80px; height: 32px; padding: 4px 8px; text-align: center;" min="1" max="10" step="1" value="5" />
            <span class="range-value" style="min-width: unset;">层</span>
          </div>
        </div>
        <div class="form-hint">${t('settings.max_depth_hint')}</div>
      </div>
    </div>

    <div class="flex items-center justify-between animate-in" style="animation-delay: 0.15s">
      <span id="save-status" class="form-hint"></span>
      <button id="save-btn" class="btn btn-primary btn-lg">
        ${t('settings.save')}
      </button>
    </div>
  `;

    // Dropdown logic function
    function updateModelsDropdown(selectedValue) {
        const modelSelect = document.getElementById('api-model');
        const models = PROVIDER_MODELS[document.getElementById('api-endpoint').value] || PROVIDER_MODELS["https://api.deepseek.com"];
        modelSelect.innerHTML = models.map(m => `<option value="${m.value}">${m.label}</option>`).join('');

        if (selectedValue) {
            // Check if selected value exists in options, otherwise append it
            let exists = Array.from(modelSelect.options).some(opt => opt.value === selectedValue);
            if (!exists) {
                const newOption = new Option(selectedValue, selectedValue);
                modelSelect.add(newOption);
            }
            modelSelect.value = selectedValue;
        }
    }

    // Bind event for endpoint change
    const endpointSelect = document.getElementById('api-endpoint');
    endpointSelect.addEventListener('change', () => {
        updateModelsDropdown();
    });

    // Initialize dropdowns with default
    updateModelsDropdown();

    // Load existing settings
    try {
        const settings = await getSettings();
        fillForm(settings);
    } catch (err) {
        console.warn('Failed to load settings:', err);
    }

    // Web search toggle logic
    const searchToggle = document.getElementById('enable-web-search');
    const tavilyGroup = document.getElementById('tavily-api-key-group');

    function updateTavilyVisibility() {
        tavilyGroup.style.display = searchToggle.checked ? 'block' : 'none';
    }
    searchToggle.addEventListener('change', updateTavilyVisibility);
    updateTavilyVisibility();

    // Range slider & input live updates
    const sizeSlider = document.getElementById('target-size');
    const sizeInput = document.getElementById('target-size-input');

    sizeSlider.addEventListener('input', () => {
        sizeInput.value = parseFloat(sizeSlider.value).toFixed(1);
    });
    sizeInput.addEventListener('input', () => {
        let val = parseFloat(sizeInput.value);
        if (!isNaN(val)) sizeSlider.value = val;
    });
    sizeInput.addEventListener('blur', () => {
        let val = parseFloat(sizeInput.value);
        if (isNaN(val) || val < 0.1) val = 0.1;
        if (val > 100) val = 100;
        sizeInput.value = val.toFixed(1);
        sizeSlider.value = val;
    });

    const depthSlider = document.getElementById('max-depth');
    const depthInput = document.getElementById('max-depth-input');
    depthSlider.addEventListener('input', () => {
        depthInput.value = parseInt(depthSlider.value, 10);
    });
    depthInput.addEventListener('input', () => {
        let val = parseInt(depthInput.value, 10);
        if (!isNaN(val)) depthSlider.value = val;
    });
    depthInput.addEventListener('blur', () => {
        let val = parseInt(depthInput.value, 10);
        if (isNaN(val) || val < 1) val = 1;
        if (val > 10) val = 10;
        depthInput.value = val;
        depthSlider.value = val;
    });

    // Browse folder button
    document.getElementById('browse-folder-btn').addEventListener('click', async () => {
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

    // Save button
    document.getElementById('save-btn').addEventListener('click', async () => {
        const btn = document.getElementById('save-btn');
        const status = document.getElementById('save-status');
        btn.disabled = true;
        btn.innerHTML = `<span class="spinner"></span> ${t('settings.saving')}`;

        try {
            await saveSettings(collectForm());
            showToast(t('settings.toast_saved'), 'success');
            status.textContent = t('settings.saved');
            status.style.color = 'var(--accent-success)';
        } catch (err) {
            showToast(t('settings.toast_save_failed') + err.message, 'error');
            status.textContent = t('settings.save_failed');
            status.style.color = 'var(--accent-danger)';
        } finally {
            btn.disabled = false;
            btn.innerHTML = t('settings.save');
        }
    });

    function fillForm(s) {
        const el = (id) => document.getElementById(id);
        if (s.apiEndpoint) {
            let endpointEl = el('api-endpoint');
            let exists = Array.from(endpointEl.options).some(opt => opt.value === s.apiEndpoint);
            if (!exists) {
                const newOption = new Option(s.apiEndpoint, s.apiEndpoint);
                endpointEl.add(newOption);
            }
            endpointEl.value = s.apiEndpoint;
        }
        if (s.apiKey) el('api-key').value = s.apiKey;
        if (s.model) {
            updateModelsDropdown(s.model);
        } else {
            updateModelsDropdown();
        }
        if (s.scanPath) el('scan-path').value = s.scanPath;
        if (s.targetSizeGB != null) {
            el('target-size').value = s.targetSizeGB;
            if (el('target-size-input')) {
                el('target-size-input').value = parseFloat(s.targetSizeGB).toFixed(1);
            }
        }
        if (s.maxDepth != null) {
            el('max-depth').value = s.maxDepth;
            if (el('max-depth-input')) {
                el('max-depth-input').value = s.maxDepth;
            }
        }
        if (s.enableWebSearch != null) {
            el('enable-web-search').checked = !!s.enableWebSearch;
            if (!!s.enableWebSearch) el('tavily-api-key-group').style.display = 'block';
        }
        if (s.tavilyApiKey != null) el('tavily-api-key').value = s.tavilyApiKey;
    }
}

function collectForm() {
    const targetSizeInputVal = document.getElementById('target-size-input') ? document.getElementById('target-size-input').value : null;
    const targetSizeVal = targetSizeInputVal || document.getElementById('target-size').value;

    const maxDepthInputVal = document.getElementById('max-depth-input') ? document.getElementById('max-depth-input').value : null;
    const maxDepthVal = maxDepthInputVal || document.getElementById('max-depth').value;

    return {
        apiEndpoint: document.getElementById('api-endpoint').value.trim(),
        apiKey: document.getElementById('api-key').value.trim(),
        model: document.getElementById('api-model').value.trim() || 'deepseek-chat',
        scanPath: document.getElementById('scan-path').value.trim(),
        targetSizeGB: parseFloat(targetSizeVal),
        maxDepth: parseInt(maxDepthVal, 10),
        enableWebSearch: document.getElementById('enable-web-search').checked,
        tavilyApiKey: document.getElementById('tavily-api-key').value.trim(),
    };
}