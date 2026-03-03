import { Router } from 'express';
import {
    OrganizeTask,
    applyTaskMoves,
    getOrganizeCapability,
    rollbackJob,
    suggestCategoriesByFilename,
    DEFAULT_CATEGORY_LIST,
    DEFAULT_EXCLUDED_PATTERNS,
} from '../organizer.js';

export const organizeRouter = Router();

const activeTasks = new Map();

organizeRouter.get('/capability', (req, res) => {
    try {
        res.json(getOrganizeCapability());
    } catch (err) {
        res.status(500).json({ error: err.message });
    }
});

organizeRouter.post('/suggest-categories', async (req, res) => {
    const {
        rootPath,
        recursive = true,
        excludedPatterns = [],
        manualCategories = [],
        modelRouting = null,
        modelSelection = null,
    } = req.body || {};

    if (!rootPath) {
        return res.status(400).json({ error: 'rootPath is required' });
    }

    try {
        const data = await suggestCategoriesByFilename({
            rootPath,
            recursive,
            excludedPatterns,
            manualCategories,
            modelRouting,
            modelSelection,
        });

        return res.json(data);
    } catch (err) {
        return res.status(500).json({
            error: err.message,
            suggestedCategories: [...manualCategories, ...DEFAULT_CATEGORY_LIST],
            source: 'filename_scan',
        });
    }
});

organizeRouter.post('/start', (req, res) => {
    const {
        rootPath,
        recursive = true,
        mode = 'fast',
        categories = DEFAULT_CATEGORY_LIST,
        allowNewCategories = true,
        excludedPatterns = DEFAULT_EXCLUDED_PATTERNS,
        parallelism = 5,
        modelRouting = null,
        modelSelection = null,
    } = req.body || {};

    if (!rootPath) {
        return res.status(400).json({ error: 'rootPath is required' });
    }

    const task = new OrganizeTask({
        rootPath,
        recursive,
        mode,
        categories,
        allowNewCategories,
        excludedPatterns,
        parallelism,
        modelRouting,
        modelSelection,
    });

    activeTasks.set(task.id, task);

    task.on('done', () => {
        setTimeout(() => activeTasks.delete(task.id), 30 * 60 * 1000);
    });
    task.on('error', () => {
        setTimeout(() => activeTasks.delete(task.id), 30 * 60 * 1000);
    });
    task.on('stopped', () => {
        setTimeout(() => activeTasks.delete(task.id), 30 * 60 * 1000);
    });

    task.start();

    res.json({
        taskId: task.id,
        selectedModel: task.selectedModel,
        selectedModels: task.selectedModels,
        selectedProviders: task.selectedProviders,
        supportsMultimodal: task.modelSupportsMultimodal,
    });
});

organizeRouter.get('/status/:taskId', (req, res) => {
    const task = activeTasks.get(req.params.taskId);
    if (!task) {
        return res.status(404).json({ error: 'Task not found' });
    }

    res.setHeader('Content-Type', 'text/event-stream');
    res.setHeader('Cache-Control', 'no-cache');
    res.setHeader('Connection', 'keep-alive');
    res.setHeader('X-Accel-Buffering', 'no');

    res.write(`data: ${JSON.stringify(task._snapshot())}\n\n`);

    const onProgress = (data) => {
        res.write(`event: progress\ndata: ${JSON.stringify(data)}\n\n`);
    };
    const onFileDone = (data) => {
        res.write(`event: file_done\ndata: ${JSON.stringify(data)}\n\n`);
    };
    const onDone = (data) => {
        res.write(`event: done\ndata: ${JSON.stringify(data)}\n\n`);
        cleanup();
    };
    const onError = (err) => {
        res.write(`event: error\ndata: ${JSON.stringify(err)}\n\n`);
        cleanup();
    };
    const onStopped = (data) => {
        res.write(`event: stopped\ndata: ${JSON.stringify(data)}\n\n`);
        cleanup();
    };

    task.on('progress', onProgress);
    task.on('file_done', onFileDone);
    task.on('done', onDone);
    task.on('error', onError);
    task.on('stopped', onStopped);

    function cleanup() {
        task.off('progress', onProgress);
        task.off('file_done', onFileDone);
        task.off('done', onDone);
        task.off('error', onError);
        task.off('stopped', onStopped);
        res.end();
    }

    req.on('close', cleanup);
});

organizeRouter.get('/result/:taskId', (req, res) => {
    const task = activeTasks.get(req.params.taskId);
    if (!task) {
        return res.status(404).json({ error: 'Task not found' });
    }

    res.json(task._snapshot());
});

organizeRouter.post('/stop/:taskId', (req, res) => {
    const task = activeTasks.get(req.params.taskId);
    if (!task) {
        return res.status(404).json({ error: 'Task not found' });
    }

    task.stop();
    return res.json({ success: true });
});

organizeRouter.post('/apply/:taskId', async (req, res) => {
    const task = activeTasks.get(req.params.taskId);
    if (!task) {
        return res.status(404).json({ error: 'Task not found' });
    }

    try {
        const manifest = await applyTaskMoves(task);
        res.json({ success: true, manifest });
    } catch (err) {
        res.status(500).json({ success: false, error: err.message });
    }
});

organizeRouter.post('/rollback/:jobId', async (req, res) => {
    const { jobId } = req.params;

    try {
        const result = await rollbackJob(jobId);
        res.json({ success: true, ...result });
    } catch (err) {
        res.status(500).json({ success: false, error: err.message });
    }
});
