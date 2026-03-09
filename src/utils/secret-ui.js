import { getSettings } from './api.js';
import { t } from './i18n.js';

let cachedCredentialsStatus = null;

function emitCredentialsStatusChanged(status) {
  window.dispatchEvent(new CustomEvent('credentials-status-updated', { detail: status }));
}

function rememberStatus(status) {
  cachedCredentialsStatus = status || null;
  if (status) emitCredentialsStatusChanged(status);
  return status;
}

export function getCachedCredentialsStatus() {
  return cachedCredentialsStatus;
}

export async function refreshCredentialsStatus() {
  const settings = await getSettings();
  return rememberStatus(settings?.credentialsStatus || null);
}

export function registerCredentialsStatusChangeHandler(handler) {
  window.addEventListener('credentials-status-updated', handler);
}

export async function ensureRequiredCredentialsConfigured({
  providerEndpoints = [],
  requireSearchApi = false,
  reasonText = '',
} = {}) {
  const settings = await getSettings();
  const status = rememberStatus(settings?.credentialsStatus || null) || {};
  const missingProviderEndpoints = Array.from(
    new Set(
      (providerEndpoints || [])
        .map((endpoint) => String(endpoint || '').trim())
        .filter(Boolean),
    ),
  ).filter((endpoint) => !status?.providerHasApiKey?.[endpoint]);
  const missingSearchApi = !!requireSearchApi && !status?.searchApiHasKey;

  if (!missingProviderEndpoints.length && !missingSearchApi) {
    return status;
  }

  window.dispatchEvent(new CustomEvent('open-provider-manager-requested', {
    detail: {
      reasonText: reasonText || t('settings.api_key_managed_hint'),
      missingProviderEndpoints,
      requireSearchApi: missingSearchApi,
    },
  }));

  throw new Error(reasonText || t('settings.api_key_managed_hint'));
}

export function getProviderCredentialPresence(settings, endpoint) {
  return !!settings?.credentialsStatus?.providerHasApiKey?.[String(endpoint || '').trim()];
}

export function getSearchCredentialPresence(settings) {
  return !!settings?.credentialsStatus?.searchApiHasKey;
}
