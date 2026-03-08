/**
 * server/routes/scan.js
 * Scan routes backed by Rust sidecar + SQLite persistence.
 */
import { Router } from 'express';
import { ScanTask } from '../scanner.js';
import { getActiveProviderConfig, loadSettings } from './settings.js';
import {
    deleteScanTask,
    getTaskSnapshot,
    listScanTasks,
    patchTask,
} from '../scan-store.js';

export const scanRouter = Router();

const activeTasks = new Map();
const cleanupTimers = new Map();
const NON_TERMINAL_TASK_STATUSES = new Set(['idle', 'scanning', 'analyzing']);
const TASK_RETENTION_MS = 60 * 1000;

function clearCleanupTimer(taskId) {
    const timer = cleanupTimers.get(taskId);
    if (timer) {
        clearTimeout(timer);
        cleanupTimers.delete(taskId);
    }
}

function scheduleCleanup(taskId) {
    clearCleanupTimer(taskId);
    const timer = setTimeout(() => {
        cleanupTimers.delete(taskId);
        activeTasks.delete(taskId);
    }, TASK_RETENTION_MS);
    cleanupTimers.set(taskId, timer);
}

function releaseTask(taskId) {
    clearCleanupTimer(taskId);
    activeTasks.delete(taskId);
}

function sendJsonError(res, err, fallbackMessage = 'Internal server error') {
    const message = err?.message || fallbackMessage;
    console.error('[scan]', message);
    if (!res.headersSent) {
        res.status(500).json({ error: message });
    }
}

function initSse(res) {
    res.setHeader('Content-Type', 'text/event-stream');
    res.setHeader('Cache-Control', 'no-cache');
    res.setHeader('Connection', 'keep-alive');
    res.setHeader('X-Accel-Buffering', 'no');
}

function writeSse(res, eventName, payload) {
    if (eventName) {
        res.write(`event: ${eventName}\n`);
    }
    res.write(`data: ${JSON.stringify(payload)}\n\n`);
}

function sendTerminalSnapshot(res, snapshot) {
    const terminalSnapshot = ['done', 'stopped', 'error'].includes(snapshot.status)
        ? snapshot
        : { ...snapshot, status: 'stopped' };

    initSse(res);
    writeSse(res, null, terminalSnapshot);

    if (terminalSnapshot.status === 'done') {
        writeSse(res, 'done', terminalSnapshot);
    } else if (terminalSnapshot.status === 'error') {
        writeSse(res, 'error', {
            message: terminalSnapshot.errorMessage || 'Task failed',
            snapshot: terminalSnapshot,
        });
    } else {
        writeSse(res, 'stopped', terminalSnapshot);
    }

    res.end();
}

async function loadStoredSnapshot(taskId) {
    const snapshot = await getTaskSnapshot(taskId);
    if (!snapshot) {
        return null;
    }

    if (activeTasks.has(taskId) || !NON_TERMINAL_TASK_STATUSES.has(snapshot.status)) {
        return snapshot;
    }

    await patchTask(taskId, {
        status: 'stopped',
        currentPath: snapshot.currentPath,
        currentDepth: snapshot.currentDepth,
        errorMessage: null,
        finishedAt: snapshot.finishedAt || new Date().toISOString(),
    });

    return getTaskSnapshot(taskId);
}

scanRouter.get('/active', (req, res) => {
    const active = [];
    for (const [id, task] of activeTasks) {
        if (['scanning', 'analyzing', 'idle'].includes(task.status)) {
            active.push({ taskId: id, ...task._snapshot() });
        }
    }
    res.json(active);
});

scanRouter.get('/history', async (req, res) => {
    try {
        const rawLimit = Number(req.query.limit || 20);
        const limit = Number.isFinite(rawLimit) ? Math.max(1, Math.min(200, rawLimit)) : 20;
        let history = await listScanTasks(limit);
        const staleTasks = history.filter((task) => (
            !activeTasks.has(task.taskId) && NON_TERMINAL_TASK_STATUSES.has(task.status)
        ));

        if (staleTasks.length > 0) {
            await Promise.all(staleTasks.map((task) => patchTask(task.taskId, {
                status: 'stopped',
                currentPath: task.currentPath,
                currentDepth: task.currentDepth,
                errorMessage: null,
                finishedAt: task.finishedAt || new Date().toISOString(),
            })));
            history = await listScanTasks(limit);
        }

        res.json(history);
    } catch (err) {
        sendJsonError(res, err, 'Failed to load scan history');
    }
});

