import type { OrganizeResultRow } from '../../types';

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
};

export type OrganizeBrowserFolder = {
  id: string;
  name: string;
  path: string[];
  folders: OrganizeBrowserFolder[];
  files: OrganizeBrowserFile[];
  fileCount: number;
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
  for (const child of folder.folders as MutableFolder[]) {
    const sortedChild = sortFolder(child);
    folder.fileCount += sortedChild.fileCount;
  }
  return {
    id: folder.id,
    name: folder.name,
    path: folder.path,
    folders: folder.folders,
    files: folder.files,
    fileCount: folder.fileCount,
  };
}

export function buildOrganizeBrowserTree(rows: OrganizeResultRow[]): OrganizeBrowserFolder {
  const root = createFolder('', []);

  rows.forEach((row, index) => {
    const folderPath = rowFolderPath(row);
    const folder = insertFolder(root, folderPath);
    folder.files.push({
      id: fileId(row, index),
      name: fileName(row),
      path: String(row.path || '').trim(),
      itemType: String(row.itemType || 'file').trim() || 'file',
      categoryPath: folderPath,
      leafNodeId: String(row.leafNodeId || '').trim(),
      classificationError: rowClassificationError(row),
    });
  });

  return sortFolder(root);
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
