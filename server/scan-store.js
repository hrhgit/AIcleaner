import { createHash } from 'crypto';
import { existsSync, mkdirSync } from 'fs';
import { mkdir, readFile, rename, writeFile } from 'fs/promises';
import { basename, dirname, join, resolve } from 'path';
import { fileURLToPath } from 'url';

const __dirname = dirname(fileURLToPath(import.meta.url));
const DATA_DIR = join(__dirname, 'data');
const SCAN_CACHE_DIR = join(DATA_DIR, 'scan-cache');
const CHILD_CHUNK_SIZE = 500;

function ensureBaseDir() {
    if (!existsSync(SCAN_CACHE_DIR)) {
        mkdirSync(SCAN_CACHE_DIR, { recursive: true });
    }
}

function taskDir(taskId) {
    return join(SCAN_CACHE_DIR, taskId);
}

function nodesDir(taskId) {
    return join(taskDir(taskId), 'nodes');
}

function manifestFile(taskId) {
    return join(taskDir(taskId), 'manifest.json');
}

function nodeFile(taskId, nodeId) {
    return join(nodesDir(taskId), `${nodeId}.json`);
}

function childChunkFile(taskId, nodeId, index) {
    return join(nodesDir(taskId), `${nodeId}.children.${index}.json`);
}

function normalizePathValue(pathValue) {
    return resolve(String(pathValue || ''));
}

async function readJson(file, fallback = null) {
    try {
        const raw = await readFile(file, 'utf-8');
        return JSON.parse(raw);
    } catch (err) {
        if (err && err.code === 'ENOENT') {
            return fallback;
        }
        throw err;
    }
}

async function writeJsonAtomic(file, data) {
    await mkdir(dirname(file), { recursive: true });
    const tempFile = `${file}.${process.pid}.${Date.now()}.tmp`;
    await writeFile(tempFile, JSON.stringify(data, null, 2), 'utf-8');
    await rename(tempFile, file);
}

export function createNodeId(pathValue) {
    const normalized = normalizePathValue(pathValue).toLowerCase();
    return createHash('sha1').update(normalized).digest('hex');
}

export function createDirNode({ path, depth, children = [] }) {
    const normalizedPath = normalizePathValue(path);
    return {
        id: createNodeId(normalizedPath),
        path: normalizedPath,
        name: basename(normalizedPath) || normalizedPath,
        type: 'directory',
        depth,
        childCount: children.length,
        children,
        cachedAt: new Date().toISOString(),
    };
}

export async function initScanTaskStore(taskId, options = {}) {
    ensureBaseDir();

    const rootPath = normalizePathValue(options.targetPath || '');
    const initialSnapshot = {
        id: taskId,
        status: 'idle',
        currentPath: rootPath,
        currentDepth: 0,
        scannedCount: 0,
        totalEntries: 0,
        processedEntries: 0,
        deletableCount: 0,
        totalCleanable: 0,
        targetSize: Number.isFinite(options.targetSize) ? options.targetSize : null,
        tokenUsage: { prompt: 0, completion: 0, total: 0 },
        deletable: [],
    };

    const manifest = {
        taskId,
        rootPath,
        rootNodeId: createNodeId(rootPath),
        maxDepth: Number.isFinite(options.maxDepth) ? options.maxDepth : 5,
        createdAt: new Date().toISOString(),
        updatedAt: new Date().toISOString(),
        status: 'idle',
        snapshot: initialSnapshot,
    };

    await writeJsonAtomic(manifestFile(taskId), manifest);
    return manifest;
}

export async function saveDirNode(taskId, node) {
    ensureBaseDir();
    const children = Array.isArray(node.children) ? node.children : [];
    const chunkCount = Math.ceil(children.length / CHILD_CHUNK_SIZE);

    for (let index = 0; index < chunkCount; index += 1) {
        const chunk = children.slice(index * CHILD_CHUNK_SIZE, (index + 1) * CHILD_CHUNK_SIZE);
        await writeJsonAtomic(childChunkFile(taskId, node.id, index), chunk);
    }

    const storedNode = {
        ...node,
        childCount: children.length,
        chunkCount,
        chunkSize: CHILD_CHUNK_SIZE,
    };
    delete storedNode.children;

    await writeJsonAtomic(nodeFile(taskId, node.id), storedNode);
    return storedNode;
}

export async function loadDirNode(taskId, nodeId) {
    ensureBaseDir();
    return readJson(nodeFile(taskId, nodeId), null);
}

export async function loadDirChildren(taskId, nodeOrId) {
    const node = typeof nodeOrId === 'string'
        ? await loadDirNode(taskId, nodeOrId)
        : nodeOrId;

    if (!node) return [];

    const chunkCount = Number(node.chunkCount || 0);
    const children = [];
    for (let index = 0; index < chunkCount; index += 1) {
        const chunk = await readJson(childChunkFile(taskId, node.id, index), []);
        if (Array.isArray(chunk) && chunk.length > 0) {
            children.push(...chunk);
        }
    }
    return children;
}

export async function* loadDirChildChunks(taskId, nodeOrId) {
    const node = typeof nodeOrId === 'string'
        ? await loadDirNode(taskId, nodeOrId)
        : nodeOrId;

    if (!node) return;

    const chunkCount = Number(node.chunkCount || 0);
    for (let index = 0; index < chunkCount; index += 1) {
        const chunk = await readJson(childChunkFile(taskId, node.id, index), []);
        if (Array.isArray(chunk) && chunk.length > 0) {
            yield chunk;
        }
    }
}

export async function loadScanTask(taskId) {
    ensureBaseDir();
    return readJson(manifestFile(taskId), null);
}

export async function saveScanSnapshot(taskId, snapshot) {
    ensureBaseDir();
    const manifest = await loadScanTask(taskId);
    if (!manifest) return null;

    const nextManifest = {
        ...manifest,
        updatedAt: new Date().toISOString(),
        status: snapshot?.status || manifest.status,
        snapshot,
    };

    await writeJsonAtomic(manifestFile(taskId), nextManifest);
    return nextManifest;
}
