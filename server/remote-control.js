import pLimit from 'p-limit';

export const REMOTE_CONCURRENCY = 5;
const DEFAULT_MAX_RETRIES = 3;
const DEFAULT_BASE_DELAY_MS = 500;
const DEFAULT_JITTER_MS = 200;

const remoteLimiter = pLimit(REMOTE_CONCURRENCY);

function sleep(ms) {
    return new Promise((resolve) => setTimeout(resolve, ms));
}

function getStatusCode(error) {
    return error?.status ?? error?.statusCode ?? error?.response?.status ?? null;
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

export function withRemoteLimit(task) {
    return remoteLimiter(task);
}

export async function retryWithBackoff(task, options = {}) {
    const maxRetries = options.maxRetries ?? DEFAULT_MAX_RETRIES;
    const baseDelayMs = options.baseDelayMs ?? DEFAULT_BASE_DELAY_MS;
    const jitterMs = options.jitterMs ?? DEFAULT_JITTER_MS;
    const shouldRetry = options.shouldRetry ?? isRetryableRemoteError;

    let attempt = 0;
    while (true) {
        try {
            return await task(attempt);
        } catch (error) {
            const canRetry = attempt < maxRetries && shouldRetry(error);
            if (!canRetry) {
                throw error;
            }

            const delay = baseDelayMs * (2 ** attempt) + Math.floor(Math.random() * (jitterMs + 1));
            await sleep(delay);
            attempt += 1;
        }
    }
}
