import { Router } from 'express';
import { exec } from 'child_process';
import { rm } from 'fs/promises';
import { existsSync } from 'fs';

export const filesRouter = Router();

/**
 * POST /api/files/open-location
 * Body: { path: string }
 */
filesRouter.post('/open-location', (req, res) => {
    const targetPath = req.body.path;
    if (!targetPath) {
        return res.status(400).json({ success: false, error: 'Path is required' });
    }

    if (!existsSync(targetPath)) {
        return res.status(404).json({ success: false, error: 'File or directory does not exist' });
    }

    // Windows specific command to open file explorer and select the file
    // Uses cmd directly to safely handle quotes in paths with spaces
    try {
        exec(`explorer.exe /select,"${targetPath}"`, (err) => {
            if (err) {
                // explorer.exe inside node can sometimes exit with code 1 even on success
                console.warn('[Files] Open location command returned error code, but explorer may have opened:', err.message);
            }
            res.json({ success: true });
        });
    } catch (err) {
        res.status(500).json({ success: false, error: err.message });
    }
});

/**
 * POST /api/files/delete
 * Body: { paths: string[] }
 */
filesRouter.post('/delete', async (req, res) => {
    const { paths } = req.body;
    if (!paths || !Array.isArray(paths)) {
        return res.status(400).json({ success: false, error: 'Paths array is required' });
    }

    const results = {
        deleted: [],
        failed: []
    };

    for (const targetPath of paths) {
        try {
            if (existsSync(targetPath)) {
                await rm(targetPath, { recursive: true, force: true });
                results.deleted.push(targetPath);
            } else {
                results.failed.push({ path: targetPath, error: 'Not found' });
            }
        } catch (err) {
            results.failed.push({ path: targetPath, error: err.message });
        }
    }

    res.json({ success: true, results });
});
