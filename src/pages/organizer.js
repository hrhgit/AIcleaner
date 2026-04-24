import {
  applyOrganize,
  browseFolder,
  connectOrganizeStream,
  getProviderModels,
  getOrganizeCapability,
  getOrganizeResult,
  getSettings,
  saveSettings,
  rollbackOrganize,
  startOrganize,
  stopOrganize,
} from '../utils/api.js';
import { getLang, t } from '../utils/i18n.js';
import { escapeHtml } from '../utils/html.js';
import {
  ensureRequiredCredentialsConfigured,
  getProviderCredentialPresence,
  getSearchCredentialPresence,
} from '../utils/secret-ui.js';
import {
  DEFAULT_PROVIDER_ENDPOINT,
  defaultModelByEndpoint,
  ensureProviderOptionExists,
  normalizeRemoteModels,
  populateProviderOptions,
  PROVIDER_MODELS,
  PROVIDER_OPTIONS,
} from '../utils/provider-registry.js';
import { showToast } from '../utils/toast.js';
import {
  cleanupLegacyPersistedState,
  DEFAULT_BATCH_SIZE,
  DEFAULT_EXCLUSIONS,
  DEFAULT_SUMMARY_MODE,
  getPersisted,
  getPersistedApplyManifest,
  invalidateOrganizerRuntimeCacheIfNeeded,
  PERSIST_KEYS,
  persistForm,
  removePersisted,
  restoreDefaults,
  setPersisted,
  setPersistedApplyManifest,
  SUMMARY_MODES,
} from './organizer-storage.js';
import {
  COPY_TEXT_ROUTE_BTN_ID,
  MODEL_SELECT_IDS,
  PROVIDER_SELECT_IDS,
} from './organizer/constants.js';
import { getMoveResultText } from './organizer/move-result.js';
import {
  renderOrganizerMoveResultPanel,
  renderOrganizerPreviewPanel,
  renderOrganizerStatsGrid,
  renderPipelineStage,
  renderRoutingCard,
  stripDecorativePrefix,
} from './organizer/templates.js';
import { createOrganizerLogController } from './organizer/log.js';

let activeTaskId = null;
let activeEventSource = null;
let latestSnapshot = null;
let latestCapability = null;
let providerApiKeyMap = {};
const remoteModelsCache = new Map();
const modelsRequestToken = { text: 0, image: 0, video: 0, audio: 0 };
let organizerProviderSettingsUpdatedHandler = null;
let renderVersion = 0;
const organizerLog = createOrganizerLogController({
  getActiveTaskId: () => activeTaskId,
  getLatestSnapshot: () => latestSnapshot,
});

function getErrorMessage(err) {
  if (typeof err === 'string' && err.trim()) {
    return err.trim();
  }
  if (err && typeof err === 'object') {
    if (typeof err.message === 'string' && err.message.trim()) {
      return err.message.trim();
    }
    if (typeof err.error === 'string' && err.error.trim()) {
      return err.error.trim();
    }
  }
  const text = String(err ?? '').trim();
  return text && text !== '[object Object]' ? text : 'unknown error';
}

function parseListInput(text) {
  return String(text || '')
    .split(/[\n,]/)
    .map((x) => x.trim())
    .filter(Boolean)
    .filter((x, idx, arr) => arr.indexOf(x) === idx);
}

function getApiKeyForEndpoint(endpoint) {
  return !!providerApiKeyMap?.[String(endpoint || '').trim()];
}

function syncProviderApiKeys(settings = {}) {
  providerApiKeyMap = {};
  if (settings?.providerConfigs && typeof settings.providerConfigs === 'object') {
    for (const [endpoint] of Object.entries(settings.providerConfigs)) {
      providerApiKeyMap[String(endpoint).trim()] = getProviderCredentialPresence(settings, endpoint);
    }
  }
}

function refreshOrganizerApiConfigHint() {
  const hintEl = document.getElementById('org-api-config-hint');
  if (!hintEl) return;
  const selectedEndpoints = Object.values(PROVIDER_SELECT_IDS)
    .map((id) => String(document.getElementById(id)?.value || '').trim())
    .filter(Boolean);
  const uniqueEndpoints = Array.from(new Set(selectedEndpoints));
  const hasMissingConfig = uniqueEndpoints.some((endpoint) => !getApiKeyForEndpoint(endpoint));
  hintEl.hidden = !hasMissingConfig;
  hintEl.style.display = hasMissingConfig ? 'flex' : 'none';
}

function readModelRoutingFromDOM() {
  const textEndpoint = document.getElementById(PROVIDER_SELECT_IDS.text)?.value?.trim() || '';
  const imageEndpoint = document.getElementById(PROVIDER_SELECT_IDS.image)?.value?.trim() || '';
  const videoEndpoint = document.getElementById(PROVIDER_SELECT_IDS.video)?.value?.trim() || '';
  const audioEndpoint = document.getElementById(PROVIDER_SELECT_IDS.audio)?.value?.trim() || '';

  return {
    text: {
      endpoint: textEndpoint,
      model: document.getElementById(MODEL_SELECT_IDS.text)?.value?.trim() || '',
    },
    image: {
      endpoint: imageEndpoint,
      model: document.getElementById(MODEL_SELECT_IDS.image)?.value?.trim() || '',
    },
    video: {
      endpoint: videoEndpoint,
      model: document.getElementById(MODEL_SELECT_IDS.video)?.value?.trim() || '',
    },
    audio: {
      endpoint: audioEndpoint,
      model: document.getElementById(MODEL_SELECT_IDS.audio)?.value?.trim() || '',
    },
  };
}

function collectForm() {
  const rootPath = document.getElementById('org-root-path')?.value?.trim() || '';
  const recursive = true;
  const excludedPatterns = parseListInput(document.getElementById('org-exclusions')?.value || '');
  const batchSizeRaw = Number(document.getElementById('org-batch-size')?.value || DEFAULT_BATCH_SIZE);
  const maxClusterDepthInput = String(document.getElementById('org-max-cluster-depth')?.value || '').trim();
  const parsedMaxClusterDepth = maxClusterDepthInput ? Number(maxClusterDepthInput) : null;
  const useWebSearch = !!document.getElementById('org-enable-web-search')?.checked;
  const summaryStrategyRaw = String(document.getElementById('org-summary-mode')?.value || '').trim();
  const summaryStrategy = SUMMARY_MODES.includes(summaryStrategyRaw) ? summaryStrategyRaw : DEFAULT_SUMMARY_MODE;
  const modelRouting = readModelRoutingFromDOM();
  const batchSize = Number.isFinite(batchSizeRaw)
    ? Math.max(1, Math.min(200, Math.floor(batchSizeRaw)))
    : DEFAULT_BATCH_SIZE;
  const maxClusterDepth = Number.isFinite(parsedMaxClusterDepth) && parsedMaxClusterDepth > 0
    ? Math.floor(parsedMaxClusterDepth)
    : null;

  return {
    rootPath,
    recursive,
    excludedPatterns: excludedPatterns.length ? excludedPatterns : [...DEFAULT_EXCLUSIONS],
    batchSize,
    summaryStrategy,
    maxClusterDepth,
    useWebSearch,
    modelRouting,
    responseLanguage: getLang(),
  };
}

function resolveSearchApi(settings) {
  const source = settings?.searchApi && typeof settings.searchApi === 'object'
    ? settings.searchApi
    : {};
  const scopes = source?.scopes && typeof source.scopes === 'object'
    ? source.scopes
    : {};

  return {
    provider: 'tavily',
    enabled: !!source.enabled,
    apiKey: String(source.apiKey || '').trim(),
    scopes: {
      classify: !!scopes.classify,
      organizer: !!scopes.organizer,
    },
  };
}

async function syncClassifyWebSearchToSettings(isEnabled) {
  const settings = await getSettings();
  const currentSearchApi = resolveSearchApi(settings);
  const nextSearchApi = {
    provider: 'tavily',
    enabled: !!isEnabled,
    scopes: {
      classify: !!isEnabled,
      organizer: !!isEnabled,
    },
  };

  await saveSettings({
    searchApi: nextSearchApi,
  });
}

