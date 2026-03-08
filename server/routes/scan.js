/**
 * server/routes/scan.js
 * 扫描路由 — 启动/停止扫描任务，SSE 推送实时进度
 */
import { Router } from 'express';
import { ScanTask } from '../scanner.js';
import { loadScanTask } from '../scan-store.js';

export const scanRouter = Router();

// Active tasks registry
const activeTasks = new Map();

function scheduleCleanup(task) {
    setTimeout(() => activeTasks.delete(task.id), 5 * 60 * 1000);
}

async function loadStoredSnapshot(taskId) {
    const manifest = await loadScanTask(taskId);
    return manifest?.snapshot || null;
}

/**
 * GET /api/scan/active
 * Returns a list of currently active (running) scan tasks
 */
scanRouter.get('/active', (req, res) => {
    const active = [];
    for (const [id, task] of activeTasks) {
        if (['scanning', 'analyzing', 'idle'].includes(task.status)) {
            active.push({ taskId: id, ...task._snapshot() });
        }
    }
    res.json(active);
});

/**
 * POST /api/scan/start
 * Body: { targetPath, targetSizeGB, maxDepth }
 */
scanRouter.post('/start', (req, res) => {
    const { targetPath, targetSizeGB, maxDepth } = req.body;

    if (!targetPath) {
        return res.status(400).json({ error: 'targetPath is required' });
    }

    const task = new ScanTask({
        targetPath,
        targetSize: (targetSizeGB || 1) * 1024 * 1024 * 1024,
        maxDepth: maxDepth || 5,
    });

    activeTasks.set(task.id, task);

    // Auto-cleanup when done
    task.on('done', () => {
        scheduleCleanup(task);
    });
    task.on('error', () => {
        scheduleCleanup(task);
    });
    task.on('stopped', () => {
        scheduleCleanup(task);
    });

    // Start scanning (async)
    task.start();

    res.json({ taskId: task.id, status: 'started' });
});

/**
 * GET /api/scan/status/:taskId
 * SSE stream for real-time progress updates
 */
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
            res.write(`event: error\ndata: ${JSON.stringify({ message: 'Task failed', snapshot: terminalSnapshot })}\n\n`);
        } else {
            res.write(`event: stopped\ndata: ${JSON.stringify(terminalSnapshot)}\n\n`);
        }

        return res.end();
    }

    // SSE headers
    res.setHeader('Content-Type', 'text/event-stream');
    res.setHeader('Cache-Control', 'no-cache');
    res.setHeader('Connection', 'keep-alive');
    res.setHeader('X-Accel-Buffering', 'no');

    // Send current snapshot immediately
    res.write(`data: ${JSON.stringify(task._snapshot())}\n\n`);

    // Stream progress
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
    task.on('done', onDone);
    task.on('error', onError);
    task.on('stopped', onStopped);

    function cleanup() {
        task.off('progress', onProgress);
        task.off('found', onFound);
        task.off('agent_call', onAgentCall);
        task.off('agent_response', onAgentResponse);
        task.off('done', onDone);
        task.off('error', onError);
        task.off('stopped', onStopped);
        res.end();
    }

    req.on('close', cleanup);
});

/**
 * POST /api/scan/stop/:taskId
 */
scanRouter.post('/stop/:taskId', (req, res) => {
    const task = activeTasks.get(req.params.taskId);
    if (!task) {
        return res.status(404).json({ error: 'Task not found' });
    }
    task.stop();
    res.json({ success: true });
});

/**
 * GET /api/scan/result/:taskId
 * Get final results of a completed scan
 */
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
