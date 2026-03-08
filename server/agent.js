/**
 * server/agent.js
 * LLM analysis engine for file/folder classification with optional web-search second pass.
 */
import OpenAI from 'openai';
import { loadSettings } from './routes/settings.js';
import { performTavilySearch } from './search.js';
import { retryWithBackoff, withRemoteLimit } from './remote-control.js';

let clientCache = null;
let lastConfig = '';

function getClient() {
    const settings = loadSettings();
    const configKey = `${settings.apiEndpoint}|${settings.apiKey}|${settings.model}`;

    if (clientCache && lastConfig === configKey) {
        return { client: clientCache, model: settings.model || 'gpt-4o-mini' };
    }

    clientCache = new OpenAI({
        apiKey: settings.apiKey || 'sk-placeholder',
        baseURL: settings.apiEndpoint || 'https://api.openai.com/v1',
    });
    lastConfig = configKey;

    return { client: clientCache, model: settings.model || 'gpt-4o-mini' };
}

function formatSize(bytes) {
    if (bytes < 1024) return `${bytes} B`;
    if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
    if (bytes < 1024 * 1024 * 1024) return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
    return `${(bytes / (1024 * 1024 * 1024)).toFixed(2)} GB`;
}

function extractJsonText(content = '') {
    let clean = content.trim();
    if (clean.startsWith('```json')) {
        clean = clean.replace(/^```json/, '').replace(/```$/, '').trim();
    } else if (clean.startsWith('```')) {
        clean = clean.replace(/^```/, '').replace(/```$/, '').trim();
    }
    return clean;
}

function normalizeResultArray(parsed) {
    if (Array.isArray(parsed)) return parsed;
    if (Array.isArray(parsed?.results)) return parsed.results;
    if (Array.isArray(parsed?.items)) return parsed.items;
    if (Array.isArray(parsed?.analysis)) return parsed.analysis;
    if (parsed && typeof parsed === 'object') return [parsed];
    return [];
}

function normalizeClassification(value) {
    const raw = String(value || '').trim().toLowerCase();
    if (raw === 'safe_to_delete') return 'safe_to_delete';
    if (raw === 'keep') return 'keep';
    if (raw === 'suspicious') return 'suspicious';
    if (raw === 'needs_search') return 'suspicious';
    return 'suspicious';
}

function normalizeRisk(value, fallback = 'medium') {
    const raw = String(value || '').trim().toLowerCase();
    if (raw === 'low' || raw === 'medium' || raw === 'high') return raw;
    return fallback;
}

function normalizeDirectoryReview(parsed, entries, dirName) {
    const source = (parsed && typeof parsed === 'object' && !Array.isArray(parsed)) ? parsed : {};
    const folderRaw = source.folder && typeof source.folder === 'object' ? source.folder : {};
    const childRaw = Array.isArray(source.children)
        ? source.children
        : Array.isArray(source.entries)
            ? source.entries
            : Array.isArray(source.items)
                ? source.items
                : [];

    const folder = {
        name: String(folderRaw.name || dirName || ''),
        classification: normalizeClassification(folderRaw.classification),
        purpose: String(folderRaw.purpose || ''),
        reason: String(folderRaw.reason || ''),
        risk: normalizeRisk(folderRaw.risk, 'medium'),
    };

    const children = entries.map((entry, idx) => {
        const byIndex = childRaw.find((item) => Number(item?.index || 0) === idx + 1);
        const byName = childRaw.find((item) => String(item?.name || '') === entry.name);
        const hit = byIndex || byName || {};
        return {
            index: idx + 1,
            name: entry.name,
            classification: normalizeClassification(hit.classification),
            purpose: String(hit.purpose || ''),
            reason: String(hit.reason || ''),
            risk: normalizeRisk(hit.risk, 'medium'),
        };
    });

    return { folder, children };
}

