import { describe, expect, it } from 'vitest';
import {
  buildOrganizeBrowserTree,
  CLASSIFICATION_ERROR_FOLDER_NAME,
  findOrganizeBrowserFolder,
  UNCATEGORIZED_FOLDER_NAME,
} from './organizeBrowser';

describe('organize browser tree', () => {
  it('builds nested folders from category paths', () => {
    const tree = buildOrganizeBrowserTree([
      {
        path: 'E:/Work/contracts/rent.pdf',
        name: 'rent.pdf',
        itemType: 'file',
        categoryPath: ['文档', '合同'],
        leafNodeId: 'leaf-contract',
      },
    ]);

    const docs = findOrganizeBrowserFolder(tree, ['文档']);
    const contracts = findOrganizeBrowserFolder(tree, ['文档', '合同']);

    expect(docs?.fileCount).toBe(1);
    expect(contracts?.files).toHaveLength(1);
    expect(contracts?.files[0]).toMatchObject({
      id: 'file:E:/Work/contracts/rent.pdf',
      name: 'rent.pdf',
      itemType: 'file',
      leafNodeId: 'leaf-contract',
    });
  });

  it('puts empty category paths into the uncategorized folder', () => {
    const tree = buildOrganizeBrowserTree([
      {
        path: 'E:/Work/loose.txt',
        name: 'loose.txt',
        categoryPath: [],
      },
    ]);

    const uncategorized = findOrganizeBrowserFolder(tree, [UNCATEGORIZED_FOLDER_NAME]);

    expect(uncategorized?.files.map((file) => file.name)).toEqual(['loose.txt']);
  });

  it('keeps classification errors visible in the error folder', () => {
    const tree = buildOrganizeBrowserTree([
      {
        path: 'E:/Work/bad.bin',
        name: 'bad.bin',
        categoryPath: ['原分类'],
        classificationError: 'model output was invalid',
      },
    ]);

    const errorFolder = findOrganizeBrowserFolder(tree, [CLASSIFICATION_ERROR_FOLDER_NAME]);

    expect(errorFolder?.files).toHaveLength(1);
    expect(errorFolder?.files[0].classificationError).toBe('model output was invalid');
    expect(findOrganizeBrowserFolder(tree, ['原分类'])).toBeNull();
  });

  it('uses stable ids for folders and files without paths', () => {
    const tree = buildOrganizeBrowserTree([
      { index: 7, name: 'draft.txt', categoryPath: ['文档'] },
      { index: 8, name: 'draft.txt', categoryPath: ['文档'] },
    ]);

    const docs = findOrganizeBrowserFolder(tree, ['文档']);

    expect(docs?.id).toBe('folder:文档');
    expect(docs?.files.map((file) => file.id)).toEqual([
      'file:index:7:draft.txt',
      'file:index:8:draft.txt',
    ]);
  });
});
