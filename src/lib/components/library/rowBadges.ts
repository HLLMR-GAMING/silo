// The badges a library row or card shows, most important first. Pure, so it unit-tests without
// the app. Rows show at most three (capBadges) and a "+n" for the rest, so the order matters:
//   problems (Modified, Can't load, Ignored name, Conflict, Flagged)
//   → status (Other copy loads, Verified build, Update, Folder mod, Hidden)
//   → info (Map, Has settings, script count)
//   rowBadges(mod, { verdict: "modified", hasUpdate: true }) -> [Modified, Update, ...]
import type { IconName } from "../../ui/icons";
import type { ModEntry, VerifyStatus } from "../../types";

export type BadgeTone = "neutral" | "info" | "success" | "warn" | "danger" | "accent";
export type BadgeGroup = "problem" | "status" | "info";

/** One badge: the word, its icon and tone, and a longer explanation for the tooltip. */
export interface BadgeSpec {
  id: string;
  label: string;
  tone: BadgeTone;
  icon: IconName;
  group: BadgeGroup;
  /** Tooltip text: what the badge means for this mod. */
  title: string;
}

/** What the row knows about the mod beyond its scan entry. */
export interface RowBadgeContext {
  /** The integrity verdict, once the mod was checked (drawer or sweep). */
  verdict?: VerifyStatus | null;
  hasUpdate?: boolean;
  /** Settings Silo can edit (from the mods-with-settings scan). */
  hasSettings?: boolean;
  /** Part of a warning-or-worse conflict. */
  conflicted?: boolean;
  /** The user's own "flag" curation. */
  flagged?: boolean;
  /** Hidden by the user (only listed while "Hidden" is on). */
  hidden?: boolean;
  /** A parked copy whose tech name the game loads from another copy: that copy's file name (#421). */
  shadowedBy?: string;
}

type BadgeMod = Pick<ModEntry, "error" | "ignoredDigitPrefix" | "kind" | "isMap" | "scriptCount">;

/**
 * The badges for one mod, in display order (problems, then status, then info).
 * @param mod the scanned mod
 * @param ctx verdict, update and curation facts the scan entry doesn't carry
 * @returns every badge that applies; cap them with capBadges(…, 3)
 */
export function rowBadges(mod: BadgeMod, ctx: RowBadgeContext = {}): BadgeSpec[] {
  const out: BadgeSpec[] = [];
  const add = (b: BadgeSpec) => out.push(b);

  // Problems: something is wrong with the file or how the game will treat it.
  if (ctx.verdict === "modified") {
    add({ id: "modified", label: "Modified", tone: "warn", icon: "modified", group: "problem", title: "Modified from the published build" });
  }
  if (mod.error) {
    add({ id: "error", label: "Can't load", tone: "danger", icon: "problem", group: "problem", title: mod.error });
  }
  if (mod.ignoredDigitPrefix) {
    add({ id: "ignored", label: "Ignored name", tone: "warn", icon: "problem", group: "problem", title: "The name starts with a number, so the game ignores this mod." });
  }
  if (ctx.conflicted) {
    add({ id: "conflict", label: "Conflict", tone: "warn", icon: "problem", group: "problem", title: "Conflicts with another mod. See Diagnose." });
  }
  if (ctx.flagged) {
    add({ id: "flagged", label: "Flagged", tone: "danger", icon: "flagged", group: "problem", title: "You flagged this mod" });
  }

  // Status: where the mod stands.
  if (ctx.shadowedBy) {
    add({ id: "shadowed", label: "Other copy loads", tone: "warn", icon: "copy", group: "status", title: `The game loads another copy of this mod, ${ctx.shadowedBy} in your mods folder, so this one stays parked.` });
  }
  if (ctx.verdict === "verified") {
    add({ id: "verified", label: "Verified build", tone: "success", icon: "verifiedBuild", group: "status", title: "Matches the published build" });
  }
  if (ctx.hasUpdate) {
    add({ id: "update", label: "Update", tone: "accent", icon: "update", group: "status", title: "A newer version is available" });
  }
  if (mod.kind === "dir") {
    add({ id: "folder", label: "Folder mod", tone: "info", icon: "folder", group: "status", title: "Unpacked folder mod (a dev build). Silo shows it but leaves it unmanaged." });
  }
  if (ctx.hidden) {
    add({ id: "hidden", label: "Hidden", tone: "neutral", icon: "hidden", group: "status", title: "You hid this mod" });
  }

  // Info: what kind of mod it is.
  if (mod.isMap) {
    add({ id: "map", label: "Map", tone: "neutral", icon: "map", group: "info", title: "A map" });
  }
  if (ctx.hasSettings) {
    add({ id: "settings", label: "Has settings", tone: "neutral", icon: "modSettings", group: "info", title: "Has settings you can edit" });
  }
  if (mod.scriptCount > 0) {
    const n = mod.scriptCount;
    add({
      id: "scripts",
      label: `${n} script${n === 1 ? "" : "s"}`,
      tone: "neutral",
      icon: "script",
      group: "info",
      title: `Adds ${n} Lua script${n === 1 ? "" : "s"}`,
    });
  }
  return out;
}