async function syncSummaryModeTikaToSettings(summaryMode, settings = null) {
  if (!['local_summary', 'agent_summary'].includes(String(summaryMode || '').trim())) {
    return settings;
  }

  const currentSettings = settings || await getSettings();
  const currentTika = currentSettings?.contentExtraction?.tika && typeof currentSettings.contentExtraction.tika === 'object'
    ? currentSettings.contentExtraction.tika
    : {};

  const nextEnabled = true;
  const nextAutoStart = true;
  const currentEnabled = !!currentTika.enabled;
  const currentAutoStart = !!currentTika.autoStart;

  if (currentEnabled === nextEnabled && currentAutoStart === nextAutoStart) {
    return currentSettings;
  }

  await saveSettings({
    contentExtraction: {
      tika: {
        enabled: nextEnabled,
        autoStart: nextAutoStart,
        url: String(currentTika.url || '').trim() || 'http://127.0.0.1:9998',
        jarPath: String(currentTika.jarPath || '').trim(),
      },
    },
  });

  return getSettings();
}

function organizerText(zh, en) {
  return getLang() === 'en' ? en : zh;
}

function getOrganizerSummaryModeLabel(mode) {
  if (mode === 'local_summary') return t('organizer.summary_mode_local_summary');
  if (mode === 'agent_summary') return t('organizer.summary_mode_agent_summary');
  return t('organizer.summary_mode_filename_only');
}

function getOrganizerSummaryModeDescription(mode) {
  if (mode === 'local_summary') return t('organizer.summary_mode_local_summary_hint');
  if (mode === 'agent_summary') return t('organizer.summary_mode_agent_summary_hint');
  return t('organizer.summary_mode_filename_only_hint');
}

function getOrganizerSummaryStrategy(value) {
  const raw = typeof value === 'string'
    ? value
    : value?.summaryStrategy || value?.summaryMode || '';
  return SUMMARY_MODES.includes(String(raw || '').trim()) ? String(raw).trim() : DEFAULT_SUMMARY_MODE;
}

function getOrganizerRepresentation(row = {}) {
  return row?.representation && typeof row.representation === 'object'
    ? row.representation
    : {};
}

function getOrganizerSummaryText(row = {}) {
  const representation = getOrganizerRepresentation(row);
  const summary = String(representation.long || representation.short || representation.metadata || '').trim();
  if (summary) return summary;
  const summarySource = String(representation.source || '').trim();
  if (summarySource === 'filename_only') return t('organizer.summary_placeholder_filename_only');
  if (summarySource === 'agent_fallback_local') return t('organizer.summary_placeholder_agent_fallback');
  return t('organizer.summary_placeholder_empty');
}
function setStatusText(snapshot) {
  const el = document.getElementById('org-status');
  if (!el) return;
  if (!snapshot) {
    el.textContent = t('organizer.status_idle');
    updateStatusDecor(null);
    return;
  }

  const statusMap = {
    idle: t('organizer.status_idle'),
    scanning: t('organizer.status_scanning'),
    classifying: t('organizer.status_classifying'),
    stopping: t('organizer.status_stopping'),
    stopped: t('organizer.status_stopped'),
    completed: t('organizer.status_completed'),
    moving: t('organizer.status_moving'),
    done: t('organizer.status_done'),
    error: t('organizer.status_error'),
  };

  el.textContent = statusMap[snapshot.status] || snapshot.status;
  updateStatusDecor(snapshot);
}

function getSelectionsFromDOM() {
  const selectedProviders = {};
  const selectedModels = {};
  for (const modality of Object.keys(PROVIDER_SELECT_IDS)) {
    selectedProviders[modality] = String(document.getElementById(PROVIDER_SELECT_IDS[modality])?.value || '').trim();
    selectedModels[modality] = String(document.getElementById(MODEL_SELECT_IDS[modality])?.value || '').trim();
  }
  const hasSelection = Object.values(selectedProviders).some(Boolean) && Object.values(selectedModels).some(Boolean);
  return { selectedProviders, selectedModels, hasSelection };
}

function supportsMultimodalBySelection(selectedProviders, selectedModels) {
  const endpoint = String(selectedProviders?.image || '').toLowerCase();
  const model = String(selectedModels?.image || '').toLowerCase();
  const value = `${endpoint}|${model}`;
  return [
    'gpt-4o',
    'gpt-4.1',
    'gemini',
    'qwen-vl',
    'qvq',
    'glm-4v',
    'claude',
  ].some((token) => value.includes(token));
}

function renderCapability(snapshot) {
  const modelEl = document.getElementById('org-model-name');
  const mmEl = document.getElementById('org-mm-badge');
  if (!modelEl || !mmEl) return;

  const domSelections = getSelectionsFromDOM();
  const useDomSelections = !snapshot && domSelections.hasSelection;
  const selectedModels = snapshot?.selectedModels
    || (useDomSelections ? domSelections.selectedModels : latestCapability?.selectedModels);
  const selectedProviders = snapshot?.selectedProviders
    || (useDomSelections ? domSelections.selectedProviders : latestCapability?.selectedProviders);
  const fallbackModel = selectedModels?.text || '-';
  const labelByEndpoint = new Map(PROVIDER_OPTIONS.map((item) => [item.value, item.label]));
  const renderCell = (modality) => {
    const endpoint = selectedProviders?.[modality] || '';
    const providerLabel = labelByEndpoint.get(endpoint) || endpoint || 'N/A';
    const modelName = selectedModels?.[modality] || fallbackModel;
    return `${providerLabel}/${modelName}`;
  };
  const model = [
    `${t('organizer.model_text')}:${renderCell('text')}`,
    `${t('organizer.model_image')}:${renderCell('image')}`,
    `${t('organizer.model_video')}:${renderCell('video')}`,
    `${t('organizer.model_audio')}:${renderCell('audio')}`,
  ].join(' | ');
  const supports = typeof snapshot?.supportsMultimodal === 'boolean'
    ? snapshot.supportsMultimodal
    : useDomSelections
      ? supportsMultimodalBySelection(selectedProviders, selectedModels)
      : latestCapability?.supportsMultimodal;

  modelEl.textContent = model;

  mmEl.classList.remove('badge-success', 'badge-warning', 'badge-danger');
  if (supports === true) {
    mmEl.textContent = t('organizer.multimodal_supported');
    mmEl.classList.add('badge-success');
  } else if (supports === false) {
    mmEl.textContent = t('organizer.multimodal_not_supported');
    mmEl.classList.add('badge-warning');
  } else {
    mmEl.textContent = t('organizer.multimodal_unknown');
    mmEl.classList.add('badge-danger');
  }
}

function renderModelSelectOptions(select, models, selectedValue) {
  if (!select) return;
  select.innerHTML = '';
  for (const model of models) {
    select.add(new Option(String(model.label || model.value), String(model.value)));
  }
  if (selectedValue) {
    const exists = Array.from(select.options).some((opt) => opt.value === selectedValue);
    if (!exists) {
      select.add(new Option(selectedValue, selectedValue));
    }
    select.value = selectedValue;
  } else if (select.options.length > 0) {
    select.value = select.options[0].value;
  }
}

