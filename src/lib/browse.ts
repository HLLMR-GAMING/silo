// Pure helpers shared across the Browse-tab components (card grid + detail drawer).

import type { BrowseMod, CatalogModDetail, ModSourceOption } from "./types";
import type { BadgeSpec } from "./components/library/rowBadges";
import { catalogImage } from "./api";
import { displayVersion } from "./format";
import { describeError, formatDetail } from "./errorCopy";

// GIANTS' ModHub CDN refuses requests that don't come from its own site. Silo respects
// that: a thumbnail still pointing there (not yet cached by SiloAPI) shows the placeholder.
const MODHUB_CDN = /(?:giants-software\.com|farming-simulator\.com)/i;
// GitHub-hosted images go through the Rust proxy (the CSP keeps the webview off them).
const GITHUB_HOSTED = /(?:githubusercontent\.com|github\.com)/i;

// Image loading. SiloAPI caches catalog images server-side and hands back an `imageUrl` on
// its own host — those load DIRECTLY as a plain <img> (CSP allows silo-api). GitHub images
// go through the Rust proxy, which returns a data: URL. Dedupe in-session so paging back
// doesn't refetch.
const imageCache = new Map<string, Promise<string>>();
export function loadCatalogImage(url: string): Promise<string> {
  if (MODHUB_CDN.test(url)) return Promise.resolve("");
  // Cached SiloAPI URL (or any other allowed host): the browser can load and cache it itself.
  if (!GITHUB_HOSTED.test(url)) return Promise.resolve(url);
  let p = imageCache.get(url);
  if (!p) {
    p = catalogImage(url).catch(() => "");
    imageCache.set(url, p);
  }
  return p;
}

export const SOURCE_LABEL: Record<string, string> = {
  github: "GitHub",
  modhub: "ModHub",
};

/** A source's name, spelled out ("GitHub", "ModHub"); an unknown source shows as it came. */
export const label = (s: string): string => SOURCE_LABEL[s] ?? s;

/**
 * A card's source chip: the source spelled out, then that source's own version.
 *   sourceChip({ source: "github", version: "1.0.6.0" }) -> "GitHub 1.0.6.0"
 * @param s the source option
 * @returns the chip text; just the name when the source has no version
 */
export function sourceChip(s: Pick<ModSourceOption, "source" | "version">): string {
  const v = s.version?.trim();
  return v ? `${label(s.source)} ${displayVersion(v)}` : label(s.source);
}

/** Why a source can't be installed directly — shown on hover and in the drawer. */
export function gatedReason(source: string): string {
  if (source === "modhub")
    return "ModHub mods download from GIANTS' website, so Silo opens the mod's page there.";
  return "This source doesn't allow direct downloads, so Silo opens the mod's page.";
}

/** Compact download counts: 980, 12k, 1.3M. */
export function fmtCount(n: number): string {
  if (n < 1000) return `${n}`;
  if (n < 1_000_000) return `${(n / 1000).toFixed(n < 10_000 ? 1 : 0)}k`;
  return `${(n / 1_000_000).toFixed(1)}M`;
}

