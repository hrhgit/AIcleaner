/**
 * server/routes/scan.js
 * Scan routes backed by Rust sidecar + SQLite persistence.
 */
import { Router } from 'express';
import { ScanTask } from '../scanner.js';
import {
    deleteScanTask,
    getTaskSnapshot,
    listScanTasks,
    patchTask,
} from '../scan-store.js';

export const scanRouter = Router();

const activeTasks = new Map();
const NON_TERMINAL_TASK_STATUSES = new Set(['idle', 'scanning', 'analyzing']);

function scheduleCleanup(task) {
    setTimeout(() => activeTasks.delete(task.id), 24 * 60 * 60 * 1000);
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
});

scanRouter.delete('/history/:taskId', async (req, res) => {
    const task = activeTasks.get(req.params.taskId);
    if (task && ['idle', 'scanning', 'analyzing'].includes(task.status)) {
        return res.status(409).json({ error: 'Task is still running' });
    }

    const deleted = await deleteScanTask(req.params.taskId);
    if (!deleted) {
        return res.status(404).json({ error: 'Task not found' });
    }

    activeTasks.delete(req.params.taskId);
    res.json({ success: true });
});

scanRouter.post('/start', (req, res) => {
    const { targetPath, targetSizeGB, maxDepth, autoAnalyze } = req.body;

    if (!targetPath) {
        return res.status(400).json({ error: 'targetPath is required' });
    }

    const task = new ScanTask({
        targetPath,
        targetSize: (targetSizeGB || 1) * 1024 * 1024 * 1024,
        maxDepth: maxDepth || 5,
        autoAnalyze: autoAnalyze !== false,
    });

    activeTasks.set(task.id, task);
    task.on('done', () => scheduleCleanup(task));
    task.on('error', () => scheduleCleanup(task));
    task.on('stopped', () => scheduleCleanup(task));

    task.start().catch((err) => {
        console.error('[scan/start] task failed:', err.message);
    });

    res.json({ taskId: task.id, status: 'started' });
});

scanRouter.get('/status/:taskId', async (req, res) => {
    const task = activeTasks.get(req.params.taskId);

    if (!task) {
        const stored = await loadStoredSnapshot(req.params.taskId);
        if (!stored) {
            return res.status(404).json({ error: 'Task not found' });
        }

        const terminalSnapshot = ['done', 'stopped', 'error'].includes(stored.status)
            ? stored
            : { ...stored, status: 'stopped' };

        res.setHeader('Content-Type', 'text/event-stream');
        res.setHeader('Cache-Control', 'no-cache');
        res.setHeader('Connection', 'keep-alive');
        res.setHeader('X-Accel-Buffering', 'no');
        res.write(`data: ${JSON.stringify(terminalSnapshot)}\n\n`);

        if (terminalSnapshot.status === 'done') {
            res.write(`event: done\ndata: ${JSON.stringify(terminalSnapshot)}\n\n`);
        } else if (terminalSnapshot.status === 'error') {
            res.write(`event: error\ndata: ${JSON.stringify({ message: terminalSnapshot.errorMessage || 'Task failed', snapshot: terminalSnapshot })}\n\n`);
        } else {
            res.write(`event: stopped\ndata: ${JSON.stringify(terminalSnapshot)}\n\n`);
        }

        return res.end();
    }

    if (['done', 'stopped', 'error'].includes(task.status)) {
        const snap = task._snapshot();
        res.setHeader('Content-Type', 'text/event-stream');
        res.setHeader('Cache-Control', 'no-cache');
        res.setHeader('Connection', 'keep-alive');
        res.setHeader('X-Accel-Buffering', 'no');
        res.write(`data: ${JSON.stringify(snap)}\n\n`);

        if (snap.status === 'done') {
            res.write(`event: done\ndata: ${JSON.stringify(snap)}\n\n`);
        } else if (snap.status === 'error') {
            res.write(`event: error\ndata: ${JSON.stringify({ message: snap.errorMessage || 'Task failed', snapshot: snap })}\n\n`);
        } else {
            res.write(`event: stopped\ndata: ${JSON.stringify(snap)}\n\n`);
        }
        return res.end();
    }

    res.setHeader('Content-Type', 'text/event-stream');
    res.setHeader('Cache-Control', 'no-cache');
    res.setHeader('Connection', 'keep-alive');
    res.setHeader('X-Accel-Buffering', 'no');

    res.write(`data: ${JSON.stringify(task._snapshot())}\n\n`);

    const onProgress = (snap) => {
        res.write(`data: ${JSON.stringify(snap)}\n\n`);
    };
    const onFound = (item) => {
        res.write(`event: found\ndata: ${JSON.stringify(item)}\n\n`);
    };
    const onAgentCall = (data) => {
        res.write(`event: agent_call\ndata: ${JSON.stringify(data)}\n\n`);
    };
    const onAgentResponse = (data) => {
        res.write(`event: agent_response\ndata: ${JSON.stringify(data)}\n\n`);
    };
    const onWarning = (data) => {
        res.write(`event: warning\ndata: ${JSON.stringify(data)}\n\n`);
    };
    const onDone = (snap) => {
        res.write(`event: done\ndata: ${JSON.stringify(snap)}\n\n`);
        cleanup();
    };
    const onError = (err) => {
        res.write(`event: error\ndata: ${JSON.stringify(err)}\n\n`);
        cleanup();
    };
    const onStopped = () => {
        res.write(`event: stopped\ndata: ${JSON.stringify(task._snapshot())}\n\n`);
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
        res.end();
    }

    req.on('close', cleanup);
});

scanRouter.post('/stop/:taskId', async (req, res) => {
    const task = activeTasks.get(req.params.taskId);
    if (!task) {
        return res.status(404).json({ error: 'Task not found' });
    }
    task.stop();
    res.json({ success: true });
});

scanRouter.get('/result/:taskId', async (req, res) => {
    const task = activeTasks.get(req.params.taskId);
    if (task) {
        return res.json(task._snapshot());
    }

    const stored = await loadStoredSnapshot(req.params.taskId);
    if (!stored) {
        return res.status(404).json({ error: 'Task not found' });
    }
    res.json(stored);
});
