// Two library rows can share a tech name: an archived copy and another entry in the mods folder,
// such as an unzipped folder beside an archived zip (#421). FS25 knows a mod by its tech name, so
// the on/off choice stays keyed by tech name (`activeSet`). But a row shows active only when it
// is the copy in the mods folder: an archived copy the scan found parked while another copy of
// its name is in the mods folder is "shadowed". It shows parked, and its hint names that copy.
//   const shadowed = shadowedCopies(mods);
//   rowActive(mod, activeSet, shadowed)   // what the row's switch, label and filters show
//   shadowed.get(mod.path)                // the row the game reads instead, if any

/** The parts of a library row this reads. */
export interface CopyRow {
  techName: string;
  path: string;
  /** What the scan found: this file or folder is in the mods folder. */
  active: boolean;
}

/**
 * The tech names the scan found active: the Library's on/off set after a scan.
 * @param mods the scanned rows
 */
export function activeTechNames(mods: readonly CopyRow[]): Set<string> {
  return new Set(mods.filter((m) => m.active).map((m) => m.techName));
}

/**
 * The rows another copy of the same tech name shadows: parked in the scan while that copy is in
 * the mods folder. With one row per tech name (the usual case) it is empty.
 * @param mods the scanned rows
 * @returns each shadowed row's path → the active row of its tech name (the first one listed)
 */
export function shadowedCopies<M extends CopyRow>(mods: readonly M[]): Map<string, M> {
  const loaded = new Map<string, M>();
  for (const m of mods) if (m.active && !loaded.has(m.techName)) loaded.set(m.techName, m);
  const out = new Map<string, M>();
  for (const m of mods) {
    const by = m.active ? undefined : loaded.get(m.techName);
    if (by) out.set(m.path, by);
  }
  return out;
}

/**
 * Whether a row shows active: its tech name is on, and it isn't a shadowed copy. Between a switch
 * and the next scan this follows the switch (the optimistic `activeSet`).
 * @param mod the row
 * @param active the tech names that are on
 * @param shadowed from shadowedCopies (by path)
 */
export function rowActive(mod: CopyRow, active: ReadonlySet<string>, shadowed: ReadonlyMap<string, unknown>): boolean {
  return active.has(mod.techName) && !shadowed.has(mod.path);
}

/**
 * Whether a switch found a copy the Library doesn't show yet: set_active left a wanted name's
 * archived copy parked because another copy is in the mods folder (`otherCopies`), and no row of
 * that name is shown shadowed (the other copy arrived after the last scan). Then the rows are
 * stale and the caller rescans, so the switched row doesn't show active.
 * @param otherCopies what set_active reported
 * @param mods the rows shown
 * @param shadowed from shadowedCopies(mods)
 */
export function copiesNotShown(
  otherCopies: readonly string[] | undefined,
  mods: readonly CopyRow[],
  shadowed: ReadonlyMap<string, unknown>,
): boolean {
  return (otherCopies ?? []).some((t) => !mods.some((m) => m.techName === t && shadowed.has(m.path)));
}

/**
 * Why a shadowed copy can't be turned on, for the refusal toast.
 * @param name the row's title
 * @param other the file or folder name of the copy in the mods folder
 */
export function shadowedText(name: string, other: string): string {
  return `${name} is already on from another copy in your mods folder, ${other}. The game loads only one copy of a mod. To use this one, move ${other} out of your mods folder.`;
}
