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
export function entryName(path: string): string {
  return path.split(/[\\/]/).pop() ?? path;
}

/**
 * The tech names the scan found more than once, each with its copies. Compared exactly, as the
 * scan tells mods apart (a name differing only in case can be Silo's own link to a copy, which
 * is no second copy). A tech name with a file at its name that isn't Silo's copy is left out:
 * Problems lists that under "A different build" with its own choices.
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
    const list = groups.get(m.techName);
    if (list) list.push(m);
    else groups.set(m.techName, [m]);
  }
  const out: DuplicateGroup<M>[] = [];
  for (const copies of groups.values()) {
    if (copies.length < 2) continue;
    if (copies.some((c) => foreign.has(entryName(c.path).toLowerCase()))) continue;
    out.push({ techName: copies[0].techName, copies });
  }
  return out;
}

/** Where a copy is, in words: its category folder in the library, or the mods folder. */
export function copyPlace(m: { path: string; organized: boolean }): string {
  if (!m.organized) return "in your mods folder";
  const parts = m.path.split(/[\\/]/);
  const folder = parts.length >= 2 ? parts[parts.length - 2] : "";
  return folder ? `in your library, ${folder}` : "in your library";
}

/**
 * The copy the game loads now, when exactly one of them shows as on; otherwise none (the scan can
 * mark two library copies of one name active, and then it can't be told which one loads).
 */
export function loadedCopy<M extends CopyLike>(copies: readonly M[], isOn: (m: M) => boolean): M | null {
  const on = copies.filter(isOn);
  return on.length === 1 ? on[0] : null;
}
