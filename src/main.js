/**
 * src/main.js
 * 应用入口 — 路由管理与页面切换
 */
import { renderSettings } from './pages/settings.js';
import { renderScanner } from './pages/scanner.js';
import { renderResults } from './pages/results.js';
import { t, toggleLang, emitLangChange, registerLangChangeHandler, setLang, getLang } from './utils/i18n.js';

const pages = {
    settings: renderSettings,
    scanner: renderScanner,
    results: renderResults,
};

let currentPage = null;

function navigate(pageName) {
    const container = document.getElementById('page-container');
    if (!container) return;

    // Update active nav
    document.querySelectorAll('.nav-link').forEach(link => {
        link.classList.toggle('active', link.dataset.page === pageName);
    });

    // Render page
    currentPage = pageName;
    container.innerHTML = '';
    container.style.animation = 'none';
    // Trigger reflow to restart animation
    void container.offsetHeight;
    container.style.animation = '';

    const renderer = pages[pageName];
    if (renderer) {
        // Re-render immediately when language changes
        registerLangChangeHandler(() => {
            if (currentPage === pageName) renderer(container); // Refresh active page
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
    const hash = window.location.hash.replace('#/', '');
    return pages[hash] ? hash : 'settings';
}

// Event listeners
window.addEventListener('hashchange', () => {
    navigate(getPageFromHash());
});

document.addEventListener('DOMContentLoaded', () => {
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
    }

    document.querySelectorAll('.lang-opt').forEach(opt => {
        opt.addEventListener('click', () => {
            setLang(opt.dataset.lang);
            emitLangChange();
            updateLangUI();
        });
    });

    // Initial lang UI state
    updateLangUI();

    // Initial page
    navigate(getPageFromHash());
});

// Toast utility
export function showToast(message, type = 'success') {
    const existing = document.querySelector('.toast');
    if (existing) existing.remove();

    const toast = document.createElement('div');
    toast.className = `toast toast-${type}`;
    toast.innerHTML = `
    <span>${type === 'success' ? '✓' : type === 'error' ? '✗' : 'ℹ'}</span>
    <span>${message}</span>
  `;
    document.body.appendChild(toast);

    setTimeout(() => {
        toast.style.opacity = '0';
        toast.style.transform = 'translateY(8px)';
        setTimeout(() => toast.remove(), 300);
    }, 3000);
}
