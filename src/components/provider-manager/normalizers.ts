import type { ProviderConfig, SearchApiSettings, Settings, ProviderModelOption, ProviderRow } from '../../types';
import {
  buildProviderDisplayName,
  createCustomProviderRow,
  DEFAULT_PROVIDER_ENDPOINT,
  inferProviderTemplate,
  normalizeProviderApiFormat,
  normalizeProviderEndpoint,
  normalizeThinkingLevel,
} from '../../utils/provider-registry';

export function normalizeProviders(settings: Settings | null | undefined): {
  providers: ProviderRow[];
  defaultProviderEndpoint: string;
} {
  const merged: ProviderRow[] = [];
  const byEndpoint = settings?.providerConfigs && typeof settings.providerConfigs === 'object'
    ? settings.providerConfigs
    : {};

  let customCounter = 1;
  for (const [key, rawConfig] of Object.entries(byEndpoint)) {
    const endpoint = String(rawConfig?.endpoint || key || '').trim();
    if (!endpoint) continue;
    const matchedTemplate = inferProviderTemplate(String(rawConfig?.name || ''), endpoint);
    merged.push({
      id: `custom:${endpoint}`,
      name: String(rawConfig?.name || matchedTemplate?.label || buildProviderDisplayName(endpoint) || `Custom Provider ${customCounter++}`),
      endpoint,
      apiKey: '',
      apiFormat: normalizeProviderApiFormat(rawConfig?.apiFormat || matchedTemplate?.defaultApiFormat),
      model: String(rawConfig?.model || ''),
      thinkingEnabled: !!rawConfig?.thinking?.enabled,
      thinkingLevel: normalizeThinkingLevel(rawConfig?.thinking?.level),
      preset: false,
    });
  }

  if (!merged.length) {
    merged.push({
      ...createCustomProviderRow(1),
      name: 'OpenAI',
      endpoint: DEFAULT_PROVIDER_ENDPOINT,
      apiFormat: 'openai',
      preset: false,
    });
  }

  let defaultProviderEndpoint = String(settings?.defaultProviderEndpoint || '').trim();
  if (!merged.some((item) => item.endpoint === defaultProviderEndpoint)) {
    defaultProviderEndpoint = merged[0]?.endpoint || DEFAULT_PROVIDER_ENDPOINT;
  }

  return { providers: merged, defaultProviderEndpoint };
}

export function normalizeSearchApi(settings: Settings | null | undefined): Required<SearchApiSettings> {
  const source = settings?.searchApi && typeof settings.searchApi === 'object'
    ? settings.searchApi
    : {};
  const scopesSource = source.scopes && typeof source.scopes === 'object'
    ? source.scopes
    : {};
  const workflowEnabled = !!(source.enabled || scopesSource.classify || scopesSource.organizer);

  return {
    provider: 'tavily',
    enabled: workflowEnabled,
    scopes: {
      classify: workflowEnabled,
      organizer: workflowEnabled,
    },
  };
}

export function buildProviderSettingsPayload(
  providers: ProviderRow[],
  defaultProviderEndpoint: string,
  searchApi: Required<SearchApiSettings>,
): Pick<Settings, 'providerConfigs' | 'defaultProviderEndpoint' | 'searchApi'> {
  const providerConfigs = Object.fromEntries(
    providers
      .map((provider) => {
        const endpoint = normalizeProviderEndpoint(provider.endpoint, provider.apiFormat);
        if (!endpoint) return null;
        return [endpoint, {
          name: String(provider.name || buildProviderDisplayName(endpoint)),
          endpoint,
          apiFormat: provider.apiFormat,
          model: String(provider.model || ''),
          thinking: {
            enabled: !!provider.thinkingEnabled,
            level: normalizeThinkingLevel(provider.thinkingLevel),
          },
        }];
      })
      .filter(Boolean) as Array<[string, ProviderConfig]>,
  ) as Record<string, ProviderConfig>;
  const knownEndpoints = Object.keys(providerConfigs);
  const fallbackDefault = knownEndpoints[0] || DEFAULT_PROVIDER_ENDPOINT;
  const normalizedDefaultCandidate = providers
    .find((provider) => provider.endpoint === defaultProviderEndpoint)
    ?.endpoint || defaultProviderEndpoint;
  const normalizedDefault = knownEndpoints.includes(normalizeProviderEndpoint(
    normalizedDefaultCandidate,
    providers.find((provider) => provider.endpoint === defaultProviderEndpoint)?.apiFormat || 'openai',
  ))
    ? normalizeProviderEndpoint(
        normalizedDefaultCandidate,
        providers.find((provider) => provider.endpoint === defaultProviderEndpoint)?.apiFormat || 'openai',
      )
    : fallbackDefault;

  return {
    providerConfigs,
    defaultProviderEndpoint: normalizedDefault,
    searchApi: {
      provider: 'tavily',
      enabled: !!(searchApi.enabled || searchApi.scopes.classify || searchApi.scopes.organizer),
      scopes: {
        classify: !!(searchApi.scopes.classify || searchApi.scopes.organizer),
        organizer: !!(searchApi.scopes.organizer || searchApi.scopes.classify),
      },
    },
  };
}

export function buildDirtyCredentialsPayload(
  providerSecrets: Record<string, string>,
  searchApiKey: string,
  dirty: { providerSecrets: Record<string, boolean>; searchApiKey: boolean },
): { providerSecrets?: Record<string, string>; searchApiKey?: string } {
  const dirtySecrets = Object.fromEntries(
    Object.entries(providerSecrets).filter(([endpoint]) => dirty.providerSecrets[endpoint]),
  );
  return {
    ...(Object.keys(dirtySecrets).length ? { providerSecrets: dirtySecrets } : {}),
    ...(dirty.searchApiKey ? { searchApiKey } : {}),
  };
}

export function mergeProviderModelOptions(
  currentModel: string,
  models: ProviderModelOption[],
): ProviderModelOption[] {
  const selectedModel = String(currentModel || '').trim();
  const options = models.length ? [...models] : [];
  if (selectedModel && !options.some((model) => model.value === selectedModel)) {
    options.unshift({ value: selectedModel, label: selectedModel });
  }
  return options;
}

export function applyLoadedProviderModels(
  providers: ProviderRow[],
  endpoint: string,
  models: ProviderModelOption[],
): ProviderRow[] {
  const firstModel = models[0]?.value || '';
  return providers.map((row) => row.endpoint === endpoint
    ? {
        ...row,
        model: row.model || firstModel,
        modelLoaded: true,
      }
    : row);
}
