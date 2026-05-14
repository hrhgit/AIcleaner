import type { OrganizeResultRow, TreeNode } from '../../types';

export const UNCATEGORIZED_FOLDER_NAME = '其他待定';
export const CLASSIFICATION_ERROR_FOLDER_NAME = '分类错误';

export type OrganizeBrowserFile = {
  id: string;
  name: string;
  path: string;
  itemType: string;
  categoryPath: string[];
  leafNodeId: string;
  classificationError: string;
  reason: string;
};

export type OrganizeBrowserSampleItem = {
  name: string;
  reason: string;
};

export type OrganizeBrowserFolder = {
  id: string;
  name: string;
  path: string[];
  folders: OrganizeBrowserFolder[];
  files: OrganizeBrowserFile[];
  fileCount: number;
  sampleItems: OrganizeBrowserSampleItem[];
};

type MutableFolder = OrganizeBrowserFolder & {
  folderMap: Map<string, MutableFolder>;
};

function createFolder(name: string, path: string[]): MutableFolder {
  return {
    id: path.length ? `folder:${path.join('/')}` : 'folder:root',
    name,
    path,
    folders: [],
    files: [],
    fileCount: 0,
    sampleItems: [],
    folderMap: new Map(),
  };
}

function normalizeCategoryPath(value: unknown): string[] {
  if (!Array.isArray(value)) return [];
  return value
    .filter((item): item is string => typeof item === 'string')
    .map((item) => item.trim())
    .filter(Boolean);
}

function basename(path: string): string {
  return path.replace(/[\\/]+$/, '').split(/[\\/]/).filter(Boolean).pop() || path || '-';
}

function fileName(row: OrganizeResultRow): string {
  const name = String(row.name || '').trim();
  if (name) return name;
  return basename(String(row.path || '').trim());
}

function fileId(row: OrganizeResultRow, index: number): string {
  const path = String(row.path || '').trim();
  if (path) return `file:${path}`;
  return `file:index:${row.index ?? index}:${fileName(row)}`;
}

function rowClassificationError(row: OrganizeResultRow): string {
  return String(row.classificationError || '').trim();
}

function rowFolderPath(row: OrganizeResultRow): string[] {
  const error = rowClassificationError(row);
  if (error) return [CLASSIFICATION_ERROR_FOLDER_NAME];
  const categoryPath = normalizeCategoryPath(row.categoryPath);
  return categoryPath.length ? categoryPath : [UNCATEGORIZED_FOLDER_NAME];
}

function insertFolder(root: MutableFolder, folderPath: string[]): MutableFolder {
  let current = root;
  for (const segment of folderPath) {
    const existing = current.folderMap.get(segment);
    if (existing) {
      current = existing;
      continue;
    }
    const child = createFolder(segment, [...current.path, segment]);
    current.folderMap.set(segment, child);
    current.folders.push(child);
    current = child;
  }
  return current;
}

function sortFolder(folder: MutableFolder): OrganizeBrowserFolder {
  folder.folders.sort((left, right) => left.name.localeCompare(right.name, 'zh-Hans-CN'));
  folder.files.sort((left, right) => left.name.localeCompare(right.name, 'zh-Hans-CN'));
  folder.fileCount = folder.files.length;
  // Collect sample items from direct files (up to 5)
  const sampleItems: OrganizeBrowserSampleItem[] = [];
  for (const file of folder.files) {
    if (sampleItems.length >= 5) break;
    sampleItems.push({ name: file.name, reason: file.reason });
  }
  for (const child of folder.folders as MutableFolder[]) {
    const sortedChild = sortFolder(child);
    folder.fileCount += sortedChild.fileCount;
    // Fill remaining sample slots from child folders
    for (const item of sortedChild.sampleItems) {
      if (sampleItems.length >= 5) break;
      sampleItems.push(item);
    }
  }
  return {
    id: folder.id,
    name: folder.name,
    path: folder.path,
    folders: folder.folders,
    files: folder.files,
    fileCount: folder.fileCount,
    sampleItems,
  };
}

export function buildOrganizeBrowserTree(rows: OrganizeResultRow[]): OrganizeBrowserFolder {
  const root = createFolder('', []);

  rows.forEach((row, index) => {
    const folderPath = rowFolderPath(row);
    const displayPath = normalizeCategoryPath(row.categoryPath);
    const folder = insertFolder(root, folderPath);
    folder.files.push({
      id: fileId(row, index),
      name: fileName(row),
      path: String(row.path || '').trim(),
      itemType: String(row.itemType || 'file').trim() || 'file',
      categoryPath: displayPath,
      leafNodeId: String(row.leafNodeId || '').trim(),
      classificationError: rowClassificationError(row),
      reason: String(row.reason || '').trim(),
    });
  });

  return sortFolder(root);
}

function normalizeTreeNodeName(node: TreeNode): string {
  return String(node.name || '').trim() || '-';
}

function treeNodeItemCount(node: TreeNode): number {
  const explicit = Number(node.itemCount);
  if (Number.isFinite(explicit) && explicit >= 0) return explicit;
  const children = Array.isArray(node.children) ? node.children : [];
  return children.reduce((sum, child) => sum + treeNodeItemCount(child), 0);
}

function treeNodeId(node: TreeNode, path: string[], index: number): string {
  const rawId = String(node.nodeId || node.id || '').trim();
  if (rawId) return `folder:${rawId}`;
  const fallbackPath = [...path, normalizeTreeNodeName(node)].join('/');
  return `folder:tree:${fallbackPath || index}`;
}

function treeNodeToFolder(node: TreeNode, path: string[], index: number): OrganizeBrowserFolder {
  const name = normalizeTreeNodeName(node);
  const nextPath = [...path, name];
  const children = Array.isArray(node.children) ? node.children : [];
  const folders = children.map((child, childIndex) => treeNodeToFolder(child, nextPath, childIndex));
  return {
    id: treeNodeId(node, path, index),
    name,
    path: nextPath,
    folders,
    files: [],
    fileCount: treeNodeItemCount(node),
    sampleItems: [],
  };
}

export function buildOrganizeBrowserTreeFromTreeNodes(nodes: TreeNode[]): OrganizeBrowserFolder {
  return {
    id: 'folder:root',
    name: '',
    path: [],
    folders: nodes.map((node, index) => treeNodeToFolder(node, [], index)),
    files: [],
    fileCount: nodes.reduce((sum, node) => sum + treeNodeItemCount(node), 0),
    sampleItems: [],
  };
}

export function findOrganizeBrowserFolder(
  root: OrganizeBrowserFolder,
  path: string[],
): OrganizeBrowserFolder | null {
  let current: OrganizeBrowserFolder = root;
  for (const segment of path) {
    const next = current.folders.find((folder) => folder.name === segment);
    if (!next) return null;
    current = next;
  }
  return current;
}
