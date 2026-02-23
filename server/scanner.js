/**
 * server/scanner.js
 * 分层扫描引擎 — 使用 dust CLI 进行磁盘分析，结合 LLM Agent 进行文件分类
 */
import { execSync, execFile } from 'child_process';
import { EventEmitter } from 'events';
import { join, dirname, basename } from 'path';
import { fileURLToPath } from 'url';
import { existsSync, statSync, readdirSync } from 'fs';
import { analyzeEntries, verifyDirectoryDelete } from './agent.js';

const __dirname = dirname(fileURLToPath(import.meta.url));
const DUST_BIN = join(__dirname, '..', 'bin', 'dust.exe');

// Protected paths that should never be scanned or recommended for deletion
const PROTECTED_PATTERNS = [
    /^windows$/i,
    /^program files/i,
    /^programdata$/i,
    /^\$recycle\.bin$/i,
    /^system volume information$/i,
    /^recovery$/i,
    /^boot$/i,
    /^users$/i,
    /^documents and settings$/i,
    /^pagefile\.sys$/i,
    /^hiberfil\.sys$/i,
    /^swapfile\.sys$/i,
];

function isProtected(name) {
    return PROTECTED_PATTERNS.some(p => p.test(name));
}

/**
 * Determine if a dust entry is a directory.
 * dust outputs "children": [] for BOTH files and directories when using -j,
 * so we cannot rely solely on `children !== undefined`.
 * Strategy:
 *   1. If children array has actual items → definitely a directory
 *   2. If the path/name has a file extension → treat as file
 *   3. Otherwise (no extension, children is empty) → treat as directory
 */
function _dustEntryIsDir(entryPath, dustEntry) {
    // If entry actually has children with content, it's definitely a dir
    if (dustEntry && Array.isArray(dustEntry.children) && dustEntry.children.length > 0) {
        return true;
    }
    // Use file extension as heuristic: no extension → directory
    const base = basename(entryPath);
    const dotIndex = base.lastIndexOf('.');
    // No dot, or dot only at the start (like .accelerate) → directory
    if (dotIndex <= 0) return true;
    // Has a real extension like .txt, .zip, .jpg → file
    return false;
}

/**
 * Run dust CLI and parse JSON output.
 * Fallback to fs-based scanning if dust is unavailable.
 */
function runDust(targetPath, depth = 1) {
    try {
        // Try dust CLI first
        const dustPath = existsSync(DUST_BIN) ? DUST_BIN : 'dust';
        // Prevent trailing backslash from escaping the closing double quote
        const escapedPath = targetPath.replace(/\\$/, '\\\\');
        const cmd = `"${dustPath}" -d ${depth} -j "${escapedPath}" 2>&1`;
        const output = execSync(cmd, {
            encoding: 'utf-8',
            maxBuffer: 50 * 1024 * 1024,
            timeout: 60000,
        });

        // dust JSON output is a single object { size, name, children: [...] }
        // The output can be prepended by warning messages
        const firstBrace = output.indexOf('{');
        if (firstBrace === -1) return [];
        const jsonStr = output.slice(firstBrace);

        const parsed = JSON.parse(jsonStr);
        return parsed.children || [];
    } catch (err) {
        console.warn('[Scanner] dust failed, falling back to fs scan:', err.message);
        return fsFallback(targetPath);
    }
}

/**
 * Fallback: use Node.js fs to list directory contents with sizes.
 */
function fsFallback(targetPath) {
    try {
        const items = readdirSync(targetPath, { withFileTypes: true });
        return items.map(item => {
            const fullPath = join(targetPath, item.name);
            try {
                const st = statSync(fullPath);
                return {
                    name: item.name,
                    size: st.size,
                    type: item.isDirectory() ? 'directory' : 'file',
                    path: fullPath,
                };
            } catch {
                return { name: item.name, size: 0, type: 'unknown', path: fullPath };
            }
        });
    } catch (err) {
        console.error('[Scanner] fs fallback also failed:', err.message);
        return [];
    }
}

