import { EventEmitter } from 'events';
import { basename, dirname, extname, join, relative } from 'path';
import { fileURLToPath } from 'url';
import { existsSync, mkdirSync, readFileSync, writeFileSync } from 'fs';
import { access, readdir, stat } from 'fs/promises';
import OpenAI from 'openai';
import pLimit from 'p-limit';
import fsExtra from 'fs-extra';

import { loadSettings } from './routes/settings.js';
import { isRateLimitError, retryWithBackoff, withRemoteLimit } from './remote-control.js';
import { extractFileContent } from './content-extractor.js';
import { performTavilySearch } from './search.js';

const __dirname = dirname(fileURLToPath(import.meta.url));
const DATA_DIR = join(__dirname, 'data');
const JOBS_DIR = join(DATA_DIR, 'organize-jobs');
const CONTEXT_RETRY_TEXT_CHAR_LIMIT = 12000;
const SEARCH_CONTEXT_MAX_CHARS = 2400;

export const DEFAULT_CATEGORY_LIST = [
    '工作学习',
    '财务票据',
    '媒体素材',
    '开发项目',
    '安装与压缩',
    '临时下载',
    '其他待定',
];

export const DEFAULT_EXCLUDED_PATTERNS = [
    '.git',
    '.svn',
    '.hg',
    'node_modules',
    'dist',
    'build',
    'out',
    'tmp',
    'temp',
    '$recycle.bin',
    'windows',
    'program files',
    'program files (x86)',
];

const VIDEO_EXTENSIONS = new Set(['.mp4', '.mov', '.mkv', '.avi', '.wmv', '.flv', '.webm', '.m4v']);
const AUDIO_EXTENSIONS = new Set(['.mp3', '.wav', '.m4a', '.aac', '.flac', '.ogg', '.wma', '.opus']);

function ensureJobDir() {
    if (!existsSync(JOBS_DIR)) {
        mkdirSync(JOBS_DIR, { recursive: true });
    }
}

function nowIso() {
    return new Date().toISOString();
}

function uniqueId(prefix = 'org') {
    return `${prefix}_${Date.now().toString(36)}${Math.random().toString(36).slice(2, 7)}`;
}

function extractJsonText(content = '') {
    let clean = String(content || '').trim();
    if (clean.startsWith('```json')) {
        clean = clean.replace(/^```json/i, '').replace(/```$/, '').trim();
    } else if (clean.startsWith('```')) {
        clean = clean.replace(/^```/, '').replace(/```$/, '').trim();
    }
    return clean;
}

function normalizeCategories(categories) {
    const source = Array.isArray(categories) ? categories : [];
    const out = [];
    for (const item of source) {
        const name = String(item || '').trim();
        if (name && !out.includes(name)) {
            out.push(name);
        }
    }
    if (!out.includes('其他待定')) {
        out.push('其他待定');
    }
    if (out.length === 0) {
        return [...DEFAULT_CATEGORY_LIST];
    }
    return out;
}

function normalizeExcludedPatterns(patterns) {
    const merged = [...DEFAULT_EXCLUDED_PATTERNS, ...(Array.isArray(patterns) ? patterns : [])];
    return merged
        .map((x) => String(x || '').trim().toLowerCase())
        .filter(Boolean)
        .filter((x, idx, arr) => arr.indexOf(x) === idx);
}

function normalizeParallelism(parallelism) {
    const n = Number(parallelism);
    if (!Number.isFinite(n)) return 5;
    return Math.max(1, Math.min(20, Math.floor(n)));
}

