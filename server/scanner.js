/**
 * server/scanner.js
 * Layered scanning engine with manual folder-by-folder AI analysis.
 */
import { execSync } from 'child_process';
import { EventEmitter } from 'events';
import { join, dirname, basename, resolve } from 'path';
import { fileURLToPath } from 'url';
import { existsSync, statSync, readdirSync } from 'fs';
import { analyzeDirectoryReview } from './agent.js';
import {
    createDirNode,
    createNodeId,
    initScanTaskStore,
    loadDirChildren,
    loadDirNode,
    saveDirNode,
    saveScanSnapshot,
} from './scan-store.js';

const __dirname = dirname(fileURLToPath(import.meta.url));
const DUST_BIN = join(__dirname, '..', 'bin', 'dust.exe');

function dustEntryIsDir(entryPath, dustEntry) {
    if (dustEntry && Array.isArray(dustEntry.children) && dustEntry.children.length > 0) {
        return true;
    }

    const base = basename(entryPath);
    const dotIndex = base.lastIndexOf('.');
    if (dotIndex <= 0) return true;
    return false;
}

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
            path: resolve(e.path || e.name || join(parentPath, e.name || '')),
        }));
}

function toFolderEntry(node, children = []) {
    const estimatedSize = Number.isFinite(node.size)
        ? node.size
        : children.reduce((sum, child) => sum + (child.size || 0), 0);

    return {
        name: node.name,
        path: node.path,
        size: estimatedSize,
        type: 'directory',
        nodeId: node.id,
    };
}

export class ScanTask extends EventEmitter {
    constructor(options) {
        super();
        this.id = Date.now().toString(36) + Math.random().toString(36).slice(2, 7);
        this.targetPath = resolve(String(options.targetPath || ''));
        this.targetSize = options.targetSize || Infinity;
        this.maxDepth = options.maxDepth || 5;
        this.rootNodeId = createNodeId(this.targetPath);
        this.stopped = false;

        this.deletable = [];
        this.deletablePathSet = new Set();
        this.totalCleanable = 0;
        this.scannedCount = 0;
        this.totalEntries = 0;
        this.processedEntries = 0;
        this.tokenUsage = { prompt: 0, completion: 0, total: 0 };
        this.currentPath = this.targetPath;
        this.currentDepth = 0;
        this.status = 'idle';
        this.persistQueue = Promise.resolve();
    }

    stop() {
        if (this.stopped) return;
        this.stopped = true;
        this.status = 'stopped';
        const snap = this._snapshot();
        this.emit('stopped', snap);
        this._queueSnapshotPersist(snap);
    }

    async start() {
        await initScanTaskStore(this.id, {
            targetPath: this.targetPath,
            targetSize: this.targetSize,
            maxDepth: this.maxDepth,
        });

        this.status = 'scanning';
        await this._emitProgress();

        try {
            await this._buildTree(this.targetPath, 0, null);

            if (!this.stopped) {
                this.status = 'done';
                const snap = this._snapshot();
                this.emit('done', snap);
                await this._queueSnapshotPersist(snap);
            }
        } catch (err) {
            this.status = 'error';
            const snap = this._snapshot();
            this.emit('error', { message: err.message, snapshot: snap });
            await this._queueSnapshotPersist(snap);
        }
    }

    async analyzeFolder(folderPath) {
        if (this.stopped) {
            throw new Error('Task is stopped');
        }
        if (this.status === 'analyzing') {
            throw new Error('Directory analysis is already running');
        }

        const resolvedPath = resolve(String(folderPath || ''));
        const rootNode = await loadDirNode(this.id, createNodeId(resolvedPath));
        if (!rootNode) {
            throw new Error(`Folder is not in cached scan tree: ${resolvedPath}`);
        }

        this.status = 'analyzing';
        this.currentPath = rootNode.path;
        this.currentDepth = rootNode.depth || 0;
        await this._emitProgress();

        const visited = new Set();
        await this._analyzeFolderRecursive(rootNode, visited);

        if (!this.stopped) {
            this.status = 'done';
            const snap = this._snapshot();
            this.emit('done', snap);
            await this._queueSnapshotPersist(snap);
            return snap;
        }

        return this._snapshot();
    }

    _hasReachedTarget() {
        return this.totalCleanable >= this.targetSize;
    }

    _trackTokenUsage(tokenUsage) {
        this.tokenUsage.prompt += tokenUsage?.prompt || 0;
        this.tokenUsage.completion += tokenUsage?.completion || 0;
        this.tokenUsage.total += tokenUsage?.total || 0;
    }

    _queueSnapshotPersist(snapshot = this._snapshot()) {
        this.persistQueue = this.persistQueue
            .catch(() => null)
            .then(() => saveScanSnapshot(this.id, snapshot))
            .catch((err) => {
                console.error('[Scanner] Failed to persist snapshot:', err.message);
                return null;
            });
        return this.persistQueue;
    }

