/**
 * download-dust.js
 * Automatically downloads the dust CLI binary for Windows if not already present.
 * Runs as a postinstall script.
 */
import { existsSync, mkdirSync, createWriteStream, chmodSync } from 'fs';
import { join, dirname } from 'path';
import { fileURLToPath } from 'url';
import https from 'https';
import { execSync } from 'child_process';

const __dirname = dirname(fileURLToPath(import.meta.url));
const BIN_DIR = join(__dirname, '..', 'bin');
const DUST_PATH = join(BIN_DIR, 'dust.exe');

const DUST_VERSION = 'v1.1.2';
const DUST_URL = `https://github.com/bootandy/dust/releases/download/${DUST_VERSION}/dust-${DUST_VERSION}-x86_64-pc-windows-msvc.zip`;

async function download(url, dest) {
    return new Promise((resolve, reject) => {
        const followRedirect = (url) => {
            https.get(url, (res) => {
                if (res.statusCode >= 300 && res.statusCode < 400 && res.headers.location) {
                    followRedirect(res.headers.location);
                    return;
                }
                if (res.statusCode !== 200) {
                    reject(new Error(`Download failed with status ${res.statusCode}`));
                    return;
                }
                const file = createWriteStream(dest);
                res.pipe(file);
                file.on('finish', () => { file.close(); resolve(); });
                file.on('error', reject);
            }).on('error', reject);
        };
        followRedirect(url);
    });
}

async function main() {
    if (existsSync(DUST_PATH)) {
        console.log('[dust] Binary already exists at', DUST_PATH);
        return;
    }

    if (!existsSync(BIN_DIR)) {
        mkdirSync(BIN_DIR, { recursive: true });
    }

    const zipPath = join(BIN_DIR, 'dust.zip');
    console.log(`[dust] Downloading dust ${DUST_VERSION}...`);

    try {
        await download(DUST_URL, zipPath);
        console.log('[dust] Extracting...');
        // Use PowerShell to extract
        execSync(`powershell -Command "Expand-Archive -Path '${zipPath}' -DestinationPath '${BIN_DIR}' -Force"`, { stdio: 'inherit' });

        // Find dust.exe in extracted contents (might be in a nested folder)
        const findResult = execSync(`powershell -Command "Get-ChildItem -Path '${BIN_DIR}' -Recurse -Filter 'dust.exe' | Select-Object -First 1 -ExpandProperty FullName"`, { encoding: 'utf-8' }).trim();

        if (findResult && findResult !== DUST_PATH) {
            execSync(`powershell -Command "Move-Item -Path '${findResult}' -Destination '${DUST_PATH}' -Force"`, { stdio: 'inherit' });
        }

        // Cleanup zip and empty dirs
        execSync(`powershell -Command "Remove-Item -Path '${zipPath}' -Force; Get-ChildItem -Path '${BIN_DIR}' -Directory | Remove-Item -Recurse -Force -ErrorAction SilentlyContinue"`, { stdio: 'inherit' });
        console.log('[dust] Installed dust to', DUST_PATH);
    } catch (err) {
        console.error('[dust] Failed to download dust. You can manually place dust.exe in the bin/ directory.');
        console.error(err.message);
    }
}

main();
