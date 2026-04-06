import { error as writeError, warn as writeWarn } from '@tauri-apps/plugin-log';
import { normalizeAppError } from './errors.js';

const isTauri = typeof window !== 'undefined' && '__TAURI_INTERNALS__' in window;

function toKeyValues(context = {}) {
  const values = {};
  for (const [key, value] of Object.entries(context || {})) {
    if (value == null || value === '') continue;
    values[key] = String(value);
  }
  return values;
}

export function logClientError(source, err, fallback = {}) {
  const normalized = normalizeAppError(err, {
    ...fallback,
    context: {
      operation: source,
      ...(fallback?.context || {}),
    },
  });

  console.error(`[client:${source}]`, normalized.detail, normalized);

  if (isTauri) {
    writeError(
      `[${source}] ${normalized.code}: ${normalized.detail}`,
      {
        keyValues: toKeyValues({
          operation: normalized.context?.operation || source,
          taskId: normalized.context?.taskId,
          path: normalized.context?.path,
          endpoint: normalized.context?.endpoint,
          model: normalized.context?.model,
          stage: normalized.context?.stage,
          httpStatus: normalized.context?.httpStatus,
        }),
      },
    ).catch(() => {});
  }

  return normalized;
}

export function logClientWarn(source, message, context = {}) {
  const text = String(message || '').trim();
  if (!text) return;

  console.warn(`[client:${source}]`, text, context);

  if (isTauri) {
    writeWarn(
      `[${source}] ${text}`,
      { keyValues: toKeyValues(context) },
    ).catch(() => {});
  }
}
