import pLimit from 'p-limit';

export const REMOTE_CONCURRENCY = 5;
const DEFAULT_MAX_RETRIES = 3;
const DEFAULT_MAX_RETRIES_FOR_429 = 7;
const DEFAULT_BASE_DELAY_MS = 500;
const DEFAULT_JITTER_MS = 200;
const DEFAULT_MAX_DELAY_MS = 30000;

const remoteLimiter = pLimit(REMOTE_CONCURRENCY);

function sleep(ms) {
    return new Promise((resolve) => setTimeout(resolve, ms));
}

function parseStatusFromMessage(error) {
    const message = String(error?.message || '').toLowerCase();
    const match = message.match(/\bstatus code\s*\(?(\d{3})\)?/i) || message.match(/\b(\d{3})\s*status code\b/i);
    if (!match) return null;
    const n = Number(match[1]);
    return Number.isFinite(n) ? n : null;
}

function getStatusCode(error) {
    return error?.status ?? error?.statusCode ?? error?.response?.status ?? parseStatusFromMessage(error) ?? null;
}

function getHeaderValue(headers, key) {
    if (!headers) return '';
    if (typeof headers.get === 'function') {
        return headers.get(key) || '';
    }

    const lower = key.toLowerCase();
    for (const [k, v] of Object.entries(headers)) {
        if (String(k).toLowerCase() === lower) {
            return Array.isArray(v) ? String(v[0] || '') : String(v || '');
        }
    }
    return '';
}

function getRetryAfterMs(error) {
    const headers = error?.headers || error?.response?.headers;
    const raw = getHeaderValue(headers, 'retry-after');
    if (!raw) return 0;

    const seconds = Number(raw);
    if (Number.isFinite(seconds)) {
        return Math.max(0, Math.floor(seconds * 1000));
    }

    const dateMs = Date.parse(raw);
    if (Number.isFinite(dateMs)) {
        return Math.max(0, dateMs - Date.now());
    }
    return 0;
}

function hasAny(text, patterns) {
    return patterns.some((pattern) => text.includes(pattern));
}

export function isRetryableRemoteError(error) {
    const status = getStatusCode(error);
    if (status === 408 || status === 429 || status >= 500) {
        return true;
    }

    const code = String(error?.code || '').toLowerCase();
    if (hasAny(code, ['etimedout', 'timeout', 'econnreset', 'econnrefused', 'ehostunreach', 'eai_again'])) {
        return true;
    }

    const name = String(error?.name || '').toLowerCase();
    if (hasAny(name, ['ratelimiterror', 'apiconnectionerror', 'timeout'])) {
        return true;
    }

    const message = String(error?.message || '').toLowerCase();
    return hasAny(message, [
        'rate limit',
        'too many requests',
        'timed out',
        'timeout',
        'connection reset',
        'network',
        'temporarily unavailable',
        'try again',
    ]);
}

export function isRateLimitError(error) {
    const status = getStatusCode(error);
    if (status === 429) return true;
    const message = String(error?.message || '').toLowerCase();
    return hasAny(message, ['429 status code', 'status code 429', 'rate limit', 'too many requests']);
}

export function withRemoteLimit(task) {
    return remoteLimiter(task);
}

export async function retryWithBackoff(task, options = {}) {
    const maxRetries = options.maxRetries ?? DEFAULT_MAX_RETRIES;
    const maxRetriesFor429 = options.maxRetriesFor429 ?? DEFAULT_MAX_RETRIES_FOR_429;
    const baseDelayMs = options.baseDelayMs ?? DEFAULT_BASE_DELAY_MS;
    const jitterMs = options.jitterMs ?? DEFAULT_JITTER_MS;
    const maxDelayMs = options.maxDelayMs ?? DEFAULT_MAX_DELAY_MS;
    const shouldRetry = options.shouldRetry ?? isRetryableRemoteError;

    let attempt = 0;
    while (true) {
        try {
            return await task(attempt);
        } catch (error) {
            const status = getStatusCode(error);
            const retryLimit = status === 429 ? maxRetriesFor429 : maxRetries;
            const canRetry = attempt < retryLimit && shouldRetry(error);
            if (!canRetry) {
                throw error;
            }

            const backoffDelay = baseDelayMs * (2 ** attempt) + Math.floor(Math.random() * (jitterMs + 1));
            const retryAfterDelay = status === 429 ? getRetryAfterMs(error) : 0;
            const delay = Math.min(Math.max(backoffDelay, retryAfterDelay), maxDelayMs);
            await sleep(delay);
            attempt += 1;
        }
    }
}