function parseDustSize(str) {
    if (typeof str === 'number') return str;
    if (!str) return 0;
    const match = str.toString().trim().match(/^([\d.]+)\s*([KMGTP]?B?)$/i);
    if (!match) return 0;
    const val = parseFloat(match[1]);
    const unit = match[2].toUpperCase();
    if (unit.startsWith('K')) return val * 1024;
    if (unit.startsWith('M')) return val * 1024 * 1024;
    if (unit.startsWith('G')) return val * 1024 * 1024 * 1024;
    if (unit.startsWith('T')) return val * 1024 * 1024 * 1024 * 1024;
    return val;
}

/**
 * Parse dust JSON entries into a normalized format.
 */
function normalizeDustEntries(dustEntries, parentPath) {
    return dustEntries
        .filter(e => {
            const entryPath = e.name || e.path || '';
            // `dust` .name could be absolute path. Make sure we skip the parent itself.
            return entryPath !== parentPath && basename(entryPath) !== '';
        })
        .map(e => ({
            name: basename(e.name || e.path || ''),
            size: parseDustSize(e.size || e.bytes),
            type: _dustEntryIsDir(e.name || e.path || '', e) ? 'directory' : 'file',
            path: e.path || e.name || join(parentPath, e.name || ''),
        }));
}

/**
 * ScanTask — manages a single hierarchical scan operation.
 */
export class ScanTask extends EventEmitter {
    constructor(options) {
        super();
        this.id = Date.now().toString(36) + Math.random().toString(36).slice(2, 7);
        this.targetPath = options.targetPath;
        this.targetSize = options.targetSize || Infinity; // bytes
        this.maxDepth = options.maxDepth || 5;
        this.stopped = false;

        // Results
        this.deletable = [];        // Files recommended for deletion
        this.totalCleanable = 0;    // Bytes
        this.scannedCount = 0;
        this.totalEntries = 0;      // Total entries discovered (for progress)
        this.processedEntries = 0;  // Entries that finished LLM analysis
        this.tokenUsage = { prompt: 0, completion: 0, total: 0 };
        this.currentPath = '';
        this.currentDepth = 0;
        this.status = 'idle';       // idle | scanning | analyzing | done | error | stopped
    }

    stop() {
        this.stopped = true;
        this.status = 'stopped';
        this.emit('stopped');
    }

    async start() {
        this.status = 'scanning';
        this.emit('progress', this._snapshot());

        try {
            await this._scanLayer(this.targetPath, 0);
            if (!this.stopped) {
                this.status = 'done';
                this.emit('done', this._snapshot());
            }
        } catch (err) {
            this.status = 'error';
            this.emit('error', { message: err.message, snapshot: this._snapshot() });
        }
    }

