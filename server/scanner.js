/**
 * server/scanner.js
 * Rust sidecar driven scan task with SQLite-backed history and automatic analysis.
 */
import { spawn } from 'child_process';
import { EventEmitter } from 'events';
import { existsSync } from 'fs';
import { dirname, join, resolve } from 'path';
import readline from 'readline';
import { fileURLToPath } from 'url';
import { analyzeScanNode } from './agent.js';
import {
    applyAnalysisDelta,
    createNodeId,
    getScanDbPath,
    getTaskSnapshot,
    initScanTaskStore,
    loadDirChildren,
    loadDirNodeByPath,
    patchTask,
    saveScanSnapshot,
    upsertScanFinding,
} from './scan-store.js';

const __dirname = dirname(fileURLToPath(import.meta.url));
const SIDECAR_BIN = join(__dirname, '..', 'bin', 'scanner.exe');

function ensureScannerBinary() {
    if (!existsSync(SIDECAR_BIN)) {
        throw new Error(`scanner.exe is missing at ${SIDECAR_BIN}`);
    }
    return SIDECAR_BIN;
}

function toFolderEntry(node) {
    return {
        name: node.name,
        path: node.path,
        size: Number(node.totalSize ?? node.size ?? 0),
        type: 'directory',
        nodeId: node.id,
    };
}

function toNodeQueueItem(node) {
    return {
        id: node.id || node.nodeId || null,
        nodeId: node.nodeId || node.id || null,
        name: node.name,
        path: node.path,
        size: Number(node.totalSize ?? node.size ?? 0),
        type: node.type === 'directory' ? 'directory' : 'file',
        depth: Number(node.depth || 0),
        totalSize: Number(node.totalSize ?? node.size ?? 0),
        childCount: Number(node.childCount || 0),
    };
}

function compareQueuedNodes(a, b) {
    const sizeDiff = Number(b.size || 0) - Number(a.size || 0);
    if (sizeDiff !== 0) return sizeDiff;
    return String(a.path || '').localeCompare(String(b.path || ''), undefined, { sensitivity: 'base' });
}

function nodePathKey(pathValue) {
    return resolve(String(pathValue || '')).toLowerCase();
}

async function analyzeTaskNode(taskId, nodeInput, options = {}) {
    const source = String(options.source || 'scan_analysis');
    const resolvedPath = resolve(String(nodeInput?.path || nodeInput || ''));
    const loadedNode = await loadDirNodeByPath(taskId, resolvedPath);
    if (!loadedNode) {
        throw new Error(`Node is not in cached scan tree: ${resolvedPath}`);
    }

    const node = toNodeQueueItem(loadedNode);
    const childDirectories = node.type === 'directory'
        ? (await loadDirChildren(taskId, node, { dirsOnly: true })).map(toNodeQueueItem)
        : [];

    await patchTask(taskId, {
        status: 'analyzing',
        currentPath: node.path,
        currentDepth: node.depth || 0,
        clearFinishedAt: true,
    });

    options.onProgress?.({
        currentPath: node.path,
        currentDepth: node.depth || 0,
        status: 'analyzing',
    });

    options.onAgentCall?.({
        nodeType: node.type,
        nodePath: node.path,
        nodeName: node.name,
        nodeSize: node.size,
        childDirectories: childDirectories.map((child) => ({
            name: child.name,
            path: child.path,
            size: child.size,
        })),
    });

    if (options.shouldStop?.()) {
        throw new Error('Task is stopped');
    }

    const review = await analyzeScanNode(node, childDirectories);

    if (options.shouldStop?.()) {
        throw new Error('Task is stopped');
    }

    options.onAgentResponse?.({
        nodeType: node.type,
        nodePath: node.path,
        nodeName: node.name,
        nodeSize: node.size,
        model: review.trace.model,
        elapsed: review.trace.elapsed,
        reasoning: review.trace.reasoning,
        rawContent: review.trace.rawContent,
        userPrompt: review.trace.userPrompt,
        error: review.trace.error,
        tokenUsage: review.tokenUsage,
        classification: review.classification,
        risk: review.risk,
        hasPotentialDeletableSubfolders: node.type === 'directory'
            ? !!review.hasPotentialDeletableSubfolders
            : false,
    });

    await upsertScanFinding(taskId, {
        path: node.path,
        name: node.name,
        size: node.size,
        type: node.type,
        classification: review.classification,
        purpose: '',
        reason: review.reason,
        risk: review.risk,
        source,
    });
    await applyAnalysisDelta(taskId, {
        processedEntriesDelta: 1,
        tokenUsage: review.tokenUsage,
        currentPath: node.path,
        currentDepth: node.depth || 0,
        status: 'analyzing',
    });

    let safeItem = null;
    if (review.classification === 'safe_to_delete') {
        safeItem = node.type === 'directory'
            ? {
                ...toFolderEntry(node),
                reason: review.reason,
                risk: review.risk,
            }
            : {
                name: node.name,
                path: node.path,
                size: node.size,
                type: 'file',
                reason: review.reason,
                risk: review.risk,
            };
        options.onFound?.(safeItem);
    }

    const snapshot = await getTaskSnapshot(taskId);
    return {
        snapshot,
        review,
        node,
        childDirectories,
        safeItem,
    };
}