scanRouter.delete('/history/:taskId', async (req, res) => {
    try {
        const taskId = req.params.taskId;
        const task = activeTasks.get(taskId);
        if (task && ['idle', 'scanning', 'analyzing'].includes(task.status)) {
            return res.status(409).json({ error: 'Task is still running' });
        }

        const deleted = await deleteScanTask(taskId);
        if (!deleted) {
            return res.status(404).json({ error: 'Task not found' });
        }

        releaseTask(taskId);
        res.json({ success: true });
    } catch (err) {
        sendJsonError(res, err, 'Failed to delete scan history');
    }
});

scanRouter.post('/start', (req, res) => {
    const { targetPath, targetSizeGB, maxDepth, autoAnalyze } = req.body;
    const shouldAutoAnalyze = autoAnalyze !== false;

    if (!targetPath) {
        return res.status(400).json({ error: 'targetPath is required' });
    }

    if (shouldAutoAnalyze) {
        const activeProvider = getActiveProviderConfig(loadSettings());
        if (!activeProvider.apiKey) {
            return res.status(400).json({
                error: `API key is required for scan analysis. Configure the selected provider (${activeProvider.endpoint || 'current provider'}) before starting a scan.`,
            });
        }
    }

    const task = new ScanTask({
        targetPath,
        targetSize: (targetSizeGB || 1) * 1024 * 1024 * 1024,
        maxDepth: maxDepth || 5,
        autoAnalyze: shouldAutoAnalyze,
    });

    activeTasks.set(task.id, task);
    task.on('done', () => scheduleCleanup(task.id));
    task.on('error', () => scheduleCleanup(task.id));
    task.on('stopped', () => scheduleCleanup(task.id));

    task.start().catch((err) => {
        console.error('[scan/start] task failed:', err.message);
    });

    res.json({ taskId: task.id, status: 'started' });
});

scanRouter.get('/status/:taskId', async (req, res) => {
    let streamStarted = false;
    let task;

    try {
        task = activeTasks.get(req.params.taskId);

        if (!task) {
            const stored = await loadStoredSnapshot(req.params.taskId);
            if (!stored) {
                return res.status(404).json({ error: 'Task not found' });
            }
            sendTerminalSnapshot(res, stored);
            return;
        }

        if (['done', 'stopped', 'error'].includes(task.status)) {
            sendTerminalSnapshot(res, task._snapshot());
            return;
        }

        initSse(res);
        writeSse(res, null, task._snapshot());
        streamStarted = true;

        const onProgress = (snap) => {
            writeSse(res, null, snap);
        };
        const onFound = (item) => {
            writeSse(res, 'found', item);
        };
        const onAgentCall = (data) => {
            writeSse(res, 'agent_call', data);
        };
        const onAgentResponse = (data) => {
            writeSse(res, 'agent_response', data);
        };
        const onWarning = (data) => {
            writeSse(res, 'warning', data);
        };
        const onDone = (snap) => {
            writeSse(res, 'done', snap);
            cleanup();
        };
        const onError = (err) => {
            writeSse(res, 'error', err);
            cleanup();
        };
        const onStopped = () => {
            writeSse(res, 'stopped', task._snapshot());
            cleanup();
        };

        task.on('progress', onProgress);
        task.on('found', onFound);
        task.on('agent_call', onAgentCall);
        task.on('agent_response', onAgentResponse);
        task.on('warning', onWarning);
        task.on('done', onDone);
        task.on('error', onError);
        task.on('stopped', onStopped);

        function cleanup() {
            task.off('progress', onProgress);
            task.off('found', onFound);
            task.off('agent_call', onAgentCall);
            task.off('agent_response', onAgentResponse);
            task.off('warning', onWarning);
            task.off('done', onDone);
            task.off('error', onError);
            task.off('stopped', onStopped);
            if (!res.writableEnded) {
                res.end();
            }
        }

        req.on('close', cleanup);
    } catch (err) {
        const message = err?.message || 'Failed to stream scan status';
        console.error('[scan/status]', message);
        if (streamStarted || res.headersSent) {
            if (!res.writableEnded) {
                writeSse(res, 'error', { message });
                res.end();
            }
            return;
        }
        res.status(500).json({ error: message });
    }
});

scanRouter.post('/stop/:taskId', async (req, res) => {
    try {
        const task = activeTasks.get(req.params.taskId);
        if (!task) {
            return res.status(404).json({ error: 'Task not found' });
        }
        task.stop();
        res.json({ success: true });
    } catch (err) {
        sendJsonError(res, err, 'Failed to stop scan task');
    }
});

scanRouter.get('/result/:taskId', async (req, res) => {
    try {
        const task = activeTasks.get(req.params.taskId);
        if (task) {
            return res.json(task._snapshot());
        }

        const stored = await loadStoredSnapshot(req.params.taskId);
        if (!stored) {
            return res.status(404).json({ error: 'Task not found' });
        }
        res.json(stored);
    } catch (err) {
        sendJsonError(res, err, 'Failed to load scan result');
    }
});
