import Database from 'better-sqlite3';
import { createHash } from 'crypto';
import { existsSync, mkdirSync } from 'fs';
import { dirname, join, resolve, basename, extname } from 'path';
import { fileURLToPath } from 'url';

const __dirname = dirname(fileURLToPath(import.meta.url));
const DATA_DIR = join(__dirname, 'data');
const DB_PATH = join(DATA_DIR, 'scan-cache.sqlite');

let db = null;

function ensureBaseDir() {
    if (!existsSync(DATA_DIR)) {
        mkdirSync(DATA_DIR, { recursive: true });
    }
}

function getDb() {
    if (db) return db;
    ensureBaseDir();
    db = new Database(DB_PATH);
    db.pragma('journal_mode = WAL');
    db.pragma('synchronous = NORMAL');
    db.pragma('foreign_keys = ON');
    db.pragma('busy_timeout = 5000');
    ensureSchema(db);
    return db;
}

function ensureSchema(database) {
    database.exec(`
        CREATE TABLE IF NOT EXISTS scan_tasks (
            task_id TEXT PRIMARY KEY,
            root_path TEXT NOT NULL,
            status TEXT NOT NULL,
            target_size INTEGER,
            max_depth INTEGER,
            auto_analyze INTEGER NOT NULL DEFAULT 1,
            current_path TEXT,
            current_depth INTEGER NOT NULL DEFAULT 0,
            scanned_count INTEGER NOT NULL DEFAULT 0,
            total_entries INTEGER NOT NULL DEFAULT 0,
            processed_entries INTEGER NOT NULL DEFAULT 0,
            deletable_count INTEGER NOT NULL DEFAULT 0,
            total_cleanable INTEGER NOT NULL DEFAULT 0,
            token_prompt INTEGER NOT NULL DEFAULT 0,
            token_completion INTEGER NOT NULL DEFAULT 0,
            token_total INTEGER NOT NULL DEFAULT 0,
            error_message TEXT,
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL,
            finished_at TEXT
        );

        CREATE TABLE IF NOT EXISTS scan_nodes (
            task_id TEXT NOT NULL,
            node_id TEXT NOT NULL,
            parent_id TEXT,
            path TEXT NOT NULL,
            name TEXT NOT NULL,
            type TEXT NOT NULL,
            depth INTEGER NOT NULL,
            self_size INTEGER NOT NULL DEFAULT 0,
            total_size INTEGER NOT NULL DEFAULT 0,
            child_count INTEGER NOT NULL DEFAULT 0,
            mtime_ms INTEGER,
            ext TEXT,
            PRIMARY KEY (task_id, node_id),
            UNIQUE (task_id, path)
        );

        CREATE TABLE IF NOT EXISTS scan_findings (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            task_id TEXT NOT NULL,
            path TEXT NOT NULL,
            name TEXT NOT NULL,
            type TEXT NOT NULL,
            size INTEGER NOT NULL DEFAULT 0,
            classification TEXT NOT NULL,
            purpose TEXT,
            reason TEXT,
            risk TEXT,
            source TEXT NOT NULL,
            created_at TEXT NOT NULL,
            UNIQUE (task_id, path)
        );

        CREATE INDEX IF NOT EXISTS idx_scan_nodes_parent
        ON scan_nodes(task_id, parent_id, total_size DESC);

        CREATE INDEX IF NOT EXISTS idx_scan_nodes_path
        ON scan_nodes(task_id, path);

        CREATE INDEX IF NOT EXISTS idx_scan_findings_classification
        ON scan_findings(task_id, classification, size DESC);
    `);
}

function normalizePathValue(pathValue) {
    return resolve(String(pathValue || ''));
}

function toIsoNow() {
    return new Date().toISOString();
}

function rowToTokenUsage(row = {}) {
    return {
        prompt: Number(row.token_prompt || 0),
        completion: Number(row.token_completion || 0),
        total: Number(row.token_total || 0),
    };
}

