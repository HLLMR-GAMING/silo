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
 * Whether, after a set_active, the rows may show the wrong copy of a name, so the caller reads
 * the mods folder again before letting go of the write. Either a row is shown shadowed (its
 * other copy may have gone since the last scan, and then set_active linked this one), or
 * set_active left a wanted name's archived copy parked for another copy (`otherCopies`) that no
 * row shows yet (it came since the last scan). With one row per name and no `otherCopies`
 * (the usual case) it is false, and a clean switch needs no rescan.
 * @param otherCopies what set_active reported
 * @param shadowed from shadowedCopies(mods)
 */
export function copiesMayHaveMoved(
  otherCopies: readonly string[] | undefined,
  shadowed: ReadonlyMap<string, unknown>,
): boolean {
  if (shadowed.size > 0) return true;
  return (otherCopies ?? []).length > 0;
}

/**
 * Why a shadowed copy can't be turned on, for the refusal toast.
 * @param name the row's title
 * @param other the file or folder name of the copy in the mods folder
 */
export function shadowedText(name: string, other: string): string {
  return `${name} is already on from another copy in your mods folder, ${other}. The game loads only one copy of a mod. To use this one, move ${other} out of your mods folder.`;
}