    async _emitProgress() {
        const snap = this._snapshot();
        this.emit('progress', snap);
        await this._queueSnapshotPersist(snap);
    }

    _addDeletable(entry, result) {
        if (this.stopped || this._hasReachedTarget()) {
            return false;
        }

        if (!entry?.path || this.deletablePathSet.has(entry.path)) {
            return false;
        }

        const item = {
            name: entry.name,
            path: entry.path,
            size: Number(entry.size || 0),
            type: entry.type,
            purpose: result?.purpose || '',
            reason: result?.reason || '',
            risk: result?.risk || 'low',
        };

        this.deletable.push(item);
        this.deletablePathSet.add(entry.path);
        this.totalCleanable += item.size;
        this.emit('found', item);
        return true;
    }

    async _buildTree(dirPath, depth, size = null) {
        if (this.stopped || depth > this.maxDepth) {
            return;
        }

        this.currentPath = dirPath;
        this.currentDepth = depth;
        this.status = 'scanning';
        await this._emitProgress();

        const rawEntries = runDust(dirPath, 1);
        const entries = normalizeDustEntries(rawEntries, dirPath);

        this.scannedCount += entries.length;
        this.totalEntries += entries.length;

        const node = createDirNode({
            path: dirPath,
            depth,
            size,
            children: entries.map((entry) => ({
                name: entry.name,
                size: entry.size,
                type: entry.type,
                path: entry.path,
                nodeId: entry.type === 'directory' && depth < this.maxDepth
                    ? createNodeId(entry.path)
                    : null,
            })),
        });

        await saveDirNode(this.id, node);
        await this._emitProgress();

        if (depth >= this.maxDepth) {
            return;
        }

        for (const entry of entries) {
            if (this.stopped) return;
            if (entry.type !== 'directory') continue;
            await this._buildTree(entry.path, depth + 1, entry.size);
        }
    }

    async _analyzeFolderRecursive(node, visited) {
        if (this.stopped || this._hasReachedTarget()) {
            return;
        }
        if (!node || visited.has(node.path)) {
            return;
        }

        visited.add(node.path);
        this.currentPath = node.path;
        this.currentDepth = node.depth || 0;
        this.status = 'analyzing';
        await this._emitProgress();

        const children = await loadDirChildren(this.id, node);
        const entries = children.map((entry) => ({
            name: entry.name,
            size: entry.size,
            type: entry.type,
            path: entry.path,
            nodeId: entry.nodeId || null,
        }));

        this.emit('agent_call', {
            batchIndex: `folder:${node.name}`,
            batchSize: entries.length,
            dirPath: node.path,
            entries: entries.map((e) => ({ name: e.name, type: e.type, size: e.size })),
        });

        const review = await analyzeDirectoryReview(node.name, node.path, entries);
        if (this.stopped) return;

        this._trackTokenUsage(review.tokenUsage);
        this.processedEntries += entries.length;

        const childClassifications = review.children.reduce((acc, item) => {
            acc[item.classification] = (acc[item.classification] || 0) + 1;
            return acc;
        }, {});

        this.emit('agent_response', {
            model: review.trace.model,
            elapsed: review.trace.elapsed,
            reasoning: review.trace.reasoning,
            rawContent: review.trace.rawContent,
            userPrompt: review.trace.userPrompt,
            error: review.trace.error,
            tokenUsage: review.tokenUsage,
            resultsCount: review.children.length + 1,
            classifications: {
                [`folder_${review.folder.classification}`]: 1,
                ...childClassifications,
            },
        });

        if (review.folder.classification === 'safe_to_delete') {
            const folderEntry = toFolderEntry(node, entries);
            this._addDeletable(folderEntry, review.folder);
            await this._emitProgress();
            return;
        }

        for (let idx = 0; idx < review.children.length; idx += 1) {
            if (this.stopped || this._hasReachedTarget()) {
                return;
            }

            const result = review.children[idx];
            const entry = entries[idx];
            if (!entry) continue;

            if (result.classification === 'safe_to_delete') {
                this._addDeletable(entry, result);
            }
        }

        await this._emitProgress();

        for (let idx = 0; idx < review.children.length; idx += 1) {
            if (this.stopped || this._hasReachedTarget()) {
                return;
            }

            const result = review.children[idx];
            const entry = entries[idx];
            if (!entry || entry.type !== 'directory') continue;
            if (result.classification === 'safe_to_delete') continue;

            const childNode = await loadDirNode(this.id, entry.nodeId || createNodeId(entry.path));
            if (!childNode) continue;
            await this._analyzeFolderRecursive(childNode, visited);
        }
    }

    _snapshot() {
        return {
            id: this.id,
            status: this.status,
            targetPath: this.targetPath,
            rootNodeId: this.rootNodeId,
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
