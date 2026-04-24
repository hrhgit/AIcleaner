import { escapeHtml } from '../../utils/html.js';
import { getLang, t } from '../../utils/i18n.js';
import { getProviderLabel } from '../../utils/provider-registry.js';
import {
  DEFAULT_BATCH_SIZE,
  DEFAULT_SUMMARY_MODE,
  getPersisted,
  isOrganizerLogCollapsed,
  isOrganizerRecordGroupCollapsed,
  PERSIST_KEYS,
  removePersisted,
  setOrganizerLogCollapsed,
  setOrganizerRecordGroupCollapsed,
  setPersisted,
  SUMMARY_MODES,
} from '../organizer-storage.js';

function organizerText(zh, en) {
  return getLang() === 'en' ? en : zh;
}

function getOrganizerLogTime() {
  return new Date().toLocaleTimeString([], { hour12: false });
}

function getOrganizerSummaryModeLabel(mode) {
  if (mode === 'local_summary') return t('organizer.summary_mode_local_summary');
  if (mode === 'agent_summary') return t('organizer.summary_mode_agent_summary');
  return t('organizer.summary_mode_filename_only');
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

function getOrganizerCategoryLabel(data = {}) {
  if (String(data.reason || '').trim() === 'classification_error' || String(data.classificationError || '').trim()) {
    return organizerText('分类错误', 'Classification Error');
  }
  if (Array.isArray(data.categoryPath) && data.categoryPath.length > 0) {
    return data.categoryPath.map((segment) => String(segment || '').trim()).filter(Boolean).join(' / ');
  }
  return String(data.category || '').trim() || t('organizer.uncategorized');
}

function getOrganizerSummarySourceLabel(source) {
  if (source === 'agent_summary') return organizerText('Agent 摘要', 'Agent Summary');
  if (source === 'agent_fallback_local') return organizerText('Agent 失败后回退本地摘要', 'Agent fallback to local summary');
  if (source === 'local_summary') return organizerText('本地摘要', 'Local Summary');
  if (source === 'filename_only') return organizerText('仅文件名', 'Filename Only');
  return String(source || '').trim() || '-';
}

function normalizeOrganizerLogStringList(value) {
  return Array.isArray(value)
    ? value.map((item) => String(item || '').trim()).filter(Boolean)
    : [];
}

function formatOrganizerRouteLabel(endpoint, model) {
  const providerLabel = getProviderLabel(endpoint);
  return `${providerLabel}/${String(model || '').trim() || '-'}`;
}

function isGroupedOrganizerLogType(type) {
  return ['scanning', 'analyzing', 'found'].includes(type);
}

function getOrganizerLogIcon(type) {
  if (type === 'found') return '+';
  if (type === 'analyzing') return '*';
  if (type === 'agent_call') return '>';
  if (type === 'agent_response') return '<';
  if (type === 'error') return '!';
  return '-';
}

export function createOrganizerLogController({
  getActiveTaskId = () => null,
  getLatestSnapshot = () => null,
} = {}) {
  let entries = [];
  const expandedDetailIds = new Set();
  const loggedRawBatchKeys = new Set();
  const loggedSummaryKeys = new Set();
  let taskId = null;
  let progressBaseline = null;

  function resolveTaskId(fallback = null) {
    return taskId || getActiveTaskId() || getLatestSnapshot()?.id || fallback || null;
  }

  function buildEntry({
    type = 'scanning',
    kind = 'simple',
    summary = '',
    text = '',
    detail = '',
    batchKey = '',
    summaryKey = '',
    taskId: entryTaskId = resolveTaskId(),
  } = {}) {
    const detailText = String(detail || '').trim();
    return {
      id: `${Date.now()}-${Math.random().toString(16).slice(2, 10)}`,
      type,
      kind,
      time: getOrganizerLogTime(),
      taskId: entryTaskId ? String(entryTaskId) : null,
      batchKey: String(batchKey || '').trim() || null,
      summaryKey: String(summaryKey || '').trim() || null,
      summary: String(summary || text || '').trim(),
      text: String(text || summary || '').trim(),
      detailHtml: detailText ? escapeHtml(detailText).replace(/\n/g, '<br>') : '',
    };
  }

  function buildLocalSummaryDetail(row, { category = '-', route = '-' } = {}) {
    if (!row || getOrganizerSummaryStrategy(row) === 'filename_only') return '';
    const representation = getOrganizerRepresentation(row);
    const extraction = row?.localExtraction && typeof row.localExtraction === 'object' ? row.localExtraction : null;
    const extractionKeywords = normalizeOrganizerLogStringList(extraction?.keywords);
    const extractionWarnings = normalizeOrganizerLogStringList(extraction?.warnings);
    const extractionMetadata = normalizeOrganizerLogStringList(extraction?.metadata);
    const summaryKeywords = normalizeOrganizerLogStringList(representation?.keywords);
    const summaryWarnings = normalizeOrganizerLogStringList(row?.warnings ?? row?.summaryWarnings);
    const excerpt = String(extraction?.excerpt || '').trim();
    const metadataSummary = String(representation?.metadata || '').trim();
    const shortSummary = String(representation?.short || '').trim();
    const longSummary = String(representation?.long || '').trim();
    const title = String(extraction?.title || '').trim();
    const parser = String(extraction?.parser || '').trim();
    const confidence = String(representation?.confidence || '').trim();

    if (!extraction && !metadataSummary && !shortSummary && !longSummary && !summaryWarnings.length && !summaryKeywords.length) {
      return '';
    }

    return [
      `${organizerText('文件', 'Item')}: ${String(row?.name || row?.path || '-').trim() || '-'}`,
      `${organizerText('路径', 'Path')}: ${String(row?.path || '-').trim() || '-'}`,
      `${organizerText('分类', 'Category')}: ${category}`,
      `${organizerText('模型', 'Model')}: ${route}`,
      `${organizerText('输入模式', 'Summary Mode')}: ${getOrganizerSummaryModeLabel(getOrganizerSummaryStrategy(row))}`,
      `${organizerText('摘要来源', 'Summary Source')}: ${getOrganizerSummarySourceLabel(representation?.source)}`,
      parser ? `${organizerText('提取器', 'Extractor')}: ${parser}` : '',
      title ? `${organizerText('标题', 'Title')}: ${title}` : '',
      confidence ? `${organizerText('置信度', 'Confidence')}: ${confidence}` : '',
      extractionKeywords.length ? `${organizerText('提取关键词', 'Extraction Keywords')}: ${extractionKeywords.join(', ')}` : '',
      summaryKeywords.length ? `${organizerText('摘要关键词', 'Summary Keywords')}: ${summaryKeywords.join(', ')}` : '',
      extractionWarnings.length ? `${organizerText('提取告警', 'Extraction Warnings')}: ${extractionWarnings.join(' | ')}` : '',
      summaryWarnings.length ? `${organizerText('摘要告警', 'Summary Warnings')}: ${summaryWarnings.join(' | ')}` : '',
      extractionMetadata.length ? `\n${organizerText('提取元数据', 'Extraction Metadata')}:\n${extractionMetadata.join('\n')}` : '',
      excerpt ? `\n${organizerText('本地提取摘录', 'Local Extraction Excerpt')}:\n${excerpt}` : '',
      metadataSummary ? `\n${organizerText('元信息摘要', 'Metadata Summary')}:\n${metadataSummary}` : '',
      shortSummary ? `\n${organizerText('短摘要', 'Short Summary')}:\n${shortSummary}` : '',
      longSummary ? `\n${organizerText('长摘要', 'Long Summary')}:\n${longSummary}` : '',
    ].filter(Boolean).join('\n').trim();
  }

  function syncLoggedKeys() {
    loggedRawBatchKeys.clear();
    loggedSummaryKeys.clear();
    for (const entry of entries) {
      const batchKey = String(entry?.batchKey || '').trim();
      if (batchKey) loggedRawBatchKeys.add(batchKey);
      const summaryKey = String(entry?.summaryKey || '').trim();
      if (summaryKey) loggedSummaryKeys.add(summaryKey);
    }
  }

  function buildSummaryKey(row, fallbackTaskId = resolveTaskId()) {
    const resolvedTaskId = String(fallbackTaskId || row?.taskId || '').trim();
    const pathKey = String(row?.path || row?.relativePath || row?.name || '').trim();
    if (!resolvedTaskId || !pathKey) return '';
    return `${resolvedTaskId}::summary::${pathKey}`;
  }

  function setDetailExpanded(entryId, expanded) {
    if (entryId == null) return;
    if (expanded) expandedDetailIds.add(entryId);
    else expandedDetailIds.delete(entryId);
  }

  function isPinnedToBottom(log) {
    if (!log) return true;
    return (log.scrollHeight - log.scrollTop - log.clientHeight) <= 24;
  }

  function refreshPanel() {
    const collapsed = isOrganizerLogCollapsed();
    const panel = document.getElementById('org-log-panel');
    const toggleBtn = document.getElementById('org-toggle-log-btn');
    const hint = document.getElementById('org-log-collapsed-hint');
    const hasPreview = entries.length > 0;
    if (panel) {
      panel.classList.toggle('is-collapsed', collapsed);
      panel.classList.toggle('is-clickable-preview', collapsed && hasPreview);
      panel.setAttribute('aria-expanded', collapsed ? 'false' : 'true');
      if (collapsed && hasPreview) {
        panel.setAttribute('role', 'button');
        panel.setAttribute('tabindex', '0');
        panel.setAttribute('aria-label', t('organizer.log_preview_hint'));
      } else {
        panel.removeAttribute('role');
        panel.removeAttribute('tabindex');
        panel.removeAttribute('aria-label');
      }
    }
    if (toggleBtn) {
      toggleBtn.textContent = collapsed ? t('organizer.log_expand') : t('organizer.log_collapse');
      toggleBtn.setAttribute('aria-expanded', collapsed ? 'false' : 'true');
    }
    if (hint) {
      hint.style.display = collapsed && hasPreview ? '' : 'none';
    }
  }

  function createSimpleElement(entry) {
    const el = document.createElement('div');
    el.className = `scan-log-entry ${entry.type}`;
    el.innerHTML = `
      <span class="log-icon">${getOrganizerLogIcon(entry.type)}</span>
      <span class="log-time" style="color: var(--text-muted); margin-right: 6px;">[${entry.time}]</span>
      <span class="log-text">${entry.text}</span>
    `;
    return el;
  }

  function createDetailElement(entry) {
    const expanded = expandedDetailIds.has(entry.id);
    const wrapper = document.createElement('div');
    wrapper.className = `scan-log-entry ${entry.type}`;
    wrapper.innerHTML = `
      <span class="log-icon">${getOrganizerLogIcon(entry.type)}</span>
      <div class="log-content">
        <div class="log-detail-header" style="cursor: pointer; user-select: none; display: flex; align-items: center; gap: 6px;">
          <span class="log-time" style="color: var(--text-muted); margin-right: 4px;">[${entry.time}]</span>
          <span class="log-detail-arrow" style="transition: transform 0.2s; display: inline-block; font-size: 0.65rem; transform: ${expanded ? 'rotate(90deg)' : 'rotate(0deg)'};">></span>
          <span class="log-summary">${entry.summary}</span>
        </div>
        <div class="log-detail-body" style="display: ${expanded ? 'block' : 'none'}; margin-top: 8px; padding: 10px 12px; background: rgba(0,0,0,0.35); border-radius: 6px; border: 1px solid rgba(255,255,255,0.06); font-size: 0.72rem; line-height: 1.7; word-break: break-all; white-space: pre-wrap; max-height: 600px; overflow-y: auto; color: var(--text-secondary);">
          ${entry.detailHtml}
        </div>
      </div>
    `;

    const header = wrapper.querySelector('.log-detail-header');
    const body = wrapper.querySelector('.log-detail-body');
    const arrow = wrapper.querySelector('.log-detail-arrow');
    header?.addEventListener('click', () => {
      if (!body || !arrow) return;
      const nextOpen = body.style.display === 'none';
      setDetailExpanded(entry.id, nextOpen);
      body.style.display = nextOpen ? 'block' : 'none';
      arrow.style.transform = nextOpen ? 'rotate(90deg)' : 'rotate(0deg)';
    });

    return wrapper;
  }

  function createRecordGroupElement(groupedEntries) {
    const wrapper = document.createElement('div');
    wrapper.className = `scan-log-group${isOrganizerRecordGroupCollapsed() ? ' is-collapsed' : ''}`;

    const header = document.createElement('div');
    header.className = 'scan-log-group-header';
    header.innerHTML = `
      <div class="scan-log-group-title">
        <span class="scan-log-group-arrow">></span>
        <span>${t('organizer.log_records')} (${groupedEntries.length})</span>
      </div>
    `;

    const body = document.createElement('div');
    body.className = 'scan-log-group-body';
    for (const entry of groupedEntries) {
      body.appendChild(createSimpleElement(entry));
    }

    header.addEventListener('click', () => {
      const nextCollapsed = !wrapper.classList.contains('is-collapsed');
      setOrganizerRecordGroupCollapsed(nextCollapsed);
      wrapper.classList.toggle('is-collapsed', nextCollapsed);
    });

    wrapper.appendChild(header);
    wrapper.appendChild(body);
    return wrapper;
  }

  function applyScrollPosition(log, { shouldStickToBottom, insertMode, previousScrollTop, previousScrollHeight }) {
    if (shouldStickToBottom) {
      log.scrollTop = log.scrollHeight;
      return;
    }

    if (insertMode === 'top') {
      const heightDelta = Math.max(0, log.scrollHeight - previousScrollHeight);
      log.scrollTop = previousScrollTop + heightDelta;
      return;
    }

    const maxScrollTop = Math.max(0, log.scrollHeight - log.clientHeight);
    log.scrollTop = Math.min(previousScrollTop, maxScrollTop);
  }

  function updateRecordGroupHeader(wrapper, count) {
    if (!wrapper) return;
    const titleText = wrapper.querySelector('.scan-log-group-title span:last-child');
    if (titleText) {
      titleText.textContent = `${t('organizer.log_records')} (${count})`;
    }
  }

  function ensureRecordGroup(log) {
    if (!log) return null;
    let group = log.querySelector('.scan-log-group');
    if (group) return group;

    group = createRecordGroupElement([]);
    log.prepend(group);
    return group;
  }

  function renderEntries(insertMode = 'reset') {
    const log = document.getElementById('org-log');
    if (!log) return;
    const shouldStickToBottom = isPinnedToBottom(log);
    const previousScrollTop = log.scrollTop;
    const previousScrollHeight = log.scrollHeight;

    log.innerHTML = '';
    const groupedEntries = entries.filter((entry) => isGroupedOrganizerLogType(entry.type));
    const detailEntries = entries.filter((entry) => !isGroupedOrganizerLogType(entry.type));

    if (groupedEntries.length > 0) {
      log.appendChild(createRecordGroupElement(groupedEntries));
    }

    for (const entry of detailEntries) {
      log.appendChild(entry.kind === 'detail' ? createDetailElement(entry) : createSimpleElement(entry));
    }

    applyScrollPosition(log, {
      shouldStickToBottom,
      insertMode,
      previousScrollTop,
      previousScrollHeight,
    });
  }

  function replaceEntries(nextEntries = [], { persist = true, taskId: nextTaskId = resolveTaskId() } = {}) {
    entries = Array.isArray(nextEntries) ? nextEntries : [];
    taskId = nextTaskId ? String(nextTaskId) : null;
    expandedDetailIds.clear();
    syncLoggedKeys();
    if (persist) {
      setPersisted(PERSIST_KEYS.logEntries, entries);
      if (taskId) setPersisted(PERSIST_KEYS.logTaskId, taskId);
      else removePersisted(PERSIST_KEYS.logTaskId);
    }

    const logEl = document.getElementById('org-log');
    if (logEl) {
      if (entries.length > 0) renderEntries();
      else logEl.innerHTML = '';
    }
    refreshPanel();
  }

  function appendEntry(entry, { persist = true } = {}) {
    if (!entry) return;
    entries = [...entries, entry];
    taskId = entry.taskId ? String(entry.taskId) : (taskId || null);
    if (entry.batchKey) loggedRawBatchKeys.add(String(entry.batchKey));
    if (entry.summaryKey) loggedSummaryKeys.add(String(entry.summaryKey));
    if (persist) {
      setPersisted(PERSIST_KEYS.logEntries, entries);
      if (taskId) setPersisted(PERSIST_KEYS.logTaskId, taskId);
    }

    const log = document.getElementById('org-log');
    if (!log) {
      refreshPanel();
      return;
    }

    const shouldStickToBottom = isPinnedToBottom(log);
    const previousScrollTop = log.scrollTop;
    const previousScrollHeight = log.scrollHeight;
    const insertMode = isGroupedOrganizerLogType(entry.type) ? 'top' : 'bottom';

    if (isGroupedOrganizerLogType(entry.type)) {
      const group = ensureRecordGroup(log);
      const body = group?.querySelector('.scan-log-group-body');
      if (body) body.appendChild(createSimpleElement(entry));
      updateRecordGroupHeader(group, entries.filter((item) => isGroupedOrganizerLogType(item.type)).length);
    } else {
      log.appendChild(entry.kind === 'detail' ? createDetailElement(entry) : createSimpleElement(entry));
    }

    applyScrollPosition(log, {
      shouldStickToBottom,
      insertMode,
      previousScrollTop,
      previousScrollHeight,
    });
    refreshPanel();
  }

  function bindPanelEvents() {
    document.getElementById('org-toggle-log-btn')?.addEventListener('click', () => {
      setOrganizerLogCollapsed(!isOrganizerLogCollapsed());
      refreshPanel();
    });
    document.getElementById('org-log-panel')?.addEventListener('click', (event) => {
      if (!isOrganizerLogCollapsed() || !entries.length) return;
      const target = event.target;
      if (target instanceof Element && target.closest('button, a, input, textarea, select, label')) return;
      setOrganizerLogCollapsed(false);
      refreshPanel();
    });
    document.getElementById('org-log-panel')?.addEventListener('keydown', (event) => {
      if (!isOrganizerLogCollapsed() || !entries.length) return;
      if (event.key !== 'Enter' && event.key !== ' ') return;
      event.preventDefault();
      setOrganizerLogCollapsed(false);
      refreshPanel();
    });
    document.getElementById('org-clear-log-btn')?.addEventListener('click', () => {
      replaceEntries([], { persist: true, taskId });
    });
  }

  function mountPanel() {
    bindPanelEvents();
    if (entries.length > 0) renderEntries();
    refreshPanel();
  }

  function syncProgressBaseline(snapshot) {
    if (!snapshot?.id) {
      progressBaseline = null;
      return;
    }
    progressBaseline = {
      taskId: String(snapshot.id),
      status: String(snapshot.status || 'idle'),
      processedFiles: Number(snapshot.processedFiles || 0),
      processedBatches: Number(snapshot.processedBatches || 0),
      totalBatches: Number(snapshot.totalBatches || 0),
      tokenTotal: Number(snapshot.tokenUsage?.total || 0),
    };
  }

  function restoreState(snapshot = null) {
    const rawEntries = getPersisted(PERSIST_KEYS.logEntries, []);
    entries = Array.isArray(rawEntries) ? rawEntries : [];
    taskId = String(getPersisted(PERSIST_KEYS.logTaskId, '') || '').trim() || null;
    expandedDetailIds.clear();
    syncLoggedKeys();
    syncProgressBaseline(snapshot);
  }

  function buildBatchRawOutputEntries(results = [], fallbackTaskId = taskId) {
    const grouped = new Map();
    for (const row of Array.isArray(results) ? results : []) {
      const batchIndex = Number(row?.batchIndex || 0);
      const rawOutput = String(row?.modelRawOutput || '').trim();
      const classificationError = String(row?.classificationError || '').trim();
      if (!batchIndex || (!rawOutput && !classificationError)) continue;
      if (!grouped.has(batchIndex)) {
        grouped.set(batchIndex, {
          batchIndex,
          taskId: String(row?.taskId || fallbackTaskId || '').trim() || null,
          route: formatOrganizerRouteLabel(row?.provider, row?.model),
          rawOutput,
          classificationError,
          names: [],
        });
      }
      const item = grouped.get(batchIndex);
      const name = String(row?.name || row?.path || '').trim();
      if (name) item.names.push(name);
      if (!item.rawOutput && rawOutput) item.rawOutput = rawOutput;
      if (!item.classificationError && classificationError) item.classificationError = classificationError;
    }

    return Array.from(grouped.values())
      .sort((a, b) => a.batchIndex - b.batchIndex)
      .map((item) => {
        const detail = [
          `${organizerText('批次', 'Batch')}: ${item.batchIndex}`,
          `${organizerText('模型', 'Model')}: ${item.route}`,
          item.names.length ? `${organizerText('文件', 'Items')}: ${item.names.join(', ')}` : '',
          item.classificationError ? `${organizerText('分类错误', 'Classification Error')}: ${item.classificationError}` : '',
          '',
          item.rawOutput || organizerText('模型没有返回可记录的 HTTP 原始响应。', 'The model did not return any recordable HTTP raw response.'),
        ].filter(Boolean).join('\n');
        return buildEntry({
          type: item.classificationError ? 'error' : 'agent_response',
          kind: 'detail',
          summary: organizerText(`批次 ${item.batchIndex} HTTP 原始响应`, `Batch ${item.batchIndex} HTTP raw response`),
          detail,
          batchKey: `${item.taskId || ''}::${item.batchIndex}`,
          taskId: item.taskId,
        });
      });
  }

  function ensureForSnapshot(snapshot) {
    const snapshotTaskId = String(snapshot?.id || '').trim();
    if (!snapshotTaskId) return;
    if (entries.length > 0 && taskId === snapshotTaskId) return;

    const summary = organizerText('已恢复最近一次归类任务', 'Restored the most recent organize task');
    const detail = [
      `${organizerText('状态', 'Status')}: ${String(snapshot.status || 'idle')}`,
      `${organizerText('目录', 'Root')}: ${snapshot.rootPath || '-'}`,
      `${organizerText('文件', 'Files')}: ${Number(snapshot.processedFiles || 0)}/${Number(snapshot.totalFiles || 0)}`,
      `${organizerText('批次', 'Batches')}: ${Number(snapshot.processedBatches || 0)}/${Number(snapshot.totalBatches || 0)}`,
      `${organizerText('Token', 'Token')}: ${Number(snapshot.tokenUsage?.total || 0).toLocaleString()}`,
    ].join('\n');
    replaceEntries([
      buildEntry({
        type: 'agent_response',
        kind: 'detail',
        summary,
        detail,
        taskId: snapshotTaskId,
      }),
      ...buildBatchRawOutputEntries(snapshot?.results, snapshotTaskId),
    ], { persist: true, taskId: snapshotTaskId });
  }

  function recordStart(form, nextTaskId, capability) {
    const textRoute = form?.modelRouting?.text || {};
    const selectedProviders = capability?.selectedProviders || {};
    const selectedModels = capability?.selectedModels || {};
    const detail = [
      `${organizerText('目录', 'Root')}: ${form.rootPath || '-'}`,
      `${organizerText('批大小', 'Batch Size')}: ${Number(form.batchSize || DEFAULT_BATCH_SIZE)}`,
      `${organizerText('聚类深度', 'Cluster Depth')}: ${form.maxClusterDepth == null ? organizerText('不限', 'Unlimited') : Number(form.maxClusterDepth)}`,
      `${organizerText('输入模式', 'Summary Mode')}: ${getOrganizerSummaryModeLabel(form.summaryStrategy)}`,
      `${organizerText('联网搜索', 'Web Search')}: ${form.useWebSearch ? organizerText('开启', 'Enabled') : organizerText('关闭', 'Disabled')}`,
      `${organizerText('文本路由', 'Text Route')}: ${formatOrganizerRouteLabel(textRoute.endpoint || selectedProviders.text, textRoute.model || selectedModels.text)}`,
    ].join('\n');

    replaceEntries([], { persist: true, taskId: nextTaskId });
    appendEntry(buildEntry({
      type: 'agent_call',
      kind: 'detail',
      summary: organizerText('开始归类任务', 'Organize task started'),
      detail,
      taskId: nextTaskId,
    }));
  }

  function recordProgress(snapshot) {
    if (!snapshot?.id) return;
    const previous = progressBaseline;
    const current = {
      taskId: String(snapshot.id),
      status: String(snapshot.status || 'idle'),
      processedFiles: Number(snapshot.processedFiles || 0),
      processedBatches: Number(snapshot.processedBatches || 0),
      totalBatches: Number(snapshot.totalBatches || 0),
      tokenTotal: Number(snapshot.tokenUsage?.total || 0),
    };

    if (!previous || previous.taskId !== current.taskId) {
      appendEntry(buildEntry({
        type: current.status === 'classifying' ? 'analyzing' : 'scanning',
        text: organizerText(
          `任务状态: ${current.status} | 文件 ${current.processedFiles}/${Number(snapshot.totalFiles || 0)} | 批次 ${current.processedBatches}/${current.totalBatches} | Token ${current.tokenTotal.toLocaleString()}`,
          `Task status: ${current.status} | Files ${current.processedFiles}/${Number(snapshot.totalFiles || 0)} | Batches ${current.processedBatches}/${current.totalBatches} | Token ${current.tokenTotal.toLocaleString()}`
        ),
        taskId: current.taskId,
      }));
      return;
    }

    if (previous.status !== current.status) {
      appendEntry(buildEntry({
        type: current.status === 'classifying' ? 'analyzing' : 'scanning',
        text: organizerText(`阶段切换为 ${current.status}`, `Stage changed to ${current.status}`),
        taskId: current.taskId,
      }));
    }

    if (previous.processedBatches !== current.processedBatches || previous.tokenTotal !== current.tokenTotal) {
      appendEntry(buildEntry({
        type: current.status === 'classifying' ? 'analyzing' : 'scanning',
        text: organizerText(
          `批次 ${current.processedBatches}/${current.totalBatches} | 已处理 ${current.processedFiles}/${Number(snapshot.totalFiles || 0)} | Token ${current.tokenTotal.toLocaleString()}`,
          `Batches ${current.processedBatches}/${current.totalBatches} | Processed ${current.processedFiles}/${Number(snapshot.totalFiles || 0)} | Token ${current.tokenTotal.toLocaleString()}`
        ),
        taskId: current.taskId,
      }));
    }
  }

  function recordFileDone(row) {
    if (!row) return;
    const rowTaskId = String(row.taskId || resolveTaskId() || '').trim() || null;
    const batchIndex = Number(row.batchIndex || 0);
    const batchKey = batchIndex ? `${rowTaskId || ''}::${batchIndex}` : '';
    const summaryKey = buildSummaryKey(row, rowTaskId);
    const category = getOrganizerCategoryLabel(row);
    const route = formatOrganizerRouteLabel(row.provider, row.model);
    const degradedText = row.degraded ? ` | ${organizerText('降级', 'Degraded')}` : '';
    const classificationError = String(row.classificationError || '').trim();
    const isClassificationErrorRow =
      String(row.reason || '').trim() === 'classification_error' || !!classificationError;
    const isFallbackBatchRow =
      String(row.reason || '').trim() === 'fallback_uncategorized'
      && (!!classificationError || (batchKey && loggedRawBatchKeys.has(batchKey)));
    if (batchKey && !loggedRawBatchKeys.has(batchKey)) {
      const rawOutput = String(row.modelRawOutput || '').trim();
      if (rawOutput || classificationError) {
        const detail = [
          `${organizerText('批次', 'Batch')}: ${batchIndex}`,
          `${organizerText('模型', 'Model')}: ${route}`,
          classificationError ? `${organizerText('分类错误', 'Classification Error')}: ${classificationError}` : '',
          classificationError ? organizerText('该批次未拿到最终分类结果，下面显示的是中间输出或可记录的原始响应。', 'This batch did not produce a final classification result. The content below is intermediate output or the raw response we managed to record.') : '',
          '',
          rawOutput || organizerText('模型没有返回可记录的 HTTP 原始响应。', 'The model did not return any recordable HTTP raw response.'),
        ].filter(Boolean).join('\n');
        appendEntry(buildEntry({
          type: classificationError ? 'error' : 'agent_response',
          kind: 'detail',
          summary: organizerText(`批次 ${batchIndex} HTTP 原始响应`, `Batch ${batchIndex} HTTP raw response`),
          detail,
          batchKey,
          taskId: rowTaskId,
        }));
      }
    }
    const localSummaryDetail = buildLocalSummaryDetail(row, { category, route });
    if (localSummaryDetail && (!summaryKey || !loggedSummaryKeys.has(summaryKey))) {
      appendEntry(buildEntry({
        type: 'agent_response',
        kind: 'detail',
        summary: organizerText(
          `本地摘要 | ${row.name || row.path || '-'}`,
          `Local summary | ${row.name || row.path || '-'}`,
        ),
        detail: localSummaryDetail,
        taskId: rowTaskId,
        summaryKey,
      }));
    }
    if (isFallbackBatchRow) return;
    appendEntry(buildEntry({
      type: isClassificationErrorRow ? 'error' : 'found',
      text: organizerText(
        `${row.name || row.path || '-'} -> ${category} | ${route}${degradedText}`,
        `${row.name || row.path || '-'} -> ${category} | ${route}${degradedText}`
      ),
      taskId: rowTaskId,
    }));
  }

  function recordTerminal(snapshot, kind, message, detail) {
    appendEntry(buildEntry({
      type: kind === 'error' ? 'error' : 'agent_response',
      kind: 'detail',
      summary: message,
      detail,
      taskId: snapshot?.id || resolveTaskId() || null,
    }));
  }

  function recordSummaryReady(row) {
    if (!row) return;
    const rowTaskId = String(row.taskId || resolveTaskId() || '').trim() || null;
    const route = formatOrganizerRouteLabel(row.provider, row.model);
    const summaryKey = buildSummaryKey(row, rowTaskId);
    if (summaryKey && loggedSummaryKeys.has(summaryKey)) return;
    const detail = buildLocalSummaryDetail(row, {
      category: organizerText('待分类', 'Pending Classification'),
      route,
    });
    if (!detail) return;
    appendEntry(buildEntry({
      type: 'agent_response',
      kind: 'detail',
      summary: organizerText(
        `分类前摘要 | ${row.name || row.path || '-'}`,
        `Pre-classification summary | ${row.name || row.path || '-'}`,
      ),
      detail,
      taskId: rowTaskId,
      summaryKey,
    }));
  }

  return {
    ensureForSnapshot,
    hasEntries: () => entries.length > 0,
    mountPanel,
    recordFileDone,
    recordProgress,
    recordStart,
    recordSummaryReady,
    recordTerminal,
    refreshPanel,
    renderEntries,
    restoreState,
    syncProgressBaseline,
  };
}
