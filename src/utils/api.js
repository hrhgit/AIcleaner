/**
 * src/utils/api.js
 * 封装后端 API 调用与 SSE 连接管理
 */

const BASE = '/api';

function previewBody(text) {
    return String(text || '')
        .replace(/\s+/g, ' ')
        .trim()
        .slice(0, 200);
}

export async function fetchJSON(path, options = {}) {
    const res = await fetch(`${BASE}${path}`, {
        headers: { 'Content-Type': 'application/json' },
        ...options,
        body: options.body ? JSON.stringify(options.body) : undefined,
    });

    const raw = await res.text();
    let data = null;

    if (raw) {
        try {
            data = JSON.parse(raw);
        } catch (err) {
            const preview = previewBody(raw);
            throw new Error(
                `Invalid JSON response from ${path}: ${err.message}${preview ? ` | ${preview}` : ''}`
            );
        }
    }

    if (!res.ok) {
        throw new Error(data?.error || data?.message || res.statusText || `HTTP ${res.status}`);
    }

    return data ?? {};
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

export function getProviderModels(endpoint, apiKey) {
    return fetchJSON('/settings/models', {
        method: 'POST',
        body: { endpoint, apiKey },
    });
}

export function getPrivilegeStatus() {
    return fetchJSON('/system/privilege');
}

export function requestElevation() {
    return fetchJSON('/system/request-elevation', { method: 'POST' });
}

export function getActiveScan() {
    return fetchJSON('/scan/active');
}

export function listScanHistory(limit = 20) {
    return fetchJSON(`/scan/history?limit=${encodeURIComponent(limit)}`);
}

export function deleteScanHistory(taskId) {
    return fetchJSON(`/scan/history/${taskId}`, { method: 'DELETE' });
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

    es.addEventListener('warning', (e) => {
        try {
            const data = JSON.parse(e.data);
            handlers.onWarning?.(data);
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

export function suggestOrganizeCategories(params) {
    return fetchJSON('/organize/suggest-categories', { method: 'POST', body: params });
}

export function getOrganizeCapability() {
    return fetchJSON('/organize/capability');
}

export function startOrganize(params) {
    return fetchJSON('/organize/start', { method: 'POST', body: params });
}

export function stopOrganize(taskId) {
    return fetchJSON(`/organize/stop/${taskId}`, { method: 'POST' });
}

export function getOrganizeResult(taskId) {
    return fetchJSON(`/organize/result/${taskId}`);
}

export function applyOrganize(taskId) {
    return fetchJSON(`/organize/apply/${taskId}`, { method: 'POST' });
}

export function rollbackOrganize(jobId) {
    return fetchJSON(`/organize/rollback/${jobId}`, { method: 'POST' });
}

/**
 * Connect to organize SSE stream.
 * @param {string} taskId
 * @param {object} handlers - { onProgress, onFileDone, onDone, onStopped, onError }
 * @returns {EventSource}
 */
export function connectOrganizeStream(taskId, handlers) {
    const es = new EventSource(`${BASE}/organize/status/${taskId}`);

    es.onmessage = (e) => {
        try {
            const data = JSON.parse(e.data);
            handlers.onProgress?.(data);
        } catch { /* ignore */ }
    };

    es.addEventListener('progress', (e) => {
        try {
            const data = JSON.parse(e.data);
            handlers.onProgress?.(data);
        } catch { /* ignore */ }
    });

    es.addEventListener('file_done', (e) => {
        try {
            const data = JSON.parse(e.data);
            handlers.onFileDone?.(data);
        } catch { /* ignore */ }
    });

    es.addEventListener('done', (e) => {
        try {
            const data = JSON.parse(e.data);
            handlers.onDone?.(data);
        } catch { /* ignore */ }
        es.close();
    });

    es.addEventListener('stopped', (e) => {
        try {
            const data = JSON.parse(e.data);
            handlers.onStopped?.(data);
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

    return es;
}
