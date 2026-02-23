/**
 * src/utils/storage.js
 * localStorage 辅助工具 — 前端临时缓存
 */

const PREFIX = 'dust_';

export function get(key, fallback = null) {
    try {
        const raw = localStorage.getItem(PREFIX + key);
        return raw ? JSON.parse(raw) : fallback;
    } catch {
        return fallback;
    }
}

export function set(key, value) {
    try {
        localStorage.setItem(PREFIX + key, JSON.stringify(value));
    } catch { /* quota exceeded — ignore */ }
}

export function remove(key) {
    localStorage.removeItem(PREFIX + key);
}

export function formatSize(bytes) {
    if (bytes == null || isNaN(bytes)) return '0 B';
    if (bytes < 1024) return `${bytes} B`;
    if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
    if (bytes < 1024 * 1024 * 1024) return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
    return `${(bytes / (1024 * 1024 * 1024)).toFixed(2)} GB`;
}
