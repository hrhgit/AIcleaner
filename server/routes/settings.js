/**
 * server/routes/settings.js
 * 设置持久化路由 — 读写 API 配置和扫描选项
 */
import { Router } from 'express';
import { existsSync } from 'fs';
import { join, dirname } from 'path';
import { fileURLToPath } from 'url';
import { readJsonFileWithBackup, writeJsonFileAtomic } from '../json-file.js';

const __dirname = dirname(fileURLToPath(import.meta.url));
const DATA_DIR = join(__dirname, '..', 'data');
const SETTINGS_FILE = join(DATA_DIR, 'settings.json');

const BUILTIN_PROVIDER_PRESETS = [
    { name: 'DeepSeek', endpoint: 'https://api.deepseek.com', model: 'deepseek-chat' },
    { name: 'OpenAI', endpoint: 'https://api.openai.com/v1', model: 'gpt-4o-mini' },
    { name: 'Google Gemini', endpoint: 'https://generativelanguage.googleapis.com/v1beta/openai/', model: 'gemini-2.5-flash' },
    { name: 'Qwen', endpoint: 'https://dashscope.aliyuncs.com/compatible-mode/v1', model: 'qwen-plus' },
    { name: 'GLM', endpoint: 'https://open.bigmodel.cn/api/paas/v4', model: 'glm-4-flash' },
    { name: 'Moonshot', endpoint: 'https://api.moonshot.cn/v1', model: 'moonshot-v1-8k' },
];

function normalizeEndpointValue(value) {
    return String(value || '').trim();
}

function defaultModelByEndpoint(endpoint) {
    const value = normalizeEndpointValue(endpoint).toLowerCase();
    if (value.includes('deepseek.com')) return 'deepseek-chat';
    if (value.includes('generativelanguage.googleapis.com')) return 'gemini-2.5-flash';
    if (value.includes('dashscope.aliyuncs.com')) return 'qwen-plus';
    if (value.includes('bigmodel.cn')) return 'glm-4-flash';
    if (value.includes('moonshot.cn')) return 'moonshot-v1-8k';
    return 'gpt-4o-mini';
}

function resolveProviderSettings(input) {
    const sourceConfigs = input?.providerConfigs && typeof input.providerConfigs === 'object'
        ? input.providerConfigs
        : {};
    const providerConfigs = {};

    for (const preset of BUILTIN_PROVIDER_PRESETS) {
        const source = sourceConfigs[preset.endpoint] || {};
        providerConfigs[preset.endpoint] = {
            name: String(source?.name || preset.name),
            endpoint: preset.endpoint,
            apiKey: String(source?.apiKey || ''),
            model: String(source?.model || preset.model),
        };
    }

    for (const [key, rawConfig] of Object.entries(sourceConfigs)) {
        const endpoint = normalizeEndpointValue(rawConfig?.endpoint || key);
        if (!endpoint || providerConfigs[endpoint]) continue;
        providerConfigs[endpoint] = {
            name: String(rawConfig?.name || endpoint),
            endpoint,
            apiKey: String(rawConfig?.apiKey || ''),
            model: String(rawConfig?.model || defaultModelByEndpoint(endpoint)),
        };
    }

    const legacyEndpoint = normalizeEndpointValue(input?.apiEndpoint)
        || BUILTIN_PROVIDER_PRESETS[0].endpoint;
    if (!providerConfigs[legacyEndpoint]) {
        providerConfigs[legacyEndpoint] = {
            name: legacyEndpoint,
            endpoint: legacyEndpoint,
            apiKey: '',
            model: defaultModelByEndpoint(legacyEndpoint),
        };
    }

    const legacyApiKey = String(input?.apiKey || '').trim();
    const legacyModel = String(input?.model || '').trim();
    if (legacyApiKey) providerConfigs[legacyEndpoint].apiKey = legacyApiKey;
    if (legacyModel) providerConfigs[legacyEndpoint].model = legacyModel;

    let defaultProviderEndpoint = normalizeEndpointValue(input?.defaultProviderEndpoint);
    if (!providerConfigs[defaultProviderEndpoint]) {
        defaultProviderEndpoint = legacyEndpoint;
    }

    const activeConfig = providerConfigs[defaultProviderEndpoint] || {
        endpoint: defaultProviderEndpoint,
        apiKey: '',
        model: defaultModelByEndpoint(defaultProviderEndpoint),
    };

    return {
        providerConfigs,
        defaultProviderEndpoint,
        apiEndpoint: defaultProviderEndpoint,
        apiKey: String(activeConfig.apiKey || ''),
        model: String(activeConfig.model || defaultModelByEndpoint(defaultProviderEndpoint)),
    };
}