function findingRowToItem(row) {
    return {
        name: row.name,
        path: row.path,
        size: Number(row.size || 0),
        type: row.type,
        purpose: row.purpose || '',
        reason: row.reason || '',
        risk: row.risk || 'low',
        classification: row.classification,
        source: row.source,
    };
}

function rowToSnapshot(row, deletable = []) {
    if (!row) return null;
    return {
        id: row.task_id,
        status: row.status,
        targetPath: row.root_path,
        autoAnalyze: !!row.auto_analyze,
        rootNodeId: createNodeId(row.root_path),
        currentPath: row.current_path || row.root_path,
        currentDepth: Number(row.current_depth || 0),
        scannedCount: Number(row.scanned_count || 0),
        totalEntries: Number(row.total_entries || 0),
        processedEntries: Number(row.processed_entries || 0),
        deletableCount: Number(row.deletable_count || 0),
        totalCleanable: Number(row.total_cleanable || 0),
        targetSize: Number(row.target_size || 0),
        tokenUsage: rowToTokenUsage(row),
        deletable,
        createdAt: row.created_at,
        updatedAt: row.updated_at,
        finishedAt: row.finished_at,
        errorMessage: row.error_message || '',
    };
}

function rowToHistoryItem(row) {
    return {
        taskId: row.task_id,
        rootPath: row.root_path,
        status: row.status,
        targetSize: Number(row.target_size || 0),
        maxDepth: Number(row.max_depth || 0),
        autoAnalyze: !!row.auto_analyze,
        currentPath: row.current_path || row.root_path,
        currentDepth: Number(row.current_depth || 0),
        scannedCount: Number(row.scanned_count || 0),
        totalEntries: Number(row.total_entries || 0),
        processedEntries: Number(row.processed_entries || 0),
        deletableCount: Number(row.deletable_count || 0),
        totalCleanable: Number(row.total_cleanable || 0),
        tokenUsage: rowToTokenUsage(row),
        createdAt: row.created_at,
        updatedAt: row.updated_at,
        finishedAt: row.finished_at,
        errorMessage: row.error_message || '',
    };
}

export function getScanDbPath() {
    return DB_PATH;
}

export function createNodeId(pathValue) {
    const normalized = normalizePathValue(pathValue).toLowerCase();
    return createHash('sha1').update(normalized).digest('hex');
}

export async function initScanTaskStore(taskId, options = {}) {
    const database = getDb();
    const now = toIsoNow();
    const rootPath = normalizePathValue(options.targetPath || '');
    const tx = database.transaction(() => {
        database.prepare('DELETE FROM scan_nodes WHERE task_id = ?').run(taskId);
        database.prepare('DELETE FROM scan_findings WHERE task_id = ?').run(taskId);
        database.prepare(`
            INSERT INTO scan_tasks (
                task_id, root_path, status, target_size, max_depth, auto_analyze,
                current_path, current_depth, scanned_count, total_entries, processed_entries,
                deletable_count, total_cleanable, token_prompt, token_completion, token_total,
                error_message, created_at, updated_at, finished_at
            ) VALUES (?, ?, 'idle', ?, ?, ?, ?, 0, 0, 0, 0, 0, 0, 0, 0, 0, NULL, ?, ?, NULL)
            ON CONFLICT(task_id) DO UPDATE SET
                root_path = excluded.root_path,
                status = 'idle',
                target_size = excluded.target_size,
                max_depth = excluded.max_depth,
                auto_analyze = excluded.auto_analyze,
                current_path = excluded.current_path,
                current_depth = 0,
                scanned_count = 0,
                total_entries = 0,
                processed_entries = 0,
                deletable_count = 0,
                total_cleanable = 0,
                token_prompt = 0,
                token_completion = 0,
                token_total = 0,
                error_message = NULL,
                created_at = excluded.created_at,
                updated_at = excluded.updated_at,
                finished_at = NULL
        `).run(
            taskId,
            rootPath,
            Number.isFinite(options.targetSize) ? Math.max(0, Math.floor(options.targetSize)) : null,
            Number.isFinite(options.maxDepth) ? Math.max(0, Math.floor(options.maxDepth)) : 5,
            options.autoAnalyze === false ? 0 : 1,
            rootPath,
            now,
            now,
        );
    });
    tx();
    return loadScanTask(taskId);
}