export class ScanTask extends EventEmitter {
    constructor(options) {
        super();
        this.id = Date.now().toString(36) + Math.random().toString(36).slice(2, 7);
        this.targetPath = resolve(String(options.targetPath || ''));
        this.targetSize = options.targetSize || Infinity;
        this.maxDepth = options.maxDepth || 5;
        this.autoAnalyze = options.autoAnalyze !== false;
        this.rootNodeId = createNodeId(this.targetPath);
        this.status = 'idle';
        this.currentPath = this.targetPath;
        this.currentDepth = 0;
        this.scannedCount = 0;
        this.totalEntries = 0;
        this.processedEntries = 0;
        this.totalCleanable = 0;
        this.deletable = [];
        this.permissionDeniedCount = 0;
        this.permissionDeniedPaths = [];
        this.tokenUsage = { prompt: 0, completion: 0, total: 0 };
        this.errorMessage = '';
        this.stopped = false;
        this.sidecar = null;
        this.sidecarFinished = false;
        this.persistQueue = Promise.resolve();
    }

    _syncFromSnapshot(snapshot) {
        if (!snapshot) return;
        this.status = snapshot.status || this.status;
        this.currentPath = snapshot.currentPath || this.currentPath;
        this.currentDepth = Number(snapshot.currentDepth || 0);
        this.scannedCount = Number(snapshot.scannedCount || 0);
        this.totalEntries = Number(snapshot.totalEntries || 0);
        this.processedEntries = Number(snapshot.processedEntries || 0);
        this.totalCleanable = Number(snapshot.totalCleanable || 0);
        this.deletable = Array.isArray(snapshot.deletable) ? snapshot.deletable : this.deletable;
        this.permissionDeniedCount = Number(snapshot.permissionDeniedCount ?? this.permissionDeniedCount);
        this.permissionDeniedPaths = Array.isArray(snapshot.permissionDeniedPaths)
            ? snapshot.permissionDeniedPaths
            : this.permissionDeniedPaths;
        this.tokenUsage = snapshot.tokenUsage || this.tokenUsage;
        this.errorMessage = snapshot.errorMessage || this.errorMessage;
    }

    async _refreshFromStore() {
        const snapshot = await getTaskSnapshot(this.id);
        if (snapshot) {
            this._syncFromSnapshot(snapshot);
        }
        return snapshot;
    }

    _snapshot() {
        return {
            id: this.id,
            status: this.status,
            targetPath: this.targetPath,
            autoAnalyze: this.autoAnalyze,
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
            permissionDeniedCount: this.permissionDeniedCount,
            permissionDeniedPaths: [...this.permissionDeniedPaths],
            errorMessage: this.errorMessage || '',
        };
    }

    _recordPermissionDenied(payload = {}) {
        const path = String(payload.path || '').trim();
        this.permissionDeniedCount += 1;
        if (path && !this.permissionDeniedPaths.includes(path)) {
            this.permissionDeniedPaths = [...this.permissionDeniedPaths, path].slice(-20);
        }
        this.emit('warning', {
            type: 'permission_denied',
            path,
            message: payload.message || 'Access is denied.',
            count: this.permissionDeniedCount,
        });
    }

