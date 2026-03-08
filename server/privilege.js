import { execFileSync } from 'child_process';

export function isWindows() {
    return process.platform === 'win32';
}

export function isAdmin() {
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
