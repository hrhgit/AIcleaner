import { useCallback, useEffect, useReducer, useRef } from 'react';
import {
  advisorCardAction,
  advisorMessageSend,
  advisorSessionGet,
  advisorSessionStart,
  browseFolder,
  connectOrganizeStream,
  getLatestOrganizeResult,
  getOrganizeResult,
  getSettings,
  saveSettings,
  startOrganize,
  stopOrganize,
} from '../../utils/api';
import { getErrorMessage } from '../../utils/errors';
import { getLang, text } from '../../utils/i18n';
import { ensureRequiredCredentialsConfigured } from '../../utils/secret-ui';
import { showToast } from '../../utils/toast';
import type { OrganizeSnapshot, OrganizeViewSnapshot, StreamHandle } from '../../types';
import {
  ADVISOR_PERSIST_KEYS,
  DEFAULT_BATCH_SIZE,
  DEFAULT_EXCLUSIONS,
  WORKFLOW_PERSIST_KEYS,
} from './constants';
import {
  advisorReducer,
  createInitialAdvisorState,
  getCurrentSnapshot,
  getStageLabel,
  isOrganizeRunning,
  sanitizeSnapshot,
} from './model';
import { removePersisted, writePersisted } from '../../utils/storage';
import { AdvisorTimeline, ContextSummary, OrganizePanel } from './AdvisorViewParts';

