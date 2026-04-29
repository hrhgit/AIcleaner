import type { SearchApiSettings, Settings, ProviderModelOption, ProviderRow } from '../../types';
import {
  DEFAULT_PROVIDER_ENDPOINT,
  defaultModelByEndpoint,
  fallbackModelsByEndpoint,
  PROVIDER_OPTIONS,
} from '../../utils/provider-registry';

export function normalizeProviders(settings: Settings | null | undefined): {
  providers: ProviderRow[];
  defaultProviderEndpoint: string;
} {
  const merged: ProviderRow[] = [];
  const byEndpoint = settings?.providerConfigs && typeof settings.providerConfigs === 'object'
    ? settings.providerConfigs
    : {};

  const presetSet = new Set<string>();
  for (const preset of PROVIDER_OPTIONS) {
    presetSet.add(preset.value);
    const config = byEndpoint[preset.value] || {};
    merged.push({
      name: String(config.name || preset.label),
      endpoint: preset.value,
      apiKey: '',
      model: String(config.model || defaultModelByEndpoint(preset.value)),
    });
  }

  for (const [key, rawConfig] of Object.entries(byEndpoint)) {
    const endpoint = String(rawConfig?.endpoint || key || '').trim();
    if (!endpoint || presetSet.has(endpoint)) continue;
    merged.push({
      name: String(rawConfig?.name || endpoint),
      endpoint,
      apiKey: '',
      model: String(rawConfig?.model || defaultModelByEndpoint(endpoint)),
    });
  }

  if (!merged.length) {
    merged.push({
      name: 'OpenAI',
      endpoint: DEFAULT_PROVIDER_ENDPOINT,
      apiKey: '',
      model: defaultModelByEndpoint(DEFAULT_PROVIDER_ENDPOINT),
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
    providers.map((provider) => [provider.endpoint, {
      name: String(provider.name || provider.endpoint),
      endpoint: provider.endpoint,
      model: String(provider.model || defaultModelByEndpoint(provider.endpoint)),
    }]),
  );
  const fallbackDefault = providers[0]?.endpoint || DEFAULT_PROVIDER_ENDPOINT;
  const normalizedDefault = providers.some((provider) => provider.endpoint === defaultProviderEndpoint)
    ? defaultProviderEndpoint
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
  endpoint: string,
  currentModel: string,
  models: ProviderModelOption[],
): ProviderModelOption[] {
  const fallbackModel = defaultModelByEndpoint(endpoint);
  const baseModels = models.length
    ? models
    : fallbackModelsByEndpoint(endpoint);
  const options = baseModels.length
    ? [...baseModels]
    : [{ value: fallbackModel, label: fallbackModel }];
  const selectedModel = String(currentModel || '').trim();

  if (selectedModel && !options.some((model) => model.value === selectedModel)) {
    options.push({ value: selectedModel, label: selectedModel });
  }

  return options;
}

export function resolveProviderModelValue(
  endpoint: string,
  currentModel: string,
  models: ProviderModelOption[],
): string {
  const selectedModel = String(currentModel || '').trim();
  if (selectedModel) return selectedModel;
  return mergeProviderModelOptions(endpoint, '', models)[0]?.value || defaultModelByEndpoint(endpoint);
}

export function applyLoadedProviderModels(
  providers: ProviderRow[],
  endpoint: string,
  models: ProviderModelOption[],
): ProviderRow[] {
  return providers.map((row) => row.endpoint === endpoint
    ? {
        ...row,
        model: resolveProviderModelValue(endpoint, row.model, models),
        modelLoaded: true,
      }
    : row);
}