function resolveSearchSettings(input) {
    const source = input?.searchApi && typeof input.searchApi === 'object'
        ? input.searchApi
        : {};
    const scopesSource = source?.scopes && typeof source.scopes === 'object'
        ? source.scopes
        : {};

    const legacyScan = typeof input?.enableWebSearch === 'boolean'
        ? input.enableWebSearch
        : false;
    const legacyClassify = typeof input?.enableWebSearchClassify === 'boolean'
        ? input.enableWebSearchClassify
        : (typeof input?.enableWebSearchOrganizer === 'boolean'
            ? input.enableWebSearchOrganizer
            : legacyScan);
    const legacyOrganizer = typeof input?.enableWebSearchOrganizer === 'boolean'
        ? input.enableWebSearchOrganizer
        : legacyScan;

    const scanEnabled = typeof scopesSource.scan === 'boolean'
        ? scopesSource.scan
        : legacyScan;
    const classifyEnabled = typeof scopesSource.classify === 'boolean'
        ? scopesSource.classify
        : (typeof scopesSource.organizer === 'boolean'
            ? scopesSource.organizer
            : legacyClassify);
    const organizerEnabled = typeof scopesSource.organizer === 'boolean'
        ? scopesSource.organizer
        : classifyEnabled;

    const enabled = typeof source.enabled === 'boolean'
        ? source.enabled
        : (scanEnabled || classifyEnabled || organizerEnabled);
    const apiKey = String(source.apiKey || input?.tavilyApiKey || '').trim();

    const searchApi = {
        provider: 'tavily',
        enabled: !!enabled,
        apiKey,
        scopes: {
            scan: !!scanEnabled,
            classify: !!classifyEnabled,
            organizer: !!organizerEnabled,
        },
    };

    return {
        searchApi,
        enableWebSearch: searchApi.enabled && searchApi.scopes.scan,
        enableWebSearchClassify: searchApi.enabled && searchApi.scopes.classify,
        enableWebSearchOrganizer: searchApi.enabled && searchApi.scopes.organizer,
        tavilyApiKey: searchApi.apiKey,
    };
}

const DEFAULT_SETTINGS = {
    apiEndpoint: process.env.VITE_API_ENDPOINT || process.env.OPENAI_BASE_URL || 'https://api.openai.com/v1',
    apiKey: process.env.VITE_API_KEY || process.env.OPENAI_API_KEY || '',
    model: process.env.VITE_MODEL || process.env.OPENAI_MODEL || 'gpt-4o-mini',
    defaultProviderEndpoint: process.env.VITE_API_ENDPOINT || process.env.OPENAI_BASE_URL || 'https://api.openai.com/v1',
    providerConfigs: {},
    scanPath: '',
    targetSizeGB: 1,
    maxDepth: 5,
    lastScanTime: null,
    enableWebSearch: false,
    enableWebSearchClassify: false,
    enableWebSearchOrganizer: false,
    tavilyApiKey: process.env.TAVILY_API_KEY || '',
    searchApi: {
        provider: 'tavily',
        enabled: false,
        apiKey: process.env.TAVILY_API_KEY || '',
        scopes: {
            scan: false,
            classify: false,
            organizer: false,
        },
    },
};

export function loadSettings() {
    try {
        if (existsSync(SETTINGS_FILE) || existsSync(`${SETTINGS_FILE}.bak`)) {
            const merged = { ...DEFAULT_SETTINGS, ...readJsonFileWithBackup(SETTINGS_FILE) };
            return {
                ...merged,
                ...resolveProviderSettings(merged),
                ...resolveSearchSettings(merged),
            };
        }
    } catch (err) {
        console.error('[Settings] Failed to load:', err.message);
    }
    return {
        ...DEFAULT_SETTINGS,
        ...resolveProviderSettings(DEFAULT_SETTINGS),
        ...resolveSearchSettings(DEFAULT_SETTINGS),
    };
}