async function initModelSelectors(defaultSelection = {}) {
  const modality = defaultSelection?.modality;
  if (!modality || !MODEL_SELECT_IDS[modality]) return;

  const providerSelect = document.getElementById(PROVIDER_SELECT_IDS[modality]);
  const modelSelect = document.getElementById(MODEL_SELECT_IDS[modality]);
  if (!providerSelect || !modelSelect) return;

  const endpoint = String(providerSelect.value || '').trim();
  const selectedModel = String(defaultSelection?.model || modelSelect.value || '').trim();
  const requestToken = ++modelsRequestToken[modality];

  modelSelect.disabled = true;
  modelSelect.innerHTML = `<option value="">${t('organizer.model_loading')}</option>`;

  const cacheKey = endpoint;
  let models = [];
  try {
    if (endpoint && getApiKeyForEndpoint(endpoint)) {
      if (remoteModelsCache.has(cacheKey)) {
        models = remoteModelsCache.get(cacheKey);
      } else {
        const resp = await getProviderModels(endpoint);
        models = normalizeRemoteModels(resp?.models || []);
        remoteModelsCache.set(cacheKey, models);
      }
    }
  } catch {
    models = [];
  }

  if (!models.length) {
    models = normalizeRemoteModels(PROVIDER_MODELS[endpoint] || PROVIDER_MODELS['https://api.deepseek.com']);
  }
  if (!models.length) {
    models = [{ value: 'gpt-4o-mini', label: 'gpt-4o-mini' }];
  }

  if (requestToken !== modelsRequestToken[modality]) return;
  renderModelSelectOptions(modelSelect, models, selectedModel || models[0]?.value);
  modelSelect.disabled = false;
}

async function initModelRoutingFields(defaultRouting = {}) {
  let settings = null;
  try {
    settings = await getSettings();
  } catch {
    settings = null;
  }

  syncProviderApiKeys(settings);

  const baseEndpoint = String(settings?.defaultProviderEndpoint || DEFAULT_PROVIDER_ENDPOINT).trim();
  const baseModel = String(
    settings?.providerConfigs?.[baseEndpoint]?.model
    || defaultModelByEndpoint(baseEndpoint)
    || defaultModelByEndpoint(DEFAULT_PROVIDER_ENDPOINT)
  ).trim();

  for (const modality of Object.keys(PROVIDER_SELECT_IDS)) {
    populateProviderOptions(document.getElementById(PROVIDER_SELECT_IDS[modality]));
  }

  for (const modality of Object.keys(PROVIDER_SELECT_IDS)) {
    const providerSelect = document.getElementById(PROVIDER_SELECT_IDS[modality]);
    const route = defaultRouting?.[modality] || {};
    const endpoint = String(route.endpoint || baseEndpoint).trim();
    const model = String(route.model || baseModel).trim();

    ensureProviderOptionExists(providerSelect, endpoint);
    if (providerSelect) providerSelect.value = endpoint;

    await initModelSelectors({ modality, model });
  }

  refreshOrganizerApiConfigHint();

  return settings;
}

async function applyTextRouteToOtherModalities() {
  const routing = readModelRoutingFromDOM();
  const textRoute = routing?.text || {};
  const targetModalities = ['image', 'video', 'audio'];

  for (const modality of targetModalities) {
    const providerSelect = document.getElementById(PROVIDER_SELECT_IDS[modality]);
    const endpoint = String(textRoute.endpoint || '').trim();

    ensureProviderOptionExists(providerSelect, endpoint);
    if (providerSelect && endpoint) {
      providerSelect.value = endpoint;
    }
  }

  for (const modality of targetModalities) {
    await initModelSelectors({ modality, model: String(textRoute.model || '').trim() });
  }

  persistForm(collectForm());
  refreshOrganizerApiConfigHint();
}

function renderPreview(snapshot) {
  const groupsEl = document.getElementById('org-preview-groups');
  const empty = document.getElementById('org-preview-empty');
  if (!groupsEl || !empty) return;

  const preview = snapshot?.preview || [];
  const resultsMap = new Map((snapshot?.results || []).map((x) => [x.path, x]));

  if (!preview.length) {
    groupsEl.innerHTML = '';
    empty.style.display = '';
    return;
  }

  empty.style.display = 'none';

  const groups = new Map();
  for (const item of preview) {
    const categoryPath = Array.isArray(item.categoryPath)
      ? item.categoryPath.map((segment) => String(segment || '').trim()).filter(Boolean)
      : [];
    const key = categoryPath.length
      ? categoryPath.join(' / ')
      : String(item.category || t('organizer.uncategorized')).trim() || t('organizer.uncategorized');
    if (!groups.has(key)) {
      groups.set(key, []);
    }
    groups.get(key).push(item);
  }

  const sortedGroups = Array.from(groups.entries()).sort(([a], [b]) => String(a).localeCompare(String(b), 'zh-Hans-CN'));
  groupsEl.innerHTML = sortedGroups.map(([categoryPathLabel, items], groupIdx) => {
    const categoryTrail = categoryPathLabel
      .split(' / ')
      .map((segment) => `<span class="organizer-category-node">${escapeHtml(segment)}</span>`)
      .join('<span class="organizer-category-separator"></span>');
    const rows = items.map((item, rowIdx) => {
      const row = resultsMap.get(item.sourcePath);
      const summaryText = getOrganizerSummaryText(row);
      const representation = getOrganizerRepresentation(row);
      const summarySource = String(representation?.source || '').trim();
      const degraded = representation?.degraded
        ? `<div style="margin-top:6px;"><span class="badge badge-warning">${t('organizer.degraded')}</span></div>`
        : '';
      const summaryBadge = summarySource
        ? `<div style="margin-top:6px;"><span class="badge badge-info">${escapeHtml(getOrganizerSummaryModeLabel(getOrganizerSummaryStrategy(row)))}</span></div>`
        : '';

      return `
        <tr style="animation: slideUp 0.2s var(--ease-out) ${Math.min(rowIdx * 0.01, 0.2)}s both;">
          <td>
            <div class="file-name" title="${escapeHtml(row?.name || '')}">${escapeHtml(row?.name || '')}</div>
            <div class="file-path" title="${escapeHtml(item.sourcePath)}">${escapeHtml(item.sourcePath)}</div>
            <div class="file-purpose" title="${escapeHtml(summaryText)}">${escapeHtml(summaryText)}</div>
            ${summaryBadge}
            ${degraded}
          </td>
          <td><div class="file-path" title="${escapeHtml(item.targetPath)}">${escapeHtml(item.targetPath)}</div></td>
        </tr>
      `;
    }).join('');

    return `
      <section class="preview-group" style="animation: slideUp 0.2s var(--ease-out) ${Math.min(groupIdx * 0.03, 0.4)}s both;">
        <div class="preview-group-header">
          <div class="organizer-category-path">${categoryTrail}</div>
          <span class="preview-group-count">${items.length}</span>
        </div>
        <div class="preview-group-table-wrap">
          <table class="data-table preview-group-table">
            <thead>
              <tr>
                <th>${t('organizer.source')}</th>
                <th>${t('organizer.target')}</th>
              </tr>
            </thead>
            <tbody>${rows}</tbody>
          </table>
        </div>
      </section>
    `;
  }).join('');
}

function getMoveStatusLabel(status) {
  if (status === 'failed') return getMoveResultText('statusFailed');
  if (status === 'skipped') return getMoveResultText('statusSkipped');
  return getMoveResultText('statusMoved');
}

function getMoveStatusBadge(status) {
  if (status === 'failed') return 'badge-danger';
  if (status === 'skipped') return 'badge-warning';
  return 'badge-success';
}

function getActiveApplyManifest(snapshot = latestSnapshot) {
  const manifest = getPersistedApplyManifest();
  if (!manifest) return null;

  const manifestTaskId = String(manifest.taskId || '').trim();
  const manifestJobId = String(manifest.jobId || '').trim();
  const snapshotTaskId = String(snapshot?.id || activeTaskId || '').trim();
  const snapshotJobId = String(snapshot?.jobId || getPersisted(PERSIST_KEYS.lastJobId, null) || '').trim();

  if (snapshotTaskId && manifestTaskId && snapshotTaskId !== manifestTaskId) {
    return null;
  }
  if (snapshotJobId && manifestJobId && snapshotJobId !== manifestJobId) {
    return null;
  }
  return manifest;
}

