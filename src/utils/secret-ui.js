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
let secretDialogPromise = null;

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

function getSecretDialogHost() {
  let host = document.getElementById('secret-dialog-host');
  if (host) return host;

  host = document.createElement('div');
  host.id = 'secret-dialog-host';
  host.className = 'app-modal';
  host.setAttribute('aria-hidden', 'true');
  host.innerHTML = `
    <div class="app-modal-overlay"></div>
    <section class="app-modal-panel card secret-dialog-panel" role="dialog" aria-modal="true" aria-labelledby="secret-dialog-title">
      <div class="app-modal-header">
        <div>
          <h2 id="secret-dialog-title" class="card-title"></h2>
          <p id="secret-dialog-message" class="form-hint secret-dialog-message"></p>
        </div>
      </div>
      <form id="secret-dialog-form" class="secret-dialog-form">
        <div class="form-group">
          <label class="form-label" for="secret-dialog-password">${t('secret.password_label')}</label>
          <input id="secret-dialog-password" class="form-input" type="password" autocomplete="current-password" />
        </div>
        <div id="secret-dialog-confirm-group" class="form-group" hidden>
          <label class="form-label" for="secret-dialog-confirm">${t('secret.password_confirm_label')}</label>
          <input id="secret-dialog-confirm" class="form-input" type="password" autocomplete="new-password" />
        </div>
        <div id="secret-dialog-error" class="form-hint secret-dialog-error" hidden></div>
        <div class="app-modal-actions">
          <button id="secret-dialog-cancel" class="btn btn-ghost" type="button"></button>
          <button id="secret-dialog-submit" class="btn btn-primary" type="submit"></button>
        </div>
      </form>
    </section>
  `;
  document.body.appendChild(host);
  return host;
}

function openSecretDialog({
  title,
  message,
  submitLabel,
  requireConfirm = false,
}) {
  if (secretDialogPromise) return secretDialogPromise;

  const host = getSecretDialogHost();
  const overlayEl = host.querySelector('.app-modal-overlay');
  const titleEl = host.querySelector('#secret-dialog-title');
  const messageEl = host.querySelector('#secret-dialog-message');
  const formEl = host.querySelector('#secret-dialog-form');
  const passwordEl = host.querySelector('#secret-dialog-password');
  const confirmGroupEl = host.querySelector('#secret-dialog-confirm-group');
  const confirmEl = host.querySelector('#secret-dialog-confirm');
  const errorEl = host.querySelector('#secret-dialog-error');
  const cancelBtn = host.querySelector('#secret-dialog-cancel');
  const submitBtn = host.querySelector('#secret-dialog-submit');

  titleEl.textContent = title;
  messageEl.textContent = message;
  cancelBtn.textContent = t('provider_modal.cancel');
  submitBtn.textContent = submitLabel;
  confirmGroupEl.hidden = !requireConfirm;
  passwordEl.value = '';
  passwordEl.autocomplete = requireConfirm ? 'new-password' : 'current-password';
  confirmEl.value = '';
  errorEl.hidden = true;
  errorEl.textContent = '';

  host.classList.add('open');
  host.setAttribute('aria-hidden', 'false');
  document.body.style.overflow = 'hidden';

  secretDialogPromise = new Promise((resolve, reject) => {
    const close = () => {
      host.classList.remove('open');
      host.setAttribute('aria-hidden', 'true');
      document.body.style.overflow = '';
      formEl.removeEventListener('submit', handleSubmit);
      cancelBtn.removeEventListener('click', handleCancel);
      overlayEl?.removeEventListener('click', handleCancel);
      document.removeEventListener('keydown', handleKeydown);
      secretDialogPromise = null;
    };

    const fail = (messageText) => {
      errorEl.textContent = messageText;
      errorEl.hidden = false;
    };

    const handleCancel = () => {
      close();
      reject(new Error(t('secret.password_required')));
    };

    const handleKeydown = (event) => {
      if (event.key === 'Escape' && host.classList.contains('open')) {
        handleCancel();
      }
    };

    const handleSubmit = (event) => {
      event.preventDefault();
      const password = String(passwordEl.value || '').trim();
      const confirmPassword = String(confirmEl.value || '').trim();
      if (!password) {
        fail(t('secret.password_required'));
        passwordEl.focus();
        return;
      }
      if (requireConfirm && password !== confirmPassword) {
        fail(t('secret.password_mismatch'));
        confirmEl.focus();
        return;
      }
      close();
      resolve(password);
    };

    formEl.addEventListener('submit', handleSubmit);
    cancelBtn.addEventListener('click', handleCancel);
    overlayEl?.addEventListener('click', handleCancel);
    document.addEventListener('keydown', handleKeydown);
    setTimeout(() => passwordEl.focus(), 0);
  });

  return secretDialogPromise;
}

export async function ensureSecretVaultReady(reasonText = '') {
  const status = await refreshSecretStatus();
  if (status?.unlocked) return status;

  if (!status?.initialized) {
    const password = await openSecretDialog({
      title: t('provider_modal.setup'),
      message: `${reasonText ? `${reasonText} ` : ''}${t('secret.setup_prompt')}`,
      submitLabel: t('provider_modal.setup'),
      requireConfirm: true,
    });
    const result = await setupSecretVault(password);
    const nextStatus = rememberStatus(result?.secretStatus || await getSecretStatus());
    showToast(
      result?.migrated ? t('secret.migrated') : t('secret.setup_done'),
      'success',
    );
    return nextStatus;
  }

  const password = await openSecretDialog({
    title: t('provider_modal.unlock'),
    message: `${reasonText ? `${reasonText} ` : ''}${t('secret.unlock_prompt')}`,
    submitLabel: t('provider_modal.unlock'),
  });
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
