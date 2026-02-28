/**
 * server/scanner.js
 * Layered scanning engine: uses dust for disk analysis + LLM agent for classification.
 */
import { execSync } from 'child_process';
import { EventEmitter } from 'events';
import { join, dirname, basename } from 'path';
import { fileURLToPath } from 'url';
import { existsSync, statSync, readdirSync } from 'fs';
import pLimit from 'p-limit';
import { analyzeEntries, verifyDirectoryDelete } from './agent.js';

const __dirname = dirname(fileURLToPath(import.meta.url));
const DUST_BIN = join(__dirname, '..', 'bin', 'dust.exe');

const BATCH_SIZE = 50;
const LAYER_CONCURRENCY = 5;

/**
 * Determine if a dust entry is a directory.
 * dust outputs "children": [] for both files and directories when using -j,
 * so we cannot rely solely on `children !== undefined`.
 */
function dustEntryIsDir(entryPath, dustEntry) {
    if (dustEntry && Array.isArray(dustEntry.children) && dustEntry.children.length > 0) {
        return true;
    }

    const base = basename(entryPath);
    const dotIndex = base.lastIndexOf('.');
    if (dotIndex <= 0) return true;
    return false;
}

/**
 * Run dust CLI and parse JSON output.
 * Fallback to fs-based scanning if dust is unavailable.
 */