function renderApplyResultPanel(snapshot = latestSnapshot) {
  const card = document.getElementById('org-move-result-card');
  const movedEl = document.getElementById('org-move-moved');
  const skippedEl = document.getElementById('org-move-skipped');
  const failedEl = document.getElementById('org-move-failed');
  const totalEl = document.getElementById('org-move-total');
  const tbody = document.getElementById('org-move-result-body');
  const empty = document.getElementById('org-move-result-empty');
  if (!card || !movedEl || !skippedEl || !failedEl || !totalEl || !tbody || !empty) return;

  const manifest = getActiveApplyManifest(snapshot);
  if (!manifest) {
    card.hidden = true;
    tbody.innerHTML = '';
    empty.style.display = '';
    return;
  }

  card.hidden = false;
  const summary = manifest.summary && typeof manifest.summary === 'object' ? manifest.summary : {};
  const moved = Number(summary.moved || 0);
  const skipped = Number(summary.skipped || 0);
  const failed = Number(summary.failed || 0);
  const total = Number(summary.total || 0);
  const issueEntries = (Array.isArray(manifest.entries) ? manifest.entries : [])
    .filter((entry) => String(entry?.status || '').trim() !== 'moved');

  movedEl.textContent = String(moved);
  skippedEl.textContent = String(skipped);
  failedEl.textContent = String(failed);
  totalEl.textContent = String(total);

  if (!issueEntries.length) {
    tbody.innerHTML = '';
    empty.querySelector('.empty-state-text').textContent = getMoveResultText('clean');
    empty.style.display = '';
    return;
  }

  empty.style.display = 'none';
  tbody.innerHTML = issueEntries.map((entry, idx) => {
    const status = String(entry?.status || '').trim() || 'failed';
    const reason = status === 'skipped'
      ? getMoveResultText('reasonSkipped')
      : (String(entry?.error || '').trim() || t('organizer.degraded_reason_unknown'));

    return `
      <tr style="animation: slideUp 0.2s var(--ease-out) ${Math.min(idx * 0.01, 0.3)}s both;">
        <td><span class="badge ${getMoveStatusBadge(status)}">${escapeHtml(getMoveStatusLabel(status))}</span></td>
        <td>
          <div class="file-path" title="${escapeHtml(entry?.sourcePath || '')}">${escapeHtml(entry?.sourcePath || '')}</div>
        </td>
        <td>
          <div class="file-path" title="${escapeHtml(entry?.targetPath || '')}">${escapeHtml(entry?.targetPath || '')}</div>
        </td>
        <td><div class="file-purpose">${escapeHtml(reason)}</div></td>
      </tr>
    `;
  }).join('');
}

function renderOrganizerResultsEmptyState(snapshot = latestSnapshot) {
  const emptyCard = document.getElementById('org-results-empty-card');
  const stack = document.getElementById('org-results-stack');
  if (!emptyCard || !stack) return;

  const hasSnapshot = !!snapshot?.id;
  emptyCard.hidden = hasSnapshot;
  stack.hidden = !hasSnapshot;
}

function updateStats(snapshot) {
  const total = snapshot?.totalFiles || 0;
  const done = snapshot?.processedFiles || 0;
  const token = snapshot?.tokenUsage?.total || 0;
  const degraded = (snapshot?.results || []).filter((x) => x.degraded).length;

  const totalEl = document.getElementById('org-total');
  const doneEl = document.getElementById('org-done');
  const tokenEl = document.getElementById('org-token');
  const degEl = document.getElementById('org-degraded');

  if (totalEl) totalEl.textContent = String(total);
  if (doneEl) doneEl.textContent = String(done);
  if (tokenEl) tokenEl.textContent = token.toLocaleString();
  if (degEl) degEl.textContent = String(degraded);

  const pct = total > 0 ? ((done / total) * 100).toFixed(1) : '0.0';
  const fill = document.getElementById('org-progress-fill');
  const pctEl = document.getElementById('org-progress-pct');
  if (fill) fill.style.width = `${pct}%`;
  if (pctEl) pctEl.textContent = `${pct}%`;
}

function updateButtons(snapshot) {
  const status = snapshot?.status || 'idle';
  const startBtn = document.getElementById('org-start-btn');
  const stopBtn = document.getElementById('org-stop-btn');
  const applyBtn = document.getElementById('org-apply-btn');
  const rollbackBtn = document.getElementById('org-rollback-btn');

  if (startBtn) {
    const running = status === 'scanning' || status === 'classifying' || status === 'stopping' || status === 'moving';
    startBtn.disabled = running;
  }

  if (stopBtn) {
    const stoppable = status === 'scanning' || status === 'classifying';
    stopBtn.disabled = !stoppable;
  }

  if (applyBtn) {
    const hasPreview = Array.isArray(snapshot?.preview) && snapshot.preview.length > 0;
    applyBtn.disabled = !(status === 'completed' || status === 'done') || !hasPreview;
  }

  if (rollbackBtn) {
    rollbackBtn.disabled = !getPersisted(PERSIST_KEYS.lastJobId, null);
  }
}

function syncBatchConfigInputs(snapshot) {
  if (!snapshot) return;

  const batchSizeInput = document.getElementById('org-batch-size');
  if (batchSizeInput && Number(snapshot.batchSize || 0) > 0) {
    const nextValue = String(snapshot.batchSize);
    if (String(batchSizeInput.value || '') !== nextValue) {
      batchSizeInput.value = nextValue;
    }
  }

  const depthInput = document.getElementById('org-max-cluster-depth');
  if (depthInput) {
    const nextValue = snapshot.maxClusterDepth == null ? '' : String(snapshot.maxClusterDepth);
    if (String(depthInput.value || '') !== nextValue) {
      depthInput.value = nextValue;
    }
  }

  const summaryModeSelect = document.getElementById('org-summary-mode');
  if (summaryModeSelect) {
    const nextValue = getOrganizerSummaryStrategy(snapshot);
    if (String(summaryModeSelect.value || '') !== nextValue) {
      summaryModeSelect.value = nextValue;
    }
  }
}

function refreshView(snapshot) {
  latestSnapshot = snapshot || null;
  if (snapshot?.id) {
    activeTaskId = snapshot.id;
    setPersisted(PERSIST_KEYS.lastTaskId, snapshot.id);
    setPersisted(PERSIST_KEYS.lastSnapshot, snapshot);
  } else {
    activeTaskId = null;
  }
  if (snapshot?.jobId) {
    setPersisted(PERSIST_KEYS.lastJobId, snapshot.jobId);
  } else {
    removePersisted(PERSIST_KEYS.lastJobId);
  }
  syncBatchConfigInputs(snapshot);
  organizerLog.syncProgressBaseline(snapshot);
  setStatusText(snapshot);
  updatePipeline(snapshot);
  renderCapability(snapshot);
  updateStats(snapshot);
  renderOrganizerResultsEmptyState(snapshot);
  renderPreview(snapshot);
  renderApplyResultPanel(snapshot);
  updateButtons(snapshot);
}

function closeActiveSSE() {
  if (activeEventSource) {
    activeEventSource.close();
    activeEventSource = null;
  }
}

