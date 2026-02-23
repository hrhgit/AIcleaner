/**
 * server/search.js
 * 与第三方搜索引擎 (Tavily) 进行交互，为未知软件/目录提供上下文
 */
import fetch from 'node-fetch';

/**
 * Perform a search using Tavily API and return a summarized description.
 * @param {string} query - The search query (e.g. folder name)
 * @param {string} apiKey - Tavily API Key
 * @returns {Promise<string|null>} - Summary text or null if failed
 */
export async function performTavilySearch(query, apiKey) {
    if (!apiKey) {
        console.warn('[Search] Tavily search aborted: No API Key provided by user.');
        return null;
    }

    try {
        console.log(`[Search] Querying Tavily for: "${query}"...`);
        const response = await fetch('https://api.tavily.com/search', {
            method: 'POST',
            headers: {
                'Content-Type': 'application/json'
            },
            body: JSON.stringify({
                api_key: apiKey,
                query: `What is this software or folder used for: "${query}"? Provide a short technical summary or identify if it is malware.`,
                search_depth: 'basic',
                include_answer: true,
                max_results: 3
            })
        });

        if (!response.ok) {
            const errBody = await response.text();
            console.error(`[Search] Tavily API Error (${response.status}):`, errBody);
            return null;
        }

        const data = await response.json();

        // Use Tavily's generated AI answer if available
        if (data.answer) {
            return data.answer;
        }

        // Fallback to summarizing the top results
        if (data.results && data.results.length > 0) {
            const snippets = data.results.map((r, i) => `Result ${i + 1}: ${r.content}`).join('\n');
            return snippets;
        }

        return "No relevant search results found.";

    } catch (error) {
        console.error('[Search] Failed to connect to Tavily:', error.message);
        return null;
    }
}