export async function loadScanTaskRow(taskId) {
    return getDb().prepare('SELECT * FROM scan_tasks WHERE task_id = ?').get(taskId) || null;
}

export async function loadScanFindings(taskId) {
    const rows = getDb().prepare(`
        SELECT * FROM scan_findings
        WHERE task_id = ? AND classification = 'safe_to_delete'
        ORDER BY size DESC, path ASC
    `).all(taskId);
    return rows.map(findingRowToItem);
}

export async function loadScanTask(taskId) {
    const row = await loadScanTaskRow(taskId);
    if (!row) return null;
    const deletable = await loadScanFindings(taskId);
    return {
        taskId: row.task_id,
        rootPath: row.root_path,
        rootNodeId: createNodeId(row.root_path),
        maxDepth: Number(row.max_depth || 0),
        createdAt: row.created_at,
        updatedAt: row.updated_at,
        status: row.status,
        snapshot: rowToSnapshot(row, deletable),
    };
}

export async function saveScanSnapshot(taskId, snapshot = {}) {
    const patch = {
        status: snapshot.status,
        currentPath: snapshot.currentPath,
        currentDepth: snapshot.currentDepth,
        scannedCount: snapshot.scannedCount,
        totalEntries: snapshot.totalEntries,
        processedEntries: snapshot.processedEntries,
        deletableCount: snapshot.deletableCount,
        totalCleanable: snapshot.totalCleanable,
        tokenPrompt: snapshot.tokenUsage?.prompt,
        tokenCompletion: snapshot.tokenUsage?.completion,
        tokenTotal: snapshot.tokenUsage?.total,
        targetSize: snapshot.targetSize,
        finishedAt: ['done', 'stopped', 'error'].includes(snapshot.status) ? toIsoNow() : null,
        clearFinishedAt: !['done', 'stopped', 'error'].includes(snapshot.status),
        errorMessage: snapshot.errorMessage || null,
    };
    await patchTask(taskId, patch);
    return loadScanTask(taskId);
}

export async function patchTask(taskId, patch = {}) {
    const database = getDb();
    const current = database.prepare('SELECT * FROM scan_tasks WHERE task_id = ?').get(taskId);
    if (!current) return null;

    const next = {
        root_path: patch.rootPath ?? current.root_path,
        status: patch.status ?? current.status,
        target_size: patch.targetSize ?? current.target_size,
        max_depth: patch.maxDepth ?? current.max_depth,
        auto_analyze: patch.autoAnalyze == null ? current.auto_analyze : (patch.autoAnalyze ? 1 : 0),
        current_path: patch.currentPath ?? current.current_path,
        current_depth: patch.currentDepth ?? current.current_depth,
        scanned_count: patch.scannedCount ?? current.scanned_count,
        total_entries: patch.totalEntries ?? current.total_entries,
        processed_entries: patch.processedEntries ?? current.processed_entries,
        deletable_count: patch.deletableCount ?? current.deletable_count,
        total_cleanable: patch.totalCleanable ?? current.total_cleanable,
        token_prompt: patch.tokenPrompt ?? current.token_prompt,
        token_completion: patch.tokenCompletion ?? current.token_completion,
        token_total: patch.tokenTotal ?? current.token_total,
        error_message: Object.prototype.hasOwnProperty.call(patch, 'errorMessage') ? patch.errorMessage : current.error_message,
        updated_at: toIsoNow(),
        finished_at: Object.prototype.hasOwnProperty.call(patch, 'finishedAt')
            ? patch.finishedAt
            : (patch.clearFinishedAt ? null : current.finished_at),
    };

    database.prepare(`
        UPDATE scan_tasks SET
            root_path = @root_path,
            status = @status,
            target_size = @target_size,
            max_depth = @max_depth,
            auto_analyze = @auto_analyze,
            current_path = @current_path,
            current_depth = @current_depth,
            scanned_count = @scanned_count,
            total_entries = @total_entries,
            processed_entries = @processed_entries,
            deletable_count = @deletable_count,
            total_cleanable = @total_cleanable,
            token_prompt = @token_prompt,
            token_completion = @token_completion,
            token_total = @token_total,
            error_message = @error_message,
            updated_at = @updated_at,
            finished_at = @finished_at
        WHERE task_id = @task_id
    `).run({ task_id: taskId, ...next });

    return loadScanTaskRow(taskId);
}

