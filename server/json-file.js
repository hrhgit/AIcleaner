import {
    existsSync,
    mkdirSync,
    readFileSync,
    renameSync,
    rmSync,
    unlinkSync,
    writeFileSync,
} from 'fs';
import { dirname } from 'path';

function backupPath(filePath) {
    return `${filePath}.bak`;
}

function tempPath(filePath) {
    return `${filePath}.${process.pid}.${Date.now()}.tmp`;
}

function parseJsonFile(filePath) {
    try {
        return JSON.parse(readFileSync(filePath, 'utf-8'));
    } catch (err) {
        err.message = `Failed to parse JSON file "${filePath}": ${err.message}`;
        throw err;
    }
}

export function readJsonFileWithBackup(filePath) {
    const backup = backupPath(filePath);
    const primaryExists = existsSync(filePath);
    const backupExists = existsSync(backup);

    if (!primaryExists && !backupExists) {
        const err = new Error(`JSON file not found: ${filePath}`);
        err.code = 'ENOENT';
        throw err;
    }

    if (primaryExists) {
        try {
            return parseJsonFile(filePath);
        } catch (primaryErr) {
            if (!backupExists) {
                throw primaryErr;
            }

            try {
                return parseJsonFile(backup);
            } catch {
                throw primaryErr;
            }
        }
    }

    return parseJsonFile(backup);
}

export function writeJsonFileAtomic(filePath, value) {
    mkdirSync(dirname(filePath), { recursive: true });

    const temp = tempPath(filePath);
    const backup = backupPath(filePath);
    const serialized = JSON.stringify(value, null, 2);

    writeFileSync(temp, serialized, 'utf-8');

    try {
        if (existsSync(backup)) {
            unlinkSync(backup);
        }

        if (existsSync(filePath)) {
            renameSync(filePath, backup);
        }

        renameSync(temp, filePath);

        if (existsSync(backup)) {
            unlinkSync(backup);
        }
    } catch (err) {
        try {
            if (!existsSync(filePath) && existsSync(backup)) {
                renameSync(backup, filePath);
            }
        } catch {
            // Best-effort rollback only.
        }

        try {
            if (existsSync(temp)) {
                rmSync(temp, { force: true });
            }
        } catch {
            // ignore temp cleanup failure
        }

        err.message = `Failed to write JSON file "${filePath}": ${err.message}`;
        throw err;
    }
}