function connectTaskStream(taskId) {
  closeActiveSSE();
  activeEventSource = connectOrganizeStream(taskId, {
    onProgress: (snap) => {
      organizerLog.recordProgress(snap);
      refreshView(snap);
    },
    onSummaryReady: (row) => {
      organizerLog.recordSummaryReady(row);
    },
    onFileDone: (row) => {
      organizerLog.recordFileDone(row);
    },
    onDone: (snap) => {
      const detail = [
        `${organizerText('状态', 'Status')}: ${String(snap?.status || 'completed')}`,
        `${organizerText('文件', 'Files')}: ${Number(snap?.processedFiles || 0)}/${Number(snap?.totalFiles || 0)}`,
        `${organizerText('批次', 'Batches')}: ${Number(snap?.processedBatches || 0)}/${Number(snap?.totalBatches || 0)}`,
        `${organizerText('降级', 'Degraded')}: ${Number((snap?.results || []).filter((item) => item?.representation?.degraded).length)}`,
        `${organizerText('Token', 'Token')}: ${Number(snap?.tokenUsage?.total || 0).toLocaleString()}`,
      ].join('\n');
      organizerLog.recordTerminal(
        snap,
        'done',
        organizerText('归类完成，结果已就绪', 'Organize task completed'),
        detail,
      );
      refreshView(snap);
      showToast(t('organizer.toast_classify_done'), 'success');
      window.location.hash = '#/organizer-results';
    },
    onStopped: (snap) => {
      const detail = [
        `${organizerText('状态', 'Status')}: ${String(snap?.status || 'stopped')}`,
        `${organizerText('文件', 'Files')}: ${Number(snap?.processedFiles || 0)}/${Number(snap?.totalFiles || 0)}`,
        `${organizerText('批次', 'Batches')}: ${Number(snap?.processedBatches || 0)}/${Number(snap?.totalBatches || 0)}`,
      ].join('\n');
      organizerLog.recordTerminal(
        snap,
        'stopped',
        organizerText('归类任务已停止', 'Organize task stopped'),
        detail,
      );
      refreshView(snap);
    },
    onError: (err) => {
      const detail = getErrorMessage(err);
      const errSnapshot = err?.snapshot && typeof err.snapshot === 'object' ? err.snapshot : latestSnapshot;
      organizerLog.recordTerminal(
        errSnapshot,
        'error',
        organizerText('归类任务失败', 'Organize task failed'),
        detail,
      );
      if (errSnapshot) {
        refreshView(errSnapshot);
      }
      showToast(`${t('organizer.toast_failed')}${getErrorMessage(err)}`, 'error');
    },
  });
}

function buildOptimisticRunningSnapshot(taskId, form, capability) {
  const selectedModels = capability?.selectedModels || latestCapability?.selectedModels || {};
  const selectedProviders = capability?.selectedProviders || latestCapability?.selectedProviders || {};
  return {
    id: taskId,
    status: 'scanning',
    error: null,
    rootPath: form.rootPath,
    recursive: true,
    excludedPatterns: Array.isArray(form.excludedPatterns) ? form.excludedPatterns : [],
    batchSize: Number(form.batchSize) || DEFAULT_BATCH_SIZE,
    summaryStrategy: form.summaryStrategy || DEFAULT_SUMMARY_MODE,
    maxClusterDepth: form.maxClusterDepth ?? null,
    useWebSearch: !!form.useWebSearch,
    webSearchEnabled: !!form.useWebSearch,
    selectedModel: selectedModels.text || '',
    selectedModels,
    selectedProviders,
    supportsMultimodal: typeof capability?.supportsMultimodal === 'boolean'
      ? capability.supportsMultimodal
      : latestCapability?.supportsMultimodal,
    tree: latestSnapshot?.tree || null,
    treeVersion: Number(latestSnapshot?.treeVersion || 0),
    totalFiles: 0,
    processedFiles: 0,
    totalBatches: 0,
    processedBatches: 0,
    tokenUsage: { prompt: 0, completion: 0, total: 0 },
    results: [],
    preview: [],
    createdAt: new Date().toISOString(),
    completedAt: null,
    jobId: null,
  };
}

async function handleBrowse() {
  const btn = document.getElementById('org-browse-btn');
  if (!btn) return;

  btn.disabled = true;
  btn.textContent = t('organizer.browsing');
  try {
    const result = await browseFolder();
    if (!result.cancelled && result.path) {
      const input = document.getElementById('org-root-path');
      if (input) input.value = result.path;
      const data = collectForm();
      persistForm(data);
    }
  } catch (err) {
    showToast(`${t('organizer.toast_failed')}${getErrorMessage(err)}`, 'error');
  } finally {
    btn.disabled = false;
    btn.textContent = t('settings.browse');
  }
}

async function handleStart() {
  const form = collectForm();
  if (!form.rootPath) {
    showToast(t('organizer.path_required'), 'error');
    return;
  }

  persistForm(form);

  const selectedEndpoints = Object.values(form.modelRouting || {})
    .map((item) => String(item?.endpoint || '').trim())
    .filter(Boolean);
  const missingProviderSecret = Array.from(new Set(selectedEndpoints)).some((endpoint) => !getApiKeyForEndpoint(endpoint));
  let settingsSnapshot = await getSettings().catch(() => null);
  try {
    settingsSnapshot = await syncSummaryModeTikaToSettings(form.summaryStrategy, settingsSnapshot);
  } catch (err) {
    console.warn('[Organizer] Failed to sync Tika setting for summary mode:', err);
  }
  const needsSearchSecret = !!form.useWebSearch && !getSearchCredentialPresence(settingsSnapshot || {});
  if (missingProviderSecret || needsSearchSecret) {
    try {
      await ensureRequiredCredentialsConfigured({
        providerEndpoints: Array.from(new Set(selectedEndpoints)),
        requireSearchApi: !!form.useWebSearch,
        reasonText: t('settings.api_key'),
      });
      settingsSnapshot = await getSettings();
      syncProviderApiKeys(settingsSnapshot);
      refreshOrganizerApiConfigHint();
    } catch (err) {
      showToast(getErrorMessage(err), 'error');
      return;
    }
    const stillMissingProviderSecret = Array.from(new Set(selectedEndpoints)).some((endpoint) => !getApiKeyForEndpoint(endpoint));
    const stillMissingSearchSecret = !!form.useWebSearch && !getSearchCredentialPresence(settingsSnapshot || {});
    if (stillMissingProviderSecret || stillMissingSearchSecret) {
      showToast(t('settings.api_key'), 'error');
      return;
    }
  }

  const btn = document.getElementById('org-start-btn');
  if (btn) {
    btn.disabled = true;
    btn.innerHTML = `<span class="spinner"></span> ${t('organizer.starting')}`;
  }

  try {
    const result = await startOrganize(form);
    activeTaskId = result.taskId;
    setPersistedApplyManifest(null);
    latestCapability = {
      selectedModels: result.selectedModels,
      selectedProviders: result.selectedProviders,
      supportsMultimodal: result.supportsMultimodal,
    };
    renderCapability();
    renderApplyResultPanel(null);
    setPersisted(PERSIST_KEYS.lastTaskId, activeTaskId);
    organizerLog.recordStart(form, activeTaskId, result);
    refreshView(buildOptimisticRunningSnapshot(activeTaskId, form, result));
    connectTaskStream(activeTaskId);
    showToast(t('organizer.toast_started'), 'success');
  } catch (err) {
    showToast(`${t('organizer.toast_failed')}${getErrorMessage(err)}`, 'error');
  } finally {
    if (btn) {
      btn.disabled = false;
      btn.textContent = t('organizer.start');
    }
  }
}

async function handleApply() {
  if (!activeTaskId) {
    showToast(t('organizer.no_task'), 'error');
    return;
  }

  if (!confirm(t('organizer.confirm_apply'))) {
    return;
  }

  const btn = document.getElementById('org-apply-btn');
  if (btn) {
    btn.disabled = true;
    btn.innerHTML = `<span class="spinner"></span> ${t('organizer.applying')}`;
  }

  try {
    const result = await applyOrganize(activeTaskId);
    const jobId = result?.manifest?.jobId;
    if (jobId) {
      setPersisted(PERSIST_KEYS.lastJobId, jobId);
    }
    setPersistedApplyManifest(result?.manifest || null);

    const summary = result?.manifest?.summary;
    if (summary) {
      const moved = Number(summary.moved || 0);
      const skipped = Number(summary.skipped || 0);
      const failed = Number(summary.failed || 0);
      const total = Number(summary.total || 0);
      organizerLog.recordTerminal(
        latestSnapshot,
        failed > 0 ? 'error' : 'done',
        organizerText('移动任务已完成', 'Move operation completed'),
        [
          `${getMoveResultText('moved')}: ${moved}`,
          `${getMoveResultText('skipped')}: ${skipped}`,
          `${getMoveResultText('failed')}: ${failed}`,
          `${getMoveResultText('total')}: ${total}`,
        ].join('\n'),
      );
      showToast(
        `${t('organizer.toast_apply_done')} (${getMoveResultText('moved')}: ${moved}，${getMoveResultText('skipped')}: ${skipped}，${getMoveResultText('failed')}: ${failed}，${getMoveResultText('total')}: ${total})`,
        failed > 0 ? 'error' : (skipped > 0 ? 'info' : 'success')
      );
    } else {
      organizerLog.recordTerminal(
        latestSnapshot,
        'done',
        organizerText('移动任务已完成', 'Move operation completed'),
        organizerText('移动结果已更新。', 'Move results have been updated.'),
      );
      showToast(t('organizer.toast_apply_done'), 'success');
    }

    const snapshot = await getOrganizeResult(activeTaskId);
    refreshView(snapshot);
  } catch (err) {
    showToast(`${t('organizer.toast_failed')}${getErrorMessage(err)}`, 'error');
  } finally {
    if (btn) {
      btn.disabled = false;
      btn.textContent = t('organizer.apply_move');
    }
    updateButtons(latestSnapshot);
  }
}