function saveSettings(data) {
    const merged = { ...loadSettings(), ...data };
    const normalized = {
        ...merged,
        ...resolveProviderSettings(merged),
        ...resolveSearchSettings(merged),
    };
    writeJsonFileAtomic(SETTINGS_FILE, normalized);
    return normalized;
}

export const settingsRouter = Router();

settingsRouter.get('/', (req, res) => {
    res.json(loadSettings());
});

settingsRouter.post('/', (req, res) => {
    try {
        const saved = saveSettings(req.body);
        res.json({ success: true, settings: saved });
    } catch (err) {
        res.status(500).json({ success: false, error: err.message });
    }
});

settingsRouter.post('/models', async (req, res) => {
    const endpoint = String(req.body?.endpoint || '').trim();
    const apiKey = String(req.body?.apiKey || '').trim();

    if (!endpoint) {
        return res.status(400).json({ success: false, error: 'Missing endpoint' });
    }

    let modelsUrl;
    try {
        modelsUrl = new URL(`${endpoint.replace(/\/+$/, '')}/models`);
    } catch {
        return res.status(400).json({ success: false, error: 'Invalid endpoint URL' });
    }

    const headers = { Accept: 'application/json' };
    if (apiKey) {
        headers.Authorization = `Bearer ${apiKey}`;
        headers['x-api-key'] = apiKey;
        headers['api-key'] = apiKey;
    }

    const controller = new AbortController();
    const timeout = setTimeout(() => controller.abort(), 10000);

    try {
        const resp = await fetch(modelsUrl, {
            method: 'GET',
            headers,
            signal: controller.signal,
        });
        clearTimeout(timeout);

        if (!resp.ok) {
            const text = await resp.text().catch(() => '');
            return res.status(502).json({
                success: false,
                error: `Failed to fetch models (${resp.status})`,
                detail: text.slice(0, 300),
            });
        }

        const payload = await resp.json().catch(() => ({}));
        const rawModels = Array.isArray(payload?.data)
            ? payload.data
            : Array.isArray(payload?.models)
                ? payload.models
                : [];

        const seen = new Set();
        const models = [];

        for (const item of rawModels) {
            const id = typeof item === 'string'
                ? item.trim()
                : String(item?.id || item?.name || item?.model || '').trim();
            if (!id || seen.has(id)) continue;
            seen.add(id);
            models.push({ value: id, label: id });
        }

        res.json({ success: true, models });
    } catch (err) {
        clearTimeout(timeout);
        res.status(502).json({
            success: false,
            error: err.name === 'AbortError' ? 'Fetch models timeout' : err.message,
        });
    }
});

/**
 * POST /api/settings/browse-folder
 * Opens native folder selection dialog and returns the chosen path.
 */
settingsRouter.post('/browse-folder', async (req, res) => {
    const { execSync } = await import('child_process');
    try {
        const psScript = [
            'Add-Type -AssemblyName System.Windows.Forms',
            '$dialog = New-Object System.Windows.Forms.FolderBrowserDialog',
            "$dialog.Description = '选择要扫描的文件夹'",
            '$dialog.ShowNewFolderButton = $false',
            '$result = $dialog.ShowDialog()',
            'if ($result -eq [System.Windows.Forms.DialogResult]::OK) { Write-Output $dialog.SelectedPath } else { Write-Output __CANCELLED__ }',
        ].join('; ');
        // Encode as UTF-16LE Base64 for -EncodedCommand
        const encoded = Buffer.from(psScript, 'utf16le').toString('base64');
        const selected = execSync(
            `powershell -NoProfile -EncodedCommand ${encoded}`,
            { encoding: 'utf-8', timeout: 60000 }
        ).trim();

        if (selected === '__CANCELLED__') {
            return res.json({ success: true, cancelled: true });
        }
        res.json({ success: true, cancelled: false, path: selected });
    } catch (err) {
        res.status(500).json({ success: false, error: err.message });
    }
});
