import { Router } from 'express';
import { execFile } from 'child_process';
import { join, dirname } from 'path';
import { fileURLToPath } from 'url';
import { isAdmin, isWindows } from '../privilege.js';

const __dirname = dirname(fileURLToPath(import.meta.url));
const PROJECT_ROOT = join(__dirname, '..', '..');
const SERVER_ENTRY = join(PROJECT_ROOT, 'server', 'index.js');

export const systemRouter = Router();

function escapePsSingleQuoted(text) {
    return text.replace(/'/g, "''");
}

function encodePowerShellCommand(command) {
    return Buffer.from(command, 'utf16le').toString('base64');
}

systemRouter.get('/privilege', (req, res) => {
    res.json({
        platform: process.platform,
        isAdmin: isAdmin(),
    });
});

systemRouter.post('/request-elevation', (req, res) => {
    if (!isWindows()) {
        return res.status(400).json({
            success: false,
            error: 'Elevation is only supported on Windows.',
        });
    }

    if (isAdmin()) {
        return res.json({ success: true, alreadyAdmin: true });
    }

    const delayedLaunchScript = [
        'Start-Sleep -Milliseconds 1200',
        `Set-Location '${escapePsSingleQuoted(PROJECT_ROOT)}'`,
        `& '${escapePsSingleQuoted(process.execPath)}' '${escapePsSingleQuoted(SERVER_ENTRY)}'`,
    ].join('; ');
    const encodedLaunchScript = encodePowerShellCommand(delayedLaunchScript);
    const psCommand = `Start-Process -Verb RunAs -FilePath 'powershell.exe' -WorkingDirectory '${escapePsSingleQuoted(PROJECT_ROOT)}' -ArgumentList '-NoProfile -ExecutionPolicy Bypass -EncodedCommand ${encodedLaunchScript}'`;

    execFile(
        'powershell.exe',
        ['-NoProfile', '-ExecutionPolicy', 'Bypass', '-Command', psCommand],
        { windowsHide: true, timeout: 15000 },
        (err) => {
            if (err) {
                return res.status(500).json({
                    success: false,
                    error: err.message || 'Failed to request elevation.',
                });
            }

            res.on('finish', () => {
                setTimeout(() => {
                    process.exit(0);
                }, 250);
            });

            res.json({
                success: true,
                restarting: true,
                reloadRecommended: true,
                message: 'UAC prompt triggered. The current instance will exit so the elevated server can take over.',
            });
        }
    );
});