    async _scanLayer(dirPath, depth) {
        if (this.stopped || depth >= this.maxDepth) return;
        if (this.totalCleanable >= this.targetSize) return;

        this.currentPath = dirPath;
        this.currentDepth = depth;
        this.emit('progress', this._snapshot());

        // Step 1: Run dust to get directory contents
        console.log(`[Scanner] dust scan: ${dirPath} (depth=${depth})`);
        const rawEntries = runDust(dirPath, 1);
        const entries = normalizeDustEntries(rawEntries, dirPath);

        // Filter out protected entries
        const safeEntries = entries.filter(e => !isProtected(e.name));
        this.scannedCount += safeEntries.length;
        this.totalEntries += safeEntries.length;
        console.log(`[Scanner] Found ${safeEntries.length} entries in ${dirPath}`);

        if (safeEntries.length === 0) return;

        // Step 2: Batch analyze with LLM (max 50 per batch)
        this.status = 'analyzing';
        this.emit('progress', this._snapshot());

        const batchSize = 50;
        for (let i = 0; i < safeEntries.length; i += batchSize) {
            if (this.stopped) return;
            const batch = safeEntries.slice(i, i + batchSize);

            console.log(`[Scanner] Sending batch of ${batch.length} to LLM (${dirPath})...`);

            // Emit agent_call event BEFORE LLM call
            this.emit('agent_call', {
                batchIndex: i / batchSize + 1,
                batchSize: batch.length,
                dirPath,
                entries: batch.map(e => ({ name: e.name, type: e.type, size: e.size })),
            });

            const { results, tokenUsage, trace } = await analyzeEntries(batch, dirPath);
            console.log(`[Scanner] LLM returned ${results.length} results, tokens: ${tokenUsage.total}`);

            // Emit agent_response event AFTER LLM call
            this.emit('agent_response', {
                model: trace.model,
                elapsed: trace.elapsed,
                reasoning: trace.reasoning,
                rawContent: trace.rawContent,
                userPrompt: trace.userPrompt,
                error: trace.error,
                tokenUsage,
                resultsCount: results.length,
                classifications: results.reduce((acc, r) => {
                    acc[r.classification] = (acc[r.classification] || 0) + 1;
                    return acc;
                }, {}),
            });

            // Track progress: batch has been analyzed
            this.processedEntries += batch.length;

            // Track tokens
            this.tokenUsage.prompt += tokenUsage.prompt;
            this.tokenUsage.completion += tokenUsage.completion;
            this.tokenUsage.total += tokenUsage.total;

            // Step 3: Process results
            const suspiciousDirs = [];

            for (const result of results) {
                const idx = (result.index || 1) - 1;
                const entry = batch[idx] || batch[0];

                if (result.classification === 'safe_to_delete') {
                    if (entry.type === 'directory') {
                        console.log(`[Scanner] Verifying directory delete for ${entry.name}`);

                        // Status update
                        this.status = 'analyzing';
                        this.emit('progress', this._snapshot());

                        // Quick depth=1 scan to fetch contents
                        const rawChildren = runDust(entry.path, 1);
                        const children = normalizeDustEntries(rawChildren, entry.path);

                        this.emit('agent_call', {
                            batchIndex: `二次确认 [${entry.name}]`,
                            batchSize: children.length,
                            dirPath: entry.path,
                            entries: children.map(e => ({ name: e.name, type: e.type, size: e.size })),
                        });

                        const verifyResult = await verifyDirectoryDelete(entry.name, children, entry.path);

                        // Accumulate token usage
                        this.tokenUsage.prompt += verifyResult.tokenUsage.prompt;
                        this.tokenUsage.completion += verifyResult.tokenUsage.completion;
                        this.tokenUsage.total += verifyResult.tokenUsage.total;

                        this.emit('agent_response', {
                            model: verifyResult.trace.model,
                            elapsed: verifyResult.trace.elapsed,
                            reasoning: verifyResult.trace.reasoning,
                            rawContent: verifyResult.trace.rawContent,
                            userPrompt: verifyResult.trace.userPrompt,
                            error: verifyResult.trace.error,
                            tokenUsage: verifyResult.tokenUsage,
                            resultsCount: 1,
                            classifications: { [(verifyResult.safe ? '✅二次确认安全' : '❌二次确认驳回')]: 1 },
                        });

                        if (!verifyResult.safe) {
                            console.log(`[Scanner] Folder ${entry.name} failed verification: ${verifyResult.reason}`);
                            // Mark as suspicious to drill into it and evaluate sub-items individually
                            suspiciousDirs.push(entry);
                            continue; // Skip adding the whole directory to deletable
                        }
                    }

                    const item = {
                        name: entry.name,
                        path: entry.path,
                        size: entry.size,
                        type: entry.type,
                        purpose: result.purpose,
                        reason: entry.type === 'directory' ? `${result.reason} (并已通过内部文件核查)` : result.reason,
                        risk: result.risk || 'low',
                    };
                    this.deletable.push(item);
                    this.totalCleanable += entry.size;

                    this.emit('found', item);

                    if (this.totalCleanable >= this.targetSize) {
                        this.emit('progress', this._snapshot());
                        return;
                    }
                } else if (result.classification === 'suspicious' && entry.type === 'directory') {
                    suspiciousDirs.push(entry);
                }
                // 'keep' items are simply skipped
            }

            this.emit('progress', this._snapshot());

            // Step 4: Recurse into suspicious directories
            this.status = 'scanning';
            for (const dir of suspiciousDirs) {
                if (this.stopped || this.totalCleanable >= this.targetSize) return;
                await this._scanLayer(dir.path, depth + 1);
            }
        }
    }

    _snapshot() {
        return {
            id: this.id,
            status: this.status,
            currentPath: this.currentPath,
            currentDepth: this.currentDepth,
            scannedCount: this.scannedCount,
            totalEntries: this.totalEntries,
            processedEntries: this.processedEntries,
            deletableCount: this.deletable.length,
            totalCleanable: this.totalCleanable,
            targetSize: this.targetSize,
            tokenUsage: { ...this.tokenUsage },
            deletable: this.deletable,
        };
    }
}
