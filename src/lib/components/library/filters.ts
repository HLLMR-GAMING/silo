// The Library's filters: the All / Active / Parked segment, the Favorites and Needs attention
// chips, and the Filters popover's checkboxes (Hidden, Flagged, In conflict, Has settings, Needs
// update, Maps, Script mods, uniqueType, a tag). Pure, so the predicates unit-test without the app.
// Category and search stay with App (FilterBar and the header own them).
//   filterMods(mods, { ...NO_FILTERS, status: "parked", maps: true }, ctx) -> parked maps
//   filterChips(filters) -> [{ id: "status", label: "Parked" }, { id: "maps", label: "Maps" }]
// The predicates are the old toggle row's and "show only" facets', moved here unchanged.
import type { ModEntry, VerifyStatus } from "../../types";
import { rowBadges } from "./rowBadges";

/** The segmented control: every mod, the active set, or the parked ones. */
export type StatusFilter = "all" | "active" | "parked";

export interface LibraryFilters {
  status: StatusFilter;
  /** Chip: favorites only. */
  favorites: boolean;
  /** Chip: mods with a problem badge or an update (see needsAttention). */
  attention: boolean;
  /** Popover: list hidden mods too (they're left out otherwise). */
  hidden: boolean;
  flagged: boolean;
  conflicted: boolean;
  hasSettings: boolean;
  needsUpdate: boolean;
  maps: boolean;
  scripts: boolean;
  /** Mods that declare a uniqueType. */
  unique: boolean;
  /** One tag, or null. */
  tag: string | null;
}

/** A boolean filter (everything but the segment and the tag). */
export type FilterFlag = Exclude<keyof LibraryFilters, "status" | "tag">;

/** The Filters popover's checkboxes, in display order. */
export const POPOVER_FLAGS: readonly { id: FilterFlag; label: string }[] = [
  { id: "hidden", label: "Include hidden mods" },
  { id: "flagged", label: "Flagged" },
  { id: "conflicted", label: "In conflict" },
  { id: "hasSettings", label: "Has settings" },
  { id: "needsUpdate", label: "Needs update" },
  { id: "maps", label: "Maps" },
  { id: "scripts", label: "Script mods" },
  { id: "unique", label: "Uses an exclusive slot" },
];

/** Chip labels ("Remove filter: Maps"). */
const FLAG_LABEL: Record<FilterFlag, string> = {
  favorites: "Favorites",
  attention: "Needs attention",
  hidden: "Including hidden",
  flagged: "Flagged",
  conflicted: "In conflict",
  hasSettings: "Has settings",
  needsUpdate: "Needs update",
  maps: "Maps",
  scripts: "Script mods",
  unique: "Exclusive slot",
};

export const NO_FILTERS: Readonly<LibraryFilters> = Object.freeze({
  status: "all",
  favorites: false,
  attention: false,
  hidden: false,
  flagged: false,
  conflicted: false,
  hasSettings: false,
  needsUpdate: false,
  maps: false,
  scripts: false,
  unique: false,
  tag: null,
});

/** What the filters need beyond a mod's scan entry. */
export interface FilterContext {
  curation: (techName: string) => { favorite: boolean; hidden: boolean; broken: boolean };
  /** Whether the row shows active (a copy of an active name can show parked, #421). */
  active: (mod: FilterMod) => boolean;
  /** Named in a warning-or-worse conflict (by tech name or title). */
  conflicted: (mod: FilterMod) => boolean;
  hasUpdate: (techName: string) => boolean;
  tags: (techName: string) => string[];
  /** The integrity verdict, once the mod was checked. */
  verdict?: (mod: FilterMod) => VerifyStatus | null | undefined;
}

export type FilterMod = Pick<
  ModEntry,
  | "techName"
  | "title"
  | "error"
  | "ignoredDigitPrefix"
  | "kind"
  | "isMap"
  | "scriptCount"
  | "uniqueType"
  | "hasSettings"
>;

/**
 * True when a mod needs the user's attention: it has a problem badge (Modified, Can't load,
 * Ignored name, Conflict, Flagged) or an update is available.
 * @param mod the scanned mod
 * @param facts its verdict, conflict, flag and update facts
 */
export function needsAttention(
  mod: Pick<FilterMod, "error" | "ignoredDigitPrefix" | "kind" | "isMap" | "scriptCount">,
  facts: { verdict?: VerifyStatus | null; conflicted?: boolean; flagged?: boolean; hasUpdate?: boolean },
): boolean {
  return rowBadges(mod, facts).some((b) => b.group === "problem" || b.id === "update");
}

/**
 * The mods the filters let through, in their original order.
 * @param list the mods (already category- and search-filtered, or not)
 * @param f the filters
 * @param ctx curation, active set, conflicts, updates, tags and verdicts
 */
