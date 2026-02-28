import { Router } from 'express';
import { execFileSync, execFile } from 'child_process';
import { existsSync } from 'fs';
import { join, dirname } from 'path';
import { fileURLToPath } from 'url';

const __dirname = dirname(fileURLToPath(import.meta.url));
const PROJECT_ROOT = join(__dirname, '..', '..');

export const systemRouter = Router();

function isWindows() {
    return process.platform === 'win32';
}

function isAdmin() {
    if (!isWindows()) return false;
    try {
        const out = execFileSync(
            'powershell.exe',
            [
                '-NoProfile',
                '-Command',
                '$p=[Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent(); if($p.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)){ "1" } else { "0" }',
            ],
            { encoding: 'utf-8', windowsHide: true, timeout: 10000 }
        ).trim();
        return out === '1';
    } catch {
        return false;
    }
}

function resolveStartScript() {
    const candidates = [
        join(process.cwd(), 'start.bat'),
        join(PROJECT_ROOT, 'start.bat'),
    ];
    for (const p of candidates) {
        if (existsSync(p)) return p;
    }
    return null;
}

function escapePsSingleQuoted(text) {
    return text.replace(/'/g, "''");
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

    const startScript = resolveStartScript();
    if (!startScript) {
        return res.status(500).json({
            success: false,
            error: 'Cannot find start.bat to relaunch with admin privilege.',
        });
    }

    const argList = `/c ""${startScript}""`;
    const psCommand = `Start-Process -Verb RunAs -FilePath 'cmd.exe' -WorkingDirectory '${escapePsSingleQuoted(PROJECT_ROOT)}' -ArgumentList '${escapePsSingleQuoted(argList)}'`;

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

            res.json({
                success: true,
                restarting: true,
                message: 'UAC prompt triggered. App will restart as administrator if approved.',
            });
        }
    );
});
