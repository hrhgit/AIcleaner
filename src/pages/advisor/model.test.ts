import { describe, expect, it, beforeEach } from 'vitest';
import { advisorReducer, createInitialAdvisorState, getOrganizeProgress, isOrganizeRunning } from './model';
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

  it('keeps running and progress semantics stable for organizer snapshots', () => {
    const snapshot = { status: 'classifying', totalFiles: 10, processedFiles: 3 };

    expect(isOrganizeRunning(snapshot)).toBe(true);
    expect(getOrganizeProgress(snapshot)).toBe(30);
  });
});

