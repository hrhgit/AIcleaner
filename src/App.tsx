import { useCallback, useEffect, useState } from 'react';
import { ProviderManager } from './components/provider-manager/ProviderManager';
import { AdvisorPage } from './pages/advisor/AdvisorPage';
import { browseFolder, getSettings, moveDataDir, openExternalUrl } from './utils/api';
import { getErrorMessage } from './utils/errors';
import { t, text, useLanguage } from './utils/i18n';
import { showToast } from './utils/toast';

function TrashIcon({ size = 28 }: { size?: number }) {
  return (
    <svg width={size} height={size} viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
      <path d="M3 6l3 18h12l3-18" />
      <path d="M1 6h22" />
      <path d="M8 6V4a2 2 0 0 1 2-2h4a2 2 0 0 1 2 2v2" />
      <line x1="10" y1="11" x2="10" y2="17" />
      <line x1="14" y1="11" x2="14" y2="17" />
    </svg>
  );
}

function AdvisorIcon() {
  return (
    <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2">
      <path d="M21 15a2 2 0 0 1-2 2H7l-4 4V5a2 2 0 0 1 2-2h14a2 2 0 0 1 2 2z" />
      <path d="M8 9h8" />
      <path d="M8 13h5" />
    </svg>
  );
}

function getPageFromHash(): string {
  const page = window.location.hash.replace('#/', '').trim();
  return page || 'advisor';
}

export function App() {
  const [lang, setLang] = useLanguage();
  const [page, setPage] = useState(getPageFromHash);
  const [movingCache, setMovingCache] = useState(false);
  const [cacheTitle, setCacheTitle] = useState(t('settings.cache_dir_hint'));

  const refreshMoveDataDirMeta = useCallback(async () => {
    try {
      const settings = await getSettings();
      const storage = settings.storage || {};
      const activePath = String(storage.dataDir || '').trim();
      const defaultPath = String(storage.defaultDataDir || '').trim();
      setCacheTitle(storage.customized
        ? t('settings.cache_dir_custom', { defaultPath })
        : t('settings.cache_dir_default', { defaultPath: defaultPath || activePath }));
    } catch {
      setCacheTitle(t('settings.cache_dir_hint'));
    }
  }, [lang]);

  useEffect(() => {
    const hashHandler = () => setPage(getPageFromHash());
    window.addEventListener('hashchange', hashHandler);
    return () => window.removeEventListener('hashchange', hashHandler);
  }, []);

  useEffect(() => {
    document.title = t('app.title');
    void refreshMoveDataDirMeta();
  }, [lang, refreshMoveDataDirMeta]);

  useEffect(() => {
    const clickHandler = (event: MouseEvent) => {
      const target = event.target as HTMLElement | null;
      const link = target?.closest?.('a[data-open-external="true"][href]') as HTMLAnchorElement | null;
      if (!link) return;
      event.preventDefault();
      openExternalUrl(link.href).catch((err) => {
        showToast(`Failed to open link: ${getErrorMessage(err)}`, 'error');
      });
    };
    document.addEventListener('click', clickHandler);
    return () => document.removeEventListener('click', clickHandler);
  }, []);

  const handleMoveCache = async () => {
    setMovingCache(true);
    try {
      const picked = await browseFolder();
      if (picked.cancelled || !picked.path) return;
      const result = await moveDataDir(picked.path);
      showToast(t('settings.toast_cache_dir_moved') + (result.dataDir || picked.path), 'success');
      if (result.cleanupWarning) {
        showToast(t('settings.cache_dir_cleanup_warning') + result.cleanupWarning, 'info');
      }
      await refreshMoveDataDirMeta();
    } catch (err) {
      showToast(t('settings.toast_cache_dir_move_failed') + getErrorMessage(err), 'error');
    } finally {
      setMovingCache(false);
    }
  };

  const navigateAdvisor = (event: React.MouseEvent<HTMLAnchorElement>) => {
    event.preventDefault();
    window.location.hash = '#/advisor';
    setPage('advisor');
  };

  return (
    <div id="app">
      <nav id="sidebar">
        <div className="sidebar-brand">
          <div className="brand-icon"><TrashIcon /></div>
          <span className="brand-text">AIcleaner</span>
        </div>

        <ul className="nav-links">
          <li>
            <a href="#/advisor" data-page="advisor" className={`nav-link ${page === 'advisor' ? 'active' : ''}`} onClick={navigateAdvisor}>
              <AdvisorIcon />
              <span>{text('顾问', 'Advisor')}</span>
            </a>
          </li>
        </ul>

        <div className="sidebar-provider-action">
          <ProviderManager />
          <button
            className="btn btn-sidebar-action"
            type="button"
            title={cacheTitle}
            disabled={movingCache}
            onClick={() => void handleMoveCache()}
          >
            {movingCache ? t('settings.cache_dir_moving') : t('settings.cache_dir_apply')}
          </button>
        </div>

        <div className="sidebar-footer">
          <span className="version">v1.0.0</span>
          <div className="lang-switch-container" role="group" aria-label={text('语言', 'Language')}>
            <button className={`lang-opt ${lang === 'zh' ? 'active' : ''}`} type="button" aria-pressed={lang === 'zh'} onClick={() => setLang('zh')}>中</button>
            <span className="lang-divider">/</span>
            <button className={`lang-opt ${lang === 'en' ? 'active' : ''}`} type="button" aria-pressed={lang === 'en'} onClick={() => setLang('en')}>EN</button>
          </div>
        </div>
      </nav>

      <main id="content">
        {page === 'advisor' ? <AdvisorPage /> : (
          <div className="empty-state">
            <div className="empty-state-icon">?</div>
            <div className="empty-state-text">{t('page.not_found')}</div>
          </div>
        )}
      </main>
    </div>
  );
}
