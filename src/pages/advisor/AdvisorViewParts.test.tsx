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
});
