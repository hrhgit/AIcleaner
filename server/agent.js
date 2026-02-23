/**
 * server/agent.js
 * LLM Agent — 可配置的 AI 分析引擎，接受文件元信息并返回分类结果
 */
import OpenAI from 'openai';
import { loadSettings } from './routes/settings.js';
import { performTavilySearch } from './search.js';

let clientCache = null;
let lastConfig = '';

function getClient() {
    const settings = loadSettings();
    const configKey = `${settings.apiEndpoint}|${settings.apiKey}|${settings.model}`;

    if (clientCache && lastConfig === configKey) {
        return { client: clientCache, model: settings.model };
    }

    clientCache = new OpenAI({
        apiKey: settings.apiKey || 'sk-placeholder',
        baseURL: settings.apiEndpoint || 'https://api.openai.com/v1',
    });
    lastConfig = configKey;
    return { client: clientCache, model: settings.model || 'gpt-4o-mini' };
}

/**
 * Analyze a batch of file entries with LLM.
 * @param {Array} entries - [{name, size, type, path}]
 * @param {string} parentPath - The directory being scanned
 * @returns {Promise<Array>} - Analysis results with classification
 */
export async function analyzeEntries(entries, parentPath) {
    const { client, model } = getClient();

    const entrySummary = entries.map((e, i) =>
        `${i + 1}. [${e.type}] "${e.name}" — ${formatSize(e.size)}`
    ).join('\n');

    const systemPrompt = `你是一个资深的操作系统与磁盘空间清理专家。你需要分析目标文件或目录，判断它们是否可以被安全删除。
你的内部推理过程请使用中文，以便于日志审查。

🚨 【核心规则】：
- 系统文件（Windows, Program Files, 驱动, Registry/注册表相关） → 必须判定为 "keep"
- 用户文档、照片、重要的个人数据 → 必须判定为 "keep"
- 缓存(Cache)、临时文件(Temp)、构建产物(Build artifacts)、日志(Logs)、旧下载内容 → 通常为 "safe_to_delete"
- 如果你遇到完全陌生的软件名称或目录名，且【绝对无法根据你的内部知识库推断其用途】，你 必须 判定为 "needs_search"（需要联网），而不是粗暴地猜测。
- 当你不确定且非陌生软件时，宁可判定为 "suspicious"，也不要判定为 "safe_to_delete"

💻 【输出格式要求】：
你必须且只能返回一个合法的 JSON 数组，不要返回任何其他的 Markdown 文本。
JSON 数组中的每个对象必须严格包含以下字段（Key 必须为英文，枚举值必须严格一致，但描述性内容请使用中文）：
{
  "index": <数字，与输入对应>,
  "name": "<文件名>",
  "classification": "safe_to_delete" | "suspicious" | "keep" | "needs_search", // ⚠️ 必须是这四个英文字符串之一
  "purpose": "<简短的中文描述，说明这个文件/文件夹大概是做什么用的>",
  "reason": "<详细的中文理由，解释为什么它可以/不能被删除（或需要搜索的原因）>",
  "risk": "low" | "medium" | "high" // ⚠️ 必须是这三个英文字符串之一，分别代表低、中、高风险
}`;

    const userPrompt = `请分析以下位于目录 "${parentPath}" 中的项目：

${entrySummary}

请严格按照上述要求返回 JSON 数组。`;

    const startTime = Date.now();

    try {
        const response = await client.chat.completions.create({
            model,
            messages: [
                { role: 'system', content: systemPrompt },
                { role: 'user', content: userPrompt },
            ],
            temperature: 0.1,
        });

        const elapsed = Date.now() - startTime;
        let content = response.choices[0].message.content || '';
        // DeepSeek reasoner models may include reasoning_content
        const reasoning = response.choices[0].message.reasoning_content || '';

        let tokenUsage = {
            prompt: response.usage?.prompt_tokens || 0,
            completion: response.usage?.completion_tokens || 0,
            total: response.usage?.total_tokens || 0,
        };

        // Extract JSON from markdown if present
        let cleanContent = content.trim();
        if (cleanContent.startsWith('```json')) {
            cleanContent = cleanContent.replace(/^```json/, '').replace(/```$/, '').trim();
        } else if (cleanContent.startsWith('```')) {
            cleanContent = cleanContent.replace(/^```/, '').replace(/```$/, '').trim();
        }

        const parsed = JSON.parse(cleanContent);
        let results = Array.isArray(parsed) ? parsed : (parsed.results || parsed.items || parsed.analysis || [parsed]);

        const settings = loadSettings();

        let needSearchItems = results.filter(r => r.classification === 'needs_search');

        // Follow-up PASS for needs_search
        if (needSearchItems.length > 0) {
            if (settings.enableWebSearch && settings.tavilyApiKey) {
                // Fetch search context (Run serially to not bomb API immediately)
                let searchPrompts = [];
                for (let r of needSearchItems) {
                    const summary = await performTavilySearch(r.name, settings.tavilyApiKey);
                    if (summary) {
                        searchPrompts.push(`- 文件/目录 "${r.name}": ${summary}`);
                    } else {
                        searchPrompts.push(`- 文件/目录 "${r.name}": 搜索引擎无明确信息`);
                    }
                }

                const secondPassPrompt = `这是根据你要求提供的联网搜索补充信息：\n\n${searchPrompts.join('\n')}\n\n请结合这些网页信息，为你之前判定为 'needs_search' 的项目给出最终结论。\n输出格式与之前完全一致的 JSON 数组，但 classification 只能从 "safe_to_delete" | "suspicious" | "keep" 中选择。`;

                const response2 = await client.chat.completions.create({
                    model,
                    messages: [
                        { role: 'system', content: systemPrompt },
                        { role: 'user', content: userPrompt },
                        { role: 'assistant', content: cleanContent },
                        { role: 'user', content: secondPassPrompt }
                    ],
                    temperature: 0.1,
                });

                let content2 = response2.choices[0].message.content || '';
                let cleanContent2 = content2.trim();
                if (cleanContent2.startsWith('```json')) cleanContent2 = cleanContent2.replace(/^```json/, '').replace(/```$/, '').trim();
                else if (cleanContent2.startsWith('```')) cleanContent2 = cleanContent2.replace(/^```/, '').replace(/```$/, '').trim();

                const parsed2 = JSON.parse(cleanContent2);
                let secondaryResults = Array.isArray(parsed2) ? parsed2 : (parsed2.results || parsed2.items || parsed2.analysis || [parsed2]);

                // Merge Results
                for (let sr of secondaryResults) {
                    const targetIdx = results.findIndex(original => original.index === sr.index);
                    if (targetIdx !== -1) {
                        results[targetIdx] = sr;
                    }
                }

                tokenUsage.prompt += response2.usage?.prompt_tokens || 0;
                tokenUsage.completion += response2.usage?.completion_tokens || 0;
                tokenUsage.total += response2.usage?.total_tokens || 0;

            } else {
                // Feature off or no key - automatically downgrade to suspicious
                for (let r of needSearchItems) {
                    r.classification = 'suspicious';
                    r.reason += ' (Web Search disabled, downgraded to suspicious)';
                }
            }
        }

        return {
            results: Array.isArray(results) ? results : [results],
            tokenUsage,
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
        console.error('[Agent] LLM analysis failed:', err.message);
        // Fallback: mark everything as suspicious
        return {
            results: entries.map((e, i) => ({
                index: i + 1,
                name: e.name,
                classification: 'suspicious',
                purpose: 'Analysis failed — manual review recommended',
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
 * @param {string} dirName - Name of the directory
 * @param {Array} entries - Its child entries
 * @param {string} parentPath - Directory's absolute path
 */
export async function verifyDirectoryDelete(dirName, entries, parentPath) {
    const { client, model } = getClient();

    const entrySummary = entries.map((e, i) =>
        `${i + 1}. [${e.type}] "${e.name}" — ${formatSize(e.size)}`
    ).join('\n');

    const systemPrompt = `你是一个资深的操作系统与磁盘空间清理专家。你的任务是对一个**初步判定可以删除**的文件夹进行【二次确认核查】。
你的内部推理过程请使用中文，以便于日志审查。

🚨 【任务规则】：
下面用户会提供该文件夹内部包含的文件或目录结构摘要。请仔细审查这些内容：
如果其中包含任何看似重要的数据（如系统相关文件、重要源代码、个人的图片/文档、环境配置等），你必须判定整个文件夹【不可整体删除】。
只有当里面的内容纯粹是无用的缓存（Cache）、旧中间产物（Artifacts）、日志（Logs）、空文件夹或临时文件等，你才能判定为【可以整体删除】。

💻 【输出格式要求】：
你必须且只能返回一个合法的 JSON 对象，不要含有其它 Markdown 文本：
{
  "safe": true | false, // 布尔值表示能否整体删除
  "reason": "<详细的中文理由，解释为什么判定它安全或不安全>"
}`;

    const userPrompt = `我们要二次确认文件夹 "${dirName}"（位于 "${parentPath}"）能否整体安全删除。
以下是它内部包含的文件摘要：

${entrySummary.length > 0 ? entrySummary : "(该文件夹当前为空)"}

请严格按 JSON 格式返回验证结果。`;

    const startTime = Date.now();

    try {
        const response = await client.chat.completions.create({
            model,
            messages: [
                { role: 'system', content: systemPrompt },
                { role: 'user', content: userPrompt },
            ],
            temperature: 0.1,
        });

        const elapsed = Date.now() - startTime;
        let content = response.choices[0].message.content || '';
        const reasoning = response.choices[0].message.reasoning_content || '';

        let cleanContent = content.trim();
        if (cleanContent.startsWith('```json')) {
            cleanContent = cleanContent.replace(/^```json/, '').replace(/```$/, '').trim();
        } else if (cleanContent.startsWith('```')) {
            cleanContent = cleanContent.replace(/^```/, '').replace(/```$/, '').trim();
        }

        const parsed = JSON.parse(cleanContent);

        const tokenUsage = {
            prompt: response.usage?.prompt_tokens || 0,
            completion: response.usage?.completion_tokens || 0,
            total: response.usage?.total_tokens || 0,
        };

        return {
            safe: !!parsed.safe,
            reason: parsed.reason || '',
            tokenUsage,
            trace: {
                model,
                systemPrompt,
                userPrompt,
                reasoning,
                rawContent: content,
                elapsed,
                error: null,
            }
        };
    } catch (err) {
        const elapsed = Date.now() - startTime;
        console.error('[Agent] Directory verification failed:', err.message);
        return {
            safe: false,
            reason: '验证失败：' + err.message,
            tokenUsage: { prompt: 0, completion: 0, total: 0 },
            trace: {
                model: 'gpt-4o-mini',
                systemPrompt,
                userPrompt,
                reasoning: '',
                rawContent: '',
                elapsed,
                error: err.message,
            }
        };
    }
}

function formatSize(bytes) {
    if (bytes < 1024) return `${bytes} B`;
    if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
    if (bytes < 1024 * 1024 * 1024) return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
    return `${(bytes / (1024 * 1024 * 1024)).toFixed(2)} GB`;
}
