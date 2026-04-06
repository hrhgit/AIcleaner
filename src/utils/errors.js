const DEFAULT_USER_MESSAGE = '操作失败';

function pickText(value) {
  return typeof value === 'string' && value.trim() ? value.trim() : '';
}

function normalizeContext(context = {}, fallback = {}) {
  return {
    operation: pickText(context?.operation) || pickText(fallback?.operation),
    taskId: pickText(context?.taskId) || pickText(fallback?.taskId),
    path: pickText(context?.path) || pickText(fallback?.path),
    endpoint: pickText(context?.endpoint) || pickText(fallback?.endpoint),
    model: pickText(context?.model) || pickText(fallback?.model),
    stage: pickText(context?.stage) || pickText(fallback?.stage),
    httpStatus: Number.isFinite(Number(context?.httpStatus))
      ? Number(context.httpStatus)
      : (Number.isFinite(Number(fallback?.httpStatus)) ? Number(fallback.httpStatus) : null),
  };
}

export function normalizeAppError(input, fallback = {}) {
  const source = input && typeof input === 'object' && input.error && typeof input.error === 'object'
    ? input.error
    : input;

  const code = pickText(source?.code) || pickText(fallback?.code) || 'INTERNAL_ERROR';
  const rawMessage = pickText(source?.detail)
    || pickText(source?.rawMessage)
    || pickText(source?.message)
    || pickText(source?.error)
    || pickText(typeof source === 'string' ? source : '')
    || pickText(fallback?.detail)
    || pickText(fallback?.message);
  const userMessage = pickText(source?.userMessage)
    || pickText(source?.user_message)
    || pickText(fallback?.userMessage)
    || rawMessage
    || DEFAULT_USER_MESSAGE;

  return {
    code,
    message: userMessage,
    userMessage,
    detail: rawMessage || userMessage,
    retryable: Boolean(source?.retryable ?? fallback?.retryable),
    context: normalizeContext(source?.context, fallback?.context),
  };
}

export function normalizeTaskErrorPayload(payload, fallback = {}) {
  const normalizedError = normalizeAppError(payload?.error ?? payload?.message ?? payload, fallback);
  return {
    ...(payload && typeof payload === 'object' ? payload : {}),
    error: normalizedError,
    message: normalizedError.userMessage,
  };
}

export function getErrorMessage(input, fallbackMessage = DEFAULT_USER_MESSAGE) {
  return normalizeAppError(input, { userMessage: fallbackMessage }).userMessage;
}

export function getErrorCode(input, fallbackCode = 'INTERNAL_ERROR') {
  return normalizeAppError(input, { code: fallbackCode }).code;
}

export function buildErrorFingerprint(input) {
  const error = normalizeAppError(input);
  const context = error.context || {};
  return [
    error.code,
    context.operation || '',
    context.taskId || '',
    context.path || '',
    context.stage || '',
    error.userMessage,
  ].join('|');
}
