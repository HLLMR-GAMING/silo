<script lang="ts">
  // The mod drawer, top to bottom: header (title, verdict badges, Close), the primary row
  // (Active switch, Settings, ⋯ menu), banners that need attention, About, Details, Integrity,
  // Source and updates, My notes, Category. Docked in the app shell's drawer region, so the list
  // beside it stays usable.
  // Keyboard: opening (or switching to another mod) moves focus to the title; Esc closes it while
  // focus is in the drawer (or nowhere), and App hands focus back to the mod's row.
  import { tick, untrack } from "svelte";
  import type { ModEntry, CurationRow, Conflict, CatalogModDetail, CatalogUpdate, LogReport, SourceRow } from "../types";
  import type { ForeignFile } from "../api";
  import {
    revealInFolder,
    catalogDetailByTech,
    catalogCheckUpdates,
    onCatalogLinked,
    scanLog,
    setCuration,
  } from "../api";
  import { reportError } from "../state/notify.svelte";
  import Switch from "../ui/Switch.svelte";
  import { resizable } from "../resize";
  import { getVerdict, provKey } from "../provenanceCache.svelte";
  import { go } from "../state/nav.svelte";
  import { layerCount } from "../ui/overlayStack";
  import { toast } from "../ui/toasts.svelte";
  import { cachedModIcon, loadModIcon } from "./library/modIcon";
  import ModStatus from "./ModStatus.svelte";
  import ModCuration from "./ModCuration.svelte";
  import ModRepoLink from "./ModRepoLink.svelte";
  import ModSourceLink from "./ModSourceLink.svelte";
  import ProvenanceCheck from "./ProvenanceCheck.svelte";
  import DrawerHeader from "./drawer/DrawerHeader.svelte";
  import DrawerActions from "./drawer/DrawerActions.svelte";
  import DrawerBanners from "./drawer/DrawerBanners.svelte";
  import DrawerAbout from "./drawer/DrawerAbout.svelte";
  import DrawerDetails from "./drawer/DrawerDetails.svelte";
  import DrawerCategory from "./drawer/DrawerCategory.svelte";
  import { conflictNames, conflictPartners, drawerBadges, isProblem, logHealthOf } from "./drawer/drawerBadges";
  import type { DrawerMenuId } from "./drawer/drawerMenu";
  import { versionNewer } from "./drawer/drawerText";

  type Flag = "favorite" | "hidden" | "broken";
  interface Props {
    mod: ModEntry;
    curation: CurationRow;
    tags: string[];
    active: boolean;
    organized: boolean;
    hasSettings: boolean;
    /** A newer version is known (from the last update check). The drawer also asks the catalog. */
    hasUpdate?: boolean;
    libraryTechNames: Set<string>;
    conflicts: Conflict[];
    categories: string[];
    isOverridden: boolean;
    onSetCategory: (category: string, subcategory: string | null) => void;
    onResetCategory: () => void;
    onClose: () => void;
    onToggle: (flag: Flag) => void;
    onToggleActive: () => void;
    onOpenSettings: () => void;
    onFindInBrowse: () => void;
    onCurationChange: (row: CurationRow) => void;
    /** Shows and saves a row, putting it back when the save fails (#358). Without it, the row is
     *  handed to onCurationChange and saved here. */
    saveCuration?: (row: CurationRow) => Promise<void>;
    onTagsChange: (tags: string[]) => void;
    onFilterTag: (tag: string) => void;
    repo: { owner: string; repo: string } | null;
    onRepoChange: (r: { owner: string; repo: string } | null) => void;
    /** The page this mod came from, if the user saved one. */
    source: SourceRow | null;
    onSourceChange: (row: SourceRow | null) => void;
    onInstalled: () => void;
    /** A build sitting in the mods folder that isn't Silo's managed copy (usually a manual update
     *  the user dropped in). Null when the projected file matches. */
    foreign: ForeignFile | null;
    onAdoptForeign: () => void;
    onRestoreForeign: () => void;
    /** Remove from library (#429): App confirms, then moves the file to the Recycle Bin. */
    onRemove: () => void;
  }
  let {
    mod,
    curation,
    tags,
    active,
    organized,
    hasSettings,
    hasUpdate = false,
    libraryTechNames,
    conflicts,
    categories,
    isOverridden,
    onSetCategory,
    onResetCategory,
    onClose,
    onToggle,
    onToggleActive,
    onOpenSettings,
    onFindInBrowse,
    onCurationChange,
    saveCuration,
    onTagsChange,
    onFilterTag,
    repo,
    onRepoChange,
    source,
    onSourceChange,
    onInstalled,
    foreign,
    onAdoptForeign,
    onRemove,
    onRestoreForeign,
  }: Props = $props();

  const label = $derived(mod.title ?? mod.techName);
  const uid = $props.id();

  // Is the dropped-in build newer than Silo's managed copy?
  const foreignIsNewer = $derived(
    !!(foreign?.flatVersion && foreign?.managedVersion
      ? versionNewer(foreign.flatVersion, foreign.managedVersion)
      : foreign?.flatVersion && !foreign?.managedVersion),
  );

  // Everything below is looked up per file (path, size and mtime: a rescan that hands over a fresh
  // entry for the same file doesn't reload, a new build dropped in at the same name does), best
  // effort: a missing log or an offline catalog just leaves its part out. Focus follows the path.
  const path = $derived(mod.path);
  const fileId = $derived(provKey(mod));

  let icon = $state<string | null>(null);
  /** Silo's catalog record (About) and its version verdict (Source and updates, Update badge). */
  let catalog = $state<CatalogModDetail | null>(null);
  let catalogUpd = $state<CatalogUpdate | null>(null);
  let catalogChecked = $state(false);
  /** The catalog request itself failed: Silo can't say whether the mod is in it. */
  let catalogFailed = $state(false);
  /** The last game log's triage; null until read (or when there's no log). */
  let report = $state<LogReport | null>(null);
  /** Bumped when Silo links this file to its catalog entry while the drawer shows it (#402). */
  let linked = $state(0);
  $effect(() => {
    let stop: (() => void) | null = null;
    let gone = false;
    void onCatalogLinked((names) => {
      // A mod left out of update checks (#333) isn't asked about again, whatever was linked.
      const left = untrack(() => curation.skipUpdates);
      if (!left && names.includes(untrack(() => mod.techName))) linked++;
    })
      .then((un) => (gone ? un() : (stop = un)))
      .catch(() => {});
    return () => {
      gone = true;
      stop?.();
    };
  });
  $effect(() => {
    const id = fileId;
    const gen = linked;
    untrack(() => {
      const m = mod;
      icon = cachedModIcon(m);
      catalog = null;
      catalogUpd = null;
      catalogChecked = false;
      catalogFailed = false;
      report = null;
      const still = () => provKey(mod) === id && linked === gen;
      if (!icon) void loadModIcon(m).then((u) => {
        if (still()) icon = u;
      });
      catalogDetailByTech(m.techName, m.path)
        .then((c) => {
          if (still()) catalog = c;
        })
        .catch(() => {});
      catalogCheckUpdates([{ techName: m.techName, version: m.version ?? undefined, path: m.path }])
        .then((rows) => {
          if (!still()) return;
          catalogUpd = rows.find((x) => x.techName === m.techName) ?? null;
          catalogChecked = true;
        })
        .catch(() => {
          if (!still()) return;
          catalogFailed = true;
          catalogChecked = true;
        });
      scanLog()
        .then((r) => {
          if (still()) report = r;
        })
        .catch(() => {});
    });
  });

  const health = $derived(logHealthOf(mod, report));
  const myConflicts = $derived(conflicts.filter((c) => c.severity !== "info" && conflictNames(mod, c)));
  const partners = $derived(conflictPartners(mod, conflicts));
  const verify = $derived(getVerdict(mod) ?? null);
  const badges = $derived(
    drawerBadges(mod, {
      verify,
      // The catalog counts any version as newer than none: trust it only for a versioned mod.
      hasUpdate: hasUpdate || (!!mod.version && !!catalogUpd?.hasUpdate),
      problem: isProblem(mod, conflicts, report),
    }),
  );

  // Focus: to the title when the drawer opens or shows another mod.
  let header = $state<ReturnType<typeof DrawerHeader>>();
  $effect(() => {
    void path;
    void tick().then(() => header?.focusTitle());
  });

  // Esc closes the drawer when focus is in it or nowhere (a click on plain text). The list
  // closes it on its own Esc; a menu or dialog above takes Esc first.
  let root = $state<HTMLElement | null>(null);
  $effect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== "Escape" || e.defaultPrevented || layerCount() > 0) return;
      const a = document.activeElement;
      const here = !a || a === document.body || !!root?.contains(a);
      if (!here) return;
      e.preventDefault();
      onClose();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  });

  let category = $state<ReturnType<typeof DrawerCategory>>();
  function onMenu(id: DrawerMenuId) {
    switch (id) {
      case "favorite":
      case "broken":
      case "hidden":
        return onToggle(id);
      case "category":
        return category?.focusPicker();
      case "showFile":
        return void revealInFolder(mod.path).catch(() =>
          toast({ message: "Silo couldn't show the file. It may have moved; rescan and try again.", tone: "warn" }),
        );
      case "browse":
        return onFindInBrowse();
      case "remove":
        return onRemove();
    }
  }
