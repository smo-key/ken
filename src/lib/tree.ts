// Build the Files-screen tree from the flat index rows + folder list.
import type { FileRow, FolderInfo } from "./api";

export interface TreeNode {
  name: string;
  relPath: string;
  children: TreeNode[]; // folders first, then files, both alphabetical
  file?: FileRow; // present on leaves
  excluded?: boolean; // present on folders
}

export function buildTree(files: FileRow[], folders: FolderInfo[]): TreeNode[] {
  const roots: TreeNode[] = [];
  const byPath = new Map<string, TreeNode>();

  const ensureFolder = (relPath: string, excluded = false): TreeNode => {
    const existing = byPath.get(relPath);
    if (existing) return existing;
    const name = relPath.split("/").pop()!;
    const node: TreeNode = { name, relPath, children: [], excluded };
    byPath.set(relPath, node);
    const parentPath = relPath.includes("/")
      ? relPath.slice(0, relPath.lastIndexOf("/"))
      : "";
    if (parentPath) ensureFolder(parentPath).children.push(node);
    else roots.push(node);
    return node;
  };

  for (const f of folders) ensureFolder(f.relPath, f.excluded);

  for (const file of files) {
    const name = file.relPath.split("/").pop()!;
    const node: TreeNode = { name, relPath: file.relPath, children: [], file };
    const parentPath = file.relPath.includes("/")
      ? file.relPath.slice(0, file.relPath.lastIndexOf("/"))
      : "";
    if (parentPath) ensureFolder(parentPath).children.push(node);
    else roots.push(node);
  }

  const sortRec = (nodes: TreeNode[]) => {
    nodes.sort((a, b) => {
      const aFolder = a.file === undefined;
      const bFolder = b.file === undefined;
      if (aFolder !== bFolder) return aFolder ? -1 : 1;
      return a.name.localeCompare(b.name, undefined, { sensitivity: "base" });
    });
    for (const n of nodes) sortRec(n.children);
  };
  sortRec(roots);
  return roots;
}