export function AdvisorPage() {
  const [state, dispatch] = useReducer(advisorReducer, undefined, createInitialAdvisorState);
  const stateRef = useRef(state);
  const composerRef = useRef<HTMLElement | null>(null);

  useEffect(() => {
    stateRef.current = state;
  }, [state]);

  const closeOrganizeStream = useCallback(() => {
    const current = stateRef.current.organizeStream;
    if (!current) return;
    try {
      current.close();
    } catch {
      // listener cleanup is best effort
    }
    dispatch({ type: 'setOrganizeStream', stream: null });
  }, []);

  const applyOrganizeSnapshot = useCallback((snapshot: OrganizeSnapshot | OrganizeViewSnapshot | null) => {
    const nextSnapshot = sanitizeSnapshot(snapshot);
    if (!nextSnapshot) return;
    dispatch({ type: 'setOrganizeSnapshot', snapshot: nextSnapshot });
    dispatch({ type: 'setOrganizeTaskId', taskId: String(nextSnapshot.id || stateRef.current.organizeTaskId || '') });
    if (!isOrganizeRunning(nextSnapshot)) {
      closeOrganizeStream();
    }
  }, [closeOrganizeStream]);

  const connectTaskStream = useCallback((taskId: string) => {
    closeOrganizeStream();
    if (!taskId) return;
    const stream: StreamHandle = connectOrganizeStream(taskId, {
      onProgress: (snapshot) => applyOrganizeSnapshot(snapshot),
      onDone: (snapshot) => {
        applyOrganizeSnapshot(snapshot);
        showToast(text('归类完成，可以开始对话。', 'Organize finished. You can start the conversation now.'), 'success');
      },
      onError: (payload) => {
        if (payload.snapshot) applyOrganizeSnapshot(payload.snapshot);
        showToast(`${text('归类失败: ', 'Organize failed: ')}${payload.message || text('未知错误', 'Unknown error')}`, 'error');
      },
      onStopped: (snapshot) => {
        applyOrganizeSnapshot(snapshot);
        showToast(text('归类任务已停止。', 'The organize task has been stopped.'), 'info');
      },
    });
    dispatch({ type: 'setOrganizeStream', stream });
  }, [applyOrganizeSnapshot, closeOrganizeStream]);

  const hydrateOrganizeSnapshot = useCallback(async (taskId: string, reconnect = true) => {
    if (!taskId) return;
    try {
      const snapshot = await getOrganizeResult(taskId);
      applyOrganizeSnapshot(snapshot);
      if (reconnect && isOrganizeRunning(snapshot)) connectTaskStream(taskId);
    } catch (err) {
      if (stateRef.current.organizeTaskId === taskId) {
        dispatch({ type: 'setOrganizeTaskId', taskId: '' });
      }
      console.warn('[Advisor] Failed to hydrate organize snapshot:', err);
    }
  }, [applyOrganizeSnapshot, connectTaskStream]);

  const hydrateLatestOrganizeSnapshot = useCallback(async (reconnect = true) => {
    const rootPath = String(stateRef.current.rootPath || '').trim();
    if (!rootPath) return false;
    try {
      const snapshot = sanitizeSnapshot(await getLatestOrganizeResult(rootPath));
      if (!snapshot) return false;
      applyOrganizeSnapshot(snapshot);
      if (reconnect && isOrganizeRunning(snapshot)) {
        connectTaskStream(String(snapshot.id || stateRef.current.organizeTaskId || ''));
      }
      return true;
    } catch (err) {
      console.warn('[Advisor] Failed to hydrate latest organize snapshot:', err);
      return false;
    }
  }, [applyOrganizeSnapshot, connectTaskStream]);

  const hydrateSession = useCallback(async (sessionId: string) => {
    if (!sessionId) return;
    dispatch({ type: 'patch', patch: { loading: true } });
    try {
      const sessionData = await advisorSessionGet(sessionId);
      dispatch({ type: 'setSession', sessionData, sessionId: String(sessionData.sessionId || sessionId) });
    } finally {
      dispatch({ type: 'patch', patch: { loading: false } });
    }
  }, []);

  useEffect(() => {
    let cancelled = false;
    async function bootstrap() {
      try {
        const settings = await getSettings();
        const searchApi = settings.searchApi || {};
        const scopes = searchApi.scopes || {};
        const workflowEnabled = !!(searchApi.enabled || scopes.classify || scopes.organizer);
        dispatch({ type: 'setUseWebSearch', useWebSearch: workflowEnabled });
      } catch {
        // keep persisted fallback
      }

      const refreshedLatest = await hydrateLatestOrganizeSnapshot(true);
      if (!cancelled && !refreshedLatest) {
        dispatch({ type: 'setOrganizeTaskId', taskId: '' });
        dispatch({ type: 'setOrganizeSnapshot', snapshot: null });
      }

      const sessionId = stateRef.current.sessionId;
      if (sessionId && !cancelled) {
        try {
          await hydrateSession(sessionId);
          return;
        } catch {
          removePersisted(ADVISOR_PERSIST_KEYS.sessionId);
          dispatch({ type: 'clearSession' });
        }
      }
    }
    bootstrap();
    return () => {
      cancelled = true;
      closeOrganizeStream();
    };
  }, [closeOrganizeStream, hydrateLatestOrganizeSnapshot, hydrateSession]);

  const ensureWorkflowCredentials = useCallback(async (requireSearchApi: boolean) => {
    const settings = await getSettings({ force: true });
    const defaultProviderEndpoint = String(settings.defaultProviderEndpoint || '').trim() || 'https://api.openai.com/v1';
    await ensureRequiredCredentialsConfigured({
      providerEndpoints: [defaultProviderEndpoint],
      requireSearchApi,
      reasonText: text('缺少 API Key。', 'API key required.'),
    });
  }, []);

  const scrollComposerIntoView = useCallback(() => {
    window.setTimeout(() => {
      composerRef.current?.scrollIntoView?.({ behavior: 'smooth', block: 'end' });
    }, 30);
  }, []);

  const handleBrowse = useCallback(async () => {
    try {
      const picked = await browseFolder();
      if (picked.cancelled || !picked.path) return;
      dispatch({ type: 'setRootPath', rootPath: picked.path });
    } catch (err) {
      showToast(`${text('选择目录失败: ', 'Failed to select folder: ')}${getErrorMessage(err)}`, 'error');
    }
  }, []);

  const handleStartOrganize = useCallback(async () => {
    const current = stateRef.current;
    if (!current.rootPath.trim()) {
      showToast(text('请先选择目录', 'Select a folder first'), 'error');
      return;
    }
    try {
      await ensureWorkflowCredentials(current.useWebSearch);
      dispatch({ type: 'patch', patch: { organizeStarting: true } });
      const result = await startOrganize({
        rootPath: current.rootPath.trim(),
        excludedPatterns: DEFAULT_EXCLUSIONS,
        batchSize: DEFAULT_BATCH_SIZE,
        summaryStrategy: current.summaryStrategy,
        useWebSearch: current.useWebSearch,
        responseLanguage: getLang(),
      });
      const taskId = String(result.taskId || '').trim();
      dispatch({ type: 'setOrganizeTaskId', taskId });
      dispatch({
        type: 'setOrganizeSnapshot',
        snapshot: {
          id: taskId,
          status: 'idle',
          rootPath: current.rootPath.trim(),
          excludedPatterns: DEFAULT_EXCLUSIONS,
          batchSize: DEFAULT_BATCH_SIZE,
          summaryStrategy: current.summaryStrategy,
          useWebSearch: current.useWebSearch,
          webSearchEnabled: current.useWebSearch,
          totalFiles: 0,
          processedFiles: 0,
          tree: { children: [] },
        },
      });
      connectTaskStream(taskId);
      await hydrateOrganizeSnapshot(taskId, true);
      showToast(text('归类任务已启动。', 'Organize task started.'), 'success');
    } catch (err) {
      showToast(`${text('启动归类失败: ', 'Failed to start organize: ')}${getErrorMessage(err)}`, 'error');
    } finally {
      dispatch({ type: 'patch', patch: { organizeStarting: false } });
    }
  }, [connectTaskStream, ensureWorkflowCredentials, hydrateOrganizeSnapshot]);

  const handleStopOrganize = useCallback(async () => {
    const taskId = stateRef.current.organizeTaskId;
    if (!taskId) return;
    dispatch({ type: 'patch', patch: { organizeStopping: true } });
    try {
      await stopOrganize(taskId);
    } catch (err) {
      showToast(`${text('停止归类失败: ', 'Failed to stop organize: ')}${getErrorMessage(err)}`, 'error');
    } finally {
      dispatch({ type: 'patch', patch: { organizeStopping: false } });
    }
  }, []);

  const handleStartSession = useCallback(async () => {
    const current = stateRef.current;
    if (!current.rootPath.trim()) {
      showToast(text('请先选择目录', 'Select a folder first'), 'error');
      return;
    }
    try {
      await ensureWorkflowCredentials(current.useWebSearch);
      dispatch({ type: 'patch', patch: { loading: true } });
      const sessionData = await advisorSessionStart({
        rootPath: current.rootPath.trim(),
        responseLanguage: getLang(),
      });
      dispatch({ type: 'setSession', sessionData, sessionId: String(sessionData.sessionId || '') });
    } catch (err) {
      showToast(`${text('启动会话失败: ', 'Failed to start session: ')}${getErrorMessage(err)}`, 'error');
    } finally {
      dispatch({ type: 'patch', patch: { loading: false } });
      scrollComposerIntoView();
    }
  }, [ensureWorkflowCredentials, scrollComposerIntoView]);

  const handleSend = useCallback(async () => {
    const current = stateRef.current;
    if (current.sending || !current.sessionId || !current.messageDraft.trim()) return;
    const message = current.messageDraft.trim();
    dispatch({ type: 'patch', patch: { sending: true } });
    dispatch({ type: 'setMessageDraft', messageDraft: '' });
    dispatch({ type: 'appendPendingMessage', message });
    scrollComposerIntoView();
    try {
      const sessionData = await advisorMessageSend({ sessionId: current.sessionId, message });
      dispatch({ type: 'setSession', sessionData, sessionId: String(sessionData.sessionId || current.sessionId) });
    } catch (err) {
      const errorMessage = getErrorMessage(err);
      showToast(`${text('发送失败: ', 'Send failed: ')}${errorMessage}`, 'error');
      try {
        const sessionData = await advisorSessionGet(current.sessionId);
        dispatch({ type: 'setSession', sessionData, sessionId: current.sessionId });
      } catch (hydrateErr) {
        console.warn('[Advisor] Failed to refresh session after send failure:', hydrateErr);
        dispatch({ type: 'markPendingAssistantFailed', message: `${text('回复生成失败: ', 'Reply failed: ')}${errorMessage}` });
      }
    } finally {
      dispatch({ type: 'patch', patch: { sending: false } });
      scrollComposerIntoView();
    }
  }, [scrollComposerIntoView]);

  const handleCardAction = useCallback(async (cardId: string, action: string) => {
    const current = stateRef.current;
    if (!current.sessionId || !action) return;
    dispatch({ type: 'patch', patch: { acting: true } });
    try {
      const payload = action === 'toggle_context_bar'
        ? { collapsed: !current.sessionData?.contextBar?.collapsed }
        : undefined;
      const sessionData = await advisorCardAction({
        sessionId: current.sessionId,
        cardId: cardId || '',
        action,
        payload,
      });
      dispatch({ type: 'setSession', sessionData, sessionId: current.sessionId });
    } catch (err) {
      showToast(`${text('卡片动作失败: ', 'Card action failed: ')}${getErrorMessage(err)}`, 'error');
    } finally {
      dispatch({ type: 'patch', patch: { acting: false } });
      scrollComposerIntoView();
    }
  }, [scrollComposerIntoView]);

  const handleWebSearchChange = useCallback(async (nextValue: boolean) => {
    dispatch({ type: 'patch', patch: { syncingSearch: true } });
    try {
      await saveSettings({
        searchApi: {
          provider: 'tavily',
          enabled: nextValue,
          scopes: { classify: nextValue, organizer: nextValue },
        },
      });
      dispatch({ type: 'setUseWebSearch', useWebSearch: nextValue });
    } catch (err) {
      showToast(`${text('保存联网搜索开关失败: ', 'Failed to save web search setting: ')}${getErrorMessage(err)}`, 'error');
      dispatch({ type: 'setUseWebSearch', useWebSearch: !nextValue });
    } finally {
      dispatch({ type: 'patch', patch: { syncingSearch: false } });
    }
  }, []);

  const handleMessageKeyDown = useCallback((event: React.KeyboardEvent<HTMLTextAreaElement>) => {
    if ((event.ctrlKey || event.metaKey) && event.key === 'Enter') {
      event.preventDefault();
      void handleSend();
    }
  }, [handleSend]);

  const stageLabel = getStageLabel(state);

  return (
    <section className="workflow-shell advisor-workspace">
      <section className="card workflow-hero-panel advisor-hero-panel">
        <div className="workflow-hero-row">
          <div className="workflow-hero-copy">
            <div className="workflow-kicker">{text('顾问工作流', 'Advisor Workflow')}</div>
            <h1>{text('归类、建议、预览和执行', 'Organize, Advise, Preview, Execute')}</h1>
          </div>
          <div className="workflow-hero-actions advisor-hero-actions">
            <span className="advisor-stage-chip">{stageLabel}</span>
          </div>
        </div>
        <OrganizePanel
          state={state}
          onRootPathChange={(value) => dispatch({ type: 'setRootPath', rootPath: value })}
          onBrowse={() => void handleBrowse()}
          onStartOrganize={() => void handleStartOrganize()}
          onStopOrganize={() => void handleStopOrganize()}
          onStartSession={() => void handleStartSession()}
          onSummaryChange={(value) => dispatch({ type: 'setSummaryStrategy', summaryStrategy: value })}
          onWebSearchChange={(value) => void handleWebSearchChange(value)}
        />
      </section>

      <ContextSummary sessionData={state.sessionData} state={state} onToggle={() => void handleCardAction('', 'toggle_context_bar')} />

      <section className="advisor-timeline-shell">
        <div className="advisor-timeline">
          <AdvisorTimeline state={state} onCardAction={(cardId, action) => void handleCardAction(cardId, action)} />
        </div>
      </section>

      <section className="card advisor-composer-panel" ref={composerRef}>
        <div className="advisor-composer-grid">
          <div className="advisor-composer-main">
            <label className="form-label" htmlFor="advisor-message">{text('下一步指令', 'Next Instruction')}</label>
            <textarea
              id="advisor-message"
              className="form-input advisor-composer-input"
              rows={4}
              placeholder={state.sessionData?.composer?.placeholder || text('告诉我你想先处理哪些文件。', 'Tell me which files you want to handle first.')}
              value={state.messageDraft}
              onChange={(event) => dispatch({ type: 'setMessageDraft', messageDraft: event.target.value })}
              onKeyDown={handleMessageKeyDown}
            />
          </div>
          <div className="advisor-composer-side">
            <div className="advisor-composer-stage">
              <div className="workflow-kicker workflow-kicker-subtle">{text('当前阶段', 'Current Stage')}</div>
              <div className="advisor-composer-stage-value">{stageLabel}</div>
            </div>
            <button className="btn btn-primary advisor-send-btn" type="button" disabled={state.sending || !state.sessionId} onClick={() => void handleSend()}>
              {state.sessionData?.composer?.submitLabel || text('发送', 'Send')}
            </button>
          </div>
        </div>
      </section>
    </section>
  );
}

export const advisorPersistenceReport = {
  persisted: [
    { field: 'rootPath', kind: 'normal', key: ADVISOR_PERSIST_KEYS.rootPath },
    { field: 'sessionId', kind: 'normal', key: ADVISOR_PERSIST_KEYS.sessionId },
    { field: 'messageDraft', kind: 'normal', key: ADVISOR_PERSIST_KEYS.messageDraft },
    { field: 'summaryStrategy', kind: 'normal', key: WORKFLOW_PERSIST_KEYS.summaryStrategy },
    { field: 'useWebSearch', kind: 'normal', key: WORKFLOW_PERSIST_KEYS.useWebSearch },
  ],
  transient: ['provider API keys', 'search API key', 'loading flags', 'stream handles'],
  scope: 'global',
  version: 'existing v1/v2 keys retained; no migration required',
};
