import { renderScanner } from './pages/scanner.js';
import { renderResults } from './pages/results.js';
import { renderOrganizer, renderOrganizerResults } from './pages/organizer.js';
import { browseFolder, getSettings, moveDataDir, openExternalUrl } from './utils/api.js';
import { logClientError } from './utils/client-log.js';
import { buildErrorFingerprint, getErrorMessage } from './utils/errors.js';
import { emitLangChange, registerLangChangeHandler, setLang, getLang, t } from './utils/i18n.js';
import { initProviderManager } from './components/provider-manager.js';

const pages = {
  scanner: renderScanner,
  results: renderResults,
  organizer: renderOrganizer,
  'organizer-results': renderOrganizerResults,
};

let currentPage = null;
const recentErrorToasts = new Map();
const ERROR_TOAST_DEDUPE_MS = 5000;

function hideBootSplash() {
  const splash = document.getElementById('boot-splash');
  if (!splash) return;
  splash.classList.add('hidden');
  window.setTimeout(() => splash.remove(), 220);
}

function renderFatalState(container, message) {
  if (!container) return;
  container.innerHTML = '';

  const state = document.createElement('div');
  state.className = 'empty-state';

  const icon = document.createElement('div');
  icon.className = 'empty-state-icon';
  icon.textContent = '!';

  const text = document.createElement('div');
  text.className = 'empty-state-text';
  text.textContent = String(message || t('toast.error'));

  state.append(icon, text);
  container.appendChild(state);
}

function toastErrorOnce(err, prefix = '') {
  const normalized = logClientError('ui_error', err);
  const fingerprint = buildErrorFingerprint(normalized);
  const now = Date.now();
  const lastAt = recentErrorToasts.get(fingerprint) || 0;
  if (now - lastAt < ERROR_TOAST_DEDUPE_MS) {
    return normalized;
  }
  recentErrorToasts.set(fingerprint, now);
  showToast(`${prefix}${normalized.userMessage}`, 'error');
  return normalized;
}

function navigate(pageName) {
  const container = document.getElementById('page-container');
  if (!container) return;

  document.querySelectorAll('.nav-link').forEach((link) => {
    link.classList.toggle('active', link.dataset.page === pageName);
  });

  currentPage = pageName;
  container.innerHTML = '';
  container.style.animation = 'none';
  void container.offsetHeight;
  container.style.animation = '';

  const renderer = pages[pageName];
  if (!renderer) {
    renderFatalState(container, t('page.not_found'));
    return;
  }

  registerLangChangeHandler(() => {
    if (currentPage !== pageName) return;
    try {
      renderer(container);
    } catch (err) {
      const normalized = toastErrorOnce(err, `${t('toast.error')} `);
      renderFatalState(container, normalized.userMessage);
    }
  });

  try {
    renderer(container);
  } catch (err) {
    const normalized = toastErrorOnce(err, `${t('toast.error')} `);
    renderFatalState(container, normalized.userMessage);
  }
}

function getPageFromHash() {
  const hash = window.location.hash.replace('#/', '');
  return pages[hash] ? hash : 'scanner';
}

async function handleExternalLinkClick(event) {
  const link = event.target?.closest?.('a[data-open-external="true"][href]');
  if (!link) return;

  event.preventDefault();
  try {
    await openExternalUrl(link.href);
  } catch (err) {
    showToast(`Failed to open link: ${getErrorMessage(err)}`, 'error');
  }
}

async function refreshMoveDataDirButtonMeta() {
  const btn = document.getElementById('move-data-dir-sidebar-btn');
  if (!btn) return;
  try {
    const settings = await getSettings();
    const storage = settings?.storage && typeof settings.storage === 'object' ? settings.storage : {};
    const activePath = String(storage.dataDir || '').trim();
    const defaultPath = String(storage.defaultDataDir || '').trim();
    btn.title = storage.customized
      ? t('settings.cache_dir_custom', { defaultPath })
      : t('settings.cache_dir_default', { defaultPath: defaultPath || activePath });
  } catch {
    btn.title = t('settings.cache_dir_hint');
  }
}

function initMoveDataDirButton() {
  const btn = document.getElementById('move-data-dir-sidebar-btn');
  if (!btn) return;

  btn.addEventListener('click', async () => {
    const originalText = btn.textContent || t('settings.cache_dir_apply');
    btn.disabled = true;
    btn.innerHTML = `<span class="spinner"></span> ${t('settings.browsing')}`;
    try {
      const picked = await browseFolder();
      if (picked?.cancelled || !picked?.path) {
        return;
      }
      btn.innerHTML = `<span class="spinner"></span> ${t('settings.cache_dir_moving')}`;
      const result = await moveDataDir(picked.path);
      showToast(t('settings.toast_cache_dir_moved') + (result?.dataDir || picked.path), 'success');
      if (result?.cleanupWarning) {
        showToast(t('settings.cache_dir_cleanup_warning') + result.cleanupWarning, 'info');
      }
      await refreshMoveDataDirButtonMeta();
    } catch (err) {
      showToast(t('settings.toast_cache_dir_move_failed') + getErrorMessage(err), 'error');
    } finally {
      btn.disabled = false;
      btn.textContent = originalText;
    }
  });

  refreshMoveDataDirButtonMeta();
}

window.addEventListener('hashchange', () => {
  try {
    navigate(getPageFromHash());
  } catch (err) {
    toastErrorOnce(err, `${t('toast.error')} `);
  }
});

window.addEventListener('error', (event) => {
  toastErrorOnce(event?.error || event?.message || event, `${t('toast.error')} `);
});

window.addEventListener('unhandledrejection', (event) => {
  toastErrorOnce(event?.reason || event, `${t('toast.error')} `);
});

document.addEventListener('DOMContentLoaded', () => {
  try {
    initProviderManager();
    initMoveDataDirButton();
    document.addEventListener('click', handleExternalLinkClick);

    document.querySelectorAll('.nav-link').forEach((link) => {
      link.addEventListener('click', (event) => {
        event.preventDefault();
        const page = link.dataset.page;
        window.location.hash = `#/${page}`;
      });
    });

    function updateLangUI() {
      const currentLang = getLang();
      document.querySelectorAll('.lang-opt').forEach((opt) => {
        opt.classList.toggle('active', opt.dataset.lang === currentLang);
      });
    }

    document.querySelectorAll('.lang-opt').forEach((opt) => {
      opt.addEventListener('click', () => {
        setLang(opt.dataset.lang);
        emitLangChange();
        updateLangUI();
        refreshMoveDataDirButtonMeta();
      });
    });

    updateLangUI();
    navigate(getPageFromHash());
    hideBootSplash();
  } catch (err) {
    const normalized = toastErrorOnce(err, `${t('toast.error')} `);
    renderFatalState(document.getElementById('page-container'), normalized.userMessage);
    hideBootSplash();
  }
});

export function showToast(message, type = 'success') {
  const existing = document.querySelector('.toast');
  if (existing) existing.remove();

  const toast = document.createElement('div');
  toast.className = `toast toast-${type}`;

  const icon = document.createElement('span');
  icon.textContent = type === 'success' ? 'OK' : type === 'error' ? '!' : 'i';

  const text = document.createElement('span');
  text.textContent = String(message ?? '');

  toast.append(icon, text);
  document.body.appendChild(toast);

  setTimeout(() => {
    toast.style.opacity = '0';
    toast.style.transform = 'translateY(8px)';
    setTimeout(() => toast.remove(), 300);
  }, 3000);
}
