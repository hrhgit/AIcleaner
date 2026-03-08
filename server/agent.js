/**
 * server/agent.js
 * LLM analysis engine for scan and cleanup decisions.
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
    let clean = String(content || '').trim();
    if (clean.startsWith('```json')) {
        clean = clean.replace(/^```json/i, '').replace(/```$/, '').trim();
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

function normalizeBoolean(value, fallback = false) {
    if (typeof value === 'boolean') return value;
    if (typeof value === 'number') return value !== 0;
    const raw = String(value || '').trim().toLowerCase();
    if (['true', 'yes', '1'].includes(raw)) return true;
    if (['false', 'no', '0'].includes(raw)) return false;
    return fallback;
}

function normalizeNodeReview(parsed, nodeType) {
    const source = parsed && typeof parsed === 'object' && !Array.isArray(parsed) ? parsed : {};
    const fallbackHasChildren = nodeType === 'directory';

    return {
        classification: normalizeClassification(source.classification),
        reason: String(source.reason || '').trim(),
        risk: normalizeRisk(source.risk, 'medium'),
        hasPotentialDeletableSubfolders: fallbackHasChildren
            ? normalizeBoolean(source.hasPotentialDeletableSubfolders, true)
            : false,
    };
}

function buildBatchSystemPrompt(isWebSearchEnabled) {
    const rules = [
        '你是磁盘清理安全分析助手。',
        '任务：判断每个条目是否可删除。',
        '输出必须是 JSON 数组，不要输出 Markdown，不要输出解释性前缀。',
        'classification 只能是 safe_to_delete、suspicious、keep。',
        'risk 只能是 low、medium、high。',
        '系统关键文件、用户文档、源码、配置、照片视频默认保守，优先 keep 或 suspicious。',
        '缓存、日志、临时文件、构建产物在证据充分时可判 safe_to_delete。',
        '不确定时必须判 suspicious，不要冒进判 safe_to_delete。',
    ];

    if (isWebSearchEnabled) {
        rules.push('若名称陌生且证据不足，可先输出 needs_search，后续会提供联网补充信息。');
    }

    return `${rules.join('\n')}

输出格式：
[
  {
    "index": 1,
    "name": "示例",
    "classification": "${isWebSearchEnabled ? 'safe_to_delete|suspicious|keep|needs_search' : 'safe_to_delete|suspicious|keep'}",
    "purpose": "中文用途说明",
    "reason": "中文判断依据",
    "risk": "low|medium|high"
  }
]`;
}

function buildNodeSystemPrompt(nodeType) {
    const shared = [
        '你是磁盘清理安全分析助手。',
        '只输出 JSON，不要输出 Markdown，不要输出多余解释。',
        'classification 只能是 safe_to_delete、suspicious、keep。',
        'risk 只能是 low、medium、high。',
        '不确定时必须输出 suspicious。',
        '理由必须简短、具体、中文。',
    ];

    if (nodeType === 'directory') {
        return `${shared.join('\n')}
这是目录级判断。
如果目录整体明显属于缓存、日志、临时下载、安装残留、构建产物，可判 safe_to_delete。
如果目录可能包含用户数据、项目源码、配置、工作资料，判 keep 或 suspicious。
还要判断该目录下是否可能存在值得继续向下检查的可删子文件夹。
输出格式：
{
  "classification": "safe_to_delete|suspicious|keep",
  "reason": "中文理由",
  "risk": "low|medium|high",
  "hasPotentialDeletableSubfolders": true
}`;
    }

    return `${shared.join('\n')}
这是单文件判断。
只根据文件名判断，不要假设存在额外上下文。
输出格式：
{
  "classification": "safe_to_delete|suspicious|keep",
  "reason": "中文理由",
  "risk": "low|medium|high"
}`;
}

function buildBatchUserPrompt(entries, parentPath) {
    const entrySummary = entries
        .map((entry, index) => `${index + 1}. [${entry.type}] "${entry.name}" - ${formatSize(entry.size)}`)
        .join('\n');

    return `请分析目录 "${parentPath}" 下的以下条目：

${entrySummary}

只返回 JSON 数组。`;
}

function buildDirectoryNodeUserPrompt(node, childDirectories) {
    const childSummary = childDirectories.length > 0
        ? childDirectories
            .map((child, index) => `${index + 1}. "${child.name}" - ${formatSize(child.size)}`)
            .join('\n')
        : '(没有直接子目录)';

    return `节点类型：directory
目录名：${node.name}
目录路径：${node.path}
目录总大小：${formatSize(node.size)}
直接子目录列表：
${childSummary}

请判断：
1. 该目录整体是否可删除
2. 风险等级
3. 该目录下是否可能还存在值得继续检查的可删子文件夹

只返回 JSON。`;
}

function buildFileNodeUserPrompt(node) {
    return `节点类型：file
文件名：${node.name}

请判断该文件是否可删除，并给出风险与理由。
只返回 JSON。`;
}

async function runChatCompletion({ model, messages }) {
    const { client } = getClient();
    const response = await withRemoteLimit(() =>
        retryWithBackoff(() =>
            client.chat.completions.create({
                model,
                messages,
                temperature: 0.1,
            })
        )
    );

    return {
        content: response.choices?.[0]?.message?.content || '',
        reasoning: response.choices?.[0]?.message?.reasoning_content || '',
        tokenUsage: {
            prompt: response.usage?.prompt_tokens || 0,
            completion: response.usage?.completion_tokens || 0,
            total: response.usage?.total_tokens || 0,
        },
    };
}

export async function analyzeEntries(entries, parentPath) {
    const { model } = getClient();
    const settings = loadSettings();
    const isWebSearchEnabled = !!(settings.enableWebSearch && settings.tavilyApiKey);
    const systemPrompt = buildBatchSystemPrompt(isWebSearchEnabled);
    const userPrompt = buildBatchUserPrompt(entries, parentPath);
    const startTime = Date.now();

    try {
        const firstPass = await runChatCompletion({
            model,
            messages: [
                { role: 'system', content: systemPrompt },
                { role: 'user', content: userPrompt },
            ],
        });

        const parsed = JSON.parse(extractJsonText(firstPass.content));
        const results = normalizeResultArray(parsed);
        const needSearchItems = isWebSearchEnabled
            ? results.filter((item) => String(item?.classification || '').trim().toLowerCase() === 'needs_search')
            : [];
        const tokenUsage = { ...firstPass.tokenUsage };

        if (needSearchItems.length > 0) {
            const searchResults = await Promise.allSettled(
                needSearchItems.map((item) =>
                    withRemoteLimit(() =>
                        retryWithBackoff(() =>
                            performTavilySearch(item.name, settings.tavilyApiKey, { throwOnError: true })
                        )
                    )
                )
            );

            const searchPrompts = needSearchItems.map((item, index) => {
                const result = searchResults[index];
                if (result.status === 'fulfilled' && result.value) {
                    return `- "${item.name}": ${result.value}`;
                }
                return `- "${item.name}": 未获得可靠联网信息`;
            });

            const secondPassPrompt = `以下是联网补充信息：

${searchPrompts.join('\n')}

请仅对之前标记为 needs_search 的项目给出最终结论。
classification 只能是 safe_to_delete、suspicious、keep。
只返回 JSON 数组。`;

            const secondPass = await runChatCompletion({
                model,
                messages: [
                    { role: 'system', content: systemPrompt },
                    { role: 'user', content: userPrompt },
                    { role: 'assistant', content: firstPass.content },
                    { role: 'user', content: secondPassPrompt },
                ],
            });

            const parsed2 = JSON.parse(extractJsonText(secondPass.content || '[]'));
            const refinedResults = normalizeResultArray(parsed2);

            for (const item of refinedResults) {
                const targetIndex = results.findIndex((row) => row.index === item.index);
                if (targetIndex !== -1) {
                    results[targetIndex] = item;
                }
            }

            tokenUsage.prompt += secondPass.tokenUsage.prompt;
            tokenUsage.completion += secondPass.tokenUsage.completion;
            tokenUsage.total += secondPass.tokenUsage.total;
        } else {
            for (const item of results) {
                if (String(item?.classification || '').trim().toLowerCase() === 'needs_search') {
                    item.classification = 'suspicious';
                    item.reason = `${String(item.reason || '').trim()}（未启用联网搜索，已按可疑处理）`.trim();
                }
            }
        }

        return {
            results: results.map((item, index) => ({
                index: Number(item?.index || index + 1),
                name: String(item?.name || entries[index]?.name || ''),
                classification: normalizeClassification(item?.classification),
                purpose: String(item?.purpose || '').trim(),
                reason: String(item?.reason || '').trim(),
                risk: normalizeRisk(item?.risk, 'medium'),
            })),
            tokenUsage,
            trace: {
                model,
                systemPrompt,
                userPrompt,
                reasoning: firstPass.reasoning,
                rawContent: firstPass.content,
                elapsed: Date.now() - startTime,
                error: null,
            },
        };
    } catch (err) {
        const elapsed = Date.now() - startTime;
        console.error('[Agent] LLM batch analysis failed:', err.message);

        return {
            results: entries.map((entry, index) => ({
                index: index + 1,
                name: entry.name,
                classification: 'suspicious',
                purpose: '',
                reason: `分析失败，建议人工确认：${err.message}`,
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

export async function analyzeScanNode(node, childDirectories = []) {
    const { model } = getClient();
    const normalizedType = node?.type === 'directory' ? 'directory' : 'file';
    const systemPrompt = buildNodeSystemPrompt(normalizedType);
    const userPrompt = normalizedType === 'directory'
        ? buildDirectoryNodeUserPrompt(node, childDirectories)
        : buildFileNodeUserPrompt(node);
    const startTime = Date.now();

    try {
        const response = await runChatCompletion({
            model,
            messages: [
                { role: 'system', content: systemPrompt },
                { role: 'user', content: userPrompt },
            ],
        });

        const parsed = JSON.parse(extractJsonText(response.content));
        const normalized = normalizeNodeReview(parsed, normalizedType);

        return {
            ...normalized,
            tokenUsage: response.tokenUsage,
            trace: {
                model,
                systemPrompt,
                userPrompt,
                reasoning: response.reasoning,
                rawContent: response.content,
                elapsed: Date.now() - startTime,
                error: null,
            },
        };
    } catch (err) {
        const elapsed = Date.now() - startTime;
        console.error('[Agent] Node analysis failed:', err.message);

        return {
            classification: 'suspicious',
            reason: `分析失败，建议人工确认：${err.message}`,
            risk: 'high',
            hasPotentialDeletableSubfolders: normalizedType === 'directory',
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