function sanitizeCategoryName(category) {
    const raw = String(category || '').trim() || '其他待定';
    return raw.replace(/[\\/:*?"<>|]/g, '_').replace(/\s+/g, ' ').trim() || '其他待定';
}

function fallbackCategoryFromFilename(name = '', categories = []) {
    const ext = extname(name).toLowerCase();
    const pick = (value) => (categories.includes(value) ? value : '其他待定');

    if (['.jpg', '.jpeg', '.png', '.gif', '.webp', '.mp4', '.mov', '.mp3', '.wav'].includes(ext)) {
        return pick('媒体素材');
    }
    if (['.doc', '.docx', '.xls', '.xlsx', '.ppt', '.pptx', '.pdf'].includes(ext)) {
        return pick('工作学习');
    }
    if (['.zip', '.rar', '.7z', '.msi', '.exe', '.dmg'].includes(ext)) {
        return pick('安装与压缩');
    }
    if (['.js', '.ts', '.py', '.java', '.go', '.rs', '.cpp'].includes(ext)) {
        return pick('开发项目');
    }

    return '其他待定';
}

function normalizeModelName(value) {
    const text = String(value || '').trim();
    return text || '';
}

function pickFastModelByEndpoint(endpoint, fallbackModel = 'gpt-4o-mini') {
    const value = String(endpoint || '').toLowerCase();
    if (value.includes('openai.com')) return 'gpt-4o-mini';
    if (value.includes('deepseek.com')) return 'deepseek-chat';
    if (value.includes('generativelanguage.googleapis.com')) return 'gemini-2.5-flash';
    if (value.includes('dashscope.aliyuncs.com')) return 'qwen-turbo';
    if (value.includes('bigmodel.cn')) return 'glm-4-flash';
    if (value.includes('moonshot.cn')) return 'moonshot-v1-8k';
    return normalizeModelName(fallbackModel) || 'gpt-4o-mini';
}

function pickFastModel(settings) {
    return pickFastModelByEndpoint(settings?.apiEndpoint, settings?.model);
}

function normalizeEndpointValue(value, fallbackValue = 'https://api.openai.com/v1') {
    const endpoint = String(value || '').trim();
    return endpoint || String(fallbackValue || 'https://api.openai.com/v1').trim() || 'https://api.openai.com/v1';
}

function normalizeApiKeyValue(value, fallbackValue = '') {
    const key = String(value || '').trim();
    return key || String(fallbackValue || '').trim();
}

function normalizeRouteConfig(route, fallbackRoute) {
    const source = route && typeof route === 'object' ? route : {};
    const fallback = fallbackRoute && typeof fallbackRoute === 'object' ? fallbackRoute : {};
    const endpoint = normalizeEndpointValue(source.endpoint, fallback.endpoint);
    const apiKey = normalizeApiKeyValue(source.apiKey, fallback.apiKey);
    const model = normalizeModelName(source.model)
        || pickFastModelByEndpoint(endpoint, fallback.model)
        || 'gpt-4o-mini';
    return { endpoint, apiKey, model };
}

function normalizeModelRouting(modelRouting, settings, legacyModelSelection) {
    const defaultRoute = {
        endpoint: normalizeEndpointValue(settings?.apiEndpoint, 'https://api.openai.com/v1'),
        apiKey: normalizeApiKeyValue(settings?.apiKey, ''),
        model: normalizeModelName(settings?.model) || pickFastModel(settings),
    };
    const source = modelRouting && typeof modelRouting === 'object' ? modelRouting : {};
    const legacy = legacyModelSelection && typeof legacyModelSelection === 'object' ? legacyModelSelection : {};
    const routing = {};

    for (const modality of ['text', 'image', 'video', 'audio']) {
        routing[modality] = normalizeRouteConfig(source[modality], defaultRoute);
        const legacyModel = normalizeModelName(legacy[modality]);
        if (!normalizeModelName(source?.[modality]?.model) && legacyModel) {
            routing[modality].model = legacyModel;
        }
    }
    return routing;
}

function resolveSearchApiSettings(settings) {
    const source = settings?.searchApi && typeof settings.searchApi === 'object'
        ? settings.searchApi
        : {};
    const scopesSource = source?.scopes && typeof source.scopes === 'object'
        ? source.scopes
        : {};

    const legacyScan = typeof settings?.enableWebSearch === 'boolean'
        ? settings.enableWebSearch
        : false;
    const legacyClassify = typeof settings?.enableWebSearchClassify === 'boolean'
        ? settings.enableWebSearchClassify
        : (typeof settings?.enableWebSearchOrganizer === 'boolean'
            ? settings.enableWebSearchOrganizer
            : legacyScan);
    const legacyOrganizer = typeof settings?.enableWebSearchOrganizer === 'boolean'
        ? settings.enableWebSearchOrganizer
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
    const apiKey = String(source.apiKey || settings?.tavilyApiKey || '').trim();

    return {
        enabled: !!enabled,
        apiKey,
        scopes: {
            scan: !!scanEnabled,
            classify: !!classifyEnabled,
            organizer: !!organizerEnabled,
        },
    };
}

function resolveOrganizerWebSearch(settings, useWebSearchOption = null) {
    const searchApi = resolveSearchApiSettings(settings);
    const requested = typeof useWebSearchOption === 'boolean'
        ? useWebSearchOption
        : (searchApi.enabled && (searchApi.scopes.classify || searchApi.scopes.organizer));

    return {
        requested: !!requested,
        enabled: !!requested && !!searchApi.apiKey,
        apiKey: searchApi.apiKey,
    };
}

function pickInputModalityByPath(filePath) {
    const ext = extname(String(filePath || '')).toLowerCase();
    if (VIDEO_EXTENSIONS.has(ext)) return 'video';
    if (AUDIO_EXTENSIONS.has(ext)) return 'audio';
    if (['.png', '.jpg', '.jpeg', '.webp', '.gif', '.bmp'].includes(ext)) return 'image';
    return 'text';
}

function pickRouteByModality(modality, routing) {
    const key = ['text', 'image', 'video', 'audio'].includes(modality) ? modality : 'text';
    const fallback = routing?.text || {};
    return normalizeRouteConfig(routing?.[key], fallback);
}

function supportsMultimodalByEndpoint(endpoint, model) {
    const value = `${String(endpoint || '').toLowerCase()}|${String(model || '').toLowerCase()}`;
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

export function getOrganizeCapability() {
    const settings = loadSettings();
    const routing = normalizeModelRouting(null, settings);
    const organizerWebSearch = resolveOrganizerWebSearch(settings);
    const selectedModels = {
        text: routing.text.model,
        image: routing.image.model,
        video: routing.video.model,
        audio: routing.audio.model,
    };
    const selectedProviders = {
        text: routing.text.endpoint,
        image: routing.image.endpoint,
        video: routing.video.endpoint,
        audio: routing.audio.endpoint,
    };
    return {
        selectedModel: selectedModels.text,
        selectedModels,
        selectedProviders,
        supportsMultimodal: supportsMultimodalByEndpoint(routing.image.endpoint, routing.image.model),
        apiEndpoint: settings.apiEndpoint || '',
        useWebSearch: organizerWebSearch.requested,
        webSearchEnabled: organizerWebSearch.enabled,
    };
}

function buildOpenAIClient(config) {
    const endpoint = normalizeEndpointValue(config?.endpoint, config?.apiEndpoint);
    return new OpenAI({
        apiKey: normalizeApiKeyValue(config?.apiKey, '') || 'sk-placeholder',
        baseURL: endpoint,
    });
}

async function existsAsync(targetPath) {
    try {
        await access(targetPath);
        return true;
    } catch {
        return false;
    }
}

function withSuffix(pathValue, n) {
    const ext = extname(pathValue);
    const base = ext ? pathValue.slice(0, -ext.length) : pathValue;
    return `${base} (${n})${ext}`;
}

async function resolveUniquePath(targetPath) {
    if (!(await existsAsync(targetPath))) return targetPath;
    let i = 1;
    while (true) {
        const candidate = withSuffix(targetPath, i);
        if (!(await existsAsync(candidate))) return candidate;
        i += 1;
    }
}

function shouldExcludeName(name, excludedPatterns) {
    const lower = String(name || '').toLowerCase();
    if (!lower) return true;
    if (lower.startsWith('.')) return true;
    return excludedPatterns.some((pattern) => lower === pattern || lower.includes(pattern));
}

async function collectFiles(rootPath, recursive, excludedPatterns, shouldStop = () => false) {
    const output = [];

    async function walk(currentPath, depth) {
        if (shouldStop()) {
            return;
        }

        let entries = [];
        try {
            entries = await readdir(currentPath, { withFileTypes: true });
        } catch {
            return;
        }

        for (const entry of entries) {
            if (shouldStop()) {
                return;
            }

            const fullPath = join(currentPath, entry.name);
            if (shouldExcludeName(entry.name, excludedPatterns)) {
                continue;
            }

            if (entry.isSymbolicLink()) {
                continue;
            }

            if (entry.isDirectory()) {
                if (recursive) {
                    await walk(fullPath, depth + 1);
                }
                continue;
            }

            if (!entry.isFile()) {
                continue;
            }

            try {
                const st = await stat(fullPath);
                output.push({
                    name: entry.name,
                    path: fullPath,
                    relativePath: relative(rootPath, fullPath),
                    size: st.size,
                });
            } catch {
                // Ignore files that cannot be stat'ed.
            }
        }
    }

    await walk(rootPath, 0);
    return output;
}

function normalizeGeneratedCategory(category) {
    const raw = String(category || '')
        .replace(/[\r\n\t]+/g, ' ')
        .replace(/^["'`]+|["'`]+$/g, '')
        .replace(/[\\/:*?"<>|]/g, ' ')
        .replace(/\s+/g, ' ')
        .trim();

    if (!raw) return '';
    if (raw.length > 24) return '';
    if (/[{}\[\]]/.test(raw)) return '';
    return raw;
}

function normalizeCategoryValue(category, categories, allowNewCategories = false) {
    const value = normalizeGeneratedCategory(category);
    if (!value) {
        return '其他待定';
    }
    if (categories.includes(value)) {
        return value;
    }
    if (allowNewCategories) {
        return value;
    }
    return '其他待定';
}

function parseCategoryFromResponse(content, categories, allowNewCategories = false) {
    const clean = extractJsonText(content);
    const parsed = JSON.parse(clean);

    const candidate = Array.isArray(parsed)
        ? parsed[0]
        : parsed?.result || parsed?.item || parsed;

    const category = candidate?.category || candidate?.classification || candidate?.label;
    const normalizedCategory = normalizeCategoryValue(category, categories, allowNewCategories);
    return {
        category: normalizedCategory,
        createdCategory: normalizedCategory !== '其他待定' && !categories.includes(normalizedCategory),
    };
}

function buildSystemPrompt(categories, allowNewCategories = false) {
    const lines = [
        'You classify ONE file into ONE category.',
        'Return JSON only. No markdown. No explanation.',
        'Output schema: {"index":1,"category":"<category_name>"}.',
        `Current categories (prefer reuse): ${categories.join(' | ')}`,
    ];

    if (allowNewCategories) {
        lines.push('If none of the current categories fit, you may create ONE short new category name.');
        lines.push('Prefer concise Chinese category names and avoid verbose descriptions.');
    } else {
        lines.push('You must choose category from the current categories only.');
    }

    lines.push('If unsure, choose "其他待定".');
    return lines.join('\n');
}

function buildUserPrompt({ file, mode, extracted, allowNewCategories = false }) {
    const lines = [
        `mode=${mode}`,
        `name=${file.name}`,
        `relativePath=${file.relativePath}`,
        `size=${file.size}`,
        allowNewCategories
            ? 'newCategoryPolicy=prefer_existing_category;create_new_if_no_fit'
            : 'newCategoryPolicy=existing_categories_only',
    ];

    if (extracted?.type) {
        lines.push(`detectedType=${extracted.type}`);
    }

    if (extracted?.payload?.text) {
        if (typeof extracted.payload.truncated === 'boolean') {
            lines.push(`contentTruncated=${extracted.payload.truncated}`);
        }
        if (Number.isFinite(extracted.payload.originalLength)) {
            lines.push(`contentChars=${extracted.payload.originalLength}`);
        }
        lines.push('contentTextStart');
        lines.push(extracted.payload.text);
        lines.push('contentTextEnd');
    }

    lines.push('Remember: output JSON only with fields index and category.');
    return lines.join('\n');
}

function isContextLengthError(err) {
    const message = String(err?.message || '').toLowerCase();
    return (
        message.includes('maximum context length') ||
        message.includes('context length') ||
        message.includes('too many tokens') ||
        message.includes('prompt is too long')
    );
}

function trimExtractedForRetry(extracted, maxChars = CONTEXT_RETRY_TEXT_CHAR_LIMIT) {
    const text = extracted?.payload?.text;
    if (!text) {
        return extracted;
    }

    const raw = String(text);
    if (raw.length <= maxChars) {
        return extracted;
    }

    return {
        ...extracted,
        payload: {
            ...extracted.payload,
            text: raw.slice(0, maxChars),
            truncated: true,
            originalLength: Number.isFinite(extracted.payload.originalLength)
                ? extracted.payload.originalLength
                : raw.length,
        },
    };
}

async function classifyOneFile({
    getClientForRoute,
    modelRouting,
    file,
    mode,
    categories,
    allowNewCategories = true,
    webSearch = null,
}) {
    const modality = pickInputModalityByPath(file.path);
    const route = pickRouteByModality(modality, modelRouting);
    const model = route.model;
    const client = getClientForRoute(route);
    const canUseMultimodal = supportsMultimodalByEndpoint(route.endpoint, model);
    const extracted = await extractFileContent(file.path, mode, { supportsMultimodal: canUseMultimodal });

    const systemPrompt = buildSystemPrompt(categories, allowNewCategories);
    const textPrompt = buildUserPrompt({ file, mode, extracted, allowNewCategories });

    let warnings = [...(extracted.warnings || [])];
    let degraded = !!extracted.degraded;
    let usedMultimodal = false;

    async function runRequest(messages) {
        const response = await withRemoteLimit(() =>
            retryWithBackoff(() =>
                client.chat.completions.create({
                    model,
                    messages,
                    temperature: 0,
                })
            )
        );

        const content = response.choices?.[0]?.message?.content || '';
        const tokenUsage = {
            prompt: response.usage?.prompt_tokens || 0,
            completion: response.usage?.completion_tokens || 0,
            total: response.usage?.total_tokens || 0,
        };

        return { content, tokenUsage };
    }

    let result = null;

    try {
        if (mode === 'deep' && extracted?.payload?.imageDataUrl && canUseMultimodal) {
            usedMultimodal = true;
            const messages = [
                { role: 'system', content: systemPrompt },
                {
                    role: 'user',
                    content: [
                        { type: 'text', text: textPrompt },
                        { type: 'image_url', image_url: { url: extracted.payload.imageDataUrl } },
                    ],
                },
            ];
            result = await runRequest(messages);
        } else {
            const messages = [
                { role: 'system', content: systemPrompt },
                { role: 'user', content: textPrompt },
            ];
            result = await runRequest(messages);
        }
    } catch (err) {
        if (usedMultimodal) {
            degraded = true;
            warnings.push(`multimodal_failed:${err.message}`);
            const fallbackMessages = [
                { role: 'system', content: systemPrompt },
                { role: 'user', content: buildUserPrompt({ file, mode: 'fast', extracted: { type: extracted.type }, allowNewCategories }) },
            ];
            result = await runRequest(fallbackMessages);
        } else if (isContextLengthError(err) && extracted?.payload?.text) {
            degraded = true;
            warnings.push('context_overflow_retry');
            const retryExtracted = trimExtractedForRetry(extracted);
            const fallbackMessages = [
                { role: 'system', content: systemPrompt },
                { role: 'user', content: buildUserPrompt({ file, mode: 'balanced', extracted: retryExtracted, allowNewCategories }) },
            ];
            result = await runRequest(fallbackMessages);
        } else {
            throw err;
        }
    }

    let category = '其他待定';
    let createdCategory = false;
    try {
        const parsed = parseCategoryFromResponse(result.content, categories, allowNewCategories);
        category = parsed.category;
        createdCategory = parsed.createdCategory;
    } catch (err) {
        degraded = true;
        warnings.push(`parse_failed:${err.message}`);
    }

    const totalTokenUsage = { ...result.tokenUsage };
    if (category === '其他待定' && webSearch?.enabled) {
        try {
            const query = [file.name, file.relativePath].filter(Boolean).join(' ').trim() || file.name;
            const searchContext = await withRemoteLimit(() =>
                retryWithBackoff(() =>
                    performTavilySearch(query, webSearch.apiKey, { throwOnError: true })
                )
            );
            const normalizedSearch = String(searchContext || '').trim();

            if (normalizedSearch) {
                const secondPassPrompt = [
                    textPrompt,
                    'webSearchContextStart',
                    normalizedSearch.slice(0, SEARCH_CONTEXT_MAX_CHARS),
                    'webSearchContextEnd',
                    'Use web search context above to refine the category if needed.',
                    'Keep output JSON-only with fields index and category.',
                ].join('\n');

                const secondPass = await runRequest([
                    { role: 'system', content: systemPrompt },
                    { role: 'user', content: secondPassPrompt },
                ]);

                totalTokenUsage.prompt += secondPass.tokenUsage.prompt;
                totalTokenUsage.completion += secondPass.tokenUsage.completion;
                totalTokenUsage.total += secondPass.tokenUsage.total;

                const parsed = parseCategoryFromResponse(secondPass.content, categories, allowNewCategories);
                category = parsed.category;
                createdCategory = parsed.createdCategory;
                warnings.push('web_search_refined');
            } else {
                warnings.push('web_search_empty');
            }
        } catch (err) {
            degraded = true;
            warnings.push(`web_search_failed:${err.message}`);
        }
    } else if (category === '其他待定' && webSearch?.requested && !webSearch?.enabled) {
        warnings.push('web_search_not_available');
    }

    return {
        category,
        createdCategory,
        modality,
        provider: route.endpoint,
        model,
        degraded,
        warnings,
        tokenUsage: totalTokenUsage,
    };
}

function buildPreviewMappings(rootPath, results) {
    const used = new Set();
    const items = [];

    for (const row of results) {
        const categoryDir = sanitizeCategoryName(row.category);
        const targetDir = join(rootPath, categoryDir);
        let targetPath = join(targetDir, row.name);

        let n = 1;
        while (used.has(targetPath.toLowerCase())) {
            targetPath = withSuffix(join(targetDir, row.name), n);
            n += 1;
        }

        used.add(targetPath.toLowerCase());
        items.push({
            sourcePath: row.path,
            category: row.category,
            targetPath,
        });
    }

    return items;
}

export class OrganizeTask extends EventEmitter {
    constructor(options) {
        super();
        this.id = uniqueId('task');
        this.status = 'idle';
        this.error = null;
        this.stopped = false;
        this.stopEmitted = false;

        this.rootPath = options.rootPath;
        this.recursive = !!options.recursive;
        this.mode = ['fast', 'balanced', 'deep'].includes(options.mode) ? options.mode : 'fast';
        this.categories = normalizeCategories(options.categories);
        this.allowNewCategories = options.allowNewCategories !== false;
        this.excludedPatterns = normalizeExcludedPatterns(options.excludedPatterns);
        this.parallelism = normalizeParallelism(options.parallelism || 5);
        this.settings = loadSettings();
        this.webSearch = resolveOrganizerWebSearch(this.settings, options.useWebSearch);
        this.modelRouting = normalizeModelRouting(options.modelRouting, this.settings, options.modelSelection);
        this.selectedModels = {
            text: this.modelRouting.text.model,
            image: this.modelRouting.image.model,
            video: this.modelRouting.video.model,
            audio: this.modelRouting.audio.model,
        };
        this.selectedProviders = {
            text: this.modelRouting.text.endpoint,
            image: this.modelRouting.image.endpoint,
            video: this.modelRouting.video.endpoint,
            audio: this.modelRouting.audio.endpoint,
        };
        this.selectedModel = this.selectedModels.text;
        this.modelSupportsMultimodal = supportsMultimodalByEndpoint(
            this.selectedProviders.image,
            this.selectedModels.image
        );

        this.totalFiles = 0;
        this.processedFiles = 0;
        this.tokenUsage = { prompt: 0, completion: 0, total: 0 };
        this.results = [];
        this.preview = [];
        this.createdAt = nowIso();
        this.completedAt = null;
        this.jobId = null;
    }

    _emitStopped() {
        if (this.stopEmitted) return;
        this.stopEmitted = true;
        this.emit('stopped', this._snapshot());
    }

    stop() {
        if (this.stopped) return;
        this.stopped = true;

        if (!['completed', 'done', 'error', 'stopped'].includes(this.status)) {
            this.status = 'stopped';
            this.completedAt = this.completedAt || nowIso();
            this._emitProgress();
            this._emitStopped();
        }
    }

    _snapshot() {
        return {
            id: this.id,
            status: this.status,
            error: this.error,
            rootPath: this.rootPath,
            recursive: this.recursive,
            mode: this.mode,
            categories: this.categories,
            allowNewCategories: this.allowNewCategories,
            excludedPatterns: this.excludedPatterns,
            parallelism: this.parallelism,
            useWebSearch: this.webSearch.requested,
            webSearchEnabled: this.webSearch.enabled,
            selectedModel: this.selectedModel,
            selectedModels: this.selectedModels,
            selectedProviders: this.selectedProviders,
            supportsMultimodal: this.modelSupportsMultimodal,
            totalFiles: this.totalFiles,
            processedFiles: this.processedFiles,
            tokenUsage: this.tokenUsage,
            results: this.results,
            preview: this.preview,
            createdAt: this.createdAt,
            completedAt: this.completedAt,
            jobId: this.jobId,
        };
    }

    _emitProgress() {
        this.emit('progress', this._snapshot());
    }

    async start() {
        this.status = 'scanning';
        this._emitProgress();

        let files = [];
        try {
            files = await collectFiles(this.rootPath, this.recursive, this.excludedPatterns, () => this.stopped);
        } catch (err) {
            this.status = 'error';
            this.error = err.message;
            this.emit('error', { message: err.message, snapshot: this._snapshot() });
            return;
        }

        this.totalFiles = files.length;

        if (this.stopped) {
            this.results.sort((a, b) => a.index - b.index);
            this.preview = buildPreviewMappings(this.rootPath, this.results);
            this.status = 'stopped';
            this.completedAt = this.completedAt || nowIso();
            this._emitProgress();
            this._emitStopped();
            return;
        }

        if (files.length === 0) {
            this.status = 'completed';
            this.results = [];
            this.preview = [];
            this.completedAt = nowIso();
            this.emit('done', this._snapshot());
            return;
        }

        this.status = 'classifying';
        this._emitProgress();

        const clientCache = new Map();
        const getClientForRoute = (route) => {
            const key = `${route.endpoint}|${route.apiKey}`;
            if (!clientCache.has(key)) {
                clientCache.set(key, buildOpenAIClient(route));
            }
            return clientCache.get(key);
        };

        const limiter = pLimit(this.parallelism);

        await Promise.all(
            files.map((file, i) =>
                limiter(async () => {
                    if (this.stopped) {
                        return;
                    }

                    const index = i + 1;
                    try {
                        const classified = await classifyOneFile({
                            getClientForRoute,
                            modelRouting: this.modelRouting,
                            file,
                            mode: this.mode,
                            categories: this.categories,
                            allowNewCategories: this.allowNewCategories,
                            webSearch: this.webSearch,
                        });

                        if (
                            this.allowNewCategories &&
                            classified.category !== '其他待定' &&
                            !this.categories.includes(classified.category)
                        ) {
                            this.categories.push(classified.category);
                        }

                        const row = {
                            index,
                            name: file.name,
                            path: file.path,
                            relativePath: file.relativePath,
                            size: file.size,
                            category: classified.category,
                            createdCategory: classified.createdCategory,
                            degraded: classified.degraded,
                            warnings: classified.warnings,
                            modality: classified.modality,
                            provider: classified.provider,
                            model: classified.model,
                        };

                        this.results.push(row);
                        this.tokenUsage.prompt += classified.tokenUsage.prompt;
                        this.tokenUsage.completion += classified.tokenUsage.completion;
                        this.tokenUsage.total += classified.tokenUsage.total;

                        this.emit('file_done', row);
                    } catch (err) {
                        const rateLimited = isRateLimitError(err);
                        const modality = pickInputModalityByPath(file.path);
                        const route = pickRouteByModality(modality, this.modelRouting);
                        const model = route.model;
                        const row = {
                            index,
                            name: file.name,
                            path: file.path,
                            relativePath: file.relativePath,
                            size: file.size,
                            category: rateLimited ? fallbackCategoryFromFilename(file.name, this.categories) : '其他待定',
                            createdCategory: false,
                            degraded: true,
                            warnings: [rateLimited ? `rate_limited_fallback:${err.message}` : `classify_failed:${err.message}`],
                            modality,
                            provider: route.endpoint,
                            model,
                        };
                        this.results.push(row);
                        this.emit('file_done', row);
                    } finally {
                        this.processedFiles += 1;
                        this._emitProgress();
                    }
                })
            )
        );

        this.results.sort((a, b) => a.index - b.index);
        this.preview = buildPreviewMappings(this.rootPath, this.results);

        if (this.stopped) {
            this.status = 'stopped';
            this.completedAt = this.completedAt || nowIso();
            this._emitProgress();
            this._emitStopped();
            return;
        }

        this.status = 'completed';
        this.completedAt = nowIso();
        this.emit('done', this._snapshot());
    }
}

function suggestCategoryFallbackByExt(files) {
    const out = new Set(['其他待定']);

    for (const file of files) {
        const ext = extname(file.name || '').toLowerCase();

        if (['.jpg', '.jpeg', '.png', '.gif', '.webp', '.mp4', '.mov', '.mp3', '.wav'].includes(ext)) {
            out.add('媒体素材');
            continue;
        }

        if (['.doc', '.docx', '.xls', '.xlsx', '.ppt', '.pptx', '.pdf'].includes(ext)) {
            out.add('工作学习');
            continue;
        }

        if (['.zip', '.rar', '.7z', '.msi', '.exe', '.dmg'].includes(ext)) {
            out.add('安装与压缩');
            continue;
        }

        if (['.js', '.ts', '.py', '.java', '.go', '.rs', '.cpp'].includes(ext)) {
            out.add('开发项目');
            continue;
        }
    }

    if (out.size === 1) {
        DEFAULT_CATEGORY_LIST.forEach((x) => out.add(x));
    }

    return [...out];
}

export async function suggestCategoriesByFilename(options) {
    const rootPath = options.rootPath;
    const recursive = !!options.recursive;
    const excludedPatterns = normalizeExcludedPatterns(options.excludedPatterns);
    const manualCategories = normalizeCategories(options.manualCategories || []);

    const files = await collectFiles(rootPath, recursive, excludedPatterns);
    const shortlist = files.slice(0, 400);

    if (shortlist.length === 0) {
        return {
            suggestedCategories: manualCategories,
            source: 'filename_scan',
        };
    }

    const settings = loadSettings();
    const modelRouting = normalizeModelRouting(options.modelRouting, settings, options.modelSelection);
    const textRoute = pickRouteByModality('text', modelRouting);
    const model = textRoute.model;
    const client = buildOpenAIClient(textRoute);

    const sampleText = shortlist
        .map((f, idx) => `${idx + 1}. ${f.relativePath}`)
        .join('\n');

    const systemPrompt = [
        'You generate category suggestions for file organization.',
        'Return JSON only. No markdown. No explanation.',
        'Output schema: {"categories":["...","..."]}',
        'Prefer short Chinese category names.',
    ].join('\n');

    const userPrompt = [
        'Based on filenames below, suggest 3-12 categories.',
        'Do not include duplicates.',
        sampleText,
    ].join('\n');

    try {
        const response = await withRemoteLimit(() =>
            retryWithBackoff(() =>
                client.chat.completions.create({
                    model,
                    messages: [
                        { role: 'system', content: systemPrompt },
                        { role: 'user', content: userPrompt },
                    ],
                    temperature: 0,
                })
            )
        );

        const content = response.choices?.[0]?.message?.content || '';
        const parsed = JSON.parse(extractJsonText(content));
        const aiCategories = normalizeCategories(parsed?.categories || []);

        const merged = [...manualCategories];
        for (const cat of aiCategories) {
            if (!merged.includes(cat)) {
                merged.push(cat);
            }
        }

        return {
            suggestedCategories: merged,
            source: 'filename_scan',
        };
    } catch {
        const fallback = suggestCategoryFallbackByExt(shortlist);
        const merged = [...manualCategories];
        for (const cat of fallback) {
            if (!merged.includes(cat)) {
                merged.push(cat);
            }
        }
        return {
            suggestedCategories: merged,
            source: 'filename_scan',
        };
    }
}

function manifestPath(jobId) {
    ensureJobDir();
    return join(JOBS_DIR, `${jobId}.json`);
}

function saveManifest(manifest) {
    ensureJobDir();
    writeFileSync(manifestPath(manifest.jobId), JSON.stringify(manifest, null, 2), 'utf-8');
}

function loadManifest(jobId) {
    const file = manifestPath(jobId);
    if (!existsSync(file)) {
        throw new Error(`job manifest not found: ${jobId}`);
    }
    return JSON.parse(readFileSync(file, 'utf-8'));
}

function summarizeManifestEntries(entries) {
    const moved = entries.filter((e) => e.status === 'moved').length;
    const failed = entries.filter((e) => e.status !== 'moved').length;
    return { moved, failed, total: entries.length };
}

export async function applyTaskMoves(task) {
    if (!task) {
        throw new Error('task not found');
    }

    if (!['completed', 'done'].includes(task.status)) {
        throw new Error(`task status is ${task.status}, cannot apply move`);
    }

    task.status = 'moving';
    task._emitProgress();

    const entries = [];

    for (const row of task.results) {
        const categoryDir = sanitizeCategoryName(row.category);
        const targetDir = join(task.rootPath, categoryDir);
        const targetBase = join(targetDir, row.name);

        try {
            const sourceExists = await existsAsync(row.path);
            if (!sourceExists) {
                entries.push({
                    sourcePath: row.path,
                    targetPath: targetBase,
                    category: row.category,
                    status: 'failed',
                    error: 'source_not_found',
                });
                continue;
            }

            await fsExtra.ensureDir(targetDir);
            const targetPath = await resolveUniquePath(targetBase);
            await fsExtra.move(row.path, targetPath, { overwrite: false });

            entries.push({
                sourcePath: row.path,
                targetPath,
                category: row.category,
                status: 'moved',
                error: null,
            });
        } catch (err) {
            entries.push({
                sourcePath: row.path,
                targetPath: targetBase,
                category: row.category,
                status: 'failed',
                error: err.message,
            });
        }
    }

    const jobId = uniqueId('job');
    const manifest = {
        jobId,
        taskId: task.id,
        rootPath: task.rootPath,
        createdAt: nowIso(),
        mode: task.mode,
        recursive: task.recursive,
        categories: task.categories,
        entries,
        summary: summarizeManifestEntries(entries),
    };

    saveManifest(manifest);

    task.jobId = jobId;
    task.status = 'done';
    task._emitProgress();

    return manifest;
}

export async function rollbackJob(jobId) {
    const manifest = loadManifest(jobId);
    const reversed = [...manifest.entries].reverse();

    const rollbackEntries = [];

    for (const entry of reversed) {
        if (entry.status !== 'moved') {
            rollbackEntries.push({
                sourcePath: entry.sourcePath,
                targetPath: entry.targetPath,
                status: 'skipped',
                error: 'not_moved_in_apply',
            });
            continue;
        }

        try {
            const targetExists = await existsAsync(entry.targetPath);
            if (!targetExists) {
                rollbackEntries.push({
                    sourcePath: entry.sourcePath,
                    targetPath: entry.targetPath,
                    status: 'failed',
                    error: 'target_not_found',
                });
                continue;
            }

            const sourceExists = await existsAsync(entry.sourcePath);
            if (sourceExists) {
                rollbackEntries.push({
                    sourcePath: entry.sourcePath,
                    targetPath: entry.targetPath,
                    status: 'failed',
                    error: 'source_already_exists',
                });
                continue;
            }

            await fsExtra.ensureDir(dirname(entry.sourcePath));
            await fsExtra.move(entry.targetPath, entry.sourcePath, { overwrite: false });

            rollbackEntries.push({
                sourcePath: entry.sourcePath,
                targetPath: entry.targetPath,
                status: 'rolled_back',
                error: null,
            });
        } catch (err) {
            rollbackEntries.push({
                sourcePath: entry.sourcePath,
                targetPath: entry.targetPath,
                status: 'failed',
                error: err.message,
            });
        }
    }

    manifest.lastRollback = {
        at: nowIso(),
        entries: rollbackEntries,
        summary: {
            rolledBack: rollbackEntries.filter((x) => x.status === 'rolled_back').length,
            failed: rollbackEntries.filter((x) => x.status === 'failed').length,
            skipped: rollbackEntries.filter((x) => x.status === 'skipped').length,
            total: rollbackEntries.length,
        },
    };

    saveManifest(manifest);

    return {
        jobId,
        rollback: manifest.lastRollback,
    };
}