function buildSystemPrompt(isWebSearchEnabled) {
    if (isWebSearchEnabled) {
        return `你是一个资深的操作系统与磁盘空间清理专家。
请分析给定文件/目录并返回严格 JSON 数组。

分类规则：
- 系统关键文件、用户核心文档/照片：keep
- 缓存、临时文件、构建产物、日志：通常 safe_to_delete
- 遇到无法判断且名称陌生的项目：needs_search
- 无法确定时优先 suspicious，不要冒进判 safe_to_delete

输出要求（仅 JSON，不要 Markdown）：
[
  {
    "index": 1,
    "name": "...",
    "classification": "safe_to_delete" | "suspicious" | "keep" | "needs_search",
    "purpose": "中文说明用途",
    "reason": "中文说明依据",
    "risk": "low" | "medium" | "high"
  }
]`;
    }

    return `你是一个资深的操作系统与磁盘空间清理专家。
请分析给定文件/目录并返回严格 JSON 数组。

分类规则：
- 系统关键文件、用户核心文档/照片：keep
- 缓存、临时文件、构建产物、日志：通常 safe_to_delete
- 无法确定时优先 suspicious，不要冒进判 safe_to_delete

输出要求（仅 JSON，不要 Markdown）：
[
  {
    "index": 1,
    "name": "...",
    "classification": "safe_to_delete" | "suspicious" | "keep",
    "purpose": "中文说明用途",
    "reason": "中文说明依据",
    "risk": "low" | "medium" | "high"
  }
]`;
}

/**
 * Analyze a batch of file entries with LLM.
 * @param {Array} entries - [{name, size, type, path}]
 * @param {string} parentPath - The directory being scanned
 */
export async function analyzeEntries(entries, parentPath) {
    const { client, model } = getClient();
    const settings = loadSettings();
    const isWebSearchEnabled = !!(settings.enableWebSearch && settings.tavilyApiKey);

    const entrySummary = entries
        .map((e, i) => `${i + 1}. [${e.type}] "${e.name}" - ${formatSize(e.size)}`)
        .join('\n');

    const systemPrompt = buildSystemPrompt(isWebSearchEnabled);
    const userPrompt = `请分析目录 "${parentPath}" 下的以下项目：\n\n${entrySummary}\n\n请仅返回 JSON 数组。`;

    const startTime = Date.now();

    try {
        const response = await withRemoteLimit(() =>
            retryWithBackoff(() =>
                client.chat.completions.create({
                    model,
                    messages: [
                        { role: 'system', content: systemPrompt },
                        { role: 'user', content: userPrompt },
                    ],
                    temperature: 0.1,
                })
            )
        );

        const elapsed = Date.now() - startTime;
        const content = response.choices?.[0]?.message?.content || '';
        const reasoning = response.choices?.[0]?.message?.reasoning_content || '';

        const tokenUsage = {
            prompt: response.usage?.prompt_tokens || 0,
            completion: response.usage?.completion_tokens || 0,
            total: response.usage?.total_tokens || 0,
        };

        const parsed = JSON.parse(extractJsonText(content));
        const results = normalizeResultArray(parsed);

        const needSearchItems = results.filter((r) => r.classification === 'needs_search');

        if (needSearchItems.length > 0) {
            if (isWebSearchEnabled) {
                const searchResults = await Promise.allSettled(
                    needSearchItems.map((item) =>
                        withRemoteLimit(() =>
                            retryWithBackoff(() =>
                                performTavilySearch(item.name, settings.tavilyApiKey, { throwOnError: true })
                            )
                        )
                    )
                );

                const searchPrompts = needSearchItems.map((item, idx) => {
                    const result = searchResults[idx];
                    if (result.status === 'fulfilled' && result.value) {
                        return `- "${item.name}": ${result.value}`;
                    }
                    return `- "${item.name}": 搜索结果不足，未获得明确信息`;
                });

                const secondPassPrompt = `以下是联网搜索补充信息：\n\n${searchPrompts.join('\n')}\n\n请仅对之前判定为 needs_search 的条目给出最终结论，且 classification 只能是 safe_to_delete | suspicious | keep。输出格式仍为 JSON 数组。`;

                const response2 = await withRemoteLimit(() =>
                    retryWithBackoff(() =>
                        client.chat.completions.create({
                            model,
                            messages: [
                                { role: 'system', content: systemPrompt },
                                { role: 'user', content: userPrompt },
                                { role: 'assistant', content },
                                { role: 'user', content: secondPassPrompt },
                            ],
                            temperature: 0.1,
                        })
                    )
                );

                const parsed2 = JSON.parse(extractJsonText(response2.choices?.[0]?.message?.content || '[]'));
                const secondaryResults = normalizeResultArray(parsed2);

                for (const sr of secondaryResults) {
                    const targetIdx = results.findIndex((r) => r.index === sr.index);
                    if (targetIdx !== -1) {
                        results[targetIdx] = sr;
                    }
                }

                tokenUsage.prompt += response2.usage?.prompt_tokens || 0;
                tokenUsage.completion += response2.usage?.completion_tokens || 0;
                tokenUsage.total += response2.usage?.total_tokens || 0;
            } else {
                for (const item of needSearchItems) {
                    item.classification = 'suspicious';
                    item.reason = `${item.reason || ''} (Web Search disabled, downgraded to suspicious)`.trim();
                }
            }
        }

        return {
            results,
            tokenUsage,
            trace: {
                model,
                systemPrompt,
                userPrompt,
                reasoning,
                rawContent: content,
                elapsed,
                error: null,
            },
        };
    } catch (err) {
        const elapsed = Date.now() - startTime;
        console.error('[Agent] LLM analysis failed:', err.message);

        return {
            results: entries.map((e, i) => ({
                index: i + 1,
                name: e.name,
                classification: 'suspicious',
                purpose: 'Analysis failed - manual review recommended',
                reason: err.message,
                risk: 'high',
            })),
            tokenUsage: { prompt: 0, completion: 0, total: 0 },
            trace: {
                model,
                systemPrompt,
                userPrompt,
                reasoning: '',
                rawContent: '',
                elapsed,
                error: err.message,
            },
        };
    }
}

