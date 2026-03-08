import { Router } from 'express';
import { exec } from 'child_process';
import path from 'path';
import { existsSync } from 'fs';
import { lstat, readdir, rm, rmdir } from 'fs/promises';
import { isAdmin, isWindows } from '../privilege.js';

export const filesRouter = Router();
const SKIPPABLE_DELETE_ERROR_CODES = new Set(['EBUSY', 'EPERM', 'ENOTEMPTY']);
const PERMISSION_DENIED_ERROR_CODES = new Set(['EACCES', 'EPERM']);

function isDirectoryEntry(stat) {
    return stat.isDirectory() && !stat.isSymbolicLink();
}

function isSkippableDeleteError(err) {
    return SKIPPABLE_DELETE_ERROR_CODES.has(err?.code);
}

function isPermissionDeniedError(err) {
    const code = String(err?.code || '').trim().toUpperCase();
    if (PERMISSION_DENIED_ERROR_CODES.has(code)) return true;

    const message = String(err?.message || '').toLowerCase();
    return message.includes('access is denied')
        || message.includes('permission denied')
        || message.includes('operation not permitted');
}

function formatDeleteError(err) {
    return err?.message || 'Unknown delete error';
}

function createFailureRecord(targetPath, err, overrides = {}) {
    const permissionDenied = overrides.permissionDenied ?? isPermissionDeniedError(err);
    const requiresElevation = overrides.requiresElevation ?? (permissionDenied && isWindows() && !isAdmin());
    const record = {
        path: targetPath,
        error: overrides.error || formatDeleteError(err),
        code: overrides.code || err?.code || 'UNKNOWN',
        skipped: overrides.skipped ?? isSkippableDeleteError(err),
        permissionDenied,
        requiresElevation,
    };

    if (overrides.causePath) {
        record.causePath = overrides.causePath;
    }

    return record;
}

async function cleanEntryRecursive(targetPath) {
    let stat;
    try {
        stat = await lstat(targetPath);
    } catch (err) {
        if (err?.code === 'ENOENT') {
            return { cleaned: true, failure: null };
        }
        return {
            cleaned: false,
            failure: createFailureRecord(targetPath, err),
        };
    }

    if (!isDirectoryEntry(stat)) {
        try {
            await rm(targetPath, { force: false });
            return { cleaned: true, failure: null };
        } catch (err) {
            return {
                cleaned: false,
                failure: createFailureRecord(targetPath, err),
            };
        }
    }

    let firstFailure = null;
    const childNames = await readdir(targetPath);
    for (const childName of childNames) {
        const childPath = path.join(targetPath, childName);
        const childResult = await cleanEntryRecursive(childPath);
        if (!childResult.cleaned && !firstFailure) {
            firstFailure = childResult.failure;
        }
    }

    if (firstFailure) {
        return { cleaned: false, failure: firstFailure };
    }

    try {
        await rmdir(targetPath);
        return { cleaned: true, failure: null };
    } catch (err) {
        return {
            cleaned: false,
            failure: createFailureRecord(targetPath, err),
        };
    }
}

async function cleanDirectoryContents(targetPath) {
    let firstFailure = null;
    let childNames;
    try {
        childNames = await readdir(targetPath);
    } catch (err) {
        return {
            cleaned: false,
            removedSelf: false,
            failure: createFailureRecord(targetPath, err),
        };
    }

    for (const childName of childNames) {
        const childPath = path.join(targetPath, childName);
        const childResult = await cleanEntryRecursive(childPath);
        if (!childResult.cleaned && !firstFailure) {
            firstFailure = childResult.failure;
        }
    }

    if (firstFailure) {
        return {
            cleaned: false,
            removedSelf: false,
            failure: createFailureRecord(targetPath, null, {
                error: `Skipped because a child item could not be cleaned: ${firstFailure.error}`,
                code: firstFailure.code || 'PARTIAL_DELETE',
                skipped: true,
                causePath: firstFailure.path,
                permissionDenied: !!firstFailure.permissionDenied,
                requiresElevation: !!firstFailure.requiresElevation,
            }),
        };
    }

    return { cleaned: true, removedSelf: false, failure: null };
}

export async function cleanTargetPath(targetPath) {
    const stat = await lstat(targetPath);

    if (isDirectoryEntry(stat)) {
        return cleanDirectoryContents(targetPath);
    }

    await rm(targetPath, { force: false });
    return { cleaned: true, removedSelf: true, failure: null };
}

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
 * POST /api/files/clean
 * Body: { paths: string[] }
 */
filesRouter.post('/clean', async (req, res) => {
    const { paths } = req.body;
    if (!paths || !Array.isArray(paths)) {
        return res.status(400).json({ success: false, error: 'Paths array is required' });
    }

    const results = {
        cleaned: [],
        removed: [],
        failed: []
    };

    for (const targetPath of paths) {
        if (!existsSync(targetPath)) {
            results.failed.push(createFailureRecord(targetPath, null, { error: 'Not found', code: 'ENOENT' }));
            continue;
        }

        try {
            const outcome = await cleanTargetPath(targetPath);
            if (outcome.cleaned) {
                results.cleaned.push(targetPath);
            }
            if (outcome.removedSelf) {
                results.removed.push(targetPath);
            }
            if (outcome.failure) {
                results.failed.push(outcome.failure);
            }
        } catch (err) {
            if (err?.code === 'ENOENT') {
                results.failed.push(createFailureRecord(targetPath, err, { error: 'Not found', code: 'ENOENT' }));
                continue;
            }
            results.failed.push(createFailureRecord(targetPath, err));
        }
    }

    res.json({ success: true, results });
});