export async function applyAnalysisDelta(taskId, delta = {}) {
    const row = await loadScanTaskRow(taskId);
    if (!row) return null;
    return patchTask(taskId, {
        status: delta.status ?? row.status,
        currentPath: delta.currentPath ?? row.current_path,
        currentDepth: delta.currentDepth ?? row.current_depth,
        processedEntries: Number(row.processed_entries || 0) + Number(delta.processedEntriesDelta || 0),
        tokenPrompt: Number(row.token_prompt || 0) + Number(delta.tokenUsage?.prompt || 0),
        tokenCompletion: Number(row.token_completion || 0) + Number(delta.tokenUsage?.completion || 0),
        tokenTotal: Number(row.token_total || 0) + Number(delta.tokenUsage?.total || 0),
        clearFinishedAt: true,
    });
}

export async function refreshTaskFindingStats(taskId) {
    const row = getDb().prepare(`
        SELECT
            COUNT(*) AS deletable_count,
            COALESCE(SUM(size), 0) AS total_cleanable
        FROM scan_findings
        WHERE task_id = ? AND classification = 'safe_to_delete'
    `).get(taskId);

    await patchTask(taskId, {
        deletableCount: Number(row?.deletable_count || 0),
        totalCleanable: Number(row?.total_cleanable || 0),
    });
}

export async function upsertScanFinding(taskId, finding) {
    const database = getDb();
    database.prepare(`
        INSERT INTO scan_findings (
            task_id, path, name, type, size, classification, purpose, reason, risk, source, created_at
        ) VALUES (@task_id, @path, @name, @type, @size, @classification, @purpose, @reason, @risk, @source, @created_at)
        ON CONFLICT(task_id, path) DO UPDATE SET
            name = excluded.name,
            type = excluded.type,
            size = excluded.size,
            classification = excluded.classification,
            purpose = excluded.purpose,
            reason = excluded.reason,
            risk = excluded.risk,
            source = excluded.source,
            created_at = excluded.created_at
    `).run({
        task_id: taskId,
        path: normalizePathValue(finding.path),
        name: String(finding.name || basename(finding.path || '')),
        type: finding.type || 'file',
        size: Math.max(0, Math.floor(Number(finding.size || 0))),
        classification: String(finding.classification || 'safe_to_delete'),
        purpose: String(finding.purpose || ''),
        reason: String(finding.reason || ''),
        risk: String(finding.risk || 'low'),
        source: String(finding.source || 'scan_analysis'),
        created_at: toIsoNow(),
    });
    await refreshTaskFindingStats(taskId);
}

export async function loadDirNode(taskId, nodeId) {
    const row = getDb().prepare('SELECT * FROM scan_nodes WHERE task_id = ? AND node_id = ?').get(taskId, nodeId);
    if (!row) return null;
    return {
        id: row.node_id,
        path: row.path,
        name: row.name,
        type: row.type,
        depth: Number(row.depth || 0),
        size: Number(row.total_size || 0),
        selfSize: Number(row.self_size || 0),
        totalSize: Number(row.total_size || 0),
        childCount: Number(row.child_count || 0),
        parentId: row.parent_id || null,
        mtimeMs: row.mtime_ms == null ? null : Number(row.mtime_ms),
        ext: row.ext || '',
    };
}

