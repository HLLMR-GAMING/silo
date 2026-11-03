// Two copies of one mod (#423): the game loads only one copy of a tech name, so Diagnose →
// Problems lists every tech name the scan found more than once, with "Keep this one". Pure.

/** What the grouping needs to know about a scanned row. */
export interface CopyLike {
  techName: string;
  path: string;
}

/** One mod found more than once. */
export interface DuplicateGroup<M extends CopyLike> {
  techName: string;
  /** Every copy, in scan order. */
  copies: M[];
}

/** The file or folder name at the end of a path. */
function entryName(path: string): string {
  return path.split(/[\\/]/).pop() ?? path;
}

/**
 * The tech names the scan found more than once (compared ignoring case, as the game does), each
 * with its copies. A tech name with a file at its name that isn't Silo's copy is left out: Problems
 * lists that under "A different build" with its own choices.
 * @param mods the scanned rows
 * @param foreignFiles file names in the mods folder that aren't Silo's copy of their mod
 */
export function duplicateCopies<M extends CopyLike>(
  mods: readonly M[],
  foreignFiles: readonly string[] = [],
): DuplicateGroup<M>[] {
  const foreign = new Set(foreignFiles.map((f) => f.toLowerCase()));
  const groups = new Map<string, M[]>();
  for (const m of mods) {
    const key = m.techName.toLowerCase();
    const list = groups.get(key);
    if (list) list.push(m);
    else groups.set(key, [m]);
  }
  const out: DuplicateGroup<M>[] = [];
  for (const copies of groups.values()) {
    if (copies.length < 2) continue;
    if (copies.some((c) => foreign.has(entryName(c.path).toLowerCase()))) continue;
    out.push({ techName: copies[0].techName, copies });
  }
  return out;
}