/**
 * Analyze a single directory as a whole, and classify all direct children.
 * Returns one decision for the directory itself and one decision per child entry.
 */
export async function analyzeDirectoryReview(dirName, dirPath, entries) {
    const { client, model } = getClient();

    const entrySummary = entries
        .map((e, i) => `${i + 1}. [${e.type}] "${e.name}" - ${formatSize(e.size)}`)
        .join('\n');

    const systemPrompt = `你是资深的磁盘清理专家。你将评估一个目录是否可整体删除，并同时评估其直接子项。
要求：
1) 如果目录整体可删除，folder.classification 使用 safe_to_delete。
2) 如果目录整体不可删除，folder.classification 使用 keep 或 suspicious。
3) children 必须逐项给出结论，classification 只能是 safe_to_delete | keep | suspicious。
4) 风险 risk 只能是 low | medium | high。
5) 严格输出 JSON 对象，不要输出 Markdown。`;

    const userPrompt = `请分析目录 "${dirPath}"（目录名: "${dirName}"）。
直接子项如下：
${entrySummary || '(空目录)'}

请仅返回 JSON，格式如下：
{
  "folder": {
    "name": "${dirName}",
    "classification": "safe_to_delete|keep|suspicious",
    "purpose": "中文说明",
    "reason": "中文说明",
    "risk": "low|medium|high"
  },
  "children": [
    {
      "index": 1,
      "name": "子项名称",
      "classification": "safe_to_delete|keep|suspicious",
      "purpose": "中文说明",
      "reason": "中文说明",
      "risk": "low|medium|high"
    }
  ]
}`;

    const startTime = Date.now();

    try {
        const response = await withRemoteLimit(() =>
            retryWithBackoff(() =>
                client.chat.completions.create({
                    model,
                    messages: [
                        { role: 'system', content: systemPrompt },
                        { role: 'user', content: userPrompt },
                    ],
                    temperature: 0.1,
                })
            )
        );

        const elapsed = Date.now() - startTime;
        const content = response.choices?.[0]?.message?.content || '';
        const reasoning = response.choices?.[0]?.message?.reasoning_content || '';
        const parsed = JSON.parse(extractJsonText(content));
        const review = normalizeDirectoryReview(parsed, entries, dirName);

        return {
            ...review,
            tokenUsage: {
                prompt: response.usage?.prompt_tokens || 0,
                completion: response.usage?.completion_tokens || 0,
                total: response.usage?.total_tokens || 0,
            },
            trace: {
                model,
                systemPrompt,
                userPrompt,
                reasoning,
                rawContent: content,
                elapsed,
                error: null,
            },
        };
    } catch (err) {
        const elapsed = Date.now() - startTime;
        console.error('[Agent] Directory review failed:', err.message);

        return {
            folder: {
                name: dirName,
                classification: 'suspicious',
                purpose: '分析失败，建议人工确认',
                reason: err.message,
                risk: 'high',
            },
            children: entries.map((entry, index) => ({
                index: index + 1,
                name: entry.name,
                classification: 'suspicious',
                purpose: '分析失败，建议人工确认',
                reason: err.message,
                risk: 'high',
            })),
            tokenUsage: { prompt: 0, completion: 0, total: 0 },
            trace: {
                model,
                systemPrompt,
                userPrompt,
                reasoning: '',
                rawContent: '',
                elapsed,
                error: err.message,
            },
        };
    }
}