    async _queueSnapshotPersist(snapshot = this._snapshot()) {
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
        return this._queueSnapshotPersist(snap);
    }

    stop() {
        if (this.stopped) return;
        this.stopped = true;
        this.status = 'stopped';
        if (this.sidecar && !this.sidecar.killed) {
            this.sidecar.kill();
        }
        const snap = this._snapshot();
        this.emit('stopped', snap);
        this._queueSnapshotPersist({ ...snap, status: 'stopped' });
        patchTask(this.id, {
            status: 'stopped',
            currentPath: this.currentPath,
            currentDepth: this.currentDepth,
            errorMessage: null,
            finishedAt: new Date().toISOString(),
        }).catch((err) => console.error('[Scanner] Failed to patch stopped task:', err.message));
    }

    async start() {
        await initScanTaskStore(this.id, {
            targetPath: this.targetPath,
            targetSize: this.targetSize,
            maxDepth: this.maxDepth,
            autoAnalyze: this.autoAnalyze,
        });

        this.status = 'scanning';
        await this._queueSnapshotPersist();
        this.emit('progress', this._snapshot());

        try {
            await this._runSidecarScan();
            if (this.stopped) {
                return;
            }

            if (this.autoAnalyze) {
                await this._runAutoAnalyze();
            }

            if (!this.stopped) {
                await this._refreshFromStore();
                this.status = 'done';
                const snap = this._snapshot();
                this.emit('done', snap);
                await this._queueSnapshotPersist({ ...snap, status: 'done' });
                await patchTask(this.id, {
                    status: 'done',
                    finishedAt: new Date().toISOString(),
                    errorMessage: null,
                });
            }
        } catch (err) {
            if (this.stopped) {
                return;
            }
            this.status = 'error';
            this.errorMessage = err.message;
            await patchTask(this.id, {
                status: 'error',
                errorMessage: err.message,
                finishedAt: new Date().toISOString(),
            });
            await this._refreshFromStore();
            const snap = this._snapshot();
            this.emit('error', { message: err.message, snapshot: snap });
            await this._queueSnapshotPersist({ ...snap, status: 'error', errorMessage: err.message });
        }
    }

    async _runAutoAnalyze() {
        const queue = [];
        const queued = new Set();
        const analyzed = new Set();
        const rootChildren = await loadDirChildren(this.id, this.rootNodeId);

        this._enqueueNodes(queue, queued, analyzed, rootChildren);

        while (queue.length > 0) {
            if (this.stopped) {
                return;
            }
            if (this._hasReachedTarget()) {
                return;
            }

            const node = queue.shift();
            const key = nodePathKey(node.path);
            queued.delete(key);

            if (analyzed.has(key)) {
                continue;
            }

            analyzed.add(key);
            const { review } = await this._analyzeAndRefresh(node, 'priority_queue');

            if (this.stopped) {
                return;
            }
            if (this._hasReachedTarget()) {
                return;
            }

            if (
                node.type === 'directory'
                && review.classification !== 'safe_to_delete'
                && review.hasPotentialDeletableSubfolders
            ) {
                const children = await loadDirChildren(this.id, node.nodeId || node.id);
                this._enqueueNodes(queue, queued, analyzed, children);
            }
        }
    }

    _enqueueNodes(queue, queued, analyzed, nodes) {
        for (const rawNode of nodes) {
            const node = toNodeQueueItem(rawNode);
            const key = nodePathKey(node.path);
            if (!node.path || analyzed.has(key) || queued.has(key)) {
                continue;
            }
            queued.add(key);
            queue.push(node);
        }
        queue.sort(compareQueuedNodes);
    }

    _hasReachedTarget() {
        return Number.isFinite(this.targetSize) && this.targetSize > 0 && this.totalCleanable >= this.targetSize;
    }

