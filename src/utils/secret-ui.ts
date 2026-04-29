import type { CredentialsStatus } from '../types';
import { getSettings } from './api';
import { t } from './i18n';

let cachedCredentialsStatus: CredentialsStatus | null = null;

function emitCredentialsStatusChanged(status: CredentialsStatus): void {
  window.dispatchEvent(new CustomEvent('credentials-status-updated', { detail: status }));
}

function rememberStatus(status: CredentialsStatus | null | undefined): CredentialsStatus | null {
  cachedCredentialsStatus = status || null;
  if (status) emitCredentialsStatusChanged(status);
  return cachedCredentialsStatus;
}

export function getCachedCredentialsStatus(): CredentialsStatus | null {
  return cachedCredentialsStatus;
}

export async function refreshCredentialsStatus(): Promise<CredentialsStatus | null> {
  const settings = await getSettings({ force: true });
  return rememberStatus(settings.credentialsStatus || null);
}

export async function ensureRequiredCredentialsConfigured({
  providerEndpoints = [],
  requireSearchApi = false,
  reasonText = '',
}: {
  providerEndpoints?: string[];
  requireSearchApi?: boolean;
  reasonText?: string;
} = {}): Promise<CredentialsStatus> {
  const settings = await getSettings({ force: true });
  const status = rememberStatus(settings.credentialsStatus || null) || {};
  const missingProviderEndpoints = Array.from(
    new Set(providerEndpoints.map((endpoint) => String(endpoint || '').trim()).filter(Boolean)),
  ).filter((endpoint) => !status.providerHasApiKey?.[endpoint]);
  const missingSearchApi = !!requireSearchApi && !status.searchApiHasKey;

  if (!missingProviderEndpoints.length && !missingSearchApi) return status;

  window.dispatchEvent(new CustomEvent('open-provider-manager-requested', {
    detail: {
      reasonText: reasonText || t('settings.api_key_managed_hint'),
      missingProviderEndpoints,
      requireSearchApi: missingSearchApi,
    },
  }));

  throw new Error(reasonText || t('settings.api_key_managed_hint'));
}