async function handleStop() {
  if (!activeTaskId) {
    showToast(t('organizer.no_task'), 'error');
    return;
  }

  const btn = document.getElementById('org-stop-btn');
  if (btn) {
    btn.disabled = true;
    btn.innerHTML = `<span class="spinner"></span> ${t('organizer.stopping')}`;
  }

  try {
    await stopOrganize(activeTaskId);
    if (latestSnapshot?.id === activeTaskId) {
      refreshView({
        ...latestSnapshot,
        status: 'stopping',
      });
    }
    showToast(t('organizer.toast_stopped'), 'info');
  } catch (err) {
    showToast(`${t('organizer.toast_stop_failed')}${getErrorMessage(err)}`, 'error');
  } finally {
    if (btn) {
      btn.textContent = t('organizer.stop');
    }
    updateButtons(latestSnapshot);
  }
}

async function handleRollback() {
  const jobId = getPersisted(PERSIST_KEYS.lastJobId, null);
  if (!jobId) {
    showToast(t('organizer.no_rollback_job'), 'error');
    return;
  }

  if (!confirm(t('organizer.confirm_rollback'))) {
    return;
  }

  const btn = document.getElementById('org-rollback-btn');
  if (btn) {
    btn.disabled = true;
    btn.innerHTML = `<span class="spinner"></span> ${t('organizer.rolling_back')}`;
  }

  try {
    const result = await rollbackOrganize(jobId);
    const summary = result?.rollback?.summary;
    const persistedTaskId = activeTaskId || getPersisted(PERSIST_KEYS.lastTaskId, null);
    if (summary && Number(summary.failed || 0) === 0) {
      setPersistedApplyManifest(null);
      removePersisted(PERSIST_KEYS.lastJobId);
      renderApplyResultPanel(latestSnapshot);
    }
    if (persistedTaskId) {
      try {
        const snapshot = await getOrganizeResult(persistedTaskId);
        refreshView(snapshot);
      } catch (refreshErr) {
        console.warn('[Organizer] Failed to refresh snapshot after rollback:', refreshErr);
      }
    }
    if (summary) {
      organizerLog.recordTerminal(
        latestSnapshot,
        summary.failed > 0 ? 'error' : 'done',
        organizerText('回滚任务已完成', 'Rollback completed'),
        [
          `${organizerText('已回滚', 'Rolled back')}: ${Number(summary.rolledBack || 0)}`,
          `${organizerText('总数', 'Total')}: ${Number(summary.total || 0)}`,
          `${organizerText('失败', 'Failed')}: ${Number(summary.failed || 0)}`,
        ].join('\n'),
      );
      showToast(
        `${t('organizer.toast_rollback_done')} (${summary.rolledBack}/${summary.total})`,
        summary.failed > 0 ? 'info' : 'success'
      );
    } else {
      organizerLog.recordTerminal(
        latestSnapshot,
        'done',
        organizerText('回滚任务已完成', 'Rollback completed'),
        organizerText('最近一次移动任务已回滚。', 'The most recent move task has been rolled back.'),
      );
      showToast(t('organizer.toast_rollback_done'), 'success');
    }
  } catch (err) {
    showToast(`${t('organizer.toast_failed')}${getErrorMessage(err)}`, 'error');
  } finally {
    if (btn) {
      btn.disabled = false;
      btn.textContent = t('organizer.rollback');
    }
    updateButtons(latestSnapshot);
  }
}

async function handleCopyTextRoute() {
  const btn = document.getElementById(COPY_TEXT_ROUTE_BTN_ID);
  if (btn) {
    btn.disabled = true;
  }

  try {
    await applyTextRouteToOtherModalities();
    showToast(t('organizer.toast_route_copied'), 'success');
  } catch (err) {
    showToast(`${t('organizer.toast_failed')}${getErrorMessage(err)}`, 'error');
  } finally {
    if (btn) {
      btn.disabled = false;
    }
  }
}

function bindPersistenceListeners() {
  [
    'org-root-path',
    'org-enable-web-search',
    'org-exclusions',
    'org-batch-size',
    'org-summary-mode',
    'org-max-cluster-depth',
    PROVIDER_SELECT_IDS.text,
    PROVIDER_SELECT_IDS.image,
    PROVIDER_SELECT_IDS.video,
    PROVIDER_SELECT_IDS.audio,
    MODEL_SELECT_IDS.text,
    MODEL_SELECT_IDS.image,
    MODEL_SELECT_IDS.video,
    MODEL_SELECT_IDS.audio,
  ].forEach((id) => {
    const el = document.getElementById(id);
    if (!el) return;
    const eventName = [
      'org-enable-web-search',
      'org-summary-mode',
      PROVIDER_SELECT_IDS.text,
      PROVIDER_SELECT_IDS.image,
      PROVIDER_SELECT_IDS.video,
      PROVIDER_SELECT_IDS.audio,
      MODEL_SELECT_IDS.text,
      MODEL_SELECT_IDS.image,
      MODEL_SELECT_IDS.video,
      MODEL_SELECT_IDS.audio,
    ].includes(id)
      ? 'change'
      : 'input';
    el.addEventListener(eventName, () => {
      persistForm(collectForm());
      if (id === 'org-enable-web-search') {
        syncClassifyWebSearchToSettings(!!el.checked).catch((err) => {
          console.warn('[Organizer] Failed to sync classify web search setting:', err);
        });
      }
    });
  });
}

function bindModelRoutingListeners() {
  for (const modality of Object.keys(PROVIDER_SELECT_IDS)) {
    const providerSelect = document.getElementById(PROVIDER_SELECT_IDS[modality]);
    const modelSelect = document.getElementById(MODEL_SELECT_IDS[modality]);

    providerSelect?.addEventListener('change', async () => {
      await initModelSelectors({ modality });
      persistForm(collectForm());
      refreshOrganizerApiConfigHint();
      renderCapability();
    });

    modelSelect?.addEventListener('change', () => {
      persistForm(collectForm());
      renderCapability();
    });
  }
}

function updateStatusDecor(snapshot) {
  const el = document.getElementById('org-status');
  if (!el) return;

  const status = String(snapshot?.status || 'idle').trim() || 'idle';
  el.dataset.status = status;
  el.classList.remove('badge-info', 'badge-warning', 'badge-success', 'badge-danger');

  const tone = {
    idle: 'badge-info',
    scanning: 'badge-warning',
    classifying: 'badge-warning',
    stopping: 'badge-warning',
    stopped: 'badge-info',
    completed: 'badge-success',
    moving: 'badge-warning',
    done: 'badge-success',
    error: 'badge-danger',
  }[status] || 'badge-info';

  el.classList.add(tone);
}