export function filterMods<M extends FilterMod>(list: readonly M[], f: LibraryFilters, ctx: FilterContext): M[] {
  return list.filter((m) => {
    const c = ctx.curation(m.techName);
    if (!f.hidden && c.hidden) return false;
    if (f.status === "active" && !ctx.active(m)) return false;
    if (f.status === "parked" && ctx.active(m)) return false;
    if (f.favorites && !c.favorite) return false;
    if (f.flagged && !c.broken) return false;
    if (f.conflicted && !ctx.conflicted(m)) return false;
    if (f.hasSettings && !m.hasSettings) return false;
    if (f.needsUpdate && !ctx.hasUpdate(m.techName)) return false;
    if (f.maps && !m.isMap) return false;
    if (f.scripts && !(m.scriptCount > 0)) return false;
    if (f.unique && m.uniqueType == null) return false;
    if (f.tag && !ctx.tags(m.techName).includes(f.tag)) return false;
    if (
      f.attention &&
      !needsAttention(m, {
        verdict: ctx.verdict?.(m),
        conflicted: ctx.conflicted(m),
        flagged: c.broken,
        hasUpdate: ctx.hasUpdate(m.techName),
      })
    )
      return false;
    return true;
  });
}

/** One active filter as a removable chip. */
export interface FilterChip {
  /** "status", "tag", "category" or a FilterFlag. */
  id: string;
  label: string;
}

/**
 * The filters in effect, as chips, in toolbar order (segment, chips, popover, tag, category).
 * @param f the filters
 * @param category the FilterBar's category ("" for none) and subcategory
 */
export function filterChips(f: LibraryFilters, category = "", subcategory = ""): FilterChip[] {
  const out: FilterChip[] = [];
  if (f.status !== "all") out.push({ id: "status", label: f.status === "active" ? "Active" : "Parked" });
  for (const id of ["favorites", "attention", ...POPOVER_FLAGS.map((p) => p.id)] as FilterFlag[]) {
    if (f[id]) out.push({ id, label: FLAG_LABEL[id] });
  }
  if (f.tag) out.push({ id: "tag", label: `Tag: ${f.tag}` });
  if (category) out.push({ id: "category", label: `Category: ${category}${subcategory ? ` · ${subcategory}` : ""}` });
  return out;
}

/**
 * The filters with one chip removed ("category" is App's, so it leaves them as they are).
 * @param f the filters
 * @param id the chip's id
 */
export function withoutFilter(f: LibraryFilters, id: string): LibraryFilters {
  if (id === "status") return { ...f, status: "all" };
  if (id === "tag") return { ...f, tag: null };
  if (id in f && id !== "category") return { ...f, [id]: false };
  return f;
}

/** How many of the popover's filters are on (the Filters button's count). */
export function popoverCount(f: LibraryFilters): number {
  return POPOVER_FLAGS.filter((p) => f[p.id]).length + (f.tag ? 1 : 0);
}

/** True when nothing narrows the list (category and search aside). */
export function noFilters(f: LibraryFilters): boolean {
  return filterChips(f).length === 0;
}

/**
 * The tech names of every mod in a conflict that isn't just a note: what the Conflict badge, the
 * In conflict filter and its count all read. (Conflict.mods holds display titles, which two mods
 * can share; techNames is in step with it.)
 * @param conflicts the conflict check's findings
 */
export function conflictedTechNames<C extends { severity: string; techNames?: readonly string[] }>(
  conflicts: readonly C[],
): Set<string> {
  const s = new Set<string>();
  for (const c of conflicts) {
    if (c.severity === "info") continue;
    for (const t of c.techNames ?? []) s.add(t);
  }
  return s;
}

export type NeedsUpdateOutcome = { filter: true } | { filter: false; say: string; tone: "success" | "warn" };

/**
 * What switching "Needs update" on does once the on-demand check is over: filter to the mods with
 * an update, or say why not. Only a check that reached the catalog can say everything is current.
 * @param o `available`: mods with an update; `checking`: a check is still running; `checked`: a
 *   check reached the catalog; `catalogError`: the last check's catalog failure, worded;
 *   `blocked`: why a check couldn't start ("" when it could)
 */
export function needsUpdateOutcome(o: {
  available: number;
  checking: boolean;
  checked: boolean;
  catalogError: string | null;
  blocked: string;
}): NeedsUpdateOutcome {
  if (o.available > 0) return { filter: true };
  if (o.checking) return { filter: false, say: "Wait for the check to finish.", tone: "warn" };
  if (o.catalogError) return { filter: false, say: o.catalogError, tone: "warn" };
  if (!o.checked) return { filter: false, say: o.blocked || "Wait for the check to finish.", tone: "warn" };
  return { filter: false, say: "Every mod Silo can check is up to date.", tone: "success" };
}
