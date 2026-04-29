import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import type { CredentialsStatus, ProviderModelOption, ProviderRow, SearchApiSettings } from '../../types';
import { getCredentials, getProviderModels, getSettings, saveCredentials, saveSettings } from '../../utils/api';
import { getErrorMessage } from '../../utils/errors';
import { t } from '../../utils/i18n';
import {
  fallbackModelsByEndpoint,
  normalizeRemoteModels,
} from '../../utils/provider-registry';
import { refreshCredentialsStatus } from '../../utils/secret-ui';
import { showToast } from '../../utils/toast';
import {
  applyLoadedProviderModels,
  buildDirtyCredentialsPayload,
  buildProviderSettingsPayload,
  mergeProviderModelOptions,
  normalizeProviders,
  normalizeSearchApi,
} from './normalizers';

type DirtyState = {
  providerSecrets: Record<string, boolean>;
  searchApiKey: boolean;
};

type EditableCredentials = {
  providerSecrets: Record<string, string>;
  searchApiKey: string;
};

const emptyDirtyState = (): DirtyState => ({ providerSecrets: {}, searchApiKey: false });

export function ProviderManager() {
  const [open, setOpen] = useState(false);
  const [confirmOpen, setConfirmOpen] = useState(false);
  const [loading, setLoading] = useState(false);
  const [saving, setSaving] = useState(false);
  const [providers, setProviders] = useState<ProviderRow[]>([]);
  const [defaultProviderEndpoint, setDefaultProviderEndpoint] = useState('');
  const [activeTab, setActiveTab] = useState<string>('tavily');
  const [searchApi, setSearchApi] = useState<Required<SearchApiSettings>>({
    provider: 'tavily',
    enabled: false,
    scopes: { classify: false, organizer: false },
  });
  const [credentialsStatus, setCredentialsStatus] = useState<CredentialsStatus>({});
  const [editableCredentials, setEditableCredentials] = useState<EditableCredentials>({ providerSecrets: {}, searchApiKey: '' });
  const [dirtyCredentials, setDirtyCredentials] = useState<DirtyState>(emptyDirtyState);
  const [modelOptions, setModelOptions] = useState<Record<string, ProviderModelOption[]>>({});
  const [modelLoading, setModelLoading] = useState<Record<string, boolean>>({});
  const initialSnapshot = useRef('');
  const requestTokens = useRef<Record<string, number>>({});

  const buildSnapshot = useCallback(() => JSON.stringify({
    settings: buildProviderSettingsPayload(providers, defaultProviderEndpoint, searchApi),
    credentials: editableCredentials,
  }), [defaultProviderEndpoint, editableCredentials, providers, searchApi]);

  const hasUnsavedChanges = useMemo(() => open && buildSnapshot() !== initialSnapshot.current, [buildSnapshot, open]);

  const refreshModalData = useCallback(async () => {
    setLoading(true);
    try {
      const settings = await getSettings({ force: true });
      const normalized = normalizeProviders(settings);
      const credentials = await getCredentials();
      const nextSearchApi = normalizeSearchApi(settings);
      setProviders(normalized.providers);
      setDefaultProviderEndpoint(normalized.defaultProviderEndpoint);
      setActiveTab(normalized.defaultProviderEndpoint || normalized.providers[0]?.endpoint || 'tavily');
      setSearchApi(nextSearchApi);
      setCredentialsStatus(settings.credentialsStatus || {});
      setEditableCredentials({
        providerSecrets: { ...(credentials.providerSecrets || {}) },
        searchApiKey: String(credentials.searchApiKey || ''),
      });
      setDirtyCredentials(emptyDirtyState());
      const settingsSnapshot = buildProviderSettingsPayload(normalized.providers, normalized.defaultProviderEndpoint, nextSearchApi);
      initialSnapshot.current = JSON.stringify({
        settings: settingsSnapshot,
        credentials: {
          providerSecrets: { ...(credentials.providerSecrets || {}) },
          searchApiKey: String(credentials.searchApiKey || ''),
        },
      });
    } finally {
      setLoading(false);
    }
  }, []);

  const openModal = useCallback(async () => {
    try {
      await refreshModalData();
      setConfirmOpen(false);
      setOpen(true);
    } catch (err) {
      showToast(`${t('provider_modal.failed')}${getErrorMessage(err)}`, 'error');
    }
  }, [refreshModalData]);

  const closeModal = useCallback(() => {
    setConfirmOpen(false);
    setOpen(false);
  }, []);

  const requestClose = useCallback(() => {
    if (saving) return;
    if (!hasUnsavedChanges) closeModal();
    else setConfirmOpen(true);
  }, [closeModal, hasUnsavedChanges, saving]);

  useEffect(() => {
    const handler = () => { void openModal(); };
    const keyHandler = (event: KeyboardEvent) => {
      if (event.key === 'Escape' && open) requestClose();
    };
    window.addEventListener('open-provider-manager-requested', handler);
    document.addEventListener('keydown', keyHandler);
    return () => {
      window.removeEventListener('open-provider-manager-requested', handler);
      document.removeEventListener('keydown', keyHandler);
    };
  }, [open, openModal, requestClose]);

  useEffect(() => {
    document.body.style.overflow = open ? 'hidden' : '';
    return () => {
      document.body.style.overflow = '';
    };
  }, [open]);

  const loadModelsForProvider = useCallback(async (endpoint: string, forceRefresh = false) => {
    const provider = providers.find((item) => item.endpoint === endpoint);
    if (!provider) return;
    const token = (requestTokens.current[endpoint] || 0) + 1;
    requestTokens.current[endpoint] = token;
    setModelLoading((current) => ({ ...current, [endpoint]: true }));
    try {
      const storedCredential = !!credentialsStatus.providerHasApiKey?.[endpoint];
      const apiKey = editableCredentials.providerSecrets[endpoint] || '';
      let models = modelOptions[endpoint] || [];
      if (forceRefresh || !models.length) {
        if (apiKey || storedCredential) {
          const resp = apiKey ? await getProviderModels(endpoint, apiKey) : await getProviderModels(endpoint);
          models = normalizeRemoteModels(resp.models || []);
        }
        if (!models.length) models = fallbackModelsByEndpoint(endpoint);
      }
      if (requestTokens.current[endpoint] !== token) return;
      setModelOptions((current) => ({ ...current, [endpoint]: models }));
      setProviders((current) => applyLoadedProviderModels(current, endpoint, models));
    } catch {
      if (requestTokens.current[endpoint] !== token) return;
      const fallback = fallbackModelsByEndpoint(endpoint);
      setModelOptions((current) => ({ ...current, [endpoint]: fallback }));
      setProviders((current) => applyLoadedProviderModels(current, endpoint, fallback));
    } finally {
      if (requestTokens.current[endpoint] === token) {
        setModelLoading((current) => ({ ...current, [endpoint]: false }));
      }
    }
  }, [credentialsStatus.providerHasApiKey, editableCredentials.providerSecrets, modelOptions, providers]);

  useEffect(() => {
    if (!open || !activeTab || activeTab === 'tavily') return;
    const provider = providers.find((item) => item.endpoint === activeTab);
    if (provider && !provider.modelLoaded) {
      void loadModelsForProvider(activeTab, false);
    }
  }, [activeTab, loadModelsForProvider, open, providers]);

  const handleProviderSecretChange = (endpoint: string, value: string) => {
    setEditableCredentials((current) => ({
      ...current,
      providerSecrets: { ...current.providerSecrets, [endpoint]: value },
    }));
    setDirtyCredentials((current) => ({
      ...current,
      providerSecrets: { ...current.providerSecrets, [endpoint]: true },
    }));
  };

  const handleSave = async () => {
    setSaving(true);
    setConfirmOpen(false);
    try {
      const settingsPayload = buildProviderSettingsPayload(providers, defaultProviderEndpoint, searchApi);
      await saveSettings(settingsPayload);
      const credentialPayload = buildDirtyCredentialsPayload(
        editableCredentials.providerSecrets,
        editableCredentials.searchApiKey,
        dirtyCredentials,
      );
      const credentialResult = await saveCredentials(credentialPayload);
      setCredentialsStatus(credentialResult.credentialsStatus || credentialsStatus);
      await refreshCredentialsStatus();
      initialSnapshot.current = buildSnapshot();
      showToast(t('provider_modal.saved'), 'success');
      window.dispatchEvent(new CustomEvent('provider-settings-updated', { detail: settingsPayload }));
      closeModal();
    } catch (err) {
      showToast(`${t('provider_modal.failed')}${getErrorMessage(err)}`, 'error');
    } finally {
      setSaving(false);
    }
  };

  const activeProvider = providers.find((provider) => provider.endpoint === activeTab);

  return (
    <>
      <button className="btn btn-provider-manager" type="button" onClick={() => void openModal()}>
        {t('topbar.manage_api')}
      </button>

      <div className={`app-modal ${open ? 'open' : ''}`} aria-hidden={!open}>
        <div className="app-modal-overlay" onClick={requestClose} />
        <section className="app-modal-panel card" role="dialog" aria-modal="true" aria-labelledby="provider-modal-title">
          <div className="app-modal-header">
            <div>
              <h2 id="provider-modal-title" className="card-title">{t('provider_modal.title')}</h2>
              <p className="form-hint modal-subtitle">{t('provider_modal.subtitle')}</p>
            </div>
            <button className="btn btn-ghost" type="button" onClick={requestClose}>x</button>
          </div>

          {loading ? <div className="form-hint">{t('provider_modal.loading')}</div> : (
            <div className="provider-layout">
              <div className="provider-sidebar">
                <div className="provider-group-label">{t('provider_modal.llm_group')}</div>
                {providers.map((provider) => {
                  const configured = !!editableCredentials.providerSecrets[provider.endpoint] || !!credentialsStatus.providerHasApiKey?.[provider.endpoint];
                  return (
                    <button
                      key={provider.endpoint}
                      className={`provider-tab ${activeTab === provider.endpoint ? 'active' : ''}`}
                      type="button"
                      onClick={() => setActiveTab(provider.endpoint)}
                    >
                      <span className="provider-tab-name">{provider.name}</span>
                      {configured ? <span className="provider-status-badge" /> : null}
                    </button>
                  );
                })}
                <div className="provider-group-label">{t('provider_modal.search_group')}</div>
                <button className={`provider-tab ${activeTab === 'tavily' ? 'active' : ''}`} type="button" onClick={() => setActiveTab('tavily')}>
                  <span className="provider-tab-name">{t('provider_modal.search_api_title')}</span>
                  {(editableCredentials.searchApiKey || credentialsStatus.searchApiHasKey) ? <span className="provider-status-badge" /> : null}
                </button>
              </div>

              <div className="provider-content">
                {activeTab === 'tavily' ? (
                  <div className="provider-row provider-search-row">
                    <div className="provider-row-head">
                      <div>
                        <div className="provider-name">{t('provider_modal.search_api_title')}</div>
                        <div className="provider-endpoint mono">Tavily</div>
                      </div>
                    </div>
                    <div className="provider-grid">
                      <div className="form-group">
                        <label className="form-label" htmlFor="provider-tavily-api-key">{t('provider_modal.search_api_key')}</label>
                        <input
                          id="provider-tavily-api-key"
                          type="password"
                          className="form-input"
                          placeholder={credentialsStatus.searchApiHasKey && !editableCredentials.searchApiKey ? t('provider_modal.api_key_saved_placeholder') : 'tvly-xxxxxxxxxxxxxxx'}
                          value={editableCredentials.searchApiKey}
                          onChange={(event) => {
                            setEditableCredentials((current) => ({ ...current, searchApiKey: event.target.value.trim() }));
                            setDirtyCredentials((current) => ({ ...current, searchApiKey: true }));
                          }}
                        />
                      </div>
                    </div>
                  </div>
                ) : activeProvider ? (
                  <div className="provider-row">
                    <div className="provider-row-head">
                      <label className="provider-default-toggle">
                        <input
                          type="radio"
                          name="provider-default"
                          checked={activeProvider.endpoint === defaultProviderEndpoint}
                          onChange={() => setDefaultProviderEndpoint(activeProvider.endpoint)}
                        />
                        <span>{t('provider_modal.default')}</span>
                      </label>
                      <div>
                        <div className="provider-name">{activeProvider.name}</div>
                        <div className="provider-endpoint mono">{activeProvider.endpoint}</div>
                      </div>
                    </div>
                    <div className="provider-grid">
                      <div className="form-group">
                        <label className="form-label" htmlFor="provider-api-key">{t('provider_modal.api_key')}</label>
                        <input
                          id="provider-api-key"
                          type="password"
                          className="form-input provider-api-key"
                          placeholder={credentialsStatus.providerHasApiKey?.[activeProvider.endpoint] && !editableCredentials.providerSecrets[activeProvider.endpoint]
                            ? t('provider_modal.api_key_saved_placeholder')
                            : t('provider_modal.api_key_placeholder')}
                          value={editableCredentials.providerSecrets[activeProvider.endpoint] || ''}
                          onChange={(event) => handleProviderSecretChange(activeProvider.endpoint, event.target.value.trim())}
                          onBlur={() => void loadModelsForProvider(activeProvider.endpoint, true)}
                        />
                      </div>
                      <div className="form-group">
                        <label className="form-label" htmlFor="provider-model">{t('provider_modal.model')}</label>
                        <div className="provider-model-line">
                          <select
                            id="provider-model"
                            className="form-input provider-model"
                            disabled={!!modelLoading[activeProvider.endpoint]}
                            value={activeProvider.model}
                            onChange={(event) => {
                              const model = event.target.value;
                              setProviders((current) => current.map((provider) => provider.endpoint === activeProvider.endpoint ? { ...provider, model } : provider));
                            }}
                          >
                            {mergeProviderModelOptions(
                              activeProvider.endpoint,
                              activeProvider.model,
                              modelOptions[activeProvider.endpoint] || fallbackModelsByEndpoint(activeProvider.endpoint),
                            ).map((model) => (
                              <option key={model.value} value={model.value}>{model.label}</option>
                            ))}
                          </select>
                          <button className="btn btn-ghost provider-refresh-btn" type="button" onClick={() => void loadModelsForProvider(activeProvider.endpoint, true)}>
                            {modelLoading[activeProvider.endpoint] ? t('provider_modal.loading') : t('provider_modal.refresh')}
                          </button>
                        </div>
                      </div>
                    </div>
                  </div>
                ) : null}
              </div>
            </div>
          )}

          <div className="app-modal-actions">
            <button className="btn btn-ghost" type="button" onClick={requestClose}>{t('provider_modal.cancel')}</button>
            <button className="btn btn-primary" type="button" disabled={saving} onClick={() => void handleSave()}>
              {saving ? t('provider_modal.saving') : t('provider_modal.save')}
            </button>
          </div>

          <div className="provider-close-confirm" hidden={!confirmOpen}>
            <div className="provider-close-confirm-backdrop" />
            <section className="provider-close-confirm-card card secret-dialog-panel" role="dialog" aria-modal="true" aria-labelledby="provider-close-confirm-title">
              <h3 id="provider-close-confirm-title" className="card-title">{t('provider_modal.unsaved_title')}</h3>
              <p className="secret-dialog-message">{t('provider_modal.unsaved_message')}</p>
              <div className="app-modal-actions provider-close-confirm-actions">
                <button className="btn btn-ghost" type="button" onClick={() => setConfirmOpen(false)}>{t('provider_modal.unsaved_continue')}</button>
                <button className="btn btn-danger" type="button" onClick={closeModal}>{t('provider_modal.unsaved_discard_close')}</button>
              </div>
            </section>
          </div>
        </section>
      </div>
    </>
  );
}

export const providerPersistenceReport = {
  persisted: [],
  sensitive: ['provider API keys', 'Tavily API key'],
  note: 'Secrets are never stored in localStorage by the frontend. They are read and saved only through the Tauri credential commands.',
};
