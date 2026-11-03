// Remove from library (#429): the confirmation and result wording. Pure, so it unit-tests; App
// runs the dialog and the command.
import { formatCount } from "./plural";

/** What the wording needs to know about one mod being removed. */
export interface RemovingMod {
  techName: string;
  /** The path the Library shows (what the command reports back). */
  path: string;
  title: string | null;
  /** The file or folder name Silo moves. */
  fileName: string;
  kind: "zip" | "dir";
  /** It's on now: Silo parks it first. */
  on: boolean;
}

/** "Recycle Bin" on Windows, "Trash" elsewhere. */
export function binName(windows: boolean): string {
  return windows ? "Recycle Bin" : "Trash";
}

const name = (m: RemovingMod) => m.title || m.techName;
/** Where it can't recycle (a USB or network drive, a full or turned-off bin), Windows asks. */
const asks = (windows: boolean) =>
  windows ? " If Windows can't put one there, it asks you before deleting it permanently." : "";
const what = (m: RemovingMod) => `${m.kind === "dir" ? "folder" : "file"}, ${m.fileName}`;

/**
 * The confirmation: which mods, where their files go (and that they can come back from there),
 * and that the ones on are parked first. Names up to three; the rest are counted.
 */
export function removeConfirmText(mods: RemovingMod[], windows: boolean): string {
  const bin = binName(windows);
  const on = mods.filter((m) => m.on).length;
  if (mods.length === 1) {
    const m = mods[0];
    const parked = m.on ? " It's on now, so Silo parks it first." : "";
    return `Remove ${name(m)} from your library? Its ${what(m)}, goes to the ${bin}, so you can restore it from there.${parked}${asks(windows)}`;
  }
  const shown = mods.slice(0, 3).map((m) => `${name(m)} (${m.fileName})`);
  const more = mods.length > 3 ? `, and ${formatCount(mods.length - 3)} more` : "";
  const parked =
    on === 0
      ? ""
      : on === mods.length
        ? " They're on now, so Silo parks them first."
        : ` ${formatCount(on)} of them ${on === 1 ? "is" : "are"} on now, so Silo parks ${on === 1 ? "it" : "them"} first.`;
  return `Remove ${formatCount(mods.length)} mods from your library? Their files and folders go to the ${bin}, so you can restore them from there: ${shown.join(", ")}${more}.${parked}${asks(windows)}`;
}

/** The confirm button's words. */
export function removeVerb(windows: boolean): string {
  return `Move to ${binName(windows)}`;
}

/**
 * The success toast for the mods that went to the bin, or null when none did. (Ones Windows
 * deleted permanently are told by the report's notices.)
 * @param removed what the command removed (matched by path); @param mods what was asked
 */
export function removedText(
  removed: { techName: string; path: string; recycled: boolean }[],
  mods: RemovingMod[],
  windows: boolean,
): string | null {
  removed = removed.filter((r) => r.recycled);
  if (removed.length === 0) return null;
  const bin = binName(windows);
  if (removed.length === 1) {
    const m = mods.find((x) => x.path === removed[0].path);
    return `Moved ${m ? name(m) : removed[0].techName} to the ${bin}.`;
  }
  return `Moved ${formatCount(removed.length)} mods to the ${bin}.`;
}
