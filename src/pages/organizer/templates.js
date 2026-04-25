import { t } from '../../utils/i18n.js';
import { escapeHtml } from '../../utils/html.js';
import { MODEL_SELECT_IDS, PROVIDER_SELECT_IDS } from './constants.js';
import { getMoveResultText } from './move-result.js';

export function stripDecorativePrefix(text) {
  return String(text || '').replace(/^[^\p{L}\p{N}]+/u, '').trim();
}

export function renderPipelineStage(stepId, order, label) {
  return `
    <div class="organizer-stage" id="org-stage-${stepId}" data-state="pending">
      <span class="organizer-stage-index">${order}</span>
      <div class="organizer-stage-copy">
        <span class="organizer-stage-title">${escapeHtml(label)}</span>
      </div>
    </div>
  `;
}

export function renderRoutingCard(modality) {
  const label = t(`organizer.model_${modality}`);
  return `
    <div class="organizer-route-card">
      <div class="organizer-route-card-header">
        <span class="organizer-route-chip">${escapeHtml(label)}</span>
      </div>
      <div class="provider-model-inline organizer-route-inputs">
        <select id="${PROVIDER_SELECT_IDS[modality]}" class="form-input"></select>
        <select id="${MODEL_SELECT_IDS[modality]}" class="form-input"></select>
      </div>
    </div>
  `;
}


export function renderOrganizerStatsGrid(animationDelay = '0.09s') {
  return `
    <div class="stats-grid organizer-stats-grid animate-in" style="animation-delay: ${animationDelay};">
      <div class="stat-card organizer-stat-card">
        <span class="stat-label">${t('organizer.total_files')}</span>
        <span class="stat-value" id="org-total">0</span>
      </div>
      <div class="stat-card organizer-stat-card">
        <span class="stat-label">${t('organizer.done_files')}</span>
        <span class="stat-value success" id="org-done">0</span>
      </div>
      <div class="stat-card organizer-stat-card">
        <span class="stat-label">Token</span>
        <span class="stat-value warning" id="org-token">0</span>
      </div>
      <div class="stat-card organizer-stat-card">
        <span class="stat-label">${t('organizer.degraded')}</span>
        <span class="stat-value danger" id="org-degraded">0</span>
      </div>
    </div>
  `;
}

export function renderOrganizerPreviewPanel(animationDelay = '0.13s') {
  return `
    <section id="org-category-tree-card" class="card organizer-panel organizer-tree-panel animate-in" style="animation-delay: ${animationDelay}; padding: 0; overflow: hidden;" hidden>
      <div class="card-header organizer-panel-header">
        <div>
          <h2 class="card-title">${t('organizer.tree_title')}</h2>
        </div>
        <span id="org-category-tree-count" class="badge badge-info">0</span>
      </div>
      <div id="org-category-tree" class="organizer-tree-shell"></div>
    </section>
    <section class="card organizer-panel organizer-preview-panel animate-in" style="animation-delay: ${animationDelay}; padding: 0; overflow: hidden;">
      <div class="card-header organizer-panel-header">
        <div>
          <h2 class="card-title">${t('organizer.preview_title')}</h2>
        </div>
      </div>
      <div id="org-classification-errors" class="organizer-classification-errors" hidden></div>
      <div id="org-preview-groups" class="preview-groups organizer-preview-groups"></div>
      <div id="org-preview-empty" class="empty-state organizer-empty-state" style="padding: 32px;">
        <div class="organizer-empty-glyph" aria-hidden="true"></div>
        <div class="empty-state-text">${t('organizer.preview_empty')}</div>
      </div>
    </section>
  `;
}

export function renderOrganizerMoveResultPanel(animationDelay = '0.17s') {
  return `
    <section id="org-move-result-card" class="card organizer-panel animate-in mt-24" style="animation-delay: ${animationDelay}; padding: 0; overflow: hidden;" hidden>
      <div class="card-header organizer-panel-header">
        <div>
          <h2 class="card-title">${escapeHtml(getMoveResultText('title'))}</h2>
        </div>
      </div>
      <div class="stats-grid organizer-stats-grid organizer-stats-grid-compact" style="padding: 20px 20px 0;">
        <div class="stat-card organizer-stat-card">
          <span class="stat-label">${escapeHtml(getMoveResultText('moved'))}</span>
          <span id="org-move-moved" class="stat-value success">0</span>
        </div>
        <div class="stat-card organizer-stat-card">
          <span class="stat-label">${escapeHtml(getMoveResultText('skipped'))}</span>
          <span id="org-move-skipped" class="stat-value warning">0</span>
        </div>
        <div class="stat-card organizer-stat-card">
          <span class="stat-label">${escapeHtml(getMoveResultText('failed'))}</span>
          <span id="org-move-failed" class="stat-value danger">0</span>
        </div>
        <div class="stat-card organizer-stat-card">
          <span class="stat-label">${escapeHtml(getMoveResultText('total'))}</span>
          <span id="org-move-total" class="stat-value">0</span>
        </div>
      </div>
      <div class="organizer-table-wrap" style="padding: 20px 20px 0;">
        <table class="data-table">
          <thead>
            <tr>
              <th style="width: 10%;">${escapeHtml(getMoveResultText('status'))}</th>
              <th style="width: 32%;">${t('organizer.source')}</th>
              <th style="width: 32%;">${t('organizer.target')}</th>
              <th style="width: 26%;">${escapeHtml(getMoveResultText('reason'))}</th>
            </tr>
          </thead>
          <tbody id="org-move-result-body"></tbody>
        </table>
      </div>
      <div id="org-move-result-empty" class="empty-state organizer-empty-state" style="padding: 24px;">
        <div class="empty-state-text">${escapeHtml(getMoveResultText('empty'))}</div>
      </div>
    </section>
  `;
}