function updatePipeline(snapshot) {
  const stages = {
    scanning: document.getElementById('org-stage-scanning'),
    classifying: document.getElementById('org-stage-classifying'),
    preview: document.getElementById('org-stage-preview'),
    apply: document.getElementById('org-stage-apply'),
  };
  if (Object.values(stages).some((node) => !node)) return;

  const status = String(snapshot?.status || 'idle').trim() || 'idle';
  const hasPreview = Array.isArray(snapshot?.preview) && snapshot.preview.length > 0;
  const states = {
    scanning: 'pending',
    classifying: 'pending',
    preview: 'pending',
    apply: 'pending',
  };

  if (status === 'scanning') {
    states.scanning = 'active';
  } else if (status === 'classifying') {
    states.scanning = 'done';
    states.classifying = 'active';
  } else if (status === 'stopping') {
    if (Number(snapshot?.processedFiles || 0) > 0 || Number(snapshot?.totalFiles || 0) > 0) {
      states.scanning = 'done';
      states.classifying = 'active';
    } else {
      states.scanning = 'active';
    }
  } else if (status === 'completed') {
    states.scanning = 'done';
    states.classifying = 'done';
    states.preview = 'active';
  } else if (status === 'moving') {
    states.scanning = 'done';
    states.classifying = 'done';
    states.preview = 'done';
    states.apply = 'active';
  } else if (status === 'done') {
    states.scanning = 'done';
    states.classifying = 'done';
    states.preview = 'done';
    states.apply = 'done';
  } else if (status === 'stopped' || status === 'error') {
    if (hasPreview) {
      states.scanning = 'done';
      states.classifying = 'done';
      states.preview = 'active';
    } else if (Number(snapshot?.processedFiles || 0) > 0 || Number(snapshot?.totalFiles || 0) > 0) {
      states.scanning = 'done';
      states.classifying = 'active';
    } else {
      states.scanning = 'active';
    }
  }

  for (const [name, node] of Object.entries(stages)) {
    node.dataset.state = states[name];
  }
}

async function restoreOrganizerSnapshot(cachedSnapshot, isStale) {
  try {
    latestCapability = await getOrganizeCapability();
  } catch {
    latestCapability = null;
  }
  if (isStale()) return;
  renderCapability();

  const lastTaskId = getPersisted(PERSIST_KEYS.lastTaskId, null) || cachedSnapshot?.id || null;
  if (lastTaskId) {
    try {
      const snapshot = await getOrganizeResult(lastTaskId);
      if (isStale()) return;
      activeTaskId = lastTaskId;
      organizerLog.ensureForSnapshot(snapshot);
      refreshView(snapshot);

      if (['scanning', 'classifying', 'moving'].includes(snapshot.status)) {
        connectTaskStream(lastTaskId);
      }
      return;
    } catch {
      if (cachedSnapshot?.id === lastTaskId) {
        organizerLog.ensureForSnapshot(cachedSnapshot);
        refreshView(cachedSnapshot);
        return;
      }
      refreshView(null);
      return;
    }
  }

  if (!cachedSnapshot?.id) {
    refreshView(null);
  }
}

export async function renderOrganizer(container) {
  const expectedRenderVersion = ++renderVersion;
  const isStale = () => expectedRenderVersion !== renderVersion || !container.isConnected;
  cleanupLegacyPersistedState();
  invalidateOrganizerRuntimeCacheIfNeeded();
  const defaults = restoreDefaults();
  const cachedSnapshot = getPersisted(PERSIST_KEYS.lastSnapshot, null);
  organizerLog.restoreState(cachedSnapshot);

  container.innerHTML = `
    <div class="organizer-shell">
      <section class="card organizer-hero animate-in" style="animation-delay: 0.03s;">
        <div class="organizer-hero-grid">
          <div class="organizer-hero-copy">
            <div class="page-header organizer-page-header">
              <div class="organizer-kicker">${t('organizer.config')}</div>
              <h1 class="page-title">${escapeHtml(stripDecorativePrefix(t('organizer.title')) || t('organizer.title'))}</h1>
            </div>
            <div class="organizer-pipeline">
              ${renderPipelineStage('scanning', '01', t('organizer.status_scanning'))}
              ${renderPipelineStage('classifying', '02', t('organizer.status_classifying'))}
              ${renderPipelineStage('preview', '03', t('organizer.preview_title'))}
              ${renderPipelineStage('apply', '04', t('organizer.apply_move'))}
            </div>
          </div>

          <div class="organizer-hero-side">
            <div class="organizer-status-card">
              <div class="organizer-status-row">
                <div class="organizer-status-stack">
                  <span id="org-status" class="badge badge-info">${t('organizer.status_idle')}</span>
                  <div class="organizer-status-caption">${t('organizer.progress')}</div>
                </div>
                <span id="org-progress-pct" class="organizer-progress-pct">0.0%</span>
              </div>
              <div class="progress-bar organizer-progress-bar">
                <div id="org-progress-fill" class="progress-fill" style="width:0%;"></div>
              </div>
              <div class="organizer-status-meta">
                <div>
                  <div class="form-label">${t('organizer.current_model')}</div>
                  <div id="org-model-name" class="form-hint mono organizer-model-summary">-</div>
                </div>
                <div>
                  <div class="form-label">${t('organizer.multimodal')}</div>
                  <span id="org-mm-badge" class="badge badge-danger">${t('organizer.multimodal_unknown')}</span>
                </div>
              </div>
              <div class="organizer-action-grid organizer-action-grid-compact">
                <button id="org-start-btn" class="btn btn-primary" type="button">${t('organizer.start')}</button>
                <button id="org-stop-btn" class="btn btn-danger" type="button" disabled>${t('organizer.stop')}</button>
              </div>
            </div>
          </div>
        </div>
      </section>

      <div class="organizer-main-grid">
        <div class="organizer-config-stack">
          <section class="card organizer-panel animate-in" style="animation-delay: 0.07s;">
            <div class="card-header organizer-section-header">
              <div>
                <h2 class="card-title">${t('organizer.config')}</h2>
              </div>
            </div>

            <div class="form-group">
              <label class="form-label">${t('organizer.root_path')}</label>
              <div class="organizer-path-row">
                <input id="org-root-path" class="form-input organizer-path-input" value="${escapeHtml(defaults.rootPath)}" placeholder="C:\\Users\\..." />
                <button id="org-browse-btn" class="btn btn-secondary organizer-browse-btn" type="button">${t('settings.browse')}</button>
              </div>
            </div>

            <div class="grid-2 organizer-metrics-grid">
              <div class="form-group organizer-metric-field">
                <label class="form-label">${t('organizer.batch_size')}</label>
                <input id="org-batch-size" type="number" min="1" max="200" class="form-input no-spin" value="${Number(defaults.batchSize) || DEFAULT_BATCH_SIZE}" />
              </div>
              <div class="form-group organizer-metric-field">
                <label class="form-label">${t('organizer.summary_mode')}</label>
                <select id="org-summary-mode" class="form-input">
                  <option value="filename_only" ${defaults.summaryStrategy === 'filename_only' ? 'selected' : ''}>${t('organizer.summary_mode_filename_only')}</option>
                  <option value="local_summary" ${defaults.summaryStrategy === 'local_summary' ? 'selected' : ''}>${t('organizer.summary_mode_local_summary')}</option>
                  <option value="agent_summary" ${defaults.summaryStrategy === 'agent_summary' ? 'selected' : ''}>${t('organizer.summary_mode_agent_summary')}</option>
                </select>
              </div>
              <div class="form-group organizer-metric-field">
                <label class="form-label">${t('organizer.max_cluster_depth')}</label>
                <input id="org-max-cluster-depth" type="number" min="1" class="form-input no-spin" value="${defaults.maxClusterDepth == null ? '' : Number(defaults.maxClusterDepth)}" placeholder="${t('organizer.max_cluster_depth_unlimited')}" />
              </div>
            </div>

            <div class="organizer-toggle-grid">
              <label class="organizer-toggle-card">
                <input id="org-enable-web-search" type="checkbox" ${defaults.useWebSearch === true ? 'checked' : ''} />
                <span class="organizer-toggle-copy">
                  <span class="organizer-toggle-title">${t('organizer.use_web_search')}</span>
                </span>
              </label>
            </div>
          </section>

          <section class="card organizer-panel animate-in" style="animation-delay: 0.11s;">
            <div class="card-header organizer-section-header">
              <div>
                <h2 class="card-title">${t('organizer.model_routing')}</h2>
              </div>
            </div>

            <div class="organizer-routing-grid">
              ${renderRoutingCard('text')}
              ${renderRoutingCard('image')}
              ${renderRoutingCard('video')}
              ${renderRoutingCard('audio')}
            </div>

            <div id="org-api-config-hint" class="form-hint api-config-hint">${t('settings.api_key')}</div>

            <div class="organizer-routing-footer">
              <button id="${COPY_TEXT_ROUTE_BTN_ID}" class="btn btn-secondary" type="button">${t('organizer.copy_text_route')}</button>
            </div>
          </section>

          <section class="card organizer-panel animate-in" style="animation-delay: 0.15s;">
            <div class="card-header organizer-section-header">
              <div>
                <h2 class="card-title">${t('organizer.exclusions')}</h2>
              </div>
            </div>

            <textarea id="org-exclusions" class="form-input organizer-exclusion-input" rows="8">${escapeHtml((defaults.excludedPatterns || []).join('\n'))}</textarea>
          </section>
        </div>

        <div class="organizer-results-stack">
          ${renderOrganizerStatsGrid('0.09s')}

          <section class="card organizer-panel animate-in mt-24" style="animation-delay: 0.13s;">
            <div class="card-header">
              <h2 class="card-title">${t('organizer.activity_log')}</h2>
              <div style="display:flex; gap:8px; align-items:center;">
                <button id="org-toggle-log-btn" class="btn btn-ghost" style="padding: 6px 12px; font-size: 0.75rem;">${t('organizer.log_expand')}</button>
                <button id="org-clear-log-btn" class="btn btn-ghost" style="padding: 6px 12px; font-size: 0.75rem;">Clear</button>
              </div>
            </div>
            <div id="org-log-panel" class="scan-log-panel">
              <div class="scan-activity">
                <div class="scan-log" id="org-log"></div>
              </div>
            </div>
          </section>
        </div>
      </div>
    </div>
  `;

  document.getElementById('org-browse-btn')?.addEventListener('click', handleBrowse);
  document.getElementById('org-start-btn')?.addEventListener('click', handleStart);
  document.getElementById('org-stop-btn')?.addEventListener('click', handleStop);
  document.getElementById(COPY_TEXT_ROUTE_BTN_ID)?.addEventListener('click', handleCopyTextRoute);
  document.getElementById('org-summary-mode')?.addEventListener('change', () => {
    const select = document.getElementById('org-summary-mode');
    const hint = document.getElementById('org-summary-mode-hint');
    if (hint && select) {
      hint.textContent = getOrganizerSummaryModeDescription(select.value);
    }
    syncSummaryModeTikaToSettings(select?.value).catch((err) => {
      console.warn('[Organizer] Failed to enable Tika for summary mode:', err);
    });
  });
  organizerLog.mountPanel();

  const runtimeSettings = await initModelRoutingFields(defaults.modelRouting);
  if (isStale()) return;
  if (defaults.useWebSearch == null) {
    const searchApi = resolveSearchApi(runtimeSettings);
    const organizerDefault = searchApi.enabled && (searchApi.scopes.classify || searchApi.scopes.organizer);
    const toggle = document.getElementById('org-enable-web-search');
    if (toggle) {
      toggle.checked = organizerDefault;
    }
    persistForm(collectForm());
  }
  bindModelRoutingListeners();
  bindPersistenceListeners();
  refreshOrganizerApiConfigHint();
  renderCapability();
  if (cachedSnapshot?.id) {
    organizerLog.ensureForSnapshot(cachedSnapshot);
    refreshView(cachedSnapshot);
  } else {
    refreshView(null);
  }

  if (organizerProviderSettingsUpdatedHandler) {
    window.removeEventListener('provider-settings-updated', organizerProviderSettingsUpdatedHandler);
  }
  organizerProviderSettingsUpdatedHandler = async (event) => {
    try {
      const settingsSnapshot = event?.detail && typeof event.detail === 'object'
        ? event.detail
        : await getSettings();
      syncProviderApiKeys(settingsSnapshot);
      const currentRouting = readModelRoutingFromDOM();
      for (const modality of Object.keys(PROVIDER_SELECT_IDS)) {
        await initModelSelectors({
          modality,
          model: String(currentRouting?.[modality]?.model || '').trim(),
        });
      }
      refreshOrganizerApiConfigHint();
      renderCapability();
    } catch (err) {
      console.warn('[Organizer] Failed to refresh provider settings:', err);
    }
  };
  window.addEventListener('provider-settings-updated', organizerProviderSettingsUpdatedHandler);

  await restoreOrganizerSnapshot(cachedSnapshot, isStale);
}

