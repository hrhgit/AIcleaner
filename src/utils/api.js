/**
 * src/utils/api.js
 * 封装后端 API 调用与 SSE 连接管理
 */

const BASE = '/api';

export async function fetchJSON(path, options = {}) {
    const res = await fetch(`${BASE}${path}`, {
        headers: { 'Content-Type': 'application/json' },
        ...options,
        body: options.body ? JSON.stringify(options.body) : undefined,
    });
    if (!res.ok) {
        const err = await res.json().catch(() => ({ error: res.statusText }));
        throw new Error(err.error || `HTTP ${res.status}`);
    }
    return res.json();
}

export function getSettings() {
    return fetchJSON('/settings');
}

export function saveSettings(data) {
    return fetchJSON('/settings', { method: 'POST', body: data });
}

export function browseFolder() {
    return fetchJSON('/settings/browse-folder', { method: 'POST' });
}

export function getActiveScan() {
    return fetchJSON('/scan/active');
}

export function startScan(params) {
    return fetchJSON('/scan/start', { method: 'POST', body: params });
}

export function stopScan(taskId) {
    return fetchJSON(`/scan/stop/${taskId}`, { method: 'POST' });
}

export function getScanResult(taskId) {
    return fetchJSON(`/scan/result/${taskId}`);
}

/**
 * Connect to SSE stream for scan progress.
 * @param {string} taskId
 * @param {object} handlers - { onProgress, onFound, onDone, onError }
 * @returns {EventSource}
 */
export function connectScanStream(taskId, handlers) {
    const es = new EventSource(`${BASE}/scan/status/${taskId}`);

    es.onmessage = (e) => {
        try {
            const data = JSON.parse(e.data);
            handlers.onProgress?.(data);
        } catch { /* ignore */ }
    };

    es.addEventListener('found', (e) => {
        try {
            const data = JSON.parse(e.data);
            handlers.onFound?.(data);
        } catch { /* ignore */ }
    });

    es.addEventListener('agent_call', (e) => {
        try {
            const data = JSON.parse(e.data);
            handlers.onAgentCall?.(data);
        } catch { /* ignore */ }
    });

    es.addEventListener('agent_response', (e) => {
        try {
            const data = JSON.parse(e.data);
            handlers.onAgentResponse?.(data);
        } catch { /* ignore */ }
    });

    es.addEventListener('done', (e) => {
        try {
            const data = JSON.parse(e.data);
            handlers.onDone?.(data);
        } catch { /* ignore */ }
        es.close();
    });

    es.addEventListener('error', (e) => {
        if (e.data) {
            try {
                const data = JSON.parse(e.data);
                handlers.onError?.(data);
            } catch { /* ignore */ }
        }
        es.close();
    });

    es.addEventListener('stopped', (e) => {
        try {
            const data = JSON.parse(e.data);
            handlers.onStopped?.(data);
        } catch { /* ignore */ }
        es.close();
    });

    return es;
}

export function openFileLocation(path) {
    return fetchJSON('/files/open-location', { method: 'POST', body: { path } });
}

export function deleteFiles(paths) {
    return fetchJSON('/files/delete', { method: 'POST', body: { paths } });
}
