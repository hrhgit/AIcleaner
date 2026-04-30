import { renderToStaticMarkup } from 'react-dom/server';
import { describe, expect, it } from 'vitest';
import type { OrganizeSnapshot } from '../../types';
import { OrganizeSummary } from './AdvisorViewParts';
import type { AdvisorWorkflowState } from './model';

function stateStub(): AdvisorWorkflowState {
  return {
    rootPath: 'E:/Downloads',
    summaryStrategy: 'local_summary',
    useWebSearch: false,
    sessionId: '',
    messageDraft: '',
    sessionData: null,
    organizeTaskId: 'org_test',
    organizeSnapshot: null,
    organizeStream: null,
    loading: false,
    sending: false,
    acting: false,
    organizeStarting: false,
    organizeStopping: false,
    syncingSearch: false,
  };
}

describe('OrganizeSummary', () => {
  it('renders backend batch progress for classification instead of file counter progress', () => {
    const snapshot: OrganizeSnapshot = {
      id: 'org_test',
      status: 'classifying',
      totalFiles: 100,
      processedFiles: 2,
      summaryStrategy: 'local_summary',
      progress: {
        stage: 'classification',
        label: 'Classifying batches',
        current: 3,
        total: 10,
        unit: 'batches',
        indeterminate: false,
      },
      results: [],
      tree: { children: [] },
    };

    const html = renderToStaticMarkup(<OrganizeSummary snapshot={snapshot} state={stateStub()} />);

    expect(html).toContain('分类批次');
    expect(html).toContain('批次 3/10');
    expect(html).toContain('30%');
    expect(html).not.toContain('已处理');
  });

  it('renders task timing and token metrics when available', () => {
    const snapshot: OrganizeSnapshot = {
      id: 'org_metrics',
      status: 'completed',
      totalFiles: 8,
      processedFiles: 8,
      summaryStrategy: 'agent_summary',
      durationMs: 65432,
      timingMs: {
        total: 65432,
        summaryPreparation: 1200,
        initialTree: 2300,
        classification: 61000,
        reconcile: 500,
      },
      tokenUsage: { prompt: 100, completion: 30, total: 130 },
      tokenUsageByStage: {
        summaryPreparation: { prompt: 10, completion: 4, total: 14 },
        initialTree: { prompt: 20, completion: 6, total: 26 },
        classification: { prompt: 60, completion: 18, total: 78 },
        reconcile: { prompt: 10, completion: 2, total: 12 },
      },
      progress: {
        stage: 'completed',
        label: 'Completed',
        current: 1,
        total: 1,
        unit: 'batches',
        indeterminate: false,
      },
      results: [],
      tree: { children: [] },
    };

    const html = renderToStaticMarkup(<OrganizeSummary snapshot={snapshot} state={stateStub()} />);

    expect(html).toContain('总耗时');
    expect(html).toContain('1m 5s');
    expect(html).toContain('总 Token');
    expect(html).toContain('130 / 输入 100 / 输出 30');
    expect(html).toContain('摘要准备');
    expect(html).toContain('78 / 输入 60 / 输出 18');
  });
});