export async function renderOrganizerResults(container) {
  const expectedRenderVersion = ++renderVersion;
  const isStale = () => expectedRenderVersion !== renderVersion || !container.isConnected;
  cleanupLegacyPersistedState();
  invalidateOrganizerRuntimeCacheIfNeeded();
  const cachedSnapshot = getPersisted(PERSIST_KEYS.lastSnapshot, null);
  organizerLog.restoreState(cachedSnapshot);
  if (organizerProviderSettingsUpdatedHandler) {
    window.removeEventListener('provider-settings-updated', organizerProviderSettingsUpdatedHandler);
    organizerProviderSettingsUpdatedHandler = null;
  }

  container.innerHTML = `
    <div class="page-header animate-in">
      <h1 class="page-title">${t('organizer.results_title')}</h1>
    </div>

    <section id="org-results-empty-card" class="card animate-in mb-24" style="animation-delay: 0.05s;">
      <div class="empty-state organizer-empty-state" style="padding: 36px;">
        <div class="organizer-empty-glyph" aria-hidden="true"></div>
        <div class="empty-state-text">${t('organizer.results_empty')}</div>
        <button id="org-results-go-btn" class="btn btn-primary" type="button">${t('organizer.go_organize')}</button>
      </div>
    </section>

    <div id="org-results-stack" class="organizer-results-page-stack" hidden>
      <section class="card organizer-panel animate-in mb-24" style="animation-delay: 0.08s;">
        <div class="card-header organizer-panel-header">
          <div>
            <h2 class="card-title">${t('organizer.results_title')}</h2>
          </div>
          <div class="organizer-results-toolbar">
            <span id="org-status" class="badge badge-info">${t('organizer.status_idle')}</span>
            <button id="org-apply-btn" class="btn btn-success" type="button" disabled>${t('organizer.apply_move')}</button>
            <button id="org-rollback-btn" class="btn btn-secondary" type="button">${t('organizer.rollback')}</button>
          </div>
        </div>
        <div class="organizer-result-progress">
          <span id="org-progress-pct" class="organizer-progress-pct organizer-progress-pct-inline">0.0%</span>
          <div class="progress-bar organizer-progress-bar">
            <div id="org-progress-fill" class="progress-fill" style="width:0%;"></div>
          </div>
        </div>
      </section>

      ${renderOrganizerStatsGrid('0.1s')}
      ${renderOrganizerPreviewPanel('0.14s')}
      ${renderOrganizerMoveResultPanel('0.18s')}
    </div>
  `;

  document.getElementById('org-results-go-btn')?.addEventListener('click', () => {
    window.location.hash = '#/organizer';
  });
  document.getElementById('org-apply-btn')?.addEventListener('click', handleApply);
  document.getElementById('org-rollback-btn')?.addEventListener('click', handleRollback);

  if (cachedSnapshot?.id) {
    organizerLog.ensureForSnapshot(cachedSnapshot);
    refreshView(cachedSnapshot);
  } else {
    refreshView(null);
  }

  await restoreOrganizerSnapshot(cachedSnapshot, isStale);
}








