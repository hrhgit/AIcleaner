/**
 * src/pages/settings.js
 * 设置页面 — API 配置、扫描路径、清理目标
 */
import { getSettings, saveSettings, browseFolder } from '../utils/api.js';
import { showToast } from '../main.js';

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
    <div class="page-header animate-in">
      <h1 class="page-title">⚙️ 设置</h1>
      <p class="page-subtitle">配置 AI 分析引擎和扫描参数</p>
    </div>

    <div class="card animate-in mb-24" style="animation-delay: 0.05s">
      <div class="card-header">
        <h2 class="card-title">🔑 API 配置</h2>
        <span class="badge badge-info">LLM 引擎</span>
      </div>

      <div class="form-group">
        <label class="form-label">服务商 (API Endpoint)</label>
        <select id="api-endpoint" class="form-input">
          <option value="https://api.deepseek.com">DeepSeek</option>
          <option value="https://api.openai.com/v1">OpenAI</option>
          <option value="https://generativelanguage.googleapis.com/v1beta/openai/">Google Gemini</option>
          <option value="https://dashscope.aliyuncs.com/compatible-mode/v1">通义千问 (阿里云)</option>
          <option value="https://open.bigmodel.cn/api/paas/v4">智谱 GLM</option>
          <option value="https://api.moonshot.cn/v1">Kimi (月之暗面)</option>
        </select>
        <div class="form-hint">选择提供大模型服务的厂商</div>
      </div>

      <div class="form-group">
        <label class="form-label">API Key</label>
        <input type="password" id="api-key" class="form-input"
               placeholder="在此处填写你的 API Key" />
        <div class="form-hint">密钥仅存储在本地服务器，不会上传到任何第三方</div>
      </div>

      <div class="form-group">
        <label class="form-label">模型设定</label>
        <select id="api-model" class="form-input">
          <!-- 这里将根据上面的服务商动态生成 -->
        </select>
        <div class="form-hint">选择适用的模型（根据所选的服务商自动更新）</div>
      </div>
    </div>

    <div class="card animate-in mb-24" style="animation-delay: 0.1s">
      <div class="card-header">
        <h2 class="card-title">🌐 联网与搜索设置</h2>
        <span class="badge badge-warning">专家功能</span>
      </div>

      <div class="form-group" style="display: flex; align-items: center; gap: 12px;">
        <input type="checkbox" id="enable-web-search" class="toggle-checkbox" style="width: 20px; height: 20px;" />
        <label for="enable-web-search" class="form-label" style="margin-bottom: 0; cursor: pointer;">启用 AI 自动联网搜索 (通过 Tavily)</label>
      </div>
      <div class="form-hint" style="margin-bottom: 16px;">当大模型无法确定可疑文件的用途时，将自动调用搜索引擎进行辅助判断。</div>

      <div class="form-group" id="tavily-api-key-group" style="display: none; border-left: 2px solid var(--border); padding-left: 12px; margin-left: 8px;">
        <label class="form-label">Tavily API Key</label>
        <input type="password" id="tavily-api-key" class="form-input"
               placeholder="tvly-xxxxxxxxxxxxxxx" />
        <div class="form-hint">前往 <a href="https://tavily.com/" target="_blank" style="color: var(--accent-info); text-decoration: underline;">Tavily 官网</a> 申请免费 API Key (每月 1000 次查询)</div>
      </div>
    </div>

    <div class="card animate-in mb-24" style="animation-delay: 0.15s">
      <div class="card-header">
        <h2 class="card-title">📂 扫描配置</h2>
        <span class="badge badge-secondary">扫描参数</span>
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
        <div class="form-hint">输入要扫描的文件夹绝对路径，或点击「浏览」选择</div>
      </div>

      <div class="form-group">
        <label class="form-label">期望清理空间</label>
        <div class="range-container">
          <input type="range" id="target-size" class="range-slider"
                 min="0.1" max="100" step="0.1" value="1" />
          <span id="target-size-value" class="range-value">1.0 GB</span>
        </div>
        <div class="form-hint">当可清理空间达到此目标时，扫描将自动停止</div>
      </div>

      <div class="form-group">
        <label class="form-label">最大扫描深度</label>
        <div class="range-container">
          <input type="range" id="max-depth" class="range-slider"
                 min="1" max="10" step="1" value="5" />
          <span id="max-depth-value" class="range-value">5 层</span>
        </div>
        <div class="form-hint">限制递归下探的目录层级数量</div>
      </div>
    </div>

    <div class="flex items-center justify-between animate-in" style="animation-delay: 0.15s">
      <span id="save-status" class="form-hint"></span>
      <button id="save-btn" class="btn btn-primary btn-lg">
        💾 保存设置
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

  // Range slider live updates
  const sizeSlider = document.getElementById('target-size');
  const sizeLabel = document.getElementById('target-size-value');
  sizeSlider.addEventListener('input', () => {
    sizeLabel.textContent = `${parseFloat(sizeSlider.value).toFixed(1)} GB`;
  });

  const depthSlider = document.getElementById('max-depth');
  const depthLabel = document.getElementById('max-depth-value');
  depthSlider.addEventListener('input', () => {
    depthLabel.textContent = `${depthSlider.value} 层`;
  });

  // Browse folder button
  document.getElementById('browse-folder-btn').addEventListener('click', async () => {
    const btn = document.getElementById('browse-folder-btn');
    btn.disabled = true;
    btn.textContent = '⏳ 选择中...';
    try {
      const result = await browseFolder();
      if (!result.cancelled && result.path) {
        document.getElementById('scan-path').value = result.path;
        showToast('已选择路径: ' + result.path, 'success');
      }
    } catch (err) {
      showToast('选择文件夹失败: ' + err.message, 'error');
    } finally {
      btn.disabled = false;
      btn.textContent = '📁 浏览';
    }
  });

  // Save button
  document.getElementById('save-btn').addEventListener('click', async () => {
    const btn = document.getElementById('save-btn');
    const status = document.getElementById('save-status');
    btn.disabled = true;
    btn.innerHTML = '<span class="spinner"></span> 保存中...';

    try {
      await saveSettings(collectForm());
      showToast('设置已保存', 'success');
      status.textContent = '✓ 已保存';
      status.style.color = 'var(--accent-success)';
    } catch (err) {
      showToast('保存失败: ' + err.message, 'error');
      status.textContent = '✗ 保存失败';
      status.style.color = 'var(--accent-danger)';
    } finally {
      btn.disabled = false;
      btn.innerHTML = '💾 保存设置';
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
      el('target-size-value').textContent = `${parseFloat(s.targetSizeGB).toFixed(1)} GB`;
    }
    if (s.maxDepth != null) {
      el('max-depth').value = s.maxDepth;
      el('max-depth-value').textContent = `${s.maxDepth} 层`;
    }
    if (s.enableWebSearch != null) {
      el('enable-web-search').checked = !!s.enableWebSearch;
      if (!!s.enableWebSearch) el('tavily-api-key-group').style.display = 'block';
    }
    if (s.tavilyApiKey != null) el('tavily-api-key').value = s.tavilyApiKey;
  }
}

function collectForm() {
  return {
    apiEndpoint: document.getElementById('api-endpoint').value.trim(),
    apiKey: document.getElementById('api-key').value.trim(),
    model: document.getElementById('api-model').value.trim() || 'deepseek-chat',
    scanPath: document.getElementById('scan-path').value.trim(),
    targetSizeGB: parseFloat(document.getElementById('target-size').value),
    maxDepth: parseInt(document.getElementById('max-depth').value, 10),
    enableWebSearch: document.getElementById('enable-web-search').checked,
    tavilyApiKey: document.getElementById('tavily-api-key').value.trim(),
  };
}