</script>

<div class="drawer" use:resizable bind:this={root}>
  <DrawerHeader
    bind:this={header}
    title={label}
    author={mod.author}
    version={mod.version}
    {icon}
    {badges}
    {onClose}
  />

  <DrawerActions
    title={label}
    {active}
    {hasSettings}
    settingsLater={mod.hasSettings}
    {curation}
    {onToggleActive}
    {onOpenSettings}
    {onMenu}
  />

  <DrawerBanners
    {foreign}
    {foreignIsNewer}
    {onAdoptForeign}
    {onRestoreForeign}
    logErrors={health && health.errors > 0 ? health : null}
    crashed={!!report?.crashed}
    {partners}
    conflicts={myConflicts}
    selfNames={[mod.techName, mod.title ?? ""]}
    onSeeLastRun={() => go("diagnose", "lastRun")}
    onSeeConflicts={() => go("diagnose", "conflicts")}
  />

  <DrawerAbout {catalog} title={label} />

  <DrawerDetails {mod} {organized} {libraryTechNames} lastRun={report ? health : undefined} />

  <ProvenanceCheck {mod} />

  <section class="sec" aria-labelledby="{uid}-source">
    <h3 id="{uid}-source" class="h">Source and updates</h3>
    <!-- #333: a mod the user changed themselves can be left out of checks and Update all. -->
    <div class="skip-updates">
      <Switch
        checked={!curation.skipUpdates}
        label="Check for updates"
        onchange={(on) => {
          const next = { ...curation, skipUpdates: !on };
          const saved = saveCuration ? saveCuration(next) : (onCurationChange(next), setCuration(next));
          saved.catch((e) => reportError(e, "Saving your mod flags"));
        }}
      />
      {#if curation.skipUpdates}
        <p class="skip-note">Off: Silo leaves this mod out of update checks and Update all.</p>
      {/if}
    </div>
    <ModStatus {mod} catalog={catalogUpd} checked={catalogChecked} failed={catalogFailed} pageUrl={catalog?.pageUrl ?? null} />
    <ModSourceLink {mod} {source} {onSourceChange} {onRepoChange} />
    <ModRepoLink {mod} {repo} {onRepoChange} {onInstalled} />
  </section>

  <section class="sec" aria-labelledby="{uid}-notes">
    <h3 id="{uid}-notes" class="h">My notes</h3>
    <ModCuration techName={mod.techName} {curation} {tags} {onCurationChange} {saveCuration} {onTagsChange} {onFilterTag} />
  </section>

  <DrawerCategory bind:this={category} {mod} {categories} {isOverridden} {onSetCategory} {onResetCategory} />
</div>

<style>
  .drawer {
    box-sizing: border-box;
    min-height: 100%;
    width: 420px;
    max-width: calc(100vw - 40px);
    background: var(--surface-raised);
    padding: 18px;
  }
  .sec {
    padding: var(--sp-3) 0 var(--sp-2);
    border-top: 1px solid var(--border);
  }
  .h {
    margin: 0 0 var(--sp-1);
    font-size: var(--fs-xs);
    text-transform: uppercase;
    letter-spacing: 0.04em;
    color: var(--text-muted);
    font-weight: 700;
  }
  .skip-updates {
    display: flex;
    flex-direction: column;
    gap: var(--sp-1);
    margin-bottom: var(--sp-2);
  }
  .skip-note {
    margin: 0;
    color: var(--muted);
    font-size: var(--fs-xs);
  }
</style>