export async function loadDirNodeByPath(taskId, pathValue) {
    const normalizedPath = normalizePathValue(pathValue);
    const row = getDb().prepare('SELECT * FROM scan_nodes WHERE task_id = ? AND path = ?').get(taskId, normalizedPath);
    if (!row) return null;
    return {
        id: row.node_id,
        path: row.path,
        name: row.name,
        type: row.type,
        depth: Number(row.depth || 0),
        size: Number(row.total_size || 0),
        selfSize: Number(row.self_size || 0),
        totalSize: Number(row.total_size || 0),
        childCount: Number(row.child_count || 0),
        parentId: row.parent_id || null,
        mtimeMs: row.mtime_ms == null ? null : Number(row.mtime_ms),
        ext: row.ext || '',
    };
}

export async function loadDirChildren(taskId, nodeOrId, options = {}) {
    const node = typeof nodeOrId === 'string'
        ? await loadDirNode(taskId, nodeOrId)
        : nodeOrId;

    if (!node) return [];

    const dirsOnly = !!options.dirsOnly;
    const limit = Number.isFinite(options.limit) ? Math.max(1, Math.floor(options.limit)) : null;

    const clauses = ['task_id = ?', 'parent_id = ?'];
    const params = [taskId, node.id];
    if (dirsOnly) {
        clauses.push("type = 'directory'");
    }

    let sql = `
        SELECT node_id, path, name, type, depth, self_size, total_size, child_count, mtime_ms, ext
        FROM scan_nodes
        WHERE ${clauses.join(' AND ')}
        ORDER BY total_size DESC, path COLLATE NOCASE ASC
    `;
    if (limit) {
        sql += ` LIMIT ${limit}`;
    }

    const rows = getDb().prepare(sql).all(...params);
    return rows.map((row) => ({
        name: row.name,
        size: Number(row.total_size || 0),
        type: row.type,
        path: row.path,
        nodeId: row.type === 'directory' ? row.node_id : null,
        depth: Number(row.depth || 0),
        selfSize: Number(row.self_size || 0),
        totalSize: Number(row.total_size || 0),
        childCount: Number(row.child_count || 0),
        mtimeMs: row.mtime_ms == null ? null : Number(row.mtime_ms),
        ext: row.ext || '',
    }));
}

export async function listTopLevelDirectories(taskId, limit = 5) {
    const task = await loadScanTaskRow(taskId);
    if (!task) return [];
    const rootId = createNodeId(task.root_path);
    return loadDirChildren(taskId, rootId, { dirsOnly: true, limit });
}

export async function getTaskSnapshot(taskId) {
    const task = await loadScanTask(taskId);
    return task?.snapshot || null;
}

export async function listScanTasks(limit = 20) {
    const normalizedLimit = Number.isFinite(limit) ? Math.max(1, Math.min(200, Math.floor(limit))) : 20;
    const rows = getDb().prepare(`
        SELECT *
        FROM scan_tasks
        ORDER BY datetime(updated_at) DESC, task_id DESC
        LIMIT ?
    `).all(normalizedLimit);
    return rows.map(rowToHistoryItem);
}

export async function deleteScanTask(taskId) {
    const database = getDb();
    const tx = database.transaction(() => {
        database.prepare('DELETE FROM scan_findings WHERE task_id = ?').run(taskId);
        database.prepare('DELETE FROM scan_nodes WHERE task_id = ?').run(taskId);
        return database.prepare('DELETE FROM scan_tasks WHERE task_id = ?').run(taskId);
    });
    const result = tx();
    return Number(result?.changes || 0) > 0;
}

export function closeScanDb() {
    if (db) {
        db.close();
        db = null;
    }
}

export function getDbHandle() {
    return getDb();
}

export function extForPath(pathValue) {
    return extname(pathValue || '').toLowerCase();
}