function runDust(targetPath, depth = 1) {
    try {
        const dustPath = existsSync(DUST_BIN) ? DUST_BIN : 'dust';
        const escapedPath = targetPath.replace(/\\$/, '\\\\');
        const cmd = `"${dustPath}" -d ${depth} -j "${escapedPath}" 2>&1`;
        const output = execSync(cmd, {
            encoding: 'utf-8',
            maxBuffer: 50 * 1024 * 1024,
            timeout: 60000,
        });

        const firstBrace = output.indexOf('{');
        if (firstBrace === -1) return [];

        const parsed = JSON.parse(output.slice(firstBrace));
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
        return items.map((item) => {
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
        .filter((e) => {
            const entryPath = e.name || e.path || '';
            return entryPath !== parentPath && basename(entryPath) !== '';
        })
        .map((e) => ({
            name: basename(e.name || e.path || ''),
            size: parseDustSize(e.size || e.bytes),
            type: dustEntryIsDir(e.name || e.path || '', e) ? 'directory' : 'file',
            path: e.path || e.name || join(parentPath, e.name || ''),
        }));
}

/**
 * ScanTask manages a single hierarchical scan operation.
 */
export class ScanTask extends EventEmitter {
    constructor(options) {
        super();
        this.id = Date.now().toString(36) + Math.random().toString(36).slice(2, 7);
        this.targetPath = options.targetPath;
        this.targetSize = options.targetSize || Infinity;
        this.maxDepth = options.maxDepth || 5;
        this.stopped = false;

        this.deletable = [];
        this.totalCleanable = 0;
        this.scannedCount = 0;
        this.totalEntries = 0;
        this.processedEntries = 0;
        this.tokenUsage = { prompt: 0, completion: 0, total: 0 };
        this.currentPath = '';
        this.currentDepth = 0;
        this.status = 'idle';
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

    _hasReachedTarget() {
        return this.totalCleanable >= this.targetSize;
    }

    _trackTokenUsage(tokenUsage) {
        this.tokenUsage.prompt += tokenUsage?.prompt || 0;
        this.tokenUsage.completion += tokenUsage?.completion || 0;
        this.tokenUsage.total += tokenUsage?.total || 0;
    }

    _appendSuspiciousDir(list, entry) {
        if (!entry || entry.type !== 'directory') return;
        if (!list.some((d) => d.path === entry.path)) {
            list.push(entry);
        }
    }

    _addDeletable(entry, result, verifiedDirectory = false) {
        if (this.stopped || this._hasReachedTarget()) {
            return false;
        }

        const reasonPrefix = result?.reason || '';
        const reasonSuffix = verifiedDirectory ? ' (并已通过内部文件核查)' : '';
        const item = {
            name: entry.name,
            path: entry.path,
            size: entry.size,
            type: entry.type,
            purpose: result?.purpose,
            reason: `${reasonPrefix}${reasonSuffix}`,
            risk: result?.risk || 'low',
        };

        this.deletable.push(item);
        this.totalCleanable += entry.size || 0;
        this.emit('found', item);

        if (this._hasReachedTarget()) {
            this.emit('progress', this._snapshot());
        }

        return true;
    }

    async _verifyDirectoryAndMaybeAdd(entry, result, suspiciousDirs) {
        if (this.stopped || this._hasReachedTarget()) {
            return;
        }

        console.log(`[Scanner] Verifying directory delete for ${entry.name}`);
        this.status = 'analyzing';
        this.emit('progress', this._snapshot());

        const rawChildren = runDust(entry.path, 1);
        const children = normalizeDustEntries(rawChildren, entry.path);

        this.emit('agent_call', {
            batchIndex: `二次确认 [${entry.name}]`,
            batchSize: children.length,
            dirPath: entry.path,
            entries: children.map((e) => ({ name: e.name, type: e.type, size: e.size })),
        });

        const verifyResult = await verifyDirectoryDelete(entry.name, children, entry.path);
        if (this.stopped) {
            return;
        }

        this._trackTokenUsage(verifyResult.tokenUsage);

        this.emit('agent_response', {
            model: verifyResult.trace.model,
            elapsed: verifyResult.trace.elapsed,
            reasoning: verifyResult.trace.reasoning,
            rawContent: verifyResult.trace.rawContent,
            userPrompt: verifyResult.trace.userPrompt,
            error: verifyResult.trace.error,
            tokenUsage: verifyResult.tokenUsage,
            resultsCount: 1,
            classifications: { [verifyResult.safe ? 'secondary_safe' : 'secondary_failed']: 1 },
        });

        if (!verifyResult.safe) {
            console.log(`[Scanner] Folder ${entry.name} failed verification: ${verifyResult.reason}`);
            this._appendSuspiciousDir(suspiciousDirs, entry);
            return;
        }

        this._addDeletable(entry, result, true);
    }

    async _processBatch(batch, batchIndex, dirPath, depth) {
        if (this.stopped || this._hasReachedTarget()) {
            return;
        }

        console.log(`[Scanner] Sending batch of ${batch.length} to LLM (${dirPath})...`);

        this.emit('agent_call', {
            batchIndex,
            batchSize: batch.length,
            dirPath,
            entries: batch.map((e) => ({ name: e.name, type: e.type, size: e.size })),
        });

        const { results, tokenUsage, trace } = await analyzeEntries(batch, dirPath);
        if (this.stopped) {
            return;
        }

        console.log(`[Scanner] LLM returned ${results.length} results, tokens: ${tokenUsage.total}`);

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

        this.processedEntries += batch.length;
        this._trackTokenUsage(tokenUsage);

        if (this._hasReachedTarget()) {
            return;
        }

        const suspiciousDirs = [];
        const verifyTargets = [];

        for (const result of results) {
            if (this.stopped || this._hasReachedTarget()) {
                return;
            }

            const idx = (result.index || 1) - 1;
            const entry = batch[idx] || batch[0];
            if (!entry) continue;

            if (result.classification === 'safe_to_delete') {
                if (entry.type === 'directory') {
                    verifyTargets.push({ entry, result });
                } else {
                    this._addDeletable(entry, result, false);
                }
            } else if (result.classification === 'suspicious' && entry.type === 'directory') {
                this._appendSuspiciousDir(suspiciousDirs, entry);
            }
        }

        if (verifyTargets.length > 0 && !this.stopped && !this._hasReachedTarget()) {
            const verifyLimiter = pLimit(LAYER_CONCURRENCY);
            await Promise.all(
                verifyTargets.map((target) =>
                    verifyLimiter(() => this._verifyDirectoryAndMaybeAdd(target.entry, target.result, suspiciousDirs))
                )
            );
        }

        this.emit('progress', this._snapshot());

        if (suspiciousDirs.length > 0 && !this.stopped && !this._hasReachedTarget()) {
            this.status = 'scanning';
            const recurseLimiter = pLimit(LAYER_CONCURRENCY);
            await Promise.all(
                suspiciousDirs.map((dir) => recurseLimiter(() => this._scanLayer(dir.path, depth + 1)))
            );
        }
    }

    async _scanLayer(dirPath, depth) {
        if (this.stopped || depth >= this.maxDepth || this._hasReachedTarget()) {
            return;
        }

        this.currentPath = dirPath;
        this.currentDepth = depth;
        this.emit('progress', this._snapshot());

        console.log(`[Scanner] dust scan: ${dirPath} (depth=${depth})`);
        const rawEntries = runDust(dirPath, 1);
        const entries = normalizeDustEntries(rawEntries, dirPath);

        this.scannedCount += entries.length;
        this.totalEntries += entries.length;
        console.log(`[Scanner] Found ${entries.length} entries in ${dirPath}`);

        if (entries.length === 0 || this.stopped || this._hasReachedTarget()) {
            return;
        }

        this.status = 'analyzing';
        this.emit('progress', this._snapshot());

        const batches = [];
        for (let i = 0; i < entries.length; i += BATCH_SIZE) {
            batches.push({
                batchIndex: i / BATCH_SIZE + 1,
                items: entries.slice(i, i + BATCH_SIZE),
            });
        }

        const batchLimiter = pLimit(LAYER_CONCURRENCY);
        await Promise.all(
            batches.map(({ batchIndex, items }) =>
                batchLimiter(() => this._processBatch(items, batchIndex, dirPath, depth))
            )
        );

        this.emit('progress', this._snapshot());
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
