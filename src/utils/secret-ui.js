import {
  getSecretStatus,
  lockSecretVault,
  resetSecretVault,
  setupSecretVault,
  unlockSecretVault,
} from './api.js';
import { showToast } from '../main.js';
import { t } from './i18n.js';

let cachedSecretStatus = null;

function emitSecretStatusChanged(status) {
  window.dispatchEvent(new CustomEvent('secret-status-updated', { detail: status }));
}

function rememberStatus(status) {
  cachedSecretStatus = status || null;
  if (status) emitSecretStatusChanged(status);
  return status;
}

export function getCachedSecretStatus() {
  return cachedSecretStatus;
}

export async function refreshSecretStatus() {
  return rememberStatus(await getSecretStatus());
}

export function registerSecretStatusChangeHandler(handler) {
  window.addEventListener('secret-status-updated', handler);
}

function promptForPassword(message) {
  const value = window.prompt(message, '');
  return String(value || '').trim();
}

export async function ensureSecretVaultReady(reasonText = '') {
  const status = await refreshSecretStatus();
  if (status?.unlocked) return status;

  if (!status?.initialized) {
    const password = promptForPassword(
      `${reasonText ? `${reasonText}\n\n` : ''}${t('secret.setup_prompt')}`,
    );
    if (!password) {
      throw new Error(t('secret.password_required'));
    }
    const confirmPassword = promptForPassword(t('secret.setup_confirm_prompt'));
    if (password !== confirmPassword) {
      throw new Error(t('secret.password_mismatch'));
    }
    const result = await setupSecretVault(password);
    const nextStatus = rememberStatus(result?.secretStatus || await getSecretStatus());
    showToast(
      result?.migrated ? t('secret.migrated') : t('secret.setup_done'),
      'success',
    );
    return nextStatus;
  }

  const password = promptForPassword(
    `${reasonText ? `${reasonText}\n\n` : ''}${t('secret.unlock_prompt')}`,
  );
  if (!password) {
    throw new Error(t('secret.password_required'));
  }
  const result = await unlockSecretVault(password);
  const nextStatus = rememberStatus(result?.secretStatus || await getSecretStatus());
  showToast(result?.migrated ? t('secret.migrated') : t('secret.unlock_done'), 'success');
  return nextStatus;
}

export async function lockSecretVaultInUi() {
  const result = await lockSecretVault();
  rememberStatus(result?.secretStatus || await getSecretStatus());
  showToast(t('secret.lock_done'), 'info');
}

export async function resetSecretVaultInUi() {
  if (!confirm(t('secret.reset_confirm'))) {
    return null;
  }
  const result = await resetSecretVault();
  const status = rememberStatus(result?.secretStatus || await getSecretStatus());
  showToast(t('secret.reset_done'), 'success');
  return status;
}

export function getProviderSecretPresence(settings, endpoint) {
  return !!settings?.secretStatus?.providerHasApiKey?.[String(endpoint || '').trim()];
}

export function getSearchSecretPresence(settings) {
  return !!settings?.secretStatus?.searchApiHasKey;
}
