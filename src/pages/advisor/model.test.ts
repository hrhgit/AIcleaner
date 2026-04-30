import { describe, expect, it, beforeEach } from 'vitest';
import {
  advisorReducer,
  createInitialAdvisorState,
  getOrganizeProgress,
  getOrganizeProgressDetail,
  hasDeterminateOrganizeProgress,
  isOrganizeRunning,
  normalizeOrganizeSnapshot,
} from './model';
import { ADVISOR_PERSIST_KEYS, WORKFLOW_PERSIST_KEYS } from './constants';

describe('advisor workflow model', () => {
  beforeEach(() => {
    localStorage.clear();
  });

  it('restores persisted normal fields from existing storage keys', () => {
    localStorage.setItem(ADVISOR_PERSIST_KEYS.rootPath, JSON.stringify('E:/Downloads'));
    localStorage.setItem(WORKFLOW_PERSIST_KEYS.summaryStrategy, JSON.stringify('local_summary'));
    localStorage.setItem(WORKFLOW_PERSIST_KEYS.useWebSearch, JSON.stringify(true));

    const state = createInitialAdvisorState();

    expect(state.rootPath).toBe('E:/Downloads');
    expect(state.summaryStrategy).toBe('local_summary');
    expect(state.useWebSearch).toBe(true);
  });

  it('persists root path updates into both legacy workflow and advisor keys', () => {
    const next = advisorReducer(createInitialAdvisorState(), { type: 'setRootPath', rootPath: 'E:/Work' });

    expect(next.rootPath).toBe('E:/Work');
    expect(JSON.parse(localStorage.getItem(ADVISOR_PERSIST_KEYS.rootPath) || 'null')).toBe('E:/Work');
    expect(JSON.parse(localStorage.getItem(WORKFLOW_PERSIST_KEYS.rootPath) || 'null')).toBe('E:/Work');
  });

  it('clears stale advisor session when the working directory changes', () => {
    const state = {
      ...createInitialAdvisorState(),
      rootPath: 'E:/Downloads',
      sessionId: 'session-old',
      sessionData: {
        sessionId: 'session-old',
        rootPath: 'E:/Downloads',
        timeline: [],
      },
    };
    localStorage.setItem(ADVISOR_PERSIST_KEYS.sessionId, JSON.stringify('session-old'));

    const next = advisorReducer(state, { type: 'setRootPath', rootPath: 'E:/Work' });

    expect(next.rootPath).toBe('E:/Work');
    expect(next.sessionId).toBe('');
    expect(next.sessionData).toBeNull();
    expect(localStorage.getItem(ADVISOR_PERSIST_KEYS.sessionId)).toBeNull();
  });

  it('normalizes raw backend snapshots into the stable organize view shape', () => {
    const snapshot = normalizeOrganizeSnapshot({
      id: 'org_raw',
      status: 'classifying',
      root_path: 'E:/Downloads',
      total_files: 20,
      processed_files: 4,
      total_batches: 5,
      processed_batches: 1,
      summary_strategy: 'local_summary',
      use_web_search: true,
      web_search_enabled: false,
      progress: {
        stage: 'classification',
        label: 'Classifying batches',
        current: 2,
        total: 5,
        unit: 'batches',
        indeterminate: false,
      },
      tree: { children: [] },
      results: [{ index: 1, name: 'a.txt', categoryPath: ['Docs'] }],
    });

    expect(snapshot).toMatchObject({
      id: 'org_raw',
      rootPath: 'E:/Downloads',
      totalFiles: 20,
      processedFiles: 4,
      totalBatches: 5,
      processedBatches: 1,
      summaryStrategy: 'local_summary',
      useWebSearch: true,
      webSearchEnabled: false,
    });
    expect(snapshot?.progress.current).toBe(2);
    expect(snapshot?.results[0].name).toBe('a.txt');
  });

  it('does not normalize row-level organize events as snapshots', () => {
    expect(normalizeOrganizeSnapshot({
      taskId: 'org_1',
      batchIndex: 1,
      name: 'summary-ready.txt',
      path: 'E:/Downloads/summary-ready.txt',
    })).toBeNull();
    expect(normalizeOrganizeSnapshot({
      taskId: 'org_1',
      index: 1,
      name: 'done.txt',
      path: 'E:/Downloads/done.txt',
      categoryPath: ['Docs'],
    })).toBeNull();
  });

  it('keeps running and progress semantics stable for organizer snapshots', () => {
    const snapshot = { status: 'classifying', totalFiles: 10, processedFiles: 3 };

    expect(isOrganizeRunning(snapshot)).toBe(true);
    expect(getOrganizeProgress(snapshot)).toBe(30);
  });

  it('prefers backend progress over file counters', () => {
    const snapshot = {
      status: 'classifying',
      totalFiles: 10,
      processedFiles: 1,
      progress: {
        stage: 'classification',
        label: 'Classifying batches',
        current: 3,
        total: 4,
        unit: 'batches',
        indeterminate: false,
      },
    };

    expect(hasDeterminateOrganizeProgress(snapshot)).toBe(true);
    expect(getOrganizeProgress(snapshot)).toBe(75);
    expect(getOrganizeProgressDetail(snapshot)).toBe('批次 3/4');
  });

  it('handles indeterminate progress and completed fallback', () => {
    expect(getOrganizeProgress({
      status: 'classifying',
      progress: { stage: 'initial_tree', label: 'Building tree', indeterminate: true },
    })).toBe(0);
    expect(hasDeterminateOrganizeProgress({
      status: 'error',
      progress: { stage: 'error', label: 'Error', detail: 'raw backend error', indeterminate: true },
    })).toBe(false);
    expect(getOrganizeProgress({ status: 'completed', totalFiles: 0, processedFiles: 0 })).toBe(100);
  });
});
