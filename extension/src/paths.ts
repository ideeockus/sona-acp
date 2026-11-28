// Resolving the paths of file tools the way the OS will see them (D76).

import { realpathSync } from "node:fs";
import { basename, dirname, join, resolve } from "node:path";

/** Pi's built-in file tools, and whether their path may be omitted (then it is the workdir). */
export const FILE_TOOLS: Readonly<Record<string, boolean>> = {
  read: false,
  write: false,
  edit: false,
  ls: true,
  grep: true,
  find: true,
  // Ours, to and from File Storage (D106).
  file_upload: false,
  file_download: false,
  skill_propose: false,
};

/**
 * The real absolute path behind `path`: relative to `cwd`, symlinks followed, no `.`
 * or `..`. A path that does not exist yet resolves through its nearest existing
 * ancestor, so a symlinked directory cannot carry a new file out of the workdir.
 * Anything unexpected, such as a file in the middle of the path, throws.
 */
export function resolveReal(cwd: string, path: string): string {
  let existing = resolve(cwd, path);
  const missing: string[] = [];
  for (;;) {
    try {
      return join(realpathSync(existing), ...missing);
    } catch (error) {
      const parent = dirname(existing);
      if ((error as NodeJS.ErrnoException).code !== "ENOENT" || parent === existing) {
        throw error;
      }
      missing.unshift(basename(existing));
      existing = parent;
    }
  }
}