/**
 * Verifies if a directory is truly safe to delete by examining its contents.
 */
export async function verifyDirectoryDelete(dirName, entries, parentPath) {
    const { client, model } = getClient();

    const entrySummary = entries
        .map((e, i) => `${i + 1}. [${e.type}] "${e.name}" - ${formatSize(e.size)}`)
        .join('\n');

    const systemPrompt = `你是磁盘清理安全审核员。请对“可删除目录”做二次核查。
如果目录中可能存在重要文件（系统关键内容、用户文档、源码、配置等），必须判定 safe=false。
只有当内容明显是缓存/日志/临时产物/空目录时，才可 safe=true。
输出仅 JSON：
{
  "safe": true | false,
  "reason": "中文说明"
}`;

    const userPrompt = `请二次确认目录 "${dirName}"（路径："${parentPath}"）是否可整体删除。\n\n内部条目：\n${entrySummary.length > 0 ? entrySummary : '(该目录为空)'}\n\n请仅返回 JSON。`;

    const startTime = Date.now();

    try {
        const response = await withRemoteLimit(() =>
            retryWithBackoff(() =>
                client.chat.completions.create({
                    model,
                    messages: [
                        { role: 'system', content: systemPrompt },
                        { role: 'user', content: userPrompt },
                    ],
                    temperature: 0.1,
                })
            )
        );

        const content = response.choices?.[0]?.message?.content || '';
        const reasoning = response.choices?.[0]?.message?.reasoning_content || '';
        const parsed = JSON.parse(extractJsonText(content));

        return {
            safe: !!parsed.safe,
            reason: parsed.reason || '',
            tokenUsage: {
                prompt: response.usage?.prompt_tokens || 0,
                completion: response.usage?.completion_tokens || 0,
                total: response.usage?.total_tokens || 0,
            },
            trace: {
                model,
                systemPrompt,
                userPrompt,
                reasoning,
                rawContent: content,
                elapsed: Date.now() - startTime,
                error: null,
            },
        };
    } catch (err) {
        const elapsed = Date.now() - startTime;
        console.error('[Agent] Directory verification failed:', err.message);

        return {
            safe: false,
            reason: `验证失败: ${err.message}`,
            tokenUsage: { prompt: 0, completion: 0, total: 0 },
            trace: {
                model,
                systemPrompt,
                userPrompt,
                reasoning: '',
                rawContent: '',
                elapsed,
                error: err.message,
            },
        };
    }
}