/** Pull owner/repo out of a github.com source URL, for the interactive card. */
export function parseRepo(url: string): { owner: string; repo: string } | null {
  const m = url.match(/github\.com\/([^/]+)\/([^/?#]+)/i);
  if (!m) return null;
  return { owner: m[1], repo: m[2].replace(/\.git$/i, "") };
}

/** Whether a "Read more" affordance is worth showing: an ingested full body, or a
 *  summary long enough that a dedicated modal helps. */
export function canExpand(d: CatalogModDetail): boolean {
  return !!d.descriptionFull || (d.description != null && d.description.length > 160);
}

// ── Cards: columns, install state, badges, states ──

/** Narrowest a Browse card gets before a column drops. */
export const BROWSE_CARD_MIN_W = 232;
/** Gap between Browse cards (both axes), px. */
export const BROWSE_GAP = 16;
/** Height of one Browse card at 100% UI size, px. */
export const BROWSE_CARD_HEIGHT = 356;

/**
 * A Browse card's height for the current UI size (the card's text is rem-based, so it grows with
 * Settings → UI size; VirtualGrid needs it in px).
 * @param remPx the root font size, px (16 at 100%)
 * @returns whole px, never below the 100% height
 */
export function browseCardHeight(remPx: number): number {
  const scale = Number.isFinite(remPx) && remPx > 16 ? remPx / 16 : 1;
  return Math.ceil(BROWSE_CARD_HEIGHT * scale);
}

/**
 * Columns of Browse cards for a measured width.
 * @param width the grid's inner width, px; 0 (not measured yet) gives 4
 * @returns at least 1
 */
export function browseColumns(width: number): number {
  if (!Number.isFinite(width) || width <= 0) return 4;
  return Math.max(1, Math.floor((width + BROWSE_GAP) / (BROWSE_CARD_MIN_W + BROWSE_GAP)));
}

/** An installed zip's mod name: its file name without `.zip` (the name FS25 and the library use). */
export function zipStem(filename: string): string {
  return filename.replace(/\.zip$/i, "");
}

/**
 * The library names as Browse should see them: `installed`, plus the catalog techName of each
 * mod installed this session under a different file name (`installedAs`: techName → the
 * library name it went in as), while that file is still in the library.
 */
export function withInstalledAs(
  installed: ReadonlySet<string>,
  installedAs: Readonly<Record<string, string>>,
): ReadonlySet<string> {
  const extra = Object.entries(installedAs).filter(([, name]) => installed.has(name));
  if (extra.length === 0) return installed;
  return new Set([...installed, ...extra.map(([tech]) => tech)]);
}

/** Bytes downloaded so far for one install, and the total when the server sent one. */
export interface InstallProgressEntry {
  done: number;
  total: number | null;
}

/** What a card's primary button does. */
export type InstallState =
  /** This mod is downloading; `pct` is null until the size is known. */
  | { kind: "installing"; pct: number | null }
  /** In the library and a newer version is known: [See update]. */
  | { kind: "update" }
  /** In the library: [Show in library]. */
  | { kind: "installed" }
  /** Silo can install it from `source`: [Install]. */
  | { kind: "install"; source: ModSourceOption }
  /** Index-only (ModHub): [Open on ModHub] opens `source`'s page. */
  | { kind: "open"; source: ModSourceOption }
  /** The catalog lists no source. */
  | { kind: "none" };

/** What a card knows beyond the catalog row. */
export interface InstallContext {
  /** Mod IDs (tech names) in the local library. */
  installed: ReadonlySet<string>;
  /** Mod IDs with a newer version known (the last update check). */
  updates?: ReadonlySet<string>;
  /** The catalog id being installed right now, or null. */
  installingId: string | null;
  /** Download progress for that install. */
  progress?: InstallProgressEntry;
}

/**
 * Download progress as a whole percentage.
 * @returns 0–100, or null when the total isn't known
 */
export function progressPct(p: InstallProgressEntry | undefined): number | null {
  if (!p || !p.total || p.total <= 0) return null;
  return Math.min(100, Math.max(0, Math.round((p.done / p.total) * 100)));
}

/**
 * Which primary action a Browse card shows. Installing wins, then a known update, then "in your
 * library", then the first source Silo can install from, then the first index-only source.
 * @param m the catalog row
 * @param ctx what Silo knows locally
 */
export function installState(
  m: Pick<BrowseMod, "id" | "techName" | "sources">,
  ctx: InstallContext,
): InstallState {
  if (ctx.installingId === m.id) return { kind: "installing", pct: progressPct(ctx.progress) };
  const here = m.techName != null && ctx.installed.has(m.techName);
  if (here && m.techName != null && ctx.updates?.has(m.techName)) return { kind: "update" };
  if (here) return { kind: "installed" };
  const direct = m.sources.find((s) => s.installable);
  if (direct) return { kind: "install", source: direct };
  const page = m.sources[0];
  if (page) return { kind: "open", source: page };
  return { kind: "none" };
}

/**
 * The primary button's words for a state.
 *   installLabel({ kind: "installing", pct: 42 }) -> "Installing… 42%"
 */
export function installLabel(st: InstallState): string {
  switch (st.kind) {
    case "installing":
      return st.pct === null ? "Installing…" : `Installing… ${st.pct}%`;
    case "update":
      return "See update";
    case "installed":
      return "Show in library";
    case "install":
      return "Install";
    case "open":
      return `Open on ${label(st.source.source)}`;
    case "none":
      return "See details";
  }
}

/**
 * The author claim the catalog carries.
 * @returns the claimant's login, or null when there's no claim (only a claim with a login counts)
 */
export function verifiedAuthorLogin(m: Pick<BrowseMod, "verifiedAuthor">): string | null {
  const login = m.verifiedAuthor?.login?.trim();
  return login ? login : null;
}

/**
 * A Browse card's badges, most important first (cards cap them at three, then "+n").
 * Verified build and Verified author never share a tone or icon: one is about the file, the
 * other about who published it. Verified author shows only when the catalog carries a claim.
 * @param m the catalog row
 * @param ctx what Silo knows locally
 */
export function browseBadges(
  m: Pick<BrowseMod, "techName" | "verified" | "verifiedAuthor">,
  ctx: Pick<InstallContext, "installed" | "updates">,
): BadgeSpec[] {
  const out: BadgeSpec[] = [];
  const here = m.techName != null && ctx.installed.has(m.techName);
  if (here)
    out.push({
      id: "in-library",
      label: "In your library",
      tone: "neutral",
      icon: "check",
      group: "status",
      title: "You already have this mod.",
    });
  if (here && m.techName != null && ctx.updates?.has(m.techName))
    out.push({
      id: "update",
      label: "Update",
      tone: "accent",
      icon: "update",
      group: "status",
      title: "A newer version than yours is out.",
    });
  if (m.verified === true)
    out.push({
      id: "verified-build",
      label: "Verified build",
      tone: "success",
      icon: "verifiedBuild",
      group: "status",
      title: "Silo's catalog recorded the published build, so Silo can check that your copy is exactly the same.",
    });
  const author = verifiedAuthorLogin(m);
  if (author)
    out.push({
      id: "verified-author",
      label: "Verified author",
      tone: "info",
      icon: "verifiedAuthor",
      group: "status",
      title: `${author} claimed this mod through Silo's author program.`,
    });
  return out;
}

/** The catalog error state's words. */
export interface CatalogErrorCopy {
  title: string;
  body: string;
  /** The text behind "Copy details". */
  details: string;
}

/** Codes that mean "the catalog didn't answer", rather than a problem Silo has its own words for. */
const UNREACHABLE: ReadonlySet<string> = new Set(["NET_OFFLINE", "NET_TIMEOUT", "UNKNOWN"]);

/**
 * What Browse says when the catalog can't be loaded. Network trouble (and anything Silo has no
 * words for) reads as "can't reach its catalog"; a known problem (GitHub's rate limit, say) keeps
 * its own sentence. Either way the library is fine.
 * @param e the error from the catalog request
 */
export function catalogErrorCopy(e: unknown): CatalogErrorCopy {
  const d = describeError(e, "Loading the catalog");
  const title = UNREACHABLE.has(d.code) ? "Silo can't reach its catalog right now." : d.title;
  return { title, body: "Your library still works.", details: formatDetail(d.detail) };
}

/**
 * The title of the nothing-found state.
 *   emptyResultsTitle("lizard", false) -> "No mods match “lizard”."
 * @param query the search text
 * @param filtered any category / facet / year filter is on
 */
export function emptyResultsTitle(query: string, filtered: boolean): string {
  const q = query.trim();
  if (q) return `No mods match “${q}”.`;
  if (filtered) return "No mods match these filters.";
  return "The catalog has no mods to show right now.";
}
