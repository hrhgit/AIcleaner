/**
 * src/main.js
 * 应用入口 — 路由管理与页面切换
 */
import { renderAdvisor } from './pages/advisor.js';
import { renderOrganizer, renderOrganizerResults } from './pages/organizer.js';
import { browseFolder, getSettings, moveDataDir, openExternalUrl } from './utils/api.js';
import { emitLangChange, registerLangChangeHandler, setLang, getLang, t } from './utils/i18n.js';
import { initProviderManager } from './components/provider-manager.js';
import { showToast } from './utils/toast.js';

const pages = {
    advisor: renderAdvisor,
    organizer: renderOrganizer,
    'organizer-results': renderOrganizerResults,
};

let currentPage = null;
let pageLangChangeUnsubscribe = null;

function hideBootSplash() {
    const splash = document.getElementById('boot-splash');
    if (!splash) return;
    splash.classList.add('hidden');
    window.setTimeout(() => splash.remove(), 220);
}

function normalizePageName(pageName) {
    return pageName;
}

function navigate(pageName) {
    const normalizedPage = normalizePageName(pageName);
    const container = document.getElementById('page-container');
    if (!container) return;

    // Update active nav
    document.querySelectorAll('.nav-link').forEach(link => {
        link.classList.toggle('active', link.dataset.page === normalizedPage);
    });

    // Render page
    currentPage = normalizedPage;
    container.innerHTML = '';
    container.style.animation = 'none';
    // Trigger reflow to restart animation
    void container.offsetHeight;
    container.style.animation = '';

    const renderer = pages[normalizedPage];
    if (renderer) {
        pageLangChangeUnsubscribe?.();
        pageLangChangeUnsubscribe = registerLangChangeHandler(() => {
            if (currentPage === normalizedPage) renderer(container); // Refresh active page
        });
        renderer(container);
    } else {
        container.innerHTML = `
      <div class="empty-state">
        <div class="empty-state-icon">🔍</div>
        <div class="empty-state-text" data-i18n="page.not_found">页面未找到</div>
      </div>
    `;
    }
}

function getPageFromHash() {
    const hash = normalizePageName(window.location.hash.replace('#/', ''));
    return pages[hash] ? hash : 'advisor';
}

function updateShellCopy() {
    const currentLang = getLang();
    const advisorLabel = document.getElementById('nav-advisor-label');
    const organizerLabel = document.getElementById('nav-organizer-label');
    const organizerResultsLabel = document.getElementById('nav-organizer-results-label');
    const bootSubtitle = document.getElementById('boot-subtitle');
    const bootHint = document.getElementById('boot-hint');
    if (advisorLabel) advisorLabel.textContent = currentLang === 'en' ? 'Advisor' : '顾问';
    if (organizerLabel) organizerLabel.textContent = t('nav.organizer');
    if (organizerResultsLabel) organizerResultsLabel.textContent = t('nav.organizer_results');
    if (bootSubtitle) bootSubtitle.textContent = currentLang === 'en' ? 'Loading workspace...' : '正在加载工作台...';
    if (bootHint) bootHint.textContent = currentLang === 'en'
        ? 'The first dev-mode load may be slower while frontend assets connect.'
        : '开发模式下首次连接前端资源会稍慢一些，但不应再出现空白窗口。';
}

async function handleExternalLinkClick(event) {
    const link = event.target?.closest?.('a[data-open-external="true"][href]');
    if (!link) return;

    event.preventDefault();
    try {
        await openExternalUrl(link.href);
    } catch (err) {
        showToast(`Failed to open link: ${err?.message || err}`, 'error');
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
            showToast(t('settings.toast_cache_dir_move_failed') + (err?.message || err), 'error');
        } finally {
            btn.disabled = false;
            btn.textContent = originalText;
        }
    });

    refreshMoveDataDirButtonMeta();
}

// Event listeners
window.addEventListener('hashchange', () => {
    navigate(getPageFromHash());
});

document.addEventListener('DOMContentLoaded', () => {
    initProviderManager();
    initMoveDataDirButton();
    document.addEventListener('click', handleExternalLinkClick);

    // Nav link clicks
    document.querySelectorAll('.nav-link').forEach(link => {
        link.addEventListener('click', (e) => {
            e.preventDefault();
            const page = link.dataset.page;
            window.location.hash = `#/${page}`;
        });
    });

    // Language switch
    function updateLangUI() {
        const currentLang = getLang();
        document.querySelectorAll('.lang-opt').forEach(opt => {
            opt.classList.toggle('active', opt.dataset.lang === currentLang);
        });
        updateShellCopy();
    }

    document.querySelectorAll('.lang-opt').forEach(opt => {
        opt.addEventListener('click', () => {
            setLang(opt.dataset.lang);
            emitLangChange();
            updateLangUI();
            refreshMoveDataDirButtonMeta();
        });
    });

    // Initial lang UI state
    updateLangUI();

    // Initial page
    navigate(getPageFromHash());
    window.requestAnimationFrame(() => {
        hideBootSplash();
    });
});

