import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import type { CredentialsStatus, ProviderModelOption, ProviderRow, SearchApiSettings } from '../../types';
import { getCredentials, getProviderModels, getSettings, logFrontendEvent, saveCredentials, saveSettings } from '../../utils/api';
import { getErrorMessage } from '../../utils/errors';
import { t, text } from '../../utils/i18n';
import {
  buildProviderDisplayName,
  createCustomProviderRow,
  findProviderTemplateByName,
  getProviderTemplateEndpoint,
  getProviderTemplateFormats,
  normalizeProviderEndpoint,
  normalizeRemoteModels,
  PROVIDER_NAME_OPTIONS,
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
  const [modelErrors, setModelErrors] = useState<Record<string, string>>({});
  const initialSnapshot = useRef('');
  const requestTokens = useRef<Record<string, number>>({});
  const customCounter = useRef(1);

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
      const defaultProvider = normalized.providers.find((provider) => provider.endpoint === normalized.defaultProviderEndpoint);
      setActiveTab(defaultProvider?.id || defaultProvider?.endpoint || normalized.providers[0]?.id || normalized.providers[0]?.endpoint || 'tavily');
      setSearchApi(nextSearchApi);
      setCredentialsStatus(settings.credentialsStatus || {});
      setEditableCredentials({
        providerSecrets: { ...(credentials.providerSecrets || {}) },
        searchApiKey: String(credentials.searchApiKey || ''),
      });
      setDirtyCredentials(emptyDirtyState());
      setModelOptions({});
      setModelErrors({});
      customCounter.current = normalized.providers.filter((provider) => !provider.preset).length + 1;
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
      logFrontendEvent({ event: 'provider_modal_opened' }).catch(() => {});
    } catch (err) {
      logFrontendEvent({ event: 'provider_modal_open_failed', details: { error: getErrorMessage(err) } }).catch(() => {});
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

  const updateProvider = useCallback((providerId: string, updater: (provider: ProviderRow) => ProviderRow) => {
    setProviders((current) => current.map((provider) => (provider.id || provider.endpoint) === providerId ? updater(provider) : provider));
  }, []);

  const remapProviderScopedState = useCallback((fromEndpoint: string, toEndpoint: string) => {
    if (!fromEndpoint || !toEndpoint || fromEndpoint === toEndpoint) return;
    setEditableCredentials((current) => {
      if (!(fromEndpoint in current.providerSecrets) || (toEndpoint in current.providerSecrets)) return current;
      const nextSecrets = { ...current.providerSecrets, [toEndpoint]: current.providerSecrets[fromEndpoint] };
      delete nextSecrets[fromEndpoint];
      return { ...current, providerSecrets: nextSecrets };
    });
    setDirtyCredentials((current) => {
      if (!(fromEndpoint in current.providerSecrets) || (toEndpoint in current.providerSecrets)) return current;
      const nextDirty = { ...current.providerSecrets, [toEndpoint]: current.providerSecrets[fromEndpoint] };
      delete nextDirty[fromEndpoint];
      return { ...current, providerSecrets: nextDirty };
    });
    setModelOptions((current) => {
      if (!(fromEndpoint in current) || (toEndpoint in current)) return current;
      const next = { ...current, [toEndpoint]: current[fromEndpoint] };
      delete next[fromEndpoint];
      return next;
    });
    setModelLoading((current) => {
      if (!(fromEndpoint in current) || (toEndpoint in current)) return current;
      const next = { ...current, [toEndpoint]: current[fromEndpoint] };
      delete next[fromEndpoint];
      return next;
    });
    setModelErrors((current) => {
      if (!(fromEndpoint in current) || (toEndpoint in current)) return current;
      const next = { ...current, [toEndpoint]: current[fromEndpoint] };
      delete next[fromEndpoint];
      return next;
    });
    if (requestTokens.current[fromEndpoint] != null && requestTokens.current[toEndpoint] == null) {
      requestTokens.current[toEndpoint] = requestTokens.current[fromEndpoint];
      delete requestTokens.current[fromEndpoint];
    }
    if (defaultProviderEndpoint === fromEndpoint) {
      setDefaultProviderEndpoint(toEndpoint);
    }
  }, [defaultProviderEndpoint]);

  const syncProviderTemplate = useCallback((
    providerId: string,
    nextName: string,
    nextApiFormat?: 'openai' | 'anthropic',
  ) => {
    let remapFrom = '';
    let remapTo = '';
    updateProvider(providerId, (provider) => {
      const template = findProviderTemplateByName(nextName);
      const fallbackFormat = nextApiFormat || provider.apiFormat;
      const supportedFormats = template ? getProviderTemplateFormats(template) : [];
      const resolvedFormat = template
        ? (supportedFormats.includes(fallbackFormat) ? fallbackFormat : template.defaultApiFormat)
        : fallbackFormat;
      const currentEndpoint = String(provider.endpoint || '').trim();
      const templateEndpoint = template ? getProviderTemplateEndpoint(template, resolvedFormat) : '';
      const nextEndpoint = templateEndpoint || currentEndpoint;
      if (currentEndpoint && nextEndpoint && currentEndpoint !== nextEndpoint) {
        remapFrom = currentEndpoint;
        remapTo = nextEndpoint;
      }
      return {
        ...provider,
        name: nextName,
        apiFormat: resolvedFormat,
        endpoint: nextEndpoint,
        modelLoaded: provider.modelLoaded && currentEndpoint === nextEndpoint,
      };
    });
    if (remapFrom && remapTo) {
      remapProviderScopedState(remapFrom, remapTo);
    }
  }, [remapProviderScopedState, updateProvider]);

  const loadModelsForProvider = useCallback(async (endpoint: string, forceRefresh = false) => {
    const provider = providers.find((item) => item.endpoint === endpoint);
    if (!provider || !provider.endpoint.trim()) return;
    const token = (requestTokens.current[endpoint] || 0) + 1;
    requestTokens.current[endpoint] = token;
    setModelLoading((current) => ({ ...current, [endpoint]: true }));
    setModelErrors((current) => ({ ...current, [endpoint]: '' }));
    try {
      const storedCredential = !!credentialsStatus.providerHasApiKey?.[endpoint];
      const apiKey = editableCredentials.providerSecrets[endpoint] || '';
      let models = modelOptions[endpoint] || [];
      if (forceRefresh || !models.length) {
        if (apiKey || storedCredential) {
          const resp = apiKey
            ? await getProviderModels(provider.endpoint, provider.apiFormat, apiKey)
            : await getProviderModels(provider.endpoint, provider.apiFormat);
          models = normalizeRemoteModels(resp.models || []);
        }
      }
      if (requestTokens.current[endpoint] !== token) return;
      setModelOptions((current) => ({ ...current, [endpoint]: models }));
      setProviders((current) => applyLoadedProviderModels(current, endpoint, models));
    } catch (err) {
      if (requestTokens.current[endpoint] !== token) return;
      logFrontendEvent({ event: 'provider_models_load_failed', details: { endpoint, error: getErrorMessage(err) } }).catch(() => {});
      setModelErrors((current) => ({ ...current, [endpoint]: getErrorMessage(err) }));
      setProviders((current) => current.map((row) => row.endpoint === endpoint ? { ...row, modelLoaded: true } : row));
    } finally {
      if (requestTokens.current[endpoint] === token) {
        setModelLoading((current) => ({ ...current, [endpoint]: false }));
      }
    }
  }, [credentialsStatus.providerHasApiKey, editableCredentials.providerSecrets, modelOptions, providers]);

  useEffect(() => {
    if (!open || !activeTab || activeTab === 'tavily') return;
    const provider = providers.find((item) => (item.id || item.endpoint) === activeTab);
    if (provider && !provider.modelLoaded) {
      const hasCredential = !!editableCredentials.providerSecrets[provider.endpoint] || !!credentialsStatus.providerHasApiKey?.[provider.endpoint];
      if (hasCredential) void loadModelsForProvider(activeTab, false);
      else setProviders((current) => current.map((row) => row.endpoint === activeTab ? { ...row, modelLoaded: true } : row));
    }
  }, [activeTab, credentialsStatus.providerHasApiKey, editableCredentials.providerSecrets, loadModelsForProvider, open, providers]);

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

  const handleAddCustomProvider = () => {
    const next = createCustomProviderRow(customCounter.current++);
    setProviders((current) => [...current, next]);
    setActiveTab(next.id || next.endpoint);
  };

  const handleDeleteProvider = (provider: ProviderRow) => {
    if (provider.preset) return;
    logFrontendEvent({ event: 'provider_deleted', details: { endpoint: provider.endpoint, name: provider.name } }).catch(() => {});
    setProviders((current) => current.filter((item) => (item.id || item.endpoint) !== (provider.id || provider.endpoint)));
    if (defaultProviderEndpoint === provider.endpoint) {
      const fallback = providers.find((item) => (item.id || item.endpoint) !== (provider.id || provider.endpoint))?.endpoint || '';
      setDefaultProviderEndpoint(fallback);
    }
    if (activeTab === (provider.id || provider.endpoint)) {
      const fallback = providers.find((item) => (item.id || item.endpoint) !== (provider.id || provider.endpoint));
      setActiveTab(fallback?.id || fallback?.endpoint || 'tavily');
    }
  };

  const validateProviders = (): string | null => {
    for (const provider of providers) {
      const name = String(provider.name || '').trim();
      if (!name) return t('provider_modal.validation_name_required');
      const normalizedEndpoint = normalizeProviderEndpoint(provider.endpoint, provider.apiFormat);
      if (!normalizedEndpoint) return t('provider_modal.validation_endpoint_required');
      if (!/^https?:\/\//i.test(normalizedEndpoint)) return t('provider_modal.validation_endpoint_invalid');
    }
    return null;
  };

  const handleSave = async () => {
    const validationError = validateProviders();
    if (validationError) {
      showToast(validationError, 'error');
      return;
    }
    setSaving(true);
    setConfirmOpen(false);
    try {
      const normalizedProviders = providers.map((provider) => {
        const endpoint = normalizeProviderEndpoint(provider.endpoint, provider.apiFormat);
        return {
          ...provider,
          endpoint,
          name: String(provider.name || buildProviderDisplayName(endpoint)),
        };
      });
      const settingsPayload = buildProviderSettingsPayload(normalizedProviders, defaultProviderEndpoint, searchApi);
      await saveSettings(settingsPayload);
      const credentialPayload = buildDirtyCredentialsPayload(
        editableCredentials.providerSecrets,
        editableCredentials.searchApiKey,
        dirtyCredentials,
      );
      const credentialResult = await saveCredentials(credentialPayload);
      setCredentialsStatus(credentialResult.credentialsStatus || credentialsStatus);
      await refreshCredentialsStatus();
      initialSnapshot.current = JSON.stringify({
        settings: settingsPayload,
        credentials: editableCredentials,
      });
      showToast(t('provider_modal.saved'), 'success');
      logFrontendEvent({ event: 'provider_settings_saved', details: { providerCount: normalizedProviders.length } }).catch(() => {});
      window.dispatchEvent(new CustomEvent('provider-settings-updated', { detail: settingsPayload }));
      closeModal();
    } catch (err) {
      logFrontendEvent({ event: 'provider_settings_save_failed', details: { error: getErrorMessage(err) } }).catch(() => {});
      showToast(`${t('provider_modal.failed')}${getErrorMessage(err)}`, 'error');
    } finally {
      setSaving(false);
    }
  };

  const activeProvider = providers.find((provider) => (provider.id || provider.endpoint) === activeTab);
  const activeModelOptions = activeProvider
    ? mergeProviderModelOptions(activeProvider.model, modelOptions[activeProvider.endpoint] || [])
    : [];
  const configuredProviderCount = providers.filter((provider) => (
    !!editableCredentials.providerSecrets[provider.endpoint] || !!credentialsStatus.providerHasApiKey?.[provider.endpoint]
  )).length;
  const searchConfigured = !!editableCredentials.searchApiKey || !!credentialsStatus.searchApiHasKey;
  const selectedProviderLabel = activeTab === 'tavily'
    ? t('provider_modal.search_api_title')
    : (activeProvider?.name || t('provider_modal.custom_provider'));
  const activeProviderConfigured = !!activeProvider && (
    !!editableCredentials.providerSecrets[activeProvider.endpoint] || !!credentialsStatus.providerHasApiKey?.[activeProvider.endpoint]
  );

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
            <button
              className="btn btn-ghost provider-close-btn"
              type="button"
              aria-label={text('关闭', 'Close')}
              onClick={requestClose}
            >
              {text('关闭', 'Close')}
            </button>
          </div>

          {loading ? <div className="provider-loading-state form-hint">{t('provider_modal.loading')}</div> : (
            <div className="provider-layout">
              <div className="provider-sidebar">
                <div className="provider-sidebar-summary">
                  <div className="provider-sidebar-kicker">{text('概览', 'Overview')}</div>
                  <div className="provider-sidebar-title">
                    {configuredProviderCount}/{providers.length} {text('个大模型已配置', 'LLM providers configured')}
                  </div>
                  <p className="provider-sidebar-note">
                    {text('左侧只展示你实际创建的 Provider，右侧可从预设模型商快速带出名称、协议和地址。', 'The left side only shows providers you actually created, while the right side can quickly fill vendor name, protocol, and base URL from presets.')}
                  </p>
                  <div className="provider-sidebar-stats">
                    <span className={`provider-pill ${searchConfigured ? 'configured' : ''}`}>
                      {searchConfigured ? text('联网已配置', 'Search ready') : text('联网未配置', 'Search missing')}
                    </span>
                    {hasUnsavedChanges ? (
                      <span className="provider-pill warning">{text('有未保存修改', 'Unsaved changes')}</span>
                    ) : null}
                  </div>
                </div>
                <div className="provider-group-label">{t('provider_modal.llm_group')}</div>
                {providers.map((provider) => {
                  const configured = !!editableCredentials.providerSecrets[provider.endpoint] || !!credentialsStatus.providerHasApiKey?.[provider.endpoint];
                  return (
                    <button
                      key={provider.id || `${provider.preset ? 'preset' : 'custom'}:${provider.endpoint || provider.name}`}
                      className={`provider-tab ${activeTab === (provider.id || provider.endpoint) ? 'active' : ''}`}
                      type="button"
                      onClick={() => setActiveTab(provider.id || provider.endpoint)}
                    >
                      <span className="provider-tab-copy">
                        <span className="provider-tab-name">{provider.name || t('provider_modal.custom_provider')}</span>
                        <span className="provider-tab-subtitle">
                          {findProviderTemplateByName(provider.name)
                            ? text('预设模型商模板', 'Preset vendor template')
                            : text('自定义 Provider', 'Custom provider')}
                        </span>
                      </span>
                      <span className="provider-tab-tail">
                        {provider.endpoint === defaultProviderEndpoint ? (
                          <span className="provider-tab-chip default">{text('默认', 'Default')}</span>
                        ) : null}
                        <span className={`provider-status-dot ${configured ? 'configured' : ''}`} aria-hidden="true" />
                      </span>
                    </button>
                  );
                })}
                <button className="btn btn-ghost provider-add-btn" type="button" onClick={handleAddCustomProvider}>
                  {t('provider_modal.add_custom')}
                </button>
                <div className="provider-group-label">{t('provider_modal.search_group')}</div>
                <button className={`provider-tab ${activeTab === 'tavily' ? 'active' : ''}`} type="button" onClick={() => setActiveTab('tavily')}>
                  <span className="provider-tab-copy">
                    <span className="provider-tab-name">{t('provider_modal.search_api_title')}</span>
                    <span className="provider-tab-subtitle">Tavily</span>
                  </span>
                  <span className="provider-tab-tail">
                    <span className={`provider-status-dot ${searchConfigured ? 'configured' : ''}`} aria-hidden="true" />
                  </span>
                </button>
              </div>

              <div className="provider-content">
                <div className="provider-overview-card">
                  <div className="provider-overview-main">
                    <div className="provider-overview-kicker">{text('当前选择', 'Current selection')}</div>
                    <div className="provider-overview-title">{selectedProviderLabel}</div>
                    <div className="provider-overview-subtitle mono">
                      {activeTab === 'tavily' ? 'Tavily' : (activeProvider?.endpoint || text('新建 Provider，先选择模型商或填写地址', 'New provider, start by choosing a vendor or entering a base URL'))}
                    </div>
                  </div>
                  <div className="provider-overview-badges">
                    {activeTab === 'tavily' ? (
                      <>
                        <span className={`provider-pill ${searchConfigured ? 'configured' : ''}`}>
                          {searchConfigured ? text('已保存凭据', 'Credential saved') : text('需要密钥', 'Key required')}
                        </span>
                        <span className="provider-pill neutral">{text('联网检索', 'Web search')}</span>
                      </>
                    ) : activeProvider ? (
                      <>
                        {activeProvider.endpoint === defaultProviderEndpoint ? (
                          <span className="provider-pill info">{text('默认路由', 'Default route')}</span>
                        ) : null}
                        <span className={`provider-pill ${activeProviderConfigured ? 'configured' : ''}`}>
                          {activeProviderConfigured ? text('已保存凭据', 'Credential saved') : text('需要密钥', 'Key required')}
                        </span>
                        <span className="provider-pill neutral">
                          {findProviderTemplateByName(activeProvider.name) ? text('模板映射', 'Template matched') : text('自定义', 'Custom')}
                        </span>
                        <span className="provider-pill neutral">
                          {activeProvider.apiFormat === 'anthropic' ? 'Anthropic' : 'OpenAI'}
                        </span>
                      </>
                    ) : null}
                  </div>
                </div>

                {activeTab === 'tavily' ? (
                  <div className="provider-form-stack">
                    <section className="provider-section-card provider-search-row">
                      <div className="provider-section-head">
                        <div>
                          <div className="provider-section-kicker">{text('联网能力', 'Search capability')}</div>
                          <div className="provider-section-title">{t('provider_modal.search_api_title')}</div>
                        </div>
                      </div>
                      <p className="provider-section-description">
                        {text('用于需要联网检索的流程。密钥保存在系统凭据中，不会写入前端本地存储。', 'Used by workflows that need web search. The key stays in system credentials and is never written to frontend local storage.')}
                      </p>
                      <div className="provider-grid">
                        <div className="form-group provider-grid-span">
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
                          <div className="form-hint">
                            {text('如果已经保存过密钥，保持为空即可沿用现有值。', 'If a key is already stored, leaving this blank keeps the existing value.')}
                          </div>
                        </div>
                      </div>
                    </section>
                  </div>
                ) : activeProvider ? (
                  <div className="provider-form-stack">
                    <section className="provider-section-card">
                      <div className="provider-row-head provider-row-head-compact">
                        <div>
                          <div className="provider-section-kicker">{text('路由与身份', 'Routing and identity')}</div>
                          <div className="provider-section-title">{text('基础信息', 'Basic details')}</div>
                        </div>
                        <div className="provider-inline-actions">
                          <label className="provider-default-toggle">
                            <input
                              type="radio"
                              name="provider-default"
                              checked={activeProvider.endpoint === defaultProviderEndpoint}
                              onChange={() => setDefaultProviderEndpoint(activeProvider.endpoint)}
                            />
                            <span>{t('provider_modal.default')}</span>
                          </label>
                          {!activeProvider.preset ? (
                            <button className="btn btn-danger btn-sm" type="button" onClick={() => handleDeleteProvider(activeProvider)}>
                              {t('provider_modal.delete')}
                            </button>
                          ) : null}
                        </div>
                      </div>
                      <p className="provider-section-description">
                        {text('显示名称用于界面识别，Base URL 和协议格式决定模型请求的目标接口。', 'The display name helps with identification, while Base URL and protocol format define the target API shape.')}
                      </p>
                      <div className="provider-grid">
                        <div className="form-group">
                          <label className="form-label" htmlFor="provider-name">{t('provider_modal.provider_name')}</label>
                          <input
                            id="provider-name"
                            type="text"
                            className="form-input"
                            list="provider-name-options"
                            value={activeProvider.name}
                            onChange={(event) => syncProviderTemplate(
                              activeProvider.id || activeProvider.endpoint,
                              event.target.value,
                            )}
                          />
                          <datalist id="provider-name-options">
                            {PROVIDER_NAME_OPTIONS.map((name) => (
                              <option key={name} value={name} />
                            ))}
                          </datalist>
                          <div className="form-hint">{t('provider_modal.provider_name_hint')}</div>
                        </div>
                        <div className="form-group">
                          <label className="form-label" htmlFor="provider-api-format">{t('provider_modal.api_format')}</label>
                          <select
                            id="provider-api-format"
                            className="form-input"
                            value={activeProvider.apiFormat}
                            onChange={(event) => {
                              const apiFormat = event.target.value as 'openai' | 'anthropic';
                              const template = findProviderTemplateByName(activeProvider.name);
                              if (template) {
                                syncProviderTemplate(activeProvider.id || activeProvider.endpoint, activeProvider.name, apiFormat);
                                return;
                              }
                              const currentEndpoint = activeProvider.endpoint;
                              const normalizedEndpoint = normalizeProviderEndpoint(currentEndpoint, apiFormat);
                              if (currentEndpoint && normalizedEndpoint && currentEndpoint !== normalizedEndpoint) {
                                remapProviderScopedState(currentEndpoint, normalizedEndpoint);
                              }
                              updateProvider(activeProvider.id || activeProvider.endpoint, (provider) => ({
                                ...provider,
                                apiFormat,
                                endpoint: normalizedEndpoint,
                                modelLoaded: false,
                              }));
                            }}
                          >
                            <option value="openai">OpenAI</option>
                            <option value="anthropic">Anthropic</option>
                          </select>
                        </div>
                        <div className="form-group provider-grid-span">
                          <label className="form-label" htmlFor="provider-endpoint">{t('provider_modal.endpoint')}</label>
                          <input
                            id="provider-endpoint"
                            type="text"
                            className="form-input provider-api-key"
                            value={activeProvider.endpoint}
                            placeholder="https://api.example.com/v1"
                            onChange={(event) => updateProvider(activeProvider.id || activeProvider.endpoint, (provider) => ({
                              ...provider,
                              endpoint: event.target.value.trim(),
                              modelLoaded: false,
                            }))}
                            onBlur={(event) => {
                              const normalized = normalizeProviderEndpoint(event.target.value, activeProvider.apiFormat);
                              if (activeProvider.endpoint && normalized && activeProvider.endpoint !== normalized) {
                                remapProviderScopedState(activeProvider.endpoint, normalized);
                              }
                              updateProvider(activeProvider.id || activeProvider.endpoint, (provider) => ({
                                ...provider,
                                endpoint: normalized,
                                name: String(provider.name || buildProviderDisplayName(normalized)),
                              }));
                            }}
                          />
                          <div className="form-hint">
                            {findProviderTemplateByName(activeProvider.name)
                              ? t('provider_modal.endpoint_hint_template')
                              : t('provider_modal.endpoint_hint_custom')}
                          </div>
                        </div>
                      </div>
                    </section>

                    <section className="provider-section-card">
                      <div className="provider-section-head">
                        <div>
                          <div className="provider-section-kicker">{text('凭据与模型', 'Credentials and model')}</div>
                          <div className="provider-section-title">{text('连接测试前置项', 'Connection essentials')}</div>
                        </div>
                      </div>
                      <p className="provider-section-description">
                        {text('先填 API Key，再刷新模型列表；如果接口不支持列出模型，也可以直接手动输入模型名。', 'Enter the API key first, then refresh the model list. If the endpoint cannot enumerate models, you can still type one manually.')}
                      </p>
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
                          <div className="form-hint">
                            {text('密钥保存在 Windows Credential Manager。若已存过，留空即可保持不变。', 'Keys are stored in Windows Credential Manager. If one is already saved, leave this blank to keep it.')}
                          </div>
                        </div>
                        <div className="form-group">
                          <label className="form-label" htmlFor="provider-model">{t('provider_modal.model')}</label>
                          <div className="provider-model-line">
                            <input
                              id="provider-model"
                              className="form-input provider-model"
                              list={`provider-model-options-${activeProvider.endpoint}`}
                              value={activeProvider.model}
                              placeholder={t('provider_modal.model_placeholder')}
                              onChange={(event) => updateProvider(activeProvider.id || activeProvider.endpoint, (provider) => ({ ...provider, model: event.target.value.trimStart() }))}
                            />
                            <datalist id={`provider-model-options-${activeProvider.endpoint}`}>
                              {activeModelOptions.map((model) => (
                                <option key={model.value} value={model.value}>{model.label}</option>
                              ))}
                            </datalist>
                            <button
                              className="btn btn-ghost provider-refresh-btn"
                              type="button"
                              disabled={!!modelLoading[activeProvider.endpoint]}
                              onClick={() => void loadModelsForProvider(activeProvider.endpoint, true)}
                            >
                              {modelLoading[activeProvider.endpoint] ? t('provider_modal.loading') : t('provider_modal.refresh')}
                            </button>
                          </div>
                          {modelErrors[activeProvider.endpoint] ? (
                            <div className="form-hint provider-error-text">{modelErrors[activeProvider.endpoint]}</div>
                          ) : (
                            <div className="form-hint">{t('provider_modal.model_hint')}</div>
                          )}
                        </div>
                      </div>
                    </section>

                  </div>
                ) : null}
              </div>
            </div>
          )}

          <div className="app-modal-actions">
            <button className="btn btn-ghost" type="button" onClick={requestClose}>{t('provider_modal.cancel')}</button>
            <button className="btn btn-primary" type="button" disabled={saving || !hasUnsavedChanges} onClick={() => void handleSave()}>
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