    async _analyzeAndRefresh(node, source) {
        this.status = 'analyzing';
        await this._emitProgress();
        const result = await analyzeTaskNode(this.id, node, {
            source,
            shouldStop: () => this.stopped,
            onProgress: (partial) => {
                this.currentPath = partial.currentPath || this.currentPath;
                this.currentDepth = partial.currentDepth ?? this.currentDepth;
                this.status = partial.status || this.status;
                this.emit('progress', this._snapshot());
            },
            onAgentCall: (payload) => this.emit('agent_call', payload),
            onAgentResponse: (payload) => this.emit('agent_response', payload),
            onFound: (payload) => this.emit('found', payload),
        });
        this._syncFromSnapshot(result.snapshot);
        await this._emitProgress();
        return result;
    }

    async _runSidecarScan() {
        const scannerBin = ensureScannerBinary();
        const dbPath = getScanDbPath();
        const child = spawn(scannerBin, [
            'scan',
            '--db', dbPath,
            '--task-id', this.id,
            '--root', this.targetPath,
            '--max-depth', String(this.maxDepth),
        ], {
            cwd: join(__dirname, '..'),
            windowsHide: true,
            stdio: ['ignore', 'pipe', 'pipe'],
        });

        this.sidecar = child;

        const stdout = readline.createInterface({ input: child.stdout });
        const stderr = readline.createInterface({ input: child.stderr });
        let sawCompletion = false;
        let sidecarErrorMessage = '';

        const applyProgress = async (payload = {}) => {
            this.status = 'scanning';
            this.currentPath = payload.current_path || this.currentPath;
            this.currentDepth = Number(payload.current_depth ?? this.currentDepth);
            this.scannedCount = Number(payload.scanned_count ?? this.scannedCount);
            this.totalEntries = Number(payload.total_entries ?? this.scannedCount);
            await this._refreshFromStore();
            this.emit('progress', this._snapshot());
        };

        stdout.on('line', async (line) => {
            const trimmed = String(line || '').trim();
            if (!trimmed) return;

            let payload;
            try {
                payload = JSON.parse(trimmed);
            } catch {
                console.warn('[Scanner] Ignored sidecar line:', trimmed);
                return;
            }

            const type = payload.type || '';
            try {
                if (type === 'task_started') {
                    await applyProgress(payload);
                } else if (type === 'scan_progress') {
                    await applyProgress(payload);
                } else if (type === 'scan_completed') {
                    sawCompletion = true;
                    this.status = 'scanning';
                    this.currentPath = payload.current_path || this.targetPath;
                    this.currentDepth = Number(payload.current_depth ?? 0);
                    this.scannedCount = Number(payload.scanned_count ?? this.scannedCount);
                    this.totalEntries = Number(payload.total_entries ?? this.scannedCount);
                    await patchTask(this.id, {
                        status: 'scanning',
                        currentPath: this.currentPath,
                        currentDepth: this.currentDepth,
                        scannedCount: this.scannedCount,
                        totalEntries: this.totalEntries,
                        clearFinishedAt: true,
                    });
                    await this._refreshFromStore();
                    this.emit('progress', this._snapshot());
                } else if (type === 'permission_denied') {
                    this._recordPermissionDenied(payload);
                } else if (type === 'error') {
                    sidecarErrorMessage = payload.message || 'scanner sidecar failed';
                }
            } catch (err) {
                sidecarErrorMessage = err.message;
            }
        });

        stderr.on('line', (line) => {
            const trimmed = String(line || '').trim();
            if (!trimmed) return;
            sidecarErrorMessage = trimmed;
            console.warn('[Scanner][sidecar]', trimmed);
        });

        await new Promise((resolvePromise, rejectPromise) => {
            child.once('error', (err) => {
                rejectPromise(err);
            });

            child.once('close', async (code, signal) => {
                this.sidecarFinished = true;
                stdout.close();
                stderr.close();

                if (this.stopped) {
                    return resolvePromise();
                }
                if (code === 0 && sawCompletion) {
                    await this._refreshFromStore();
                    return resolvePromise();
                }

                const message = sidecarErrorMessage || `scanner.exe exited with code ${code}${signal ? ` (${signal})` : ''}`;
                rejectPromise(new Error(message));
            });
        });
    }
}
