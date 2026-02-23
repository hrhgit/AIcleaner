/**
 * server/routes/settings.js
 * 设置持久化路由 — 读写 API 配置和扫描选项
 */
import { Router } from 'express';
import { readFileSync, writeFileSync, existsSync, mkdirSync } from 'fs';
import { join, dirname } from 'path';
import { fileURLToPath } from 'url';

const __dirname = dirname(fileURLToPath(import.meta.url));
const DATA_DIR = join(__dirname, '..', 'data');
const SETTINGS_FILE = join(DATA_DIR, 'settings.json');

const DEFAULT_SETTINGS = {
    apiEndpoint: process.env.VITE_API_ENDPOINT || process.env.OPENAI_BASE_URL || 'https://api.openai.com/v1',
    apiKey: process.env.VITE_API_KEY || process.env.OPENAI_API_KEY || '',
    model: process.env.VITE_MODEL || process.env.OPENAI_MODEL || 'gpt-4o-mini',
    scanPath: '',
    targetSizeGB: 1,
    maxDepth: 5,
    lastScanTime: null,
    enableWebSearch: false,
    tavilyApiKey: process.env.TAVILY_API_KEY || '',
};

export function loadSettings() {
    try {
        if (existsSync(SETTINGS_FILE)) {
            const raw = readFileSync(SETTINGS_FILE, 'utf-8');
            return { ...DEFAULT_SETTINGS, ...JSON.parse(raw) };
        }
    } catch (err) {
        console.error('[Settings] Failed to load:', err.message);
    }
    return { ...DEFAULT_SETTINGS };
}

function saveSettings(data) {
    if (!existsSync(DATA_DIR)) {
        mkdirSync(DATA_DIR, { recursive: true });
    }
    const merged = { ...loadSettings(), ...data };
    writeFileSync(SETTINGS_FILE, JSON.stringify(merged, null, 2), 'utf-8');
    return merged;
}

export const settingsRouter = Router();

settingsRouter.get('/', (req, res) => {
    res.json(loadSettings());
});

settingsRouter.post('/', (req, res) => {
    try {
        const saved = saveSettings(req.body);
        res.json({ success: true, settings: saved });
    } catch (err) {
        res.status(500).json({ success: false, error: err.message });
    }
});

/**
 * POST /api/settings/browse-folder
 * Opens native folder selection dialog and returns the chosen path.
 */
settingsRouter.post('/browse-folder', async (req, res) => {
    const { execSync } = await import('child_process');
    try {
        const psScript = [
            'Add-Type -AssemblyName System.Windows.Forms',
            '$dialog = New-Object System.Windows.Forms.FolderBrowserDialog',
            "$dialog.Description = '选择要扫描的文件夹'",
            '$dialog.ShowNewFolderButton = $false',
            '$result = $dialog.ShowDialog()',
            'if ($result -eq [System.Windows.Forms.DialogResult]::OK) { Write-Output $dialog.SelectedPath } else { Write-Output __CANCELLED__ }',
        ].join('; ');
        // Encode as UTF-16LE Base64 for -EncodedCommand
        const encoded = Buffer.from(psScript, 'utf16le').toString('base64');
        const selected = execSync(
            `powershell -NoProfile -EncodedCommand ${encoded}`,
            { encoding: 'utf-8', timeout: 60000 }
        ).trim();

        if (selected === '__CANCELLED__') {
            return res.json({ success: true, cancelled: true });
        }
        res.json({ success: true, cancelled: false, path: selected });
    } catch (err) {
        res.status(500).json({ success: false, error: err.message });
    }
});
