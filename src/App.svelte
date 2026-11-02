<script lang="ts">
  import { onDestroy, onMount, tick, untrack } from "svelte";
  import { get } from "svelte/store";
  import {
    DialogHost,
    Menu,
    ToastRegion,
    ask,
    askWithOption,
    EmptyState,
    dismissToast,
    toast,
    toasts,
    type ToastHandle,
  } from "./lib/ui";
  import {
    modsRootStatus,
    setModsRoot,
    clearModsRoot,
    pointGameAtModsRoot,
    setGameProfile,
    createModsFolder,
    folderLooksEmpty,
    pickFolderFor,
    type ModsRootStatus,
    scanMods,
    onScanProgress,
    getCuration,
    setCuration,
    getOverrides,
    setOverride,
    planOrganize,
    applyOrganize,
    setActive,
    flatten,
    clearScanCache,
    getSavegames,
    gameFiles,
    backupSavegame,
    detectConflicts,
    detectGame,
    modsWithSettings,
    getTags,
    setTags,
    getModRepos,
    getModSources,
    markSourceChecked,
    openExternal,
    revealInFolder,
    checkModUpdate,
    catalogCheckUpdates,
    onCatalogLinked,
    downloadUpdate,
    appBusy,
    forceQuit,
    setSelfUpdating,
    onCloseBlocked,
    saveTextFile,
    diagnosticsBundle,
    copyText,
    userDirPath,
    bisectSnapshotGet,
    bisectSnapshotClear,
    logStatus,
    markLogSeen,
    scanLog,
    appVersion,
    takeStartupNotices,
    detectForeignFiles,
    adoptForeignFile,
    restoreForeignFile,
    relinkProjections,
    onRelinkProgress,
    hardlinkSupport,
    type ForeignFile,
    confirmDialog,
    archiveMovePreflight,
    archiveMoveStart,
    archiveUseDefault,
    archiveRepoint,
    archiveAdoptPreview,
    archiveAdoptApply,
    type ArchivePreflight,
    openKnownFolder,
  } from "./lib/api";
  import { ArchiveMove } from "./lib/archiveMove.svelte";
  import { capList, isOnlyLostCancel, modsCount, problemLines, unDot } from "./lib/archiveMoveLogic";
  import { sameMods } from "./lib/loadoutSafety";
  import { launchAsking } from "./lib/launch";
  import { onOpenUrl, getCurrent } from "@tauri-apps/plugin-deep-link";
  import { getCurrentWindow } from "@tauri-apps/api/window";
  import { checkUpdate, installUpdate, type AvailableUpdate } from "./lib/updater";
  import {
    GAME_GRAPHICS_FIELDS,
    GAME_GRAPHICS_PRESETS,
    SAVEGAME_FIELDS,
    SAVEGAME_PRESETS,
    type CfgField,
    type CfgPreset,
  } from "./lib/configSchemas";
  import type {
    ModEntry,
    ScanResult,
    CurationRow,
    ModInput,
    PlannedMove,
    Savegame,
    Conflict,
    GameInfo,
    UpdateRow,
    SourceRow,
  } from "./lib/types";
  import { checkYourselfList, type CheckYourselfRow } from "./lib/sources";
  import {
    availableRows,
    droppedKey,
    groupUpdates,
    newDroppedBuilds,
    updatesBadge,
  } from "./lib/updates/sections";
  import {
    checkBlockedReason,
    installBlockedReason,
    recheckRow,
    fileStamps,
    heldRefresh,
    holdRefresh,
    refreshTargets,
    releaseInstalled,
    sameStamps,
    linkRefresh,
    mergeRows,
    runUpdateCheck,
    type HeldRefresh,
    updateAll,
    updateAllSummary,
    type ExplainedError,
  } from "./lib/updates/run";
  import { autoCheckEnabled } from "./lib/updates/autoCheck.svelte";
  import type { RowInstallState } from "./lib/components/panels/UpdatesPanel.svelte";
  import { copyOrSave } from "./lib/clipboard";
  import { parseUiScale, type UiScale } from "./lib/uiScale";
  import {
    blocked,
    blockedForeground,
    current as currentOp,
    start,
    statusLineOp,
    tryStart,
    type OpToken,
  } from "./lib/state/ops.svelte";
  import { createScanSettler, focusRescanAllowed, rescanGate, waitWhile } from "./lib/state/rescanGate";
  import { describeError, formatDetail, MOVE_UNFINISHED_COPY } from "./lib/errorCopy";
  import {
    info,
    inlineError,
    reportError,
    setNotice,
    success,
    syncNotice,
  } from "./lib/state/notify.svelte";
  import { archiveNotice, ARCHIVE_MOVE_KEY, ARCHIVE_UNAVAILABLE_KEY } from "./lib/archiveMoveNotice";

  const CATEGORIES = [
    "Maps",
    "Tractors",
    "Harvesters",
    "Implements",
    "Cars & Trucks",
    "Vehicles",
    "Placeables",
    "Objects",
    "Decorations",
    "Textures",
    "Sounds",
    "Realism",
    "Cheats",
    "Scripts & Tools",
    "Other",
  ];
  import VirtualList from "./lib/components/library/VirtualList.svelte";
  import VirtualGrid from "./lib/components/library/VirtualGrid.svelte";
  import ModListRow, { ROW_HEIGHTS, type Density } from "./lib/components/library/ModListRow.svelte";
  import ModCard, { CARD_HEIGHT } from "./lib/components/library/ModCard.svelte";
  import { rowBadges } from "./lib/components/library/rowBadges";
  import { searchMods, sortMods, type SortKey } from "./lib/components/library/libraryList";
  import { markStart, measureFrom, timed } from "./lib/perf";
  import type { MenuTarget, SelectIntent } from "./lib/components/library/roving";
  import {
    loadLibraryDensity,
    loadLibraryState,
    loadLibraryView,
    saveLibraryDensity,
    saveLibraryState,
    saveLibraryView,
    type LibraryViewMode,
  } from "./lib/components/library/viewPref";
  import {
    conflictedTechNames,
    filterChips,
    filterMods,
    NO_FILTERS,
    needsAttention,
    needsUpdateOutcome,
    withoutFilter,
    type FilterContext,
    type LibraryFilters,
  } from "./lib/components/library/filters";
  import { saveEach } from "./lib/components/library/bulk";
  import { onOtherDrive, runBulkSwitch } from "./lib/components/library/bulkSwitch";
  import { focusInside, returnFocusWhenHidden } from "./lib/components/library/focusReturn.svelte";
  import { fixedOnText, isUnparkable, parkPlan, readyAfterFiling, stillOn } from "./lib/parkable";
  import { createParkLooseExplainer } from "./lib/components/library/parkLoose";
  import BulkBar from "./lib/components/library/BulkBar.svelte";
  import ActiveFilters from "./lib/components/library/ActiveFilters.svelte";
  import { Selection } from "./lib/state/selection.svelte";
  import { createSwitchUndo, switchMessage } from "./lib/state/switchUndo";
  import { setSwitchOutcome, switchOutcome } from "./lib/state/switchOutcome";
  import { SaveLedger, withEntry } from "./lib/state/optimistic";
  import { serialDrained, serialPendingUnder } from "./lib/serial";
  import { getVerdict } from "./lib/provenanceCache.svelte";
  import FilterBar from "./lib/components/FilterBar.svelte";
  import ModSettings from "./lib/components/ModSettings.svelte";
  import ModDetail from "./lib/components/ModDetail.svelte";
  import { returnFocusOnDrawerClose } from "./lib/components/drawer/drawerFocus.svelte";
  import { currentEntry } from "./lib/components/drawer/drawerMod";
  import ConfigEditor from "./lib/components/ConfigEditor.svelte";
  import ModBrowser from "./lib/components/ModBrowser.svelte";
  import LogTriage from "./lib/components/LogTriage.svelte";
  import BindingsView from "./lib/components/BindingsView.svelte";
  import BridgeTool from "./lib/components/BridgeTool.svelte";
  import ConflictsPanel from "./lib/components/panels/ConflictsPanel.svelte";
  import ProblemsPanel from "./lib/components/panels/ProblemsPanel.svelte";
  import StatsPanel from "./lib/components/panels/StatsPanel.svelte";
  import UpdatesPanel from "./lib/components/panels/UpdatesPanel.svelte";
  import MySetsTab from "./lib/components/modsets/MySetsTab.svelte";
  import SharedLinks from "./lib/components/modsets/SharedLinks.svelte";
  import ShareDialog from "./lib/components/modsets/ShareDialog.svelte";
  import SetMembershipDialog from "./lib/components/modsets/SetMembershipDialog.svelte";
  import OpenSharedTab from "./lib/components/modsets/OpenSharedTab.svelte";
  import SavegamesTab from "./lib/components/modsets/SavegamesTab.svelte";
  import { createModSets } from "./lib/modsets/modSetsStore.svelte";
  import { activeSetLine, saveModStats, stripSetName } from "./lib/modsets/modSets";
  import type { MembershipEdit } from "./lib/modsets/membership";
  import { listenForSharedSets } from "./lib/modsets/deepLink";
  import GeneralSettings from "./lib/components/settings/GeneralSettings.svelte";
  import ModsFolderSettings from "./lib/components/settings/ModsFolderSettings.svelte";
  import LibrarySettings from "./lib/components/settings/LibrarySettings.svelte";
  import AccountSettings from "./lib/components/settings/AccountSettings.svelte";
  import DiagnosticsSettings from "./lib/components/settings/DiagnosticsSettings.svelte";
  import MaintenanceSettings from "./lib/components/settings/MaintenanceSettings.svelte";
  import AboutSettings from "./lib/components/settings/AboutSettings.svelte";
  import LibraryToolbar from "./lib/components/panels/LibraryToolbar.svelte";
  import OrganizePreview from "./lib/components/OrganizePreview.svelte";
  import Onboarding from "./lib/firstRun/Onboarding.svelte";
  import WhatsNew from "./lib/firstRun/WhatsNew.svelte";
  import {
    fileInRuns,
    relinkFiled,
    filingFailures,
    firstRunScreen,
    firstRunState,
    nMods,
    setFirstRunFlag,
    type ArchiveChoice,
    type FilingFailure,
    type FinishHow,
    type FilingRun,
    type FirstRunFlag,
    type OrganizeOutcome,
  } from "./lib/firstRun/firstRun";
  import NoticeArea from "./lib/components/NoticeArea.svelte";
  import ViewBoundary from "./lib/components/ViewBoundary.svelte";
  import KeepMounted from "./lib/components/KeepMounted.svelte";
  import AppShell from "./lib/components/shell/AppShell.svelte";
  import AppHeader from "./lib/components/shell/AppHeader.svelte";
  import ShortcutsDialog from "./lib/components/shell/ShortcutsDialog.svelte";
  import { cycleRegion, regionOf } from "./lib/components/shell/regions";
  import { BISECT_BADGE } from "./lib/components/shell/navItems";
  import { installShortcuts, shortcutsBlocked, type ShortcutAction } from "./lib/state/shortcuts";
  import { layerCount, modalOpen } from "./lib/ui/overlayStack";
  import StatusChip from "./lib/components/shell/StatusChip.svelte";
  import BisectStatus from "./lib/components/shell/BisectStatus.svelte";
  import { createCrashWatch, type LogRun } from "./lib/diagnose/crashWatch";
  import { crashNoticeModel, likelyCulprit, type CrashCulprit } from "./lib/diagnose/crashNotice";
  import { severityLabel } from "./lib/diagnose/conflictCopy";
  import { latestOnly } from "./lib/diagnose/latest";
  import { libraryReadState } from "./lib/components/shell/status";
  import PlayButton, { type PlayMenuItem } from "./lib/components/shell/PlayButton.svelte";
  import type { StatusStripItem } from "./lib/components/shell/StatusStrip.svelte";
  import ViewOutlet from "./lib/views/ViewOutlet.svelte";
  import LibraryView from "./lib/views/LibraryView.svelte";
  import BrowseView from "./lib/views/BrowseView.svelte";
  import ModSetsView from "./lib/views/ModSetsView.svelte";
  import DiagnoseView from "./lib/views/DiagnoseView.svelte";
  import UpdatesView from "./lib/views/UpdatesView.svelte";
  import ToolsView from "./lib/views/ToolsView.svelte";
  import SettingsView from "./lib/views/SettingsView.svelte";
  import HelpView from "./lib/views/HelpView.svelte";
  import Button from "./lib/ui/Button.svelte";
  import {
    libraryStatusCounts,
    stripItemSpecs,
    stripLeft,
  } from "./lib/views/libraryStatus";
  import { back, go, nav, searchPlaceholderFor, searchTargetFor } from "./lib/state/nav.svelte";
  import type { MenuItem } from "./lib/ui/overlayTypes";
  import { displayPath, displayVersion, fmtSize } from "./lib/format";
  import {
    MODS_FOLDER_KEY,
    OVERRIDE_FOLLOWED_KEY,
    OVERRIDE_KEY,
    modsFolderNotices,
    rootsFor,
    showOverrideNotice,
    switchBlockReason as switchGate,
  } from "./lib/modsStatus";

  let roots = $state<string[]>([]);
  let modsStatus = $state<ModsRootStatus | null>(null);
  let overrideNoticeOpen = $state(false);
  let mods = $state<ModEntry[]>([]);
  // True once a scan of the current mods folder finished (even with no mods), so "not in your
  // library" counts are only shown against a library Silo has actually read.
  let libraryScanned = $state(false);
  let foreignFiles = $state<ForeignFile[]>([]);
  /** Bumped by every foreign-file check and by a folder switch: an older check's answer is dropped. */
  let foreignGen = 0;
  let scanning = $state(false);
  // The last scan failed (its notice is up): the status chip says so instead of "All good".
  let scanFailed = $state(false);
  // Epoch-ms of the last scan attempt — throttles the auto-rescan-on-focus so alt-tabbing
  // doesn't re-scan on every focus.
  let lastScanAt = 0;
  /** Scans started so far: filing that waited checks no scan started meanwhile. */
  let scansStarted = 0;
  /** The running scan's op (progress events report into it). */
  let scanOp: OpToken | null = null;
  let progress = $state({ done: 0, total: 0 });
  let result = $state<ScanResult | null>(null);
  let query = $state("");
  // applyActive() and bisectApply() still report through these two names, so their bodies stay
  // exactly as they were. Nothing else writes them: this adapter hands each message on (an
  // error toast or an info toast) and empties the channel again.
  let errorMsg = $state<string | null>(null);
  let infoMsg = $state<string | null>(null);
  $effect(() => {
    const err = errorMsg;
    const note = infoMsg;
    if (err === null && note === null) return;
    untrack(() => {
      errorMsg = null;
      infoMsg = null;
      if (err !== null) reportError(err, "Applying your mods");
      if (note !== null) info(note, { duration: 10_000 });
    });
  });
  // Library category filter, driven by the shared FilterBar's parent/child dropdowns
  // ("" = all). Replaces the old category rail + `selected`.
  let libCat = $state("");
  let libSub = $state("");
  let curationMap = $state<Record<string, CurationRow>>({});
  let overrideMap = $state<
    Record<string, { category: string; subcategory: string | null }>
  >({});
  // What the database holds for each map, so a failed save goes back to it (#358).
  const curationSaved = new SaveLedger<CurationRow>();
  const overrideSaved = new SaveLedger<{ category: string; subcategory: string | null }>();
  // The Library's filters (lib/components/library/filters): the All / Active / Parked segment
  // (remembered), the Favorites / Needs attention chips and the Filters popover. Category and
  // search are libCat/libSub and `query`.
  let libFilters = $state<LibraryFilters>({ ...NO_FILTERS, status: loadLibraryState().status });
  $effect(() => saveLibraryState({ status: libFilters.status }));
  // "Change category…": a menu at the row's ⋯ button (or where the row menu opened), so Esc and a
  // choice hand focus back to that row.
  let editing = $state<{ techName: string; x: number; y: number; anchor: HTMLElement | null } | null>(null);
  let editingOpen = $state(false);
  let activeSet = $state<Set<string>>(new Set());
  // Library writes hold the "library-write" op class, one at a time: a second is refused with the
  // reason. `writeLock` counts every holder (the archive move too) and turns off the controls
  // that write; `fgLock` leaves out background ops (the archive move), so navigation, Settings,
  // read-only panels and scans stay usable during a move. Writes started from those still refuse
  // in beginWrite.
  const writeLock = $derived(blocked("library-write"));
  const fgLock = $derived(blockedForeground("library-write"));
  /** The op the status line under the top bar names (quiet ops show their progress elsewhere). */
  const statusOp = $derived(statusLineOp());
  /** Say why something the user asked for can't happen right now (nothing failed). */
  function refuse(why: string) {
    toast({ message: why, tone: "warn" });
  }
  /**
   * Start a library write, or say why it can't start now.
   * @param label what Silo is doing, fits "Silo is …" ("Filing 3 mods into the library")
   * @param progress show a progress bar for it
   * @param opts `silent`: refuse without a toast (background work that retries on its own);
   *   `duringMove`: it doesn't touch the library archive (a savegame backup), so a running or
   *   paused archive move doesn't stop it; only a foreground write does
   * @return the op's token (end it before rescanning), or null when refused
   */
  function beginWrite(
    label: string,
    progress = false,
    opts: { silent?: boolean; duringMove?: boolean } = {},
  ): OpToken | null {
    const say = (why: string) => {
      if (!opts.silent) refuse(why);
    };
    if (opts.duringMove) {
      const fg = blockedForeground("library-write");
      if (fg.blocked) {
        say(`${fg.reason} Try again when it's done.`);
        return null;
      }
      return start(label, { exclusive: "library-write", progress });
    }
    // A paused (or otherwise unfinished) move: nothing changes the library until it's resumed
    // and finished, or cancelled. A running one holds the class itself (below).
    if (archiveMove.unfinished && !archiveMove.running) {
      say(MOVE_UNFINISHED_COPY);
      return null;
    }
    const op = tryStart(label, { exclusive: "library-write", progress });
    if (!op) say(`${blocked("library-write").reason} Try again when it's done.`);
    return op;
  }
  /** The scan's own notice (it failed, or recovery left problems). A scan touches no other key. */
  const SCAN_KEY = "scan";
  /** Why a switch or a filing waits: a running scan would land on top of it with the old state. */
  const SCAN_WAIT = "Wait for the scan to finish.";
  /** How long a switch waits for a running scan (the focus rescan after alt-tabbing back is
   *  quicker than this; a long scan still refuses rather than switch much later). */
  const SCAN_SETTLE_MS = 3000;
  const scanSettle = createScanSettler();
  $effect(() => {
    if (!scanning) untrack(() => scanSettle.ended());
  });
  /** Wait briefly for a running scan to end; true once none runs (a switch may go ahead). */
  function settleScan(): Promise<boolean> {
    return scanSettle.wait(scanning, SCAN_SETTLE_MS);
  }
  /** settleScan, refusing with SCAN_WAIT when the scan still runs. True to go ahead. */
  async function settleScanOrRefuse(): Promise<boolean> {
    if (await settleScan()) return true;
    refuse(SCAN_WAIT);
    return false;
  }
  /** What Silo did while starting (e.g. restored the database), until dismissed. */
  const STARTUP_KEY = "startup";
  // Mods folder is on a cloud-sync filesystem that can't do hardlinks → Silo uses copies.
  let cloudSynced = $state(false);
  let cloudNoticeOpen = $state(false);
  function dismissCloudNotice() {
    cloudNoticeOpen = false;
    try {
      localStorage.setItem("silo.cloudNoticeDismissed", "1");
    } catch {
      /* private-mode / storage-off — the notice just returns next launch */
    }
  }
  // Auto-file newly-appeared mods into the archive on load (kept active). OPT-IN:
  // defaults OFF so a first run never moves the user's files without them choosing to.
  // Enable it via Settings, or use the explicit "Organize" button. Persisted.
  let autoFileNew = $state(
    typeof localStorage !== "undefined"
      ? localStorage.getItem("silo.autoFile") === "true"
      : false,
  );
  function setAutoFile(v: boolean) {
    autoFileNew = v;
    try {
      localStorage.setItem("silo.autoFile", String(v));
    } catch {}
  }

  // Appearance: system (follow OS) / light / dark. Persisted; applied to <html>.
  let theme = $state<"system" | "light" | "dark">(
    (typeof localStorage !== "undefined"
      ? (localStorage.getItem("silo.theme") as "system" | "light" | "dark" | null)
      : null) ?? "system",
  );
  function setTheme(t: "system" | "light" | "dark") {
    theme = t;
    try {
      localStorage.setItem("silo.theme", t);
    } catch {}
  }
  $effect(() => {
    const el = document.documentElement;
    if (theme === "system") el.removeAttribute("data-theme");
    else el.setAttribute("data-theme", theme);
  });

  // UI size: scales the root font size (rem-based tokens follow). Persisted; applied to <html>.
  let uiScale = $state<UiScale>(
    parseUiScale(typeof localStorage !== "undefined" ? localStorage.getItem("silo.uiScale") : null),
  );
  function setUiScale(s: UiScale) {
    uiScale = s;
    try {
      localStorage.setItem("silo.uiScale", String(s));
    } catch {}
  }
  $effect(() => {
    document.documentElement.style.setProperty("--ui-scale", String(uiScale));
  });

  // Silo only organizes .zip mods (the commercial distribution format). Unpacked
  // dir mods are left in place — that's how dev/work-in-progress mods live until
  // they're officially packaged.
  const isFileable = (m: ModEntry) => !m.organized && m.kind === "zip";
  const organizedCount = $derived(mods.filter((m) => m.organized).length);
  const unorganizedCount = $derived(mods.filter(isFileable).length);

  let savegames = $state<Savegame[]>([]);
  let conflicts = $state<Conflict[]>([]);
  let conflictTimer: ReturnType<typeof setTimeout> | undefined;
  // A silo://collection?url=… deep link: opens Mod sets → Open a shared set with a preview.
  // `n` changes for every link, so the same link twice previews again.
  let sharedRequest = $state<{ url: string; n: number } | null>(null);
  function openSharedLink(url: string) {
    sharedRequest = { url, n: (sharedRequest?.n ?? 0) + 1 };
    go("modSets", "open");
  }
  // Active set as manifest refs for MP sync (techName, path, kind, version).
  const activeModRefs = $derived(
    mods
      .filter((m) => activeSet.has(m.techName))
      .map((m) => ({ techName: m.techName, path: m.path, kind: m.kind, version: m.version })),
  );
  // Set when a previous bisection didn't finish (app closed mid-run) — the user's mod
  // set may be partially applied, so offer to restore it.
  let bisectRecovery = $state<string[] | null>(null);
  /** The active mods when the running diagnosis started (each round must park the rest). */
  let bisectStartSet: ReadonlySet<string> = new Set();
  // True while a guided bisection is running (snapshot saved, test halves applied). The
  // focus rescan holds off meanwhile so returning from FS25 can't rewrite the active set.
  let bisecting = $state(false);
  /** Diagnose → Last run's slot while that tab is on screen (DiagnoseView binds it), else null. */
  let diagnoseLastRunSlot = $state<HTMLElement | null>(null);
  /** The running bisection's round, for the header's mini-status (null before the first). */
  let bisectRoundNo = $state<number | null>(null);
  /** A test round is set up and waiting for the user to play it (Play says "Launch test round"). */
  let bisectLaunching = $state(false);
  /** Play is launching a test round of Find the problem mod. */
  const testRound = $derived(bisecting && bisectLaunching);
  /** The crash notice's "Find the problem mod": Last run opens the flow, then clears this. */
  let bisectRequested = $state(false);

  // ── Crash detection (Diagnose, #260) ──
  // The last game's crash, until a clean run or Dismiss: the notice shows it and the status chip
  // counts it as one problem. `parked` is set once the user parked the culprit from the notice.
  let lastCrash = $state<{ run: LogRun; culprit: CrashCulprit | null; parked: boolean } | null>(null);
  /** Bumped whenever crash detection read a new log, so Last run re-reads it. */
  let logReadKey = $state(0);
  const CRASH_KEY = "crash";
  /** Mods Find the problem mod would test: the active ones that aren't maps. */
  const bisectSuspects = $derived(mods.filter((m) => activeSet.has(m.techName) && !m.isMap).length);
  /** The library's title and active state for a log name (a tech name). */
  function libraryLookup(techName: string): { title: string; active: boolean } | null {
    const m = mods.find((x) => x.techName === techName);
    return m ? { title: m.title ?? m.techName, active: activeSet.has(m.techName) } : null;
  }

  const crashWatch = createCrashWatch({
    status: logStatus,
    scan: scanLog,
    markSeen: markLogSeen,
    quiet: () => bisecting || bisectRecovery !== null,
    onRun: (run) => {
      logReadKey++;
      if (!run.report.crashed) {
        // A clean run settles an earlier crash.
        lastCrash = null;
        return;
      }
      lastCrash = { run, culprit: likelyCulprit(run.report, libraryLookup), parked: false };
    },
  });
  onDestroy(() => crashWatch.stop());

  /** Park a library mod if it's active (with the switch's Undo toast). */
  async function parkMod(techName: string) {
    if (activeSet.has(techName)) await toggleActive(techName);
  }

  async function parkCrashCulprit() {
    const c = lastCrash?.culprit;
    if (!c) return;
    await parkMod(c.modName);
    if (lastCrash?.culprit === c && !activeSet.has(c.modName)) lastCrash = { ...lastCrash, parked: true };
  }

  function findProblemMod() {
    bisectRequested = true;
    go("diagnose", "lastRun");
  }

  // The crash notice follows lastCrash: shown while there's an unresolved crash (hidden while Find
  // the problem mod runs, which owns it until it ends), cleared after. The words and offers come
  // from crashNoticeModel; the culprit is matched to the library as it is now.
  const fgReason = $derived(fgLock.reason);
  $effect(() => {
    const crash = lastCrash;
    const model =
      crash &&
      crashNoticeModel({
        culprit: crash.culprit,
        live: crash.culprit ? libraryLookup(crash.culprit.modName) : null,
        parked: crash.parked,
        suspects: bisectSuspects,
        libraryKnown: libraryScanned || scanFailed,
        bisecting,
        recoveryPending: bisectRecovery !== null,
      });
    if (!model) {
      untrack(() => syncNotice(CRASH_KEY, null));
      return;
    }
    const off = fgReason ? { disabled: true, title: fgReason } : {};
    untrack(() =>
      syncNotice(CRASH_KEY, {
        level: "warning",
        icon: "problem",
        title: model.title,
        body: model.body,
        actions: [
          ...(model.park ? [{ label: `Park ${model.park}`, run: parkCrashCulprit, primary: true, ...off }] : []),
          ...(model.find ? [{ label: "Find the problem mod", run: findProblemMod, primary: true, ...off }] : []),
          { label: "See details", run: () => go("diagnose", "lastRun") },
        ],
        dismissible: true,
        onDismiss: () => (lastCrash = null),
      }),
    );
  });
  // The current view is nav.view (lib/state/nav). The mod drawer belongs to the Library: leaving
  // it closes the drawer, as switching to Browse did. The Browse drawer lives inside ModBrowser,
  // which unmounts on switch, so it closes itself.
  $effect(() => {
    if (nav.view !== "library") untrack(() => (detailMod = null));
  });
  let userDir = $state<string | null>(null);
  let appVer = $state<string | null>(null);
  let update = $state<AvailableUpdate | null>(null);
  let updating = $state(false);
  let updatePct = $state<number | null>(null);

  const FINISH_FIRST = "Finish what Silo is doing first, then update.";

  /** Silo is in the middle of something an install (which relaunches) must not cut short. */
  async function changingFiles(): Promise<boolean> {
    let writing = false;
    try {
      writing = await appBusy();
    } catch {
      /* the check failed; the state below still guards */
    }
    return !!(
      writeLock.blocked ||
      archiveMove.running ||
      archiveMove.unfinished ||
      bisecting ||
      bisectRecovery ||
      writing
    );
  }

  async function applyUpdate() {
    if (updating) return;
    // Set before the first await, so a second click can't start a second install (#358).
    updating = true;
    // Installing relaunches Silo: never in the middle of a change to the library.
    let busy: boolean;
    try {
      busy = await changingFiles();
    } catch (e) {
      updating = false;
      reportError(e, "Updating Silo");
      return;
    }
    if (busy) {
      updating = false;
      refuse(FINISH_FIRST);
      return;
    }
    let blocking = false;
    try {
      // The download changes nothing. Right before installing, block new library changes in
      // the backend, then check again that nothing is running.
      const installed = await installUpdate(
        (p) => (updatePct = p),
        async () => {
          await setSelfUpdating(true);
          blocking = true;
          return !(await changingFiles());
        },
      );
      if (!installed) {
        refuse(FINISH_FIRST);
        await setSelfUpdating(false);
        blocking = false;
        updating = false;
        updatePct = null;
      }
      // Installed: Silo relaunches; if we're still here, it was a no-op.
    } catch (e) {
      reportError(e, "Updating Silo", { retry: applyUpdate });
      if (blocking) await setSelfUpdating(false).catch(() => {});
      updating = false;
      updatePct = null;
    }
  }
  let configEditor = $state<{
    title: string;
    path: string;
    fields: CfgField[];
    presets: CfgPreset[];
    footnote?: string;
  } | null>(null);

  function openGameGraphics() {
    if (!userDir) return;
    configEditor = {
      title: "Graphics settings",
      path: `${userDir}/game.xml`,
      fields: GAME_GRAPHICS_FIELDS,
      presets: GAME_GRAPHICS_PRESETS,
      footnote: "Changes take effect the next time you launch the game.",
    };
  }

  function openSaveConfig(s: Savegame) {
    if (!userDir) return;
    configEditor = {
      title: `Configure: ${s.name}`,
      path: `${userDir}/${s.folder}/careerSavegame.xml`,
      fields: SAVEGAME_FIELDS,
      presets: SAVEGAME_PRESETS,
      footnote: "Edits this savegame's settings. Back up the save first if unsure.",
    };
  }

  const libStats = $derived.by(() => {
    let totalSize = 0;
    let rated = 0;
    let ratingSum = 0;
    let tagged = 0;
    const byCat: Record<string, { count: number; size: number }> = {};
    for (const m of effectiveMods) {
      totalSize += m.size;
      (byCat[m.category] ??= { count: 0, size: 0 });
      byCat[m.category].count++;
      byCat[m.category].size += m.size;
      const r = cur(m.techName).rating;
      if (r > 0) {
        rated++;
        ratingSum += r;
      }
      if (tagsOf(m.techName).length > 0) tagged++;
    }
    const cats = Object.entries(byCat)
      .map(([name, v]) => ({ name, ...v }))
      .sort((a, b) => b.size - a.size);
    const largest = [...effectiveMods].sort((a, b) => b.size - a.size).slice(0, 8);
    const maxCatSize = cats.reduce((m, c) => Math.max(m, c.size), 1);
    return {
      totalSize,
      cats,
      maxCatSize,
      largest,
      rated,
      avgRating: rated ? ratingSum / rated : 0,
      tagged,
    };
  });
  let gameInfo = $state<GameInfo | null>(null);
  let settingsModsSet = $state<Set<string>>(new Set());
  let settingsMod = $state<{ techName: string; title: string } | null>(null);

  async function launch() {
    // Only Problems ask first (Review and Note never do; more than one map is a Note). A test
    // round of Find the problem mod launches on purpose with whatever half is on.
    if (
      criticalCount > 0 &&
      !testRound &&
      !(await ask({
        title:
          criticalCount === 1
            ? "Launch with a conflict that needs fixing?"
            : `Launch with ${criticalCount} conflicts that need fixing?`,
        body: "The game may crash or misbehave with these mods together. Launching changes nothing in your library, and you can fix the conflicts after you quit.",
        confirm: "Launch anyway",
      }))
    )
      return;
    try {
      // The profile shown is the one started, or none is (#387). A paused library move that
      // left mods unlinked asks first (#415).
      if (!(await launchAsking(profileInUse(modsStatus)))) return;
      crashWatch.afterLaunch(); // look at the log when the game exits
    } catch (e) {
      reportError(e, "Launching the game");
    }
  }

  // Library health: missing dependencies, corrupt/unreadable mods, and mods the
  // game silently ignores (name starts with a digit).
  const health = $derived.by(() => {
    const lib = new Set(mods.map((m) => m.techName));
    const missingDeps: { mod: ModEntry; missing: string[] }[] = [];
    const corrupt: ModEntry[] = [];
    const ignored: ModEntry[] = [];
    for (const m of mods) {
      if (m.error) corrupt.push(m);
      if (m.ignoredDigitPrefix) ignored.push(m);
      const miss = m.dependencies.filter((d) => !lib.has(d));
      if (miss.length > 0) missingDeps.push({ mod: m, missing: miss });
    }
    return { missingDeps, corrupt, ignored, foreign: foreignFiles };
  });
  const healthCount = $derived(
    health.missingDeps.length +
      health.corrupt.length +
      health.ignored.length +
      health.foreign.length,
  );

  const criticalCount = $derived(
    conflicts.filter((c) => c.severity === "critical").length,
  );

  // Every mod (by tech name) in a non-info conflict: the Conflict badge, the "In conflict"
  // library filter and its count.
  const conflictedSet = $derived(conflictedTechNames(conflicts));

  // Only the newest check's answer lands: a slow check of an earlier active set must not
  // overwrite the check of the set that's active now (#260: after a mod set switch the chip
  // said "All good" while the new set had a conflict open).
  const conflictsLatest = latestOnly<Conflict[]>();
  /** Only the lock's on/off: fgLock is a new object on every op start, end or label change. */
  const fgBlocked = $derived(fgLock.blocked);
  async function runConflictCheck() {
    const active = mods.filter((m) => activeSet.has(m.techName));
    if (active.length < 2) {
      conflictsLatest.cancel();
      conflicts = [];
      return;
    }
    try {
      await conflictsLatest.run(
        () =>
          detectConflicts(
            active.map((m) => ({
              techName: m.techName,
              title: m.title,
              path: m.path,
              kind: m.kind,
            })),
          ),
        (c) => (conflicts = c),
      );
    } catch (e) {
      reportError(e, "Checking for conflicts", { background: true });
    }
  }

  // Re-check conflicts (debounced) whenever the active set or library changes. While a library
  // write runs (a mod set switch, a bisection round) the files are moving, so the check waits
  // for it to end: fgLock is reactive, and its release re-runs this effect.
  $effect(() => {
    void activeSet;
    void mods;
    const writing = fgBlocked;
    clearTimeout(conflictTimer);
    conflictsLatest.cancel(); // a check of the set before this change mustn't land
    if (writing) return;
    conflictTimer = setTimeout(runConflictCheck, 400);
  });

  const libraryTechNames = $derived(new Set(mods.map((m) => m.techName)));

  // Each save's user mods (DLC left out) against the library.
  const saveRows = $derived(savegames.map((s) => ({ s, st: saveModStats(s, libraryTechNames) })));

  async function loadSavegames() {
    try {
      savegames = await getSavegames();
    } catch (e) {
      reportError(e, "Reading your savegames", { background: true });
    }
  }

  function buildReport(): string {
    const L: string[] = [];
    L.push("# Silo diagnostics report");
    L.push(`Generated: ${new Date().toISOString()}`, "");
    L.push("## Environment");
    L.push(`- Mods folder: ${roots[0] ?? "not detected"}`);
    L.push(`- Game: ${gameInfo ? displayPath(gameInfo.installDir) : "not detected"}`, "");
    L.push("## Library");
    L.push(`- Mods: ${mods.length}  (organized ${organizedCount}, active ${activeSet.size})`);
    L.push(`- Total size: ${fmtSize(libStats.totalSize)}`);
    L.push(`- Maps: ${stats.maps}  ·  Script mods: ${stats.scripts}  ·  Tagged: ${libStats.tagged}`, "");
    L.push("### By category");
    for (const c of libStats.cats) L.push(`- ${c.name}: ${c.count}  (${fmtSize(c.size)})`);
    L.push("");
    const st = setStore.state;
    const setPart = st.kind === "none" ? "" : `, mod set “${st.name}”${st.kind === "custom" ? ` with ${st.changes} changes` : ""}`;
    L.push(`## Active set${setPart} (${activeSet.size} mods)`, "");
    const reviewCount = conflicts.filter((c) => c.severity === "warning").length;
    L.push(`## Conflicts (${criticalCount} problems, ${reviewCount} to review, ${conflicts.length} total)`);
    for (const c of conflicts.filter((x) => x.severity !== "info")) {
      L.push(`- [${severityLabel(c.severity)}] ${c.kind} “${c.name}”: ${c.mods.join(", ")}`);
    }
    L.push("");
    L.push(`## Health (${healthCount} issues)`);
    for (const d of health.missingDeps) {
      L.push(`- Missing dependency: ${d.mod.title ?? d.mod.techName} needs ${d.missing.join(", ")}`);
    }
    for (const m of health.ignored) L.push(`- Ignored (digit prefix): ${m.techName}`);
    for (const m of health.corrupt) L.push(`- Corrupt: ${m.techName}: ${m.error}`);
    for (const f of health.foreign)
      L.push(`- Foreign file at a managed name: ${f.fileName} (isn't Silo's, left untouched)`);
    return L.join("\n");
  }

  async function exportReport() {
    try {
      await saveTextFile("silo-report.md", await diagnosticsBundle(buildReport(), false));
    } catch (e) {
      reportError(e, "Exporting the report");
    }
  }

  async function copyDiagnostics() {
    try {
      const text = await diagnosticsBundle(buildReport(), true);
      const out = await copyOrSave(text, "silo-diagnostics.md", copyText, saveTextFile);
      if (out === "copied") success("Diagnostics copied. Paste them into your bug report.");
      else if (out === "saved")
        info("Silo couldn't use the clipboard, so it saved the diagnostics to a file instead.");
    } catch (e) {
      reportError(e, "Copying diagnostics");
    }
  }

  async function backupSave(s: Savegame) {
    // Held as a write like before (it copies files): nothing else changes them meanwhile. It
    // never touches the library archive, so an archive move doesn't stop it.
    const op = beginWrite(`Backing up ${s.name}`, false, { duringMove: true });
    if (!op) return;
    try {
      await backupSavegame(s.folder, userDir);
      success(`Backed up ${s.name}.`);
    } catch (e) {
      reportError(e, `Backing up ${s.name}`);
    } finally {
      op.end();
    }
  }

  /**
   * After a switch that threw or reported problems, read the mods folder again while the caller
   * still holds the write (as bisectRound does), so the Library, Play and the multiplayer share
   * list show what the game will load, not the optimistic set (#347).
   */
  async function reconcileHeld() {
    await waitWhile(() => scanning);
    await runScan(false, undefined, true);
  }

  /** Set the active set through the loadout apply path (optimistic set, then project it).
   *  After a throw or reported problems it rescans before it lets go of the write.
   *  @return true when it applied without errors; false when it finished with problems
   *  (reported) and the active mods changed; null when nothing changed: another library write
   *  was running (said so), or it failed (reported) and the mods folder still holds the old set */
  async function setActiveSetTo(techNames: string[], label: string): Promise<boolean | null> {
    const op = beginWrite(label);
    if (!op) return null;
    const before = activeSet;
    const next = new Set(techNames);
    activeSet = next; // optimistic: the Library shows the new set at once
    let errors: number | null = null; // null: set_active threw
    try {
      try {
        errors = (await applyActive(techNames)).errors.length;
      } catch (e) {
        // Taken as nothing applied: put the set back (unless something else set it since).
        if (activeSet === next) activeSet = before;
        reportError(e, "Applying your mods");
      }
      if (errors !== 0) await reconcileHeld();
    } finally {
      op.end();
    }
    return switchOutcome(errors, before, activeSet, !scanFailed);
  }

  /** The Undo toast of the last switch (a mod, a bulk change or a loadout); a new one dismisses it. */
  const switchUndo = createSwitchUndo({
    toast,
    blocked: (expected) => activeUndoBlocked(expected),
    reapply: (previous) => switchActiveSet(previous, "Switching back"),
    settle: settleScanOrRefuse,
  });

  /**
   * `setActiveSetTo` without a silent skip: set_active only parks filed mods, so the active mods
   * a new set leaves out that are loose zips are filed into the library first (that is how a
   * loose mod is parked; the switch's confirmation already said they'd be parked), and unzipped
   * folder mods, which Silo can't park, stay on and are named. Used by mod set switches, Use
   * this save's mods and the switch Undo.
   * @return as setActiveSetTo: true clean, false with problems, null refused, cancelled or
   *   failed with nothing changed
   */
  async function switchActiveSet(target: string[], label: string): Promise<boolean | null> {
    // A scan running now read the mods folder before this switch: landing after it, it would put
    // the old active set back on screen, and the next toggle would re-apply that (#347). Wait for
    // it, then plan from what it found.
    if (!(await settleScanOrRefuse())) return null;
    /** The active mods before any filing (taken again after the dialog's scan wait, so a change
     *  that scan found isn't counted as the switch's). Stopping short with the active mods still
     *  these, nothing happened: the caller records nothing. Changed (a mod filing couldn't link
     *  back is parked now), the caller keeps its snapshot and Undo can put it back. */
    let baseline = activeSet;
    const { loose } = parkPlan(mods, activeSet, target);
    if (loose.length > 0) {
      const one = loose.length === 1 ? loose[0] : null;
      const name = one ? (one.title ?? one.techName) : modsCount(loose.length);
      if (!(await explainParkLoose(name, one?.category ?? null, loose.length))) return null;
      // A scan started while the dialog was open (alt-tabbing back) read the folder before the
      // filing: landing after it, it would still show these mods as loose and the switch below
      // would keep them on. Wait for it, then file the explained mods that are still loose.
      if (!(await settleScanOrRefuse())) return null;
      baseline = activeSet;
      const explained = new Set(loose.map((m) => m.techName));
      const toFile = parkPlan(mods, activeSet, target).loose.filter((m) => explained.has(m.techName));
      if (toFile.length > 0) {
        // Filed and kept active (linked back): the switch below parks them through set_active,
        // so if it's refused they're simply still on, as before. Filing ends with a rescan.
        const filed = await applyLoose(
          toFile.map(looseInput),
          toFile.map((m) => m.techName),
          true,
        );
        if (filed === null) return null; // refused, and said why: nothing changed
        // Its rescan is skipped if a scan queued behind the filing started first. That one began
        // after the filing (none ran when it started, and none starts while a write is held), so
        // it reads what filing left: wait for it however long it takes, rather than judge from
        // the optimistic set (a mod filing couldn't link back would look still on).
        await waitWhile(() => scanning);
      }
    }
    // Whatever is still active and can't be parked (folder mods, a zip that couldn't be filed)
    // stays in the set, so what Silo shows is what the game loads.
    const { loose: stillLoose, fixed } = parkPlan(mods, activeSet, target);
    const stay = [...stillLoose, ...fixed];
    const result = await setActiveSetTo([...target, ...stay.map((m) => m.techName)], label);
    if (result !== null && fixed.length > 0)
      toast({ message: fixedOnText(fixed.map((m) => m.title ?? m.techName)), tone: "warn" });
    // Short of the target with the active mods as they were (only a folder mod was to be parked,
    // say, or the apply was refused): nothing to record.
    return setSwitchOutcome(result, stay.length === 0, baseline, activeSet);
  }

  /**
   * Why an Undo that puts the active set back can't run now, or null.
   * @param expected the active set the Undo assumes (the one the switch left)
   */
  function activeUndoBlocked(expected: string[]): string | null {
    if (bisecting) return "Can't undo while a diagnosis is running.";
    if (archiveMove.running) return "Can't undo while a library move is running.";
    if (!sameMods(expected, activeSet)) return "Can't undo: your active mods changed since the switch.";
    return null;
  }

  /** Saving, replacing and deleting mod sets wait for a foreground library change (their
   *  buttons are off then too); a library move doesn't stop them. True when refused. */
  function loadoutEditRefused(): boolean {
    const fg = blockedForeground("library-write");
    if (!fg.blocked) return false;
    refuse(`${fg.reason} Try again when it's done.`);
    return true;
  }

  // Saved sets (loadouts), the remembered last-applied set and every Mod sets action. Switches
  // go through setActiveSetTo and the switch Undo toast above.
  const setStore = createModSets({
    active: () => activeSet,
    library: () => (libraryScanned ? libraryTechNames : undefined),
    title: (t) => {
      const m = mods.find((x) => x.techName === t);
      return m?.title ?? t;
    },
    setActiveSetTo: switchActiveSet,
    settle: settleScanOrRefuse,
    undo: switchUndo,
    editRefused: loadoutEditRefused,
    refuse,
    activeUndoBlocked,
    // "Park them too" after mods were taken out of sets: the BulkBar's Park, as it is.
    park: (techNames) => {
      const wanted = new Set(techNames);
      return setActiveForMods(
        effectiveMods.filter((m) => wanted.has(m.techName)),
        false,
      );
    },
  });

  /** The Library's "Mod sets…" dialog (#323): the mods it's for, and whether they're the selection
   *  (cleared once it saved, as Turn on / Park do). */
  let membership = $state<{ mods: ModEntry[]; fromSelection: boolean } | null>(null);
  let membershipOpen = $state(false);
  function openMembership(list: ModEntry[], fromSelection: boolean) {
    if (list.length === 0) return;
    membership = { mods: [...list], fromSelection };
    membershipOpen = true;
  }
  async function saveMembership(edits: MembershipEdit[]): Promise<boolean> {
    const m = membership;
    if (!m) return false;
    const ok = await setStore.applyMembership(
      m.mods.map((x) => x.techName),
      edits,
    );
    if (ok && m.fromSelection) selection.clear();
    return ok;
  }
  // Forget the dialog's mods once it has closed.
  $effect(() => {
    if (!membershipOpen) untrack(() => (membership = null));
  });

  /** The Share dialog's set (a saved set, or the active mods). */
  let shareTarget = $state<{ name: string; mods: string[] } | null>(null);
  let shareOpen = $state(false);
  /** Bumped after a link is created, so Your shared links lists it. */
  let sharedLinksKey = $state(0);
  function openShare(name: string, setMods: string[]) {
    shareTarget = { name, mods: [...setMods] };
    shareOpen = true;
  }

  /** Find a mod in Browse by its tech name (Savegames, Open a shared set). */
  function findInBrowse(techName: string, title: string | null = null) {
    browseSeed = title ?? techName;
    browseSeedTech = techName;
    go("browse");
  }

  /** Turn a single mod on or park it (the Multiplayer check's fix list), with Undo. */
  function setModActive(techName: string, on: boolean) {
    if (activeSet.has(techName) !== on) void toggleActive(techName);
  }

  function fileName(path: string): string {
    return path.split(/[\\/]/).pop() ?? path;
  }

  // Apply an active set and surface any warnings the projection engine reports — e.g. it
  // refused to remove a file you'd swapped in at a mod's name. These come back in the
  // report, not as a thrown error, so a plain `await setActive` would drop them silently.
  async function applyActive(techNames: string[]) {
    const rep = await setActive(techNames);
    if (rep.errors.length) errorMsg = rep.errors.slice(0, 3).join("; ");
    if (rep.notices?.length) infoMsg = rep.notices.join(" ");
    return rep;
  }

  // Guided bisection applies its test halves and the final restore through here, so
  // problems and notices reach the user. Rejects unless the set applied cleanly, so the
  // flow stops (and keeps its snapshot) instead of carrying on with the wrong mods.
  async function bisectApply(techNames: string[]) {
    const rep = await applyActive(techNames).catch((e) => {
      errorMsg = String(e);
      throw e;
    });
    // A plain string, which BisectFlow's String(e) shows as is.
    if (rep.errors.length) throw rep.errors.slice(0, 3).join("; ");
  }

  /** One bisection round (or its final restore) as a library write. Refused, it rejects with
   *  the reason, so the flow stops and keeps its snapshot. */
  async function bisectRound(techNames: string[]) {
    const op = tryStart("Applying the next test set", { exclusive: "library-write" });
    if (!op) throw blocked("library-write").reason;
    try {
      await bisectApply(techNames);
      // Read the mods folder again, still holding the write so nothing changes it first: the
      // Library, the strip and Play show the round's real mods, and a mod meant to be parked
      // that the game would still load stops the run (set_active skips what it can't park
      // without saying so) instead of testing the wrong mods.
      while (scanning) await new Promise((r) => setTimeout(r, 50));
      await runScan(false, undefined, true);
    } finally {
      op.end();
    }
    if (scanFailed)
      throw "Silo couldn't read your mods folder to check this round, so it can't tell what the round tests. Put your mods back, then start again.";
    const stuck = stillOn(activeSet, techNames, bisectStartSet);
    if (stuck.length > 0) {
      const names = stuck.map((t) => libraryLookup(t)?.title ?? t);
      throw `Silo couldn't park ${names.slice(0, 3).join(", ")}${names.length > 3 ? ` and ${names.length - 3} more` : ""}: still in your mods folder, so this round wouldn't test what it says. Trying again won't help: put your mods back, file the mod in the Library, then start again.`;
    }
  }

  /**
   * Before Find the problem mod starts: set_active parks only mods filed in the library, so
   * loose active zips would stay on every round (and the newest mod, the likely culprit, is
   * usually loose). Offer to file them first; they stay active.
   * @return true to go on; false when declined, refused or not all of them could be filed
   */
  async function prepareBisect(): Promise<boolean> {
    // Maps stay on every round, so they never need parking.
    const loose = mods.filter((m) => activeSet.has(m.techName) && isFileable(m) && !m.isMap);
    if (loose.length === 0) return true;
    if (
      !(await ask({
        title: `File ${modsCount(loose.length)} into your library first?`,
        body:
          "Silo can only park mods it keeps in your library, and these are loose in your mods folder. " +
          "Silo files them first: they stay active, and nothing is deleted. Undo organizing in Settings → Maintenance puts them back.",
        list: loose.slice(0, 50).map((m) => m.title ?? m.techName),
        confirm: "File them and start",
      }))
    )
      return false;
    const clean = await applyLoose(
      loose.map(looseInput),
      loose.map((m) => m.techName),
      true,
    );
    // Refused (said why) or problems (reported): don't start with mods it can't park, or with
    // filed mods that weren't linked back (the snapshot would leave them out, and Put my mods
    // back would leave them parked).
    return readyAfterFiling(clean, loose, mods, activeSet);
  }

  // When a bisection ends, a snapshot still on disk means the restore didn't finish (or
  // the panel was closed mid-run): offer the restore banner now rather than next launch.
  // After a clean finish, rescan so the library shows the restored set.
  async function bisectingChanged(active: boolean) {
    const wasRunning = bisecting;
    bisecting = active;
    if (active) {
      bisectStartSet = new Set(activeSet); // the mods the search started with
      return;
    }
    bisectRoundNo = null;
    bisectLaunching = false;
    // The test rounds' logs are the flow's, not news: don't announce the last round's crash.
    if (wasRunning) void crashWatch.markCurrentSeen();
    try {
      bisectRecovery = await bisectSnapshotGet();
    } catch {
      return; /* non-critical */
    }
    if (wasRunning && !bisectRecovery) await runScan(false);
  }

  /** The path separator the dialogs show. */
  const pathSep = typeof navigator !== "undefined" && /Win/i.test(navigator.userAgent) ? "\\" : "/";
  /** Before parking a loose mod: explain once that it's filed into the library (parkLoose.ts). */
  const explainParkLoose = createParkLooseExplainer(askWithOption, undefined, pathSep);

  /** The organize input that files a loose mod into the library as it is. */
  function looseInput(mod: ModEntry): ModInput {
    return {
      techName: mod.techName,
      fileName: fileName(mod.path),
      kind: mod.kind,
      category: mod.category,
      subcategory: mod.subcategory,
    };
  }

  /** Turn one mod on or park it, then offer Undo (the switch toast). */
  async function toggleActive(techName: string) {
    // A scan running now read the active set before this switch: landing after it, it would put
    // the switch back (and the next switch would re-apply that stale set). Wait for it, then
    // switch from what the scan found (nothing to do when it already shows the mod as asked).
    if (scanning) {
      const wantOn = !activeSet.has(techName);
      if (!(await settleScan())) {
        refuse(SCAN_WAIT);
        return;
      }
      if (activeSet.has(techName) === wantOn) return;
    }
    const turningOff = activeSet.has(techName);
    const mod = effectiveMods.find((m) => m.techName === techName);
    const name = mod?.title ?? techName;
    // A loose (unorganized) mod isn't a Silo projection — it's the user's own file sitting in the
    // live folder, so set_active can't remove it (and would silently no-op, the toggle bouncing
    // back on the next scan). To park it, file it into the library (flat→archive, no re-project):
    // it leaves the live folder but stays in the library. Reversible — toggling it on re-projects.
    if (turningOff && mod && isFileable(mod)) {
      // Refuse first: don't explain something that can't happen now.
      const busy = writeRefusal();
      if (busy) {
        refuse(busy);
        return;
      }
      if (!(await explainParkLoose(name, mod.category))) return;
      const previous = [...activeSet];
      const filed = await applyLoose([looseInput(mod)], [mod.techName], false, false, () =>
        switchUndo.dismiss(),
      );
      // Problems were reported by applyLoose; a refusal changed nothing.
      if (filed) switchUndo.offer({ message: switchMessage([name], false), previous, expected: [...activeSet] });
      return;
    }
    // An unzipped folder mod Silo hasn't filed: set_active would leave it on without a word.
    if (turningOff && mod && isUnparkable(mod)) {
      refuse(fixedOnText([name]));
      return;
    }
    const op = beginWrite(turningOff ? "Parking a mod" : "Activating a mod");
    if (!op) return;
    switchUndo.dismiss();
    const before = activeSet;
    const next = new Set(activeSet);
    if (next.has(techName)) next.delete(techName);
    else next.add(techName);
    activeSet = next; // optimistic: the switch moves at once
    let errors: number | null = null; // null: set_active threw
    try {
      try {
        errors = (await applyActive([...next])).errors.length;
      } catch (e) {
        // Taken as nothing applied: put the switch back (unless something else set the mods since).
        if (activeSet === next) activeSet = before;
        reportError(e, "Changing which mods are active");
      }
      // Problems: read the mods folder again before letting go of the write, so the switch shows
      // what the game will load (#347).
      if (errors !== 0) await reconcileHeld();
    } finally {
      op.end();
    }
    // Nothing changed (it failed, and the mods folder still holds the old set): no Undo.
    const clean = switchOutcome(errors, before, activeSet, !scanFailed);
    if (clean === null) return;
    switchUndo.offer({
      message: switchMessage([name], !turningOff, clean),
      tone: clean ? "neutral" : "warn",
      previous: [...before],
      expected: [...activeSet],
    });
  }

  // Resolve a foreign file (a mod-folder file that isn't Silo's projection — typically a newer
  // build the user dropped in to update). Adopt = promote it to the managed copy; restore = put
  // Silo's copy back. Shared by the health panel and the mod drawer's "you dropped an update" banner.
  // Returns true when it worked; false when it was refused (another write) or failed, so the
  // drawer stays open.
  async function resolveForeign(fileName: string, action: "adopt" | "restore"): Promise<boolean> {
    // The file was found in the folder shown; mid-switch, Silo may already use the next one (#358).
    if (folderSwitches > 0) {
      refuse(FOLDER_SWITCH_WAIT);
      return false;
    }
    const op = beginWrite(action === "adopt" ? "Using the version in your mods folder" : "Putting Silo's version back");
    if (!op) return false;
    let done = false;
    try {
      if (action === "adopt") await adoptForeignFile(fileName);
      else await restoreForeignFile(fileName);
      done = true;
    } catch (e) {
      reportError(e, action === "adopt" ? "Using the version in your mods folder" : "Putting Silo's version back");
    } finally {
      op.end();
    }
    await runScan(false);
    await refreshForeignFiles();
    return done;
  }

  /**
   * Look for files at a managed name that aren't Silo's projection, in the current mods folder.
   * The answer is dropped when a newer check started or the folder changed meanwhile (#358):
   * hashing can be slow on a cloud-synced folder, and the old folder's files mustn't show.
   */
  async function refreshForeignFiles(): Promise<void> {
    const gen = ++foreignGen;
    const root = roots[0];
    const found = await detectForeignFiles(root).catch(() => []);
    if (gen === foreignGen && roots[0] === root) foreignFiles = found;
  }

  // Build the organize inputs for every loose (unorganized) zip currently in view.
  function looseInputs(): { inputs: ModInput[]; techNames: string[] } {
    const targets = effectiveMods.filter(isFileable);
    return {
      inputs: targets.map(looseInput),
      techNames: targets.map((m) => m.techName),
    };
  }

  /** Mods per apply_organize call when filing with progress (the dialog and the setup flow). */
  const FILE_RUN = 25;

  /**
   * File a prepared set of organize inputs into the library. `keepActive` re-projects the filed
   * mods so their loaded state is preserved (the auto-filer and the manual paths). `auto`: the
   * auto-filer, which says nothing when another write is running (the next scan retries).
   * `onStart` runs once the write is under way (parking dismisses the previous switch toast).
   * `onProgress` files in runs of FILE_RUN and reports mods done of total.
   * @returns null when refused; else the report's problem lines and anything thrown
   */
  async function fileLoose(
    inputs: ModInput[],
    techNames: string[],
    keepActive: boolean,
    opts: { auto?: boolean; onStart?: () => void; onProgress?: (done: number, total: number) => void } = {},
  ): Promise<FilingRun | null> {
    if (inputs.length === 0) return null;
    // A category pick still being saved decides where a mod goes: wait until none is, then file
    // by the categories as they are then (no await between that and taking the write), so one
    // whose save failed isn't used (#358). If the folder changed or a scan started meanwhile
    // (even one that has ended), these inputs may be stale: refuse, as a busy library would.
    if (serialPendingUnder("override:")) {
      const root = roots[0];
      const scans = scansStarted;
      await serialDrained("override:");
      const moved = folderSwitches > 0 || roots[0] !== root;
      if (moved || scanning || scansStarted !== scans) {
        if (!opts.auto) refuse(moved ? FOLDER_SWITCH_WAIT : SCAN_WAIT);
        return null;
      }
    }
    inputs = withCategoriesNow(inputs);
    const op = beginWrite(`Filing ${modsCount(inputs.length)} into the library`, !!opts.onProgress, {
      silent: opts.auto,
    });
    if (!op) return null;
    opts.onStart?.();
    let out: FilingRun;
    try {
      // With progress, in runs of FILE_RUN; otherwise in one call, as before.
      const onProgress = opts.onProgress;
      out = await fileInRuns(
        inputs,
        applyOrganize,
        FILE_RUN,
        onProgress &&
          ((done, total) => {
            op.progress(done, total);
            onProgress(done, total);
          }),
      );
      // Filed mods left the mods folder: link them back, after a partial failure too (the
      // ones that failed are still loose, so set_active leaves them alone).
      if (keepActive && out.changed > 0) {
        const next = new Set([...activeSet, ...techNames]);
        activeSet = next;
        // Reports its own problems; a throw is kept so the dialog or setup flow can finish.
        await relinkFiled(out, () => applyActive([...next]));
      }
    } finally {
      op.end();
    }
    await runScan(false);
    if (out.relinkError !== undefined) reportError(out.relinkError, "Turning your filed mods back on");
    return out;
  }

  /** Organize inputs with each mod's category as the library shows it now (its override, if any). */
  function withCategoriesNow(inputs: ModInput[]): ModInput[] {
    const now = new Map(effectiveMods.map((m) => [m.techName, m]));
    return inputs.map((i) => {
      const m = now.get(i.techName);
      return m ? { ...i, category: m.category, subcategory: m.subcategory } : i;
    });
  }

  // Apply a prepared set of organize inputs, reporting any problem as a toast (with Try again).
  // Returns true when they were filed cleanly, false on a problem (reported), null when refused.
  async function applyLoose(
    inputs: ModInput[],
    techNames: string[],
    keepActive: boolean,
    auto = false,
    onStart?: () => void,
  ): Promise<boolean | null> {
    const out = await fileLoose(inputs, techNames, keepActive, { auto, onStart });
    if (!out) return null;
    /** What went wrong: the thrown error (keeps its code), or the report's problems. */
    const problem: unknown =
      out.thrown ?? (out.errors.length ? out.errors.slice(0, 3).join("; ") : null);
    if (problem !== null) {
      // Try again files whichever of these mods are still loose, as they are now.
      const wanted = new Set(techNames);
      const retry = async () => {
        const fresh = looseInputs();
        await applyLoose(
          fresh.inputs.filter((i) => wanted.has(i.techName)),
          fresh.techNames.filter((t) => wanted.has(t)),
          keepActive,
        );
      };
      reportError(problem, "Filing mods into the library", { retry });
    }
    // Filed but not linked back (reported by fileLoose): not clean, so a caller that needs
    // them active (the auto-filer, Find the problem mod) doesn't carry on as if they were.
    return problem === null && out.relinkError === undefined;
  }

  // Auto-filer (opt-in): file new mods and keep them active — no prompt, they chose it.
  async function autoFile() {
    // Never behind the setup flow: it moves nothing until the user clicks Organize there.
    if (firstRun === "onboarding") return;
    const { inputs, techNames } = looseInputs();
    await applyLoose(inputs, techNames, true, true);
  }

  /**
   * File the planned mods with progress, for the Organize dialog and the setup flow. Report
   * problems come back as failures (shown in place, and logged to the activity list); a thrown
   * error (the game is running, the library is busy) is reported as usual and every planned mod
   * still loose counts as not moved.
   * @returns refused, or the mods that couldn't be filed
   */
  async function fileWithProgress(
    plan: PlannedMove[],
    onProgress: (done: number, total: number) => void,
  ): Promise<OrganizeOutcome> {
    const wanted = new Set(plan.map((p) => p.techName));
    const inputs = looseInputs().inputs.filter((i) => wanted.has(i.techName));
    const out = await fileLoose(
      inputs,
      inputs.map((i) => i.techName),
      true,
      { onProgress },
    );
    if (!out) return { refused: true };
    const failures = filingFailures(out.errors, plan);
    if (out.thrown !== null) {
      // Shown in place (the dialog or the setup flow), and kept in the activity list.
      const why = inlineError(out.thrown, "Filing mods into the library");
      if (out.changed === 0) return { refused: true, message: why };
      const listed = new Set(failures.map((f) => f.techName));
      const loose = new Set(looseInputs().techNames);
      for (const p of plan)
        if (loose.has(p.techName) && !listed.has(p.techName))
          failures.push({ techName: p.techName, fileName: p.fileName, message: why });
    } else if (failures.length) {
      reportError(out.errors.join("\n"), "Filing mods into the library", { quiet: true });
    }
    return { failures, filed: out.changed, relinkFailed: out.relinkError !== undefined };
  }

  // ── Organize dialog: the status strip's Review (and Settings → Library) ─────────
  // A read-only dry run of exactly what will move, shown before any file is touched. Its
  // "File new mods automatically from now on" turns on auto-file; it replaced the old "Don't
  // preview next time", whose localStorage key is cleared here.
  let organizePreview = $state<{
    inputs: ModInput[];
    techNames: string[];
    plan: PlannedMove[];
    loading: boolean;
    applying: boolean;
    progress?: { done: number; total: number } | null;
    failures?: FilingFailure[];
    /** Mods filed by the last try (with `failures`). */
    filed?: number;
    /** Why the last try filed nothing (shown in the dialog). */
    problem?: string;
    /** Filing loose mods already in the library archive (#166), not organizing mods/. */
    adopt?: { path: string | null; note: string | null };
  } | null>(null);

  try {
    localStorage.removeItem("silo.organizePreviewSkip");
  } catch {
    /* storage off: nothing to clear */
  }

  async function organizeNew() {
    const { inputs, techNames } = looseInputs();
    if (inputs.length === 0) return;
    organizePreview = { inputs, techNames, plan: [], loading: true, applying: false };
    try {
      const plan = await planOrganize(inputs);
      if (organizePreview) organizePreview = { ...organizePreview, plan, loading: false };
    } catch (e) {
      reportError(e, "Previewing what Organize moves");
      organizePreview = null;
    }
  }

  async function confirmOrganize(autoFileNext: boolean) {
    if (!organizePreview) return;
    if (organizePreview.adopt) {
      const fileNames = organizePreview.inputs.map((m) => m.fileName);
      const op = beginWrite(`Filing ${modsCount(fileNames.length)} into the library`);
      if (!op) return;
      organizePreview = { ...organizePreview, applying: true };
      let problem: unknown = null;
      try {
        const rep = await archiveAdoptApply(fileNames);
        if (rep.errors.length) problem = rep.errors.slice(0, 3).join("; ");
      } catch (e) {
        problem = e;
      } finally {
        op.end();
      }
      organizePreview = null;
      await runScan(false);
      if (problem !== null) reportError(problem, "Filing mods from the archive folder");
      return;
    }
    const { plan } = organizePreview;
    organizePreview = { ...organizePreview, applying: true, progress: { done: 0, total: plan.length } };
    const out = await fileWithProgress(plan, (done, total) => {
      if (organizePreview) organizePreview = { ...organizePreview, progress: { done, total } };
    });
    if (out.refused) {
      if (organizePreview)
        organizePreview = { ...organizePreview, applying: false, progress: null, problem: out.message ?? "" };
      return;
    }
    if (autoFileNext !== autoFileNew) setAutoFile(autoFileNext);
    if (out.failures.length) {
      if (organizePreview)
        organizePreview = {
          ...organizePreview,
          applying: false,
          progress: null,
          failures: out.failures,
          filed: out.filed,
        };
      return;
    }
    organizePreview = null;
    // The filed mods couldn't be linked back: that problem was reported instead.
    if (!out.relinkFailed)
      success(`Filed ${modsCount(out.filed)} into your library. All active mods stay active.`);
  }

  // ── First run (#250): the setup flow for a new library, "What's new" for an upgrade ─────
  /** Which first-run screen is up (decided once at startup from the database). */
  let firstRun = $state<"onboarding" | "whatsNew" | null>(null);
  let whatsNewOpen = $state(false);

  async function decideFirstRun() {
    try {
      firstRun = firstRunScreen(await firstRunState());
    } catch (e) {
      // Can't tell: show neither (the setup flow must not appear over a library it can't read).
      reportError(e, "Reading Silo's first-run settings", { quiet: true });
      return;
    }
    if (firstRun === "whatsNew") whatsNewOpen = true;
  }

  function storeFlag(name: FirstRunFlag) {
    setFirstRunFlag(name).catch((e) => reportError(e, "Saving a setting", { quiet: true }));
  }

  /** Leaving the setup flow: it doesn't come back, and "What's new" is moot for a new library. */
  function finishOnboarding(_how: FinishHow) {
    firstRun = null;
    storeFlag("onboarding_done");
    storeFlag("whats_new_seen");
    go("library");
  }

  function closeWhatsNew() {
    if (firstRun !== "whatsNew") return;
    firstRun = null;
    storeFlag("whats_new_seen");
    // An existing library: the setup flow never comes up later (e.g. after Undo organizing).
    storeFlag("onboarding_done");
  }

  /** The setup flow's "On another drive…": pick a folder and use it as the (empty) archive. */
  async function chooseArchiveFirstRun(): Promise<ArchiveChoice> {
    const why = await switchBlockReason();
    if (why) return { problem: why };
    const path = await pickFolderFor("archive");
    if (!path) return null;
    try {
      const pf = await archiveMovePreflight(path);
      if (pf.problems.length && !pf.canRepoint) return { problem: problemLines(pf.problems) };
      // Mods already there need the adopt step Settings offers; setup only uses an empty folder.
      if (pf.looseMods > 0)
        return {
          problem: `That folder already holds ${modsCount(pf.looseMods)}. Choose an empty folder, or set it up later in Settings → Library.`,
        };
      const op = beginWrite("Switching to the new library archive");
      if (!op) return { problem: blocked("library-write").reason || "Silo is busy. Try again in a moment." };
      // A re-point republishes the folder (maybe another FS25 profile, #387): crash looks wait
      // from before it until its status is adopted.
      await withCrashLooksHeld(async () => {
        try {
          await archiveRepoint(path);
        } finally {
          op.end();
        }
        await archiveMove.refresh();
        await adoptArchiveStatus(await modsRootStatus().catch(() => modsStatus));
        await runScan(false); // still held: the scan reads the folder just adopted
      });
      return { path };
    } catch (e) {
      return { problem: inlineError(e, "Switching to the new library archive") };
    }
  }

  /** The setup flow's "Inside your mods folder" after choosing another drive. */
  async function useDefaultArchiveFirstRun(): Promise<string | null> {
    const op = beginWrite("Switching to the new library archive");
    if (!op) return blocked("library-write").reason || "Silo is busy. Try again in a moment.";
    const problem = await withCrashLooksHeld(async () => {
      try {
        await archiveRepoint(null);
      } catch (e) {
        return inlineError(e, "Switching to the new library archive");
      } finally {
        op.end();
      }
      await archiveMove.refresh();
      await adoptArchiveStatus(await modsRootStatus().catch(() => modsStatus));
      await runScan(false); // still held: the scan reads the folder just adopted
      return null;
    });
    return problem;
  }

  /** The setup flow's dry run, kept for filing (so failures can name their files). */
  let firstRunPlan: PlannedMove[] = [];
  async function planFirstRun(): Promise<PlannedMove[]> {
    firstRunPlan = await planOrganize(looseInputs().inputs);
    return firstRunPlan;
  }

  async function organizeFirstRun(
    techNames: string[],
    autoFileNext: boolean,
    onProgress: (done: number, total: number) => void,
  ): Promise<OrganizeOutcome> {
    const wanted = new Set(techNames);
    const out = await fileWithProgress(
      firstRunPlan.filter((p) => wanted.has(p.techName)),
      onProgress,
    );
    if (!out.refused) {
      if (autoFileNext) setAutoFile(true);
      // Organized: the setup flow is done even if Silo closes before "Go to my library".
      storeFlag("onboarding_done");
      storeFlag("whats_new_seen");
    }
    return out;
  }

  async function rebuildLibrary() {
    // A paused archive move doesn't stop it (it only re-reads, as Settings says); a running one
    // holds the library, and beginWrite says so.
    const op = beginWrite("Re-reading every mod", false, { duringMove: !archiveMove.running });
    if (!op) return;
    // The write is held through the rescan, so nothing changes the library until it has been
    // read again. The status line names it: it re-reads every mod, so it takes a while.
    try {
      try {
        await clearScanCache();
      } catch (e) {
        reportError(e, "Re-reading every mod", { retry: rebuildLibrary });
        return;
      }
      op.setLabel("Re-reading every mod");
      await runScan(false, undefined, true);
    } finally {
      op.end();
    }
  }

  async function restoreVanilla() {
    if (
      !(await ask({
        title: "Put every mod back in your mods folder?",
        body: (() => {
          // Say the consequence: parked mods land in the mods folder too, so the game loads them.
          const parked = mods.filter((m) => !activeSet.has(m.techName)).length;
          return (
            `All ${nMods(mods.length)} go back into your mods folder, so the game loads every one of them` +
            (parked > 0 ? `, including the ${nMods(parked)} parked now` : "") +
            ". Silo removes its links. Nothing is deleted."
          );
        })(),
        confirm: "Undo organizing",
        tone: "danger",
      }))
    )
      return;
    const op = beginWrite("Undoing organizing");
    if (!op) return;
    try {
      const rep = await flatten();
      if (rep.errors.length)
        reportError(rep.errors.slice(0, 3).join("; "), "Undoing organizing");
      if (rep.notices?.length) info(rep.notices.join(" "), { duration: 10_000 });
    } catch (e) {
      reportError(e, "Undoing organizing");
    } finally {
      op.end();
    }
    await runScan();
  }

  // Overrides applied as a display layer over the scanned category.
  const effectiveMods = $derived(
    mods.map((m) => {
      const o = overrideMap[m.techName];
      return o ? { ...m, category: o.category, subcategory: o.subcategory } : m;
    }),
  );

  function openEditor(techName: string, ev: MouseEvent) {
    ev.stopPropagation();
    editing = { techName, x: ev.clientX, y: ev.clientY, anchor: null };
    editingOpen = true;
  }

  // ── Library row menu: the ⋯ button, right-click and Shift+F10 open the same items ──
  let rowMenu = $state<{ mod: ModEntry; at: MenuTarget } | null>(null);
  let rowMenuOpen = $state(false);
  // Kept outside the reactive state: Menu calls onclose (which clears rowMenu) before onselect.
  let rowMenuRun: ((id: string) => void) | null = null;
  function openRowMenu(mod: ModEntry, at: MenuTarget) {
    rowMenuRun = rowMenuFor(mod, at).run;
    rowMenu = { mod, at };
    rowMenuOpen = true;
  }
  // Forget the menu once it has closed, after its close handling (focus return reads the anchor).
  $effect(() => {
    if (!rowMenuOpen) untrack(() => (rowMenu = null));
  });
  // Seeds ModBrowser's catalog search when "Find in Browse" is used. ModBrowser remounts
  // on every view switch, so setting this before switching views seeds the fresh mount.
  let browseSeed = $state<string | null>(null);
  let browseSeedTech = $state<string | null>(null);

  function openInBrowse(mod: ModEntry) {
    browseSeed = mod.title ?? mod.techName;
    browseSeedTech = mod.techName; // so Browse can open this exact mod's drawer, not just filter to it
    go("browse");
  }

  // The reverse of openInBrowse: jump from a Browse card to the installed mod in the Library,
  // filtering to it and opening its drawer.
  function openInLibrary(techName: string) {
    const mod = mods.find((m) => m.techName === techName);
    if (!mod) return;
    query = mod.title ?? mod.techName; // seed the library search so the tile surfaces behind the drawer
    go("library");
    detailMod = mod; // open this mod's drawer
  }

  /**
   * The row menu for one mod: its items, and what each id does.
   * @param mod the row's mod
   * @param at where the menu opened (the category menu opens there too)
   */
  function rowMenuFor(mod: ModEntry, at: MenuTarget): { items: MenuItem[]; run: (id: string) => void } {
    const c = cur(mod.techName);
    const isActive = activeSet.has(mod.techName);
    const hasSettings = settingsModsSet.has(mod.techName);
    const items: MenuItem[] = [
      { id: "active", label: isActive ? "Park" : "Turn on", icon: isActive ? "circle" : "dot" },
      { id: "sets", label: "Mod sets…", icon: "modSets" },
      { id: "details", label: "Open details", icon: "info" },
      { id: "browse", label: "Find in Browse", icon: "search" },
      { id: "category", label: "Change category…", icon: "tag", separatorBefore: true },
    ];
    if (hasSettings) items.push({ id: "settings", label: "Edit settings…", icon: "modSettings" });
    items.push(
      { id: "favorite", label: c.favorite ? "Remove favorite" : "Favorite", icon: "favorite", separatorBefore: true },
      { id: "hidden", label: c.hidden ? "Unhide" : "Hide", icon: c.hidden ? "visible" : "hidden" },
      { id: "skipUpdates", label: c.skipUpdates ? "Check for updates" : "Don't check for updates", icon: "update" },
      { id: "broken", label: c.broken ? "Remove flag" : "Flag", icon: "flagged", danger: !c.broken },
    );
    const run = (id: string) => {
      switch (id) {
        case "active":
          return void toggleActive(mod.techName);
        case "sets":
          return openMembership([mod], false);
        case "details":
          detailMod = mod;
          return;
        case "browse":
          return openInBrowse(mod);
        case "category": {
          const r = at.anchor?.getBoundingClientRect();
          const x = at.point?.x ?? r?.left ?? 0;
          const y = at.point?.y ?? r?.bottom ?? 0;
          editing = { techName: mod.techName, x, y, anchor: at.anchor?.isConnected ? at.anchor : null };
          editingOpen = true;
          return;
        }
        case "settings":
          settingsMod = { techName: mod.techName, title: mod.title ?? mod.techName };
          return;
        case "favorite":
        case "hidden":
        case "broken":
        case "skipUpdates":
          return void toggleCuration(mod.techName, id);
      }
    };
    return { items, run };
  }
  const rowMenuSpec = $derived(rowMenu ? rowMenuFor(rowMenu.mod, rowMenu.at) : null);
  // Forget the category menu once it has closed (Esc, outside click, or a choice).
  $effect(() => {
    if (!editingOpen) untrack(() => (editing = null));
  });
  const CATEGORY_RESET = "silo:reset-category"; // not a category name
  /** The category menu's items: every category (the current one checked), then Reset to auto. */
  function categoryMenuItems(techName: string): MenuItem[] {
    const now = effectiveMods.find((m) => m.techName === techName)?.category;
    return [
      ...CATEGORIES.map((c) => ({ id: c, label: c, icon: c === now ? ("check" as const) : undefined })),
      { id: CATEGORY_RESET, label: "Use Silo's category", icon: "undo" as const, separatorBefore: true },
    ];
  }
  function modTitle(techName: string): string {
    const m = mods.find((x) => x.techName === techName);
    return m?.title ?? techName;
  }

  async function setCategory(
    techName: string,
    category: string,
    subcategory: string | null = null,
  ) {
    editing = null;
    try {
      await saveOverride(techName, { category, subcategory });
    } catch (e) {
      reportError(e, "Saving the category");
    }
  }

  async function resetCategory(techName: string) {
    editing = null;
    try {
      await saveOverride(techName, undefined);
    } catch (e) {
      reportError(e, "Resetting the category");
    }
  }

  /**
   * Show a mod's category change at once and save it. A failed save puts back the category the
   * database holds (unless a newer pick was made meanwhile), so Organize never files the mod under
   * a category that isn't stored (#358).
   * @param value the category, or undefined for Silo's own
   * @throws the save's error, after putting the category back
   */
  async function saveOverride(
    techName: string,
    value: { category: string; subcategory: string | null } | undefined,
  ): Promise<void> {
    const token = overrideSaved.begin(techName);
    overrideMap = withEntry(overrideMap, techName, value);
    try {
      await setOverride({ techName, category: value?.category ?? "", subcategory: value?.subcategory ?? null });
      overrideSaved.saved(techName, value);
    } catch (e) {
      const back = overrideSaved.failed(techName, token);
      if (back) overrideMap = withEntry(overrideMap, techName, back.value);
      throw e;
    }
  }

  function cur(techName: string): CurationRow {
    return (
      curationMap[techName] ?? {
        techName,
        favorite: false,
        hidden: false,
        broken: false,
        rating: 0,
        note: null,
        skipUpdates: false,
      }
    );
  }

  // Tags: techName -> string[]
  let tagMap = $state<Record<string, string[]>>({});
  let detailMod = $state<ModEntry | null>(null);
  // The drawer's mod as the library has it now (a rescan, a category change or filing replaces
  // the entry it was opened with); the drawer closes when the mod is gone.
  let drawerPrev: ModEntry | null = null;
  const drawerMod = $derived.by(() => (drawerPrev = currentEntry(detailMod, effectiveMods, drawerPrev)));
  $effect(() => {
    if (detailMod && !drawerMod) untrack(() => (detailMod = null));
  });

  const allTags = $derived.by(() => {
    const s = new Set<string>();
    for (const arr of Object.values(tagMap)) for (const t of arr) s.add(t);
    return [...s].sort((a, b) => a.localeCompare(b));
  });

  function tagsOf(techName: string): string[] {
    return tagMap[techName] ?? [];
  }
  async function loadTags() {
    try {
      const rows = await getTags();
      const m: Record<string, string[]> = {};
      for (const r of rows) (m[r.techName] ??= []).push(r.tag);
      tagMap = m;
    } catch (e) {
      reportError(e, "Loading your tags", { background: true });
    }
  }

  // GitHub repo links: techName -> {owner, repo}
  let repoMap = $state<Record<string, { owner: string; repo: string }>>({});
  async function loadRepos() {
    try {
      const rows = await getModRepos();
      repoMap = Object.fromEntries(rows.map((r) => [r.techName, { owner: r.owner, repo: r.repo }]));
    } catch (e) {
      reportError(e, "Loading linked GitHub repos", { background: true });
    }
  }

  // Source pages (where each mod came from, #196): techName -> saved page. Silo never requests
  // these; Open page hands them to the browser.
  let sourceMap = $state<Record<string, SourceRow>>({});
  async function loadSources() {
    try {
      const rows = await getModSources();
      sourceMap = Object.fromEntries(rows.map((r) => [r.techName, r]));
    } catch (e) {
      reportError(e, "Loading saved source pages", { background: true });
    }
  }
  function setSource(techName: string, row: SourceRow | null) {
    const next = { ...sourceMap };
    if (row) next[techName] = row;
    else delete next[techName];
    sourceMap = next;
  }
  async function markChecked(row: CheckYourselfRow) {
    try {
      const at = await markSourceChecked(row.techName);
      const s = sourceMap[row.techName];
      if (s) setSource(row.techName, { ...s, lastChecked: at });
    } catch (e) {
      reportError(e, "Saving the check date");
    }
  }

  let updateChecking = $state(false);
  let updateResults = $state<UpdateRow[]>([]);
  /** When the last check finished (epoch ms); null = never checked this session (no rail badge). */
  let updateCheckedAt = $state<number | null>(null);
  /** The last check's catalog request failed (Updates shows it with Try again). */
  let updateCatalogError = $state<ExplainedError | null>(null);
  /** The mods the last check asked about (one added since isn't "not tracked"). */
  let updateCheckedTechs = $state<ReadonlySet<string> | undefined>(undefined);
  /** Linked GitHub repos checked so far, while a check runs. */
  let updateProgress = $state<{ done: number; total: number } | null>(null);
  /** Per-mod install state in Updates (running, or the failure with Try again). */
  let updateRowStates = $state<Record<string, RowInstallState>>({});
  let updatingAll = $state(false);
  // Mods with a saved page that this check couldn't answer: the user checks those by hand.
  const checkYourself = $derived(checkYourselfList(mods, sourceMap, updateResults));
  /** Why Check now is off while an update installs (a check replaces every row), or "". */
  const updateCheckBlocked = $derived(checkBlockedReason({ updatingAll, rowStates: updateRowStates }));
  /** Why Update / Update all are off: a library write, or a check that's running. */
  const updateInstallLock = $derived(fgLock.reason || installBlockedReason(updateChecking));
  const linkedCount = $derived(Object.keys(repoMap).length);
  // The Updates view's sections. A mod updated since the check (by Silo, or a ModHub zip the user
  // used) drops out of the "newer version" sections, so the badge and filters stay accurate.
  /** Mods the user switched update checks off for (#333). */
  const skippedUpdates = $derived(
    new Set(Object.values(curationMap).filter((c) => c.skipUpdates).map((c) => c.techName)),
  );
  const updateSections = $derived(
    groupUpdates({
      mods,
      rows: updateResults,
      checkYourself,
      catalogReached: updateCheckedAt != null && !updateCatalogError,
      checkedTechs: updateCheckedTechs,
      skipped: skippedUpdates,
    }),
  );

  // "Needs update" is on-demand: the first time it's switched on it runs a quiet update check,
  // then filters to whatever came back (or reports that everything's current). No launch-time work.
  async function toggleNeedsUpdate(on = !libFilters.needsUpdate) {
    if (!on) {
      libFilters = { ...libFilters, needsUpdate: false };
      return;
    }
    if (updateResults.length === 0 && !updateChecking) await checkAllUpdates(true);
    const out = needsUpdateOutcome({
      available: availableUpdates.length,
      checking: updateChecking,
      checked: updateCheckedAt != null,
      catalogError: updateCatalogError?.message ?? null,
      blocked: updateCheckBlocked,
    });
    if (out.filter) libFilters = { ...libFilters, needsUpdate: true };
    else if (out.tone === "warn") refuse(out.say);
    else success(out.say);
  }

  /** A failure as Updates shows it in a row: logged, worded, and the "Copy details" text. */
  function explainUpdateError(e: unknown, op: string): ExplainedError {
    const d = describeError(e, op);
    return {
      message: inlineError(e, op),
      detail: formatDetail(d.detail),
      offline: d.code === "NET_OFFLINE" || d.code === "NET_TIMEOUT",
    };
  }

  const updateCheckDeps = {
    checkRepo: checkModUpdate,
    explain: explainUpdateError,
  };

  // The last check's rows stay up (and the badge with them) until the new check is done.
  /** Bumped when the mods folder changes: a check of the old folder still running mustn't land. */
  let updateFolderGen = 0;

  /** `link`: also link older installs under another name to their catalog entry, in the
   *  background (#402). A check that follows such links doesn't. */
  async function checkAllUpdates(quiet = false, link = true) {
    if (!quiet) go("updates");
    if (updateChecking || updateCheckBlocked) return;
    updateChecking = true;
    updateProgress = null;
    // Mods with update checks switched off (#333) aren't asked about at all.
    const checkable = mods.filter((m) => !cur(m.techName).skipUpdates);
    const asked = new Set(checkable.map((m) => m.techName));
    const gen = updateFolderGen;
    runningCheck = { gen, asked };
    try {
      const res = await runUpdateCheck(checkable, repoMap, {
        ...updateCheckDeps,
        catalog: (asks) => catalogCheckUpdates(asks, link),
        onProgress: (done, total) => {
          if (gen === updateFolderGen) updateProgress = { done, total };
        },
      });
      if (gen !== updateFolderGen) return; // the other folder's files: dropped
      updateResults = res.rows;
      updateRowStates = {};
      updateCatalogError = res.catalogError;
      updateCheckedTechs = asked;
      // A failed catalog request isn't a check: keep the last good time (or "Not checked yet").
      if (!res.catalogError) updateCheckedAt = Date.now();
    } finally {
      updateChecking = false;
      updateProgress = null;
      runningCheck = null;
    }
  }

  /** The check running now: its folder generation and the mods it asks about (#402). */
  let runningCheck: { gen: number; asked: ReadonlySet<string> } | null = null;

  /** Rows owed a refresh because Silo linked their mods while a check couldn't start (#402). */
  let heldLinkRefresh = $state.raw<HeldRefresh | null>(null);

  /** Mods Silo just linked to their catalog entries (#402): their rows are checked again, when a
   *  check the user allowed asked about them in this folder. */
  function onCatalogLinkedMods(linked: string[]) {
    const r = linkRefresh({
      linked,
      gen: updateFolderGen,
      running: runningCheck,
      lastAsked: updateCheckedTechs,
      // A refresh of other linked mods running counts as busy too.
      blocked: !!updateCheckBlocked || updateChecking || scanning || scanGate.pending,
    });
    if (r.when === "now") void refreshLinked(r.names, updateFolderGen);
    else if (r.when === "after") {
      heldLinkRefresh = holdRefresh(heldLinkRefresh, updateFolderGen, r.names);
    }
  }

  // A refresh held back meanwhile, once nothing keeps a check from starting, and only in the
  // folder it was for.
  $effect(() => {
    const h = heldRefresh(
      heldLinkRefresh,
      updateFolderGen,
      // A scan running or waiting (after an install, say) may be about to show newer files.
      updateChecking || !!updateCheckBlocked || scanning || scanGate.pending,
    );
    if (h.held !== heldLinkRefresh) heldLinkRefresh = h.held;
    if (h.run) void refreshLinked(h.run, updateFolderGen);
  });

  /** Check just these mods again (they were just linked, #402) and put their rows in. This
   *  check links nothing itself. */
  async function refreshLinked(names: string[], gen: number) {
    if (gen !== updateFolderGen) return;
    if (updateChecking || updateCheckBlocked || scanning || scanGate.pending) {
      heldLinkRefresh = holdRefresh(heldLinkRefresh, gen, names);
      return;
    }
    // Only mods still checked for updates, and not updated since the last scan that worked
    // (the library would still show the old file); the rest are dropped from this refresh.
    const leftOut = (t: string) => cur(t).skipUpdates || installedUnscanned.has(t);
    const subset = refreshTargets(names, mods, leftOut);
    if (subset.length === 0) return;
    const want = new Set(subset.map((m) => m.techName));
    const asked = fileStamps(subset);
    updateChecking = true;
    try {
      const res = await runUpdateCheck(subset, repoMap, {
        ...updateCheckDeps,
        catalog: (asks) => catalogCheckUpdates(asks, false),
      });
      // Another folder's rows, or a failed request: the rows stay as they were.
      if (gen !== updateFolderGen || res.catalogError) return;
      // The library changed under it (an install's scan, say): its answer is for older files,
      // so it's asked again once nothing is busy, never put in.
      const now = refreshTargets([...want], mods, leftOut);
      if (!sameStamps(asked, fileStamps(now))) {
        heldLinkRefresh = holdRefresh(heldLinkRefresh, gen, [...want]);
        return;
      }
      updateResults = mergeRows(updateResults, res.rows, want);
    } finally {
      updateChecking = false;
    }
  }

  /** "Couldn't check" → Try again: that mod's linked repo only. */
  async function recheckUpdate(row: UpdateRow) {
    const m = mods.find((x) => x.techName === row.techName);
    const r = repoMap[row.techName];
    if (!m || !r || updateChecking || updateRowStates[row.techName]) return;
    setRowState(row.techName, { phase: "checking" });
    const gen = updateFolderGen;
    try {
      const next = await recheckRow(row, m, r, updateCheckDeps);
      if (gen !== updateFolderGen) return; // the folder changed meanwhile
      updateResults = updateResults.map((x) => (x.techName === row.techName ? next : x));
    } finally {
      setRowState(row.techName, null);
    }
  }

  async function copyDetailsText(text: string) {
    const out = await copyOrSave(text, "silo-update-details.txt", copyText, saveTextFile).catch(
      () => "cancelled" as const,
    );
    if (out === "copied") success("Copied the details.");
    else if (out === "saved") success("Saved the details.");
  }

  function setRowState(techName: string, st: RowInstallState | null) {
    const next = { ...updateRowStates };
    if (st) next[techName] = st;
    else delete next[techName];
    updateRowStates = next;
  }

  /** The row now shows the version it installed. */
  function markInstalled(row: UpdateRow) {
    updateResults = updateResults.map((r) =>
      r.techName === row.techName ? { ...r, hasUpdate: false, current: r.latest } : r,
    );
    // The library still shows the old file until a scan started after this reads the new one.
    installedUnscanned.set(row.techName, ++installSeq);
  }

  /** Mods updated that no scan started since has read yet (by the update's number): the
   *  library's entry is still the old file, so a link refresh (#402) leaves their rows alone. */
  const installedUnscanned = new Map<string, number>();
  /** Counts updates installed, to tell which ones a scan started after. */
  let installSeq = 0;

  /** A failed install turns its row into the error, with Try again and Copy details there. */
  function markInstallFailed(row: UpdateRow, e: unknown) {
    const why = explainUpdateError(e, `Updating ${row.title}`);
    setRowState(row.techName, {
      phase: "error",
      message: why.message || "Silo couldn't update this mod.",
      detail: why.detail,
    });
  }

  async function installFromRow(row: UpdateRow) {
    if (!row.assetUrl || updateChecking) return;
    const op = beginWrite(`Updating ${row.title}`);
    if (!op) return;
    setRowState(row.techName, { phase: "installing" });
    let problems: string[] = [];
    try {
      const out = await downloadUpdate(row.path, row.assetUrl);
      setRowState(row.techName, null);
      markInstalled(row);
      problems = out.problems;
      const backup = out.backup;
      success(
        `Updated ${row.title}${row.latest ? ` to ${displayVersion(row.latest)}` : ""}. The previous version is in backups.`,
        {
          duration: 10_000,
          action: backup ? { label: "Show", run: () => void revealInFolder(backup) } : undefined,
        },
      );
    } catch (e) {
      markInstallFailed(row, e);
    } finally {
      op.end();
    }
    await runScan(false);
    // After the new version is in place (the update itself stands).
    if (problems.length) reportError(problems.join(" "), `Updating ${row.title}`);
  }

  /** Failures that would fail every remaining mod, so Update all stops there. */
  const UPDATE_ALL_STOPS = new Set([
    "GAME_RUNNING",
    "LIBRARY_BUSY",
    "MOVE_UNFINISHED",
    "MOVE_RUNNING",
    "ARCHIVE_UNAVAILABLE",
    "DISK_FULL",
    "NET_OFFLINE",
  ]);

  // Update all: one mod at a time under one library write, with "Updating 2 of 4: AutoDrive" in the
  // header, then one summary toast. No confirm: each update keeps the previous version in backups.
  async function updateAllReady() {
    const rows = updateSections.ready.filter((r) => r.assetUrl);
    if (rows.length === 0 || updatingAll || updateChecking) return;
    updatingAll = true;
    try {
      const res = await updateAll(rows, {
        begin: (label) => beginWrite(label, true),
        install: (row) => downloadUpdate(row.path, row.assetUrl!),
        stops: (e) => UPDATE_ALL_STOPS.has(describeError(e).code),
        onRowStart: (row) => setRowState(row.techName, { phase: "installing" }),
        onRowDone: (row) => {
          setRowState(row.techName, null);
          markInstalled(row);
        },
        onRowFailed: markInstallFailed,
      });
      if (res.refused) return;
      await runScan(false);
      const sum = updateAllSummary(res, (e) => describeError(e).title);
      if (sum) toast({ message: sum.message, body: sum.body, tone: sum.tone, duration: 10_000 });
    } finally {
      updatingAll = false;
    }
  }

  // ModHub's open-then-use loop: the zip the user saved from the page lands in the mods folder as
  // a different build at the mod's name; the focus rescan finds it and Silo offers to use it.
  const announcedDrops = new Set<string>();
  /** The "Use this version" offers shown for this mods folder: a folder switch takes them down. */
  let dropToasts: ToastHandle[] = [];
  $effect(() => {
    const arrivals = newDroppedBuilds(updateSections.modhub, foreignFiles, announcedDrops);
    // Untracked: showing a toast mustn't make this effect depend on the toast list.
    untrack(() => announceDrops(arrivals));
  });
  function announceDrops(arrivals: ReturnType<typeof newDroppedBuilds>) {
    const root = roots[0];
    for (const { row, file } of arrivals) {
      announcedDrops.add(droppedKey(file));
      const t = toast({
        message: `Found a newer ${row.title} (${displayVersion(file.flatVersion)}).`,
        duration: 15_000,
        action: {
          label: "Use this version",
          // Found in this mods folder: never acted on after a switch to another one (#358).
          run: () => {
            if (roots[0] === root) void resolveForeign(file.fileName, "adopt");
          },
        },
      });
      dropToasts.push(t);
    }
    // Keep the list short: the oldest offers go (taken down too, so none outlives a folder switch).
    if (dropToasts.length > 20) for (const old of dropToasts.splice(0, dropToasts.length - 20)) old.dismiss();
  }

  // The opt-in startup check: once the first scan has filled the library (a queued or skipped
  // scan resolves before that), and a moment later so the window settles first. Only checks.
  let startupCheckPending = $state(false);
  $effect(() => {
    if (!startupCheckPending || !libraryScanned || scanning || mods.length === 0) return;
    startupCheckPending = false;
    setTimeout(() => {
      if (updateCheckedAt == null) void checkAllUpdates(true);
    }, 3000);
  });

  const availableUpdates = $derived(availableRows(updateSections));
  const updatesSet = $derived(new Set(availableUpdates.map((r) => r.techName)));

  async function toggleCuration(
    techName: string,
    flag: "favorite" | "hidden" | "broken" | "skipUpdates",
  ) {
    const c = cur(techName);
    try {
      await saveCurationRow({ ...c, [flag]: !c[flag] });
    } catch (e) {
      reportError(e, "Saving your mod flags");
    }
  }

  /**
   * Show a mod's new flags / note / rating at once and save them. A failed save puts back the row
   * the database holds (unless a newer edit was made meanwhile), so nothing unsaved is shown as
   * current (#358). Every curation write goes through here, the drawer's too.
   * @param next the whole row to save
   * @throws the save's error, after putting the row back
   */
  async function saveCurationRow(next: CurationRow): Promise<void> {
    const t = next.techName;
    const token = curationSaved.begin(t);
    curationMap = withEntry(curationMap, t, next);
    try {
      await setCuration(next);
      curationSaved.saved(t, next);
    } catch (e) {
      const back = curationSaved.failed(t, token);
      if (back) curationMap = withEntry(curationMap, t, back.value);
      throw e;
    }
  }

  const q = $derived(query.trim().toLowerCase());
  // Library category tree for the FilterBar's parent/child dropdowns, built from the scan
  // (ModEntry.category + subcategory). Local counts, sorted A–Z. Shape matches FilterBar's CatNode.
  const libCatTree = $derived.by(() => {
    const map = new Map<string, { count: number; children: Map<string, number> }>();
    for (const m of effectiveMods) {
      let node = map.get(m.category);
      if (!node) {
        node = { count: 0, children: new Map() };
        map.set(m.category, node);
      }
      node.count++;
      if (m.subcategory)
        node.children.set(m.subcategory, (node.children.get(m.subcategory) ?? 0) + 1);
    }
    return [...map.entries()]
      .sort((a, b) => a[0].localeCompare(b[0]))
      .map(([name, n]) => ({
        name,
        count: n.count,
        children: [...n.children.entries()]
          .sort((a, b) => a[0].localeCompare(b[0]))
          .map(([cname, ccount]) => ({ name: cname, count: ccount, value: cname })),
      }));
  });
  /** What the filters read beyond a mod's scan entry (curation, conflicts, updates, tags). */
  const filterCtx: FilterContext = {
    curation: (t) => cur(t),
    active: (t) => activeSet.has(t),
    conflicted: (m) => conflictedSet.has(m.techName),
    hasUpdate: (t) => updatesSet.has(t),
    tags: (t) => tagsOf(t),
    verdict: (m) => getVerdict(m as ModEntry)?.status,
  };
  /** The filters in effect as chips (the category too), for ActiveFilters. */
  const libChips = $derived(filterChips(libFilters, libCat, libSub));
  function removeLibFilter(id: string) {
    if (id === "category") {
      libCat = "";
      libSub = "";
    } else {
      libFilters = withoutFilter(libFilters, id);
    }
  }
  /** Clear filters: every filter, the category and (from the empty state) the search too. */
  function clearLibFilters(alsoSearch = false) {
    libCat = "";
    libSub = "";
    libFilters = { ...NO_FILTERS };
    if (alsoSearch) query = "";
  }
  const filtered = $derived.by(() =>
    timed("silo:library-filter", () => {
      let list: readonly ModEntry[] = effectiveMods;
      if (libCat) {
        list = list.filter(
          (m) => m.category === libCat && (!libSub || m.subcategory === libSub),
        );
      }
      list = filterMods(list, libFilters, filterCtx);
      return searchMods(list, q);
    }),
  );
  /** How many mods "Needs attention" would show (hidden ones aside), for its chip. */
  const attentionCount = $derived(
    effectiveMods.reduce((n, m) => {
      const c = cur(m.techName);
      if (c.hidden && !libFilters.hidden) return n;
      const hit = needsAttention(m, {
        verdict: getVerdict(m)?.status,
        conflicted: filterCtx.conflicted(m),
        flagged: c.broken,
        hasUpdate: updatesSet.has(m.techName),
      });
      return n + (hit ? 1 : 0);
    }, 0),
  );

  let sortBy = $state<SortKey>("added");
  let sortDir = $state<"asc" | "desc">("desc");
  // Library view: the list (default, for every library size) or cards; remembered per machine.
  let libView = $state<LibraryViewMode>(loadLibraryView());
  $effect(() => saveLibraryView(libView));
  // Row density: comfortable (56px) by default, compact (44px) from Settings → General. Remembered.
  let libDensity = $state<Density>(loadLibraryDensity());
  function setLibDensity(d: Density) {
    libDensity = d;
    saveLibraryDensity(d);
  }
  // The rendered list or grid, so the drawer can hand focus back to the row it was opened from.
  let libList = $state<{
    focusKey: (key: string) => boolean;
    showKey: (key: string) => boolean;
    currentKey: () => string | null;
    focusCurrent: () => boolean;
  }>();
  /** The list or grid's element wrapper, the BulkBar and the toolbar, for handing focus on. */
  let listBodyEl = $state<HTMLElement | null>(null);
  let bulkBarEl = $state<HTMLElement | null>(null);
  let libToolbar = $state<{ focusFilters: () => boolean; focusFirst: () => boolean }>();
  /** A row's accessible name: its title and state ("Courseplay, active"). */
  const rowLabel = (m: ModEntry) => `${m.title ?? m.techName}, ${activeSet.has(m.techName) ? "active" : "parked"}`;
  // List ↔ cards: the new view starts on the mod the old one was on (focused if focus was in it,
  // else just scrolled to and made the tab stop).
  let shownView = untrack(() => libView);
  let pendingViewKey: { key: string; focus: boolean } | null = null;
  $effect.pre(() => {
    const v = libView;
    if (v === shownView) return;
    shownView = v;
    untrack(() => {
      const key = libList?.currentKey() ?? null;
      pendingViewKey = key ? { key, focus: focusInside(listBodyEl) } : null;
    });
  });
  $effect(() => {
    const l = libList; // re-bound to the new view's list once it mounts
    if (!l || !pendingViewKey) return;
    const p = pendingViewKey;
    pendingViewKey = null;
    untrack(() => (p.focus ? l.focusKey(p.key) : l.showKey(p.key)));
  });
  const modKey = (m: ModEntry) => m.path;
  // ── Multi-select (lib/state/selection): keys are mod paths, in the order of `visible` ──
  const selection = new Selection();
  /** A row's checkbox or a selection key: plain/Ctrl toggles, Shift selects from the anchor. */
  function selectMod(key: string, intent: SelectIntent) {
    selection.apply(key, intent, visibleKeys);
  }
  // A row's badges: problems, then status, then info (the row caps them at three).
  function badgesFor(mod: ModEntry) {
    const c = cur(mod.techName);
    return rowBadges(mod, {
      verdict: getVerdict(mod)?.status,
      hasUpdate: updatesSet.has(mod.techName),
      hasSettings: settingsModsSet.has(mod.techName),
      conflicted: conflictedSet.has(mod.techName),
      flagged: c.broken,
      hidden: c.hidden,
    });
  }

  const visible = $derived.by(() => {
    const list = filtered; // read first, so a keystroke's filtering isn't counted as sorting
    return timed("silo:library-sort", () => sortMods(list, sortBy, sortDir, (t) => cur(t).rating));
  });

  const visibleKeys = $derived(visible.map(modKey));
  /** The selected mods, in list order. */
  const selectedMods = $derived(
    selection.size === 0 ? [] : visible.filter((m) => selection.has(modKey(m))),
  );
  // A selection belongs to the Library visit it was made in: leaving the Library clears it.
  $effect(() => {
    if (nav.view !== "library") untrack(() => selection.clear());
  });
  // Filters, search or a rescan changed the list: forget selected mods that aren't in it.
  $effect(() => {
    const keys = new Set(visibleKeys);
    untrack(() => selection.prune(keys));
  });
  /** Esc in the list (unless an overlay above it takes Esc first): the first clears the
   *  selection; with nothing selected, it closes the mod drawer (focus stays in the list). */
  function onListKeydown(e: KeyboardEvent) {
    if (e.key !== "Escape" || e.defaultPrevented || layerCount() > 0) return;
    if (selection.size > 0) {
      e.preventDefault();
      selection.clear();
    } else if (detailMod) {
      e.preventDefault();
      detailMod = null;
    }
  }
  // The list goes when filters (or a switch under the Active / Parked segment) empty it: if a row
  // had focus, it goes to the empty state's first button, else the main area.
  returnFocusWhenHidden(
    () => scanning || filtered.length > 0,
    () => listBodyEl,
    () => (listBodyEl?.querySelector<HTMLElement>("button") ?? document.getElementById("main"))?.focus(),
  );
  // The BulkBar goes when the selection clears (Clear, Esc, a filter pruning it): if it had focus,
  // focus goes to the list's row, else the toolbar's first control.
  returnFocusWhenHidden(
    () => selection.size > 0,
    () => bulkBarEl,
    () => {
      if (!libList?.focusCurrent()) libToolbar?.focusFirst();
    },
  );

  /**
   * Why a library write can't start now (what beginWrite would say), or null. Checked before a
   * dialog, so the user isn't asked about something that would then be refused.
   */
  function writeRefusal(): string | null {
    if (archiveMove.unfinished && !archiveMove.running) return MOVE_UNFINISHED_COPY;
    const b = blocked("library-write");
    if (b.blocked) return `${b.reason} Try again when it's done.`;
    return scanning ? SCAN_WAIT : null;
  }

  /**
   * BulkBar Turn on / Park (the sequence and its dialogs are in library/bulkSwitch.ts): loose mods
   * being parked are filed into the library first, always after a dialog that says so; the rest
   * switch through the projection; then the switch toast offers Undo.
   * @param list the mods to switch
   * @param on true to turn them on, false to park them
   */
  async function setActiveForMods(list: ModEntry[], on: boolean) {
    // As a single switch does: wait briefly for a running scan instead of refusing at once.
    if (!(await settleScan())) {
      refuse(SCAN_WAIT);
      return;
    }
    const archive = archiveMove.archive;
    // Folder mods can't be parked: say so instead of reporting them parked.
    if (!on) {
      const fixed = list.filter((m) => activeSet.has(m.techName) && isUnparkable(m));
      if (fixed.length > 0) {
        toast({ message: fixedOnText(fixed.map((m) => m.title ?? m.techName)), tone: "warn" });
        list = list.filter((m) => !fixed.includes(m));
        if (list.length === 0) return;
      }
    }
    const outcome = await runBulkSwitch(list, on, {
      active: () => activeSet,
      isLoose: isFileable,
      busy: writeRefusal,
      cloudSynced,
      archiveOnOtherDrive: !!archive && !archive.isDefault && onOtherDrive(archive.path, roots[0]),
      sep: pathSep,
      ask,
      explain: (name, category) => explainParkLoose(name, category),
      fileLoose: async (techNames) => {
        if (!(await settleScan())) {
          refuse(SCAN_WAIT);
          return null;
        }
        // Built from the scan as it is now (as Try again does): mods filed meanwhile are skipped.
        const wanted = new Set(techNames);
        const fresh = looseInputs();
        const inputs = fresh.inputs.filter((i) => wanted.has(i.techName));
        if (inputs.length === 0) return true;
        return await applyLoose(
          inputs,
          fresh.techNames.filter((t) => wanted.has(t)),
          false,
          false,
          () => switchUndo.dismiss(),
        );
      },
      stillLoose: (techNames) => {
        const wanted = new Set(techNames);
        return effectiveMods.filter((m) => wanted.has(m.techName) && isFileable(m)).length;
      },
      applyProjected: async (techNames, turnOn) => {
        if (!(await settleScan())) {
          refuse(SCAN_WAIT);
          return null;
        }
        const op = beginWrite(turnOn ? "Turning on mods" : "Parking mods");
        if (!op) return null;
        switchUndo.dismiss();
        const next = new Set(activeSet);
        for (const t of techNames) {
          if (turnOn) next.add(t);
          else next.delete(t);
        }
        activeSet = next;
        let problem: unknown = null;
        let clean = true;
        try {
          const rep = await applyActive([...next]); // reports its own problems
          clean = rep.errors.length === 0;
        } catch (e) {
          problem = e;
        } finally {
          op.end();
        }
        // Reconcile the optimistic set against what's actually on disk — a partial/failed
        // projection (e.g. a file Silo couldn't prove was its own) leaves the two out of sync.
        await runScan(false);
        if (problem !== null) {
          reportError(problem, turnOn ? "Turning on mods" : "Parking mods");
          return "failed";
        }
        return clean;
      },
      toast,
      offerUndo: (o) => switchUndo.offer(o),
    });
    // Done (its toast offers Undo): the selection was for this change. Kept when it was cancelled
    // or refused, so it can be tried again.
    if (outcome === "done") selection.clear();
  }

  /**
   * BulkBar Favorite / Hide: set one curation flag on every selected mod, then offer Undo.
   * @param list the mods
   * @param flag "favorite" or "hidden"
   * @param value the flag's new value
   */
  async function setCurationFor(list: ModEntry[], flag: "favorite" | "hidden" | "skipUpdates", value: boolean) {
    const before = list.map((m) => cur(m.techName)).filter((c) => c[flag] !== value);
    if (before.length === 0) return;
    /** Set the flag on each mod in turn. Each row is built from the mod's flags, note and rating
     *  as they are when its turn comes, so an edit made in the drawer meanwhile is kept (#358);
     *  a row whose save fails goes back. */
    const write = (changes: { techName: string; on: boolean }[]) =>
      saveEach(changes, ({ techName, on }) => saveCurationRow({ ...cur(techName), [flag]: on }));
    const res = await write(before.map((c) => ({ techName: c.techName, on: value })));
    if (res.written.length === 0) {
      reportError(res.error, "Saving your mod flags");
      return;
    }
    if (res.failed.length) reportError(res.error, "Saving your mod flags", { quiet: true });
    const savedNames = new Set(res.written.map((r) => r.techName));
    const changed = before.filter((c) => savedNames.has(c.techName));
    const n = modsCount(changed.length);
    let message =
      flag === "favorite"
        ? value
          ? `Added ${n} to favorites.`
          : `Removed ${n} from favorites.`
        : flag === "skipUpdates"
          ? value
            ? `Silo won't check ${n} for updates.`
            : `Silo will check ${n} for updates again.`
          : value
            ? `Hid ${n}.`
            : `Unhid ${n}.`;
    if (res.failed.length) message += ` Couldn't update ${modsCount(res.failed.length)}.`;
    selection.clear();
    toast({
      message,
      tone: res.failed.length ? "warn" : "neutral",
      action: {
        label: "Undo",
        run: async () => {
          // Put back each mod's flag as it was, leaving its other flags as they are now.
          const back = await write(changed.map((c) => ({ techName: c.techName, on: c[flag] })));
          if (back.failed.length === 0) return;
          reportError(back.error, "Saving your mod flags", { quiet: true });
          toast({ message: `Couldn't update ${modsCount(back.failed.length)}.`, tone: "warn" });
        },
      },
    });
  }

  // Repair copy-projections into hardlinks: reclaims the duplicated space and makes future
  // activate/deactivate O(1) instead of a full-library content hash. Progress-reported so a
  // large one-time pass shows a bar instead of looking frozen.
  async function optimizeLinks() {
    const op = beginWrite("Reclaiming disk space", true);
    if (!op) return;
    let unlisten: (() => void) | null = null;
    let done = "";
    /** `done` explains why nothing changed (an info toast), not a result (a success toast). */
    let explained = false;
    let problem: unknown = null;
    try {
      unlisten = await onRelinkProgress((d, total) => op.progress(d, total));
      const rep = await relinkProjections();
      if (rep.crossDriveCopies) {
        explained = true;
        done =
          "Your library archive is on another drive and Windows can't link across drives without Developer Mode. Active mods stay as copies.";
      } else if (rep.hardlinksUnsupported) {
        explained = true;
        cloudSynced = true;
        done = "Your mods folder can't use links, so Silo keeps copies there and there's no space to reclaim.";
      } else {
        if (rep.errors.length) problem = rep.errors.slice(0, 3).join("; ");
        const gb = rep.reclaimedBytes / 1024 ** 3;
        const freed =
          gb >= 1 ? `${gb.toFixed(1)} GB` : `${Math.round(rep.reclaimedBytes / 1024 ** 2)} MB`;
        done =
          rep.upgraded > 0
            ? `Reclaimed ${freed} from ${modsCount(rep.upgraded)}.`
            : "Nothing to reclaim. Your active mods already share space with your library.";
      }
    } catch (e) {
      problem = e;
    } finally {
      unlisten?.();
      op.end();
    }
    await runScan(false); // reconcile + refresh sizes
    if (problem !== null) reportError(problem, "Reclaiming disk space");
    // Long enough to read: the cross-drive explanation is a full sentence.
    const duration = Math.max(6000, done.length * 50);
    if (done && explained) info(done, { duration });
    else if (done) success(done, { duration });
  }

  const stats = $derived.by(() => {
    let maps = 0,
      scripts = 0,
      unique = 0,
      issues = 0;
    for (const m of mods) {
      if (m.isMap) maps++;
      if (m.scriptCount > 0) scripts++;
      if (m.uniqueType) unique++;
      if (m.error || m.ignoredDigitPrefix) issues++;
    }
    return { maps, scripts, unique, issues };
  });

  const pct = $derived(
    progress.total > 0 ? Math.round((progress.done / progress.total) * 100) : 0,
  );

  // A scan, from outside runScan: one screen-reader line when a scan the user asked for starts
  // and one when it ends (not every tick, and not the quiet rescans after every write), and the
  // §5.5 timing from its start to the Library painted with the result (DevTools → Performance →
  // Timings, "silo:scan-to-render").
  let scanSaid = $state("");
  /** Set by the rescans the user starts (F5, Rescan, Try again, the first scan); read once. */
  let announceNextScan = false;
  let scanAnnounced = false;
  let scanTimed = false;
  let scanSeq = 0;
  /** runScan, announced to screen readers. */
  function rescanAnnounced(auto = true) {
    announceNextScan = true;
    return runScan(auto);
  }
  $effect(() => {
    if (scanning) {
      scanTimed = true;
      scanSeq++;
      markStart("silo:scan-to-render");
      scanAnnounced = announceNextScan;
      announceNextScan = false;
      if (scanAnnounced) scanSaid = "Scanning your mods…";
    } else if (scanTimed) {
      scanTimed = false;
      if (scanAnnounced) {
        const n = untrack(() => mods.length);
        scanSaid = untrack(() => scanFailed)
          ? "The scan didn't finish."
          : `Scan finished: ${nMods(n)}.`;
      }
      // After the DOM update, then after the frame that paints it; skipped if a newer scan has
      // started meanwhile (its start mark isn't this scan's).
      const seq = scanSeq;
      void tick().then(() =>
        requestAnimationFrame(() =>
          setTimeout(() => {
            if (seq === scanSeq && !untrack(() => scanning)) measureFrom("silo:scan-to-render");
          }, 0),
        ),
      );
    }
  });

  /** Why switching folders isn't safe right now, or null. */
  function switchBlockReason(): Promise<string | null> {
    return switchGate(
      {
        moveUnfinished: archiveMove.unfinished,
        scanning,
        busy: writeLock.blocked ? (currentOp()?.label ?? null) : null,
        blocked: !!modsStatus?.layoutProblem,
      },
      async () => !!(await bisectSnapshotGet().catch(() => null)),
    );
  }

  let statusGen = 0;
  /** Adopt a new status; if the live folder changed, reload everything folder-specific.
   *  `refreshArchive: false` when an archive operation calls (it has just read the archive
   *  status itself, and may be the archive move that refresh would ask about). */
  async function applyModsStatus(st: ModsRootStatus, { refreshArchive = true } = {}) {
    // A newer status adopted while this one waits for the archive takes over: this one doesn't
    // put its folder back. Once its folder is set, it finishes the reload (a newer status for
    // the same folder leaves that to it).
    const gen = ++statusGen;
    const prev = roots[0] ?? null;
    modsStatus = st;
    // Against the profile whose files are shown, not the last status: a status that was
    // overtaken mid-switch still leaves the newest one to load the new profile's files.
    const profileChanged = profileInUse(st) !== shownProfile;
    // Before any await: nothing from the other profile's folder stays up meanwhile (#387).
    if (profileChanged) forgetGameFiles();
    if (refreshArchive) await archiveMove.refresh(); // the archive belongs to the mods folder
    if (gen !== statusGen) return;
    roots = rootsFor(st);
    if ((roots[0] ?? null) === prev) {
      if (profileChanged) await loadGameFiles(true);
      return;
    }
    // A scan already running reads the old folder: let it land, then start over on the new one
    // (runScan skips while one runs, which would leave the old folder's mods up).
    await waitWhile(() => scanning);
    result = null;
    mods = [];
    libraryScanned = false;
    scanFailed = false;
    activeSet = new Set();
    // The status line spoke of the old folder's scan: clear it, and announce the new one (#385).
    scanSaid = "";
    // The other folder's foreign files: drop a check still running, and its "Use this version"
    // offers (#358).
    foreignGen++;
    foreignFiles = [];
    for (const t of dropToasts.splice(0)) t.dismiss();
    // The last update check was of the other folder's files (and one still running is too).
    updateFolderGen++;
    heldLinkRefresh = null;
    updateResults = [];
    updateCheckedAt = null;
    updateCatalogError = null;
    updateCheckedTechs = undefined;
    updateRowStates = {};
    if (roots.length) {
      await rescanAnnounced();
      hardlinkSupport()
        .then((ok) => (cloudSynced = !ok))
        .catch(() => {});
    }
    await loadGameFiles(profileChanged);
  }

  /** The FS25 profile folder the status uses (#387), or null for the main user folder. */
  function profileInUse(st: ModsRootStatus | null): string | null {
    return st?.gameProfile?.enabled ? st.gameProfile.dir : null;
  }

  /** Reload what Silo reads from the game's user folder: savegames, the folder the config
   *  editors open, mods with settings. When the FS25 profile changed (#387) the log is another
   *  one too: the old crash notice belonged to the other profile, so look again. */
  let gameFilesGen = 0;
  /** The FS25 profile whose game files are shown (null: the main user folder); undefined while
   *  none are (dropped, and not read again yet). */
  let shownProfile: string | null | undefined = null;

  /** The FS25 profile changed: drop everything read from the other profile's folder, and any
   *  load or crash look of it still running, until `loadGameFiles` reads the new one. */
  function forgetGameFiles() {
    gameFilesGen++;
    shownProfile = undefined;
    savegames = [];
    userDir = null;
    settingsModsSet = new Set();
    configEditor = null;
    settingsMod = null;
    lastCrash = null;
    crashWatch.logChanged();
  }

  async function loadGameFiles(profileChanged: boolean) {
    // Only the newest load lands: an older one may describe the other profile's folder.
    const gen = ++gameFilesGen;
    try {
      // One backend read: the saves and the folder their paths are built from never come from
      // two different FS25 profiles.
      const files = await gameFiles();
      if (gen !== gameFilesGen) return;
      savegames = files.savegames;
      userDir = files.userDir;
      settingsModsSet = new Set(files.modsWithSettings);
      // Where these came from, as the backend read them (not the status this app holds).
      shownProfile = files.profile;
    } catch (e) {
      if (gen === gameFilesGen) reportError(e, "Reading the game's files", { background: true });
    }
    if (profileChanged && gen === gameFilesGen) {
      logReadKey++;
      void crashWatch.check();
    }
  }

  /** Run `fn` with crash-log looks held (they may resume nested): for an operation that can
   *  publish another FS25 profile before its result is adopted (#387). Looks again after. */
  async function withCrashLooksHeld<T>(fn: () => Promise<T>): Promise<T> {
    crashWatch.pause();
    try {
      return await fn();
    } finally {
      crashWatch.resume();
      void crashWatch.check();
    }
  }

  /** Adopt a status read after an archive move, re-point or recovery the same way as a folder
   *  switch: FS25's own setting may have moved Silo to another folder or FS25 profile meanwhile
   *  (#387), so roots, the library (reset and rescan), game files and the crash notice all
   *  follow, as `applyModsStatus` does. Run as a switch (`whileSwitchingFolder`), without its
   *  archive refresh (the caller just read it). */
  async function adoptArchiveStatus(st: ModsRootStatus | null) {
    if (!st) return; // nothing read yet, and the refresh failed: keep what is shown
    await whileSwitchingFolder(() => applyModsStatus(st, { refreshArchive: false }));
  }

  let profileToggling = false;
  async function setGameProfileOn(on: boolean) {
    if (profileToggling) return;
    const why = await switchBlockReason();
    if (why) {
      refuse(why);
      return;
    }
    profileToggling = true;
    try {
      await whileSwitchingFolder(async () => applyModsStatus(await setGameProfile(on)));
    } catch (e) {
      reportError(e, "Changing how Play starts the game");
    } finally {
      profileToggling = false;
    }
  }

  /** Mods-folder switches under way (the picker, its dialogs, the rescan): no focus rescan, and
   *  no acting on a foreign file found in the folder being left. */
  let folderSwitches = 0;
  const FOLDER_SWITCH_WAIT = "Wait for the mods folder switch to finish.";
  async function whileSwitchingFolder<T>(fn: () => Promise<T>): Promise<T> {
    // No crash look while the backend may already read the new folder's FS25 profile and the
    // app still shows the old one: the run would be announced in the wrong place (#387).
    if (folderSwitches++ === 0) crashWatch.pause();
    try {
      return await fn();
    } finally {
      if (--folderSwitches === 0) {
        crashWatch.resume();
        void crashWatch.check();
      }
    }
  }

  function switchModsFolder(path: string) {
    return whileSwitchingFolder(() => switchModsFolderNow(path));
  }

  async function switchModsFolderNow(path: string) {
    const why = await switchBlockReason();
    if (why) {
      refuse(why);
      return;
    }
    try {
      if (
        (await folderLooksEmpty(path)) &&
        !(await ask({
          title: "Use an empty folder as your mods folder?",
          body: "This folder has no mods yet, so your library will look empty here. Nothing is moved; switch back to your old folder any time in Settings.",
          facts: [{ label: "Folder", value: path }],
          confirm: "Use this folder",
        }))
      )
        return;
      const st = await setModsRoot(path);
      // Messages about the old folder don't apply any more.
      for (const t of [...toasts]) dismissToast(t.id);
      // Switch first: if updating FS25's setting then fails, Silo is still on the chosen folder
      // and the mismatch banner explains (the error shows too).
      await applyModsStatus(st);
      // Never for a folder that belongs to its own FS25 profile (#387), launch on or off: that
      // profile has its own settings, and the main profile's must not be touched
      // (canWriteGameSetting is false too).
      if (
        st.mismatch &&
        st.canWriteGameSetting &&
        !(st.gameProfile && (st.gameProfile.enabled || st.gameProfile.detected)) &&
        (await ask({
          title: "Point Farming Simulator 25 at this folder too?",
          body:
            "Silo updates the game's mods folder setting and keeps a backup, so you can switch back later. " +
            "Close Farming Simulator 25 first. The game rewrites that setting when it exits.",
          facts: [
            { label: "The game reads now", value: st.gameReads ?? "(unknown)" },
            { label: "Silo uses", value: path },
          ],
          confirm: "Update the game's setting",
          cancel: "Only in Silo",
          // Enter keeps the choice that changes nothing outside Silo (#385).
          focus: "cancel",
        }))
      ) {
        await pointGameHere();
      }
    } catch (e) {
      reportError(e, "Switching mods folders");
    }
  }

  function openModsFolder() {
    openKnownFolder("modsFolder").catch((e) => reportError(e, "Opening the mods folder"));
  }

  async function changeModsFolder() {
    const why = await switchBlockReason();
    if (why) {
      refuse(why);
      return;
    }
    await whileSwitchingFolder(async () => {
      const path = await pickFolderFor("modsFolder");
      if (path) await switchModsFolder(path);
    });
  }

  async function useGameFolder() {
    const why = await switchBlockReason();
    if (why) {
      refuse(why);
      return;
    }
    await whileSwitchingFolder(async () => {
      try {
        await applyModsStatus(await clearModsRoot());
      } catch (e) {
        reportError(e, "Switching to the game's mods folder");
      }
    });
  }

  async function pointGameHere() {
    try {
      await applyModsStatus(await pointGameAtModsRoot());
    } catch (e) {
      reportError(e, "Updating FS25's mods folder setting");
    }
  }

  // Both may publish another folder (and its FS25 profile, #387): no crash look in between,
  // like a switch.
  async function retryModsFolder() {
    try {
      await whileSwitchingFolder(async () => applyModsStatus(await modsRootStatus()));
    } catch (e) {
      reportError(e, "Checking the mods folder", { retry: retryModsFolder });
    }
  }

  async function makeModsFolder() {
    await whileSwitchingFolder(async () => {
      try {
        await applyModsStatus(await createModsFolder());
      } catch (e) {
        reportError(e, "Creating the mods folder");
        // A refusal may come after the backend republished the folder in use (another one, or
        // its FS25 profile, #387): adopt that before crash looks resume and Play is pressed.
        await applyModsStatus(await modsRootStatus()).catch(() => {});
      }
    });
  }

  // ── Library archive location (#166) ──────────────────────────────────────────
  // The move's state, poll and drive/settle cycle live in archiveMove.svelte.ts; App supplies
  // the side effects that reach beyond the move.
  const archiveMove = new ArchiveMove({
    // A cancel that lost the race to a finished move isn't a failure: say what happened.
    setError: (msg) =>
      isOnlyLostCancel(msg, archiveMove.lastLostCancel)
        ? info(msg, { duration: 12_000 })
        : reportError(msg, "Moving your library"),
    refreshModsStatus: () =>
      withCrashLooksHeld(async () =>
        adoptArchiveStatus(await modsRootStatus().catch(() => modsStatus)),
      ),
    // The whole move, from before its backend call until its result is adopted (#387).
    holdCrashLooks: () => {
      crashWatch.pause();
      return () => {
        crashWatch.resume();
        void crashWatch.check();
      };
    },
    runScan: () => runScan(),
    // One adopt offer at a time (a second settle pass mustn't replace an open preview).
    offerAdopt: async () => {
      if (!organizePreview) await offerAdopt();
    },
  });

  // A running move holds the library-write class, so other writes refuse and say why. It ends
  // as soon as the move stops running (finished or paused), before the move's settle rescans.
  // It's a background op: navigation, Settings, read-only panels and scans stay usable while it
  // runs (fgLock leaves it out). The move's own notice shows its progress, so it's quiet too
  // (no status line, no activity entry).
  let moveOp: OpToken | null = null;
  $effect(() => {
    const isRunning = archiveMove.running;
    untrack(() => {
      if (isRunning && !moveOp) {
        moveOp = start("Moving your library", {
          exclusive: "library-write",
          progress: true,
          background: true,
          quiet: true,
        });
      } else if (!isRunning && moveOp) {
        moveOp.end();
        moveOp = null;
      }
    });
  });
  $effect(() => {
    const p = archiveMove.progress;
    if (p && p.total > 0) untrack(() => moveOp?.progress(p.done, p.total));
  });
  $effect(() => () => moveOp?.end());

  // The archive's notice: an unfinished move (with Pause / Resume / Cancel), else an archive
  // whose folder isn't available.
  $effect(() => {
    const n = archiveNotice(
      {
        archive: archiveMove.archive,
        progress: archiveMove.progress,
        running: archiveMove.running,
        unfinished: archiveMove.unfinished,
        cancelling: archiveMove.cancelling,
        pauseRequested: archiveMove.pauseRequested,
      },
      {
        pause: () => archiveMove.pause(),
        resume: () => archiveMove.resume(),
        cancel: () => archiveMove.cancel(),
        retry: async () => {
          await archiveMove.refresh();
          await runScan();
        },
        chooseAnother: changeArchive,
      },
    );
    syncNotice(ARCHIVE_MOVE_KEY, n?.key === ARCHIVE_MOVE_KEY ? n : null);
    syncNotice(ARCHIVE_UNAVAILABLE_KEY, n?.key === ARCHIVE_UNAVAILABLE_KEY ? n : null);
  });

  // The startup read gave up: try once more whenever Settings opens.
  $effect(() => {
    if (nav.view === "settings" && archiveMove.loadFailed) void archiveMove.refresh();
  });

  /** Point Silo at another archive folder (`path`, or null = the default) without moving
   *  anything, then rescan so the library reconciles against it. `why`: the current archive is
   *  missing, or it holds no mods and a move to `path` was refused (`refusal`: the first reason). */
  async function repointArchive(path: string | null, why: "missing" | "empty", refusal = "") {
    const old = archiveMove.archive?.path ?? "its old folder";
    const target = path ?? "The default archive inside your mods folder";
    const ok = await ask(
      why === "empty"
        ? {
            title:
              path === null
                ? "Use the default library archive?"
                : `Use ${target} as your library archive?`,
            body:
              `Silo can't move your library there${refusal ? ` (${unDot(refusal)})` : ""}, but your current archive is empty, ` +
              "so Silo can use this folder as your library instead. Nothing is moved; you can choose another archive folder later in Settings.",
            facts: [
              { label: "Current archive", value: old },
              { label: "New archive", value: target },
            ],
            confirm: "Use this folder",
          }
        : {
            title:
              path === null
                ? "Start fresh with the default library archive?"
                : `Use ${target} as your library archive?`,
            body:
              path === null
                ? "Your library archive isn't available. Nothing is moved or copied, and mods still on the old drive stay there, untouched. If you restored your archive somewhere, cancel, use Choose another and pick that folder instead."
                : "Your library archive isn't available. Nothing is moved or copied. Use this if you moved or restored your archive there yourself. Mods still on the old drive stay there, untouched, and you can point Silo back at it later in Settings.",
            facts: [
              { label: "Missing archive", value: old },
              { label: "New archive", value: target },
            ],
            confirm: path === null ? "Use default" : "Use this folder",
          },
    );
    if (!ok) return;
    try {
      const op = beginWrite("Switching to the new library archive");
      if (!op) return;
      // A re-point republishes the folder (maybe another FS25 profile, #387): crash looks wait
      // from before it until its status is adopted, as for a move.
      await withCrashLooksHeld(async () => {
        try {
          await archiveRepoint(path);
        } finally {
          op.end();
        }
        await archiveMove.refresh();
        await adoptArchiveStatus(await modsRootStatus().catch(() => modsStatus));
        await runScan(); // still held: the scan reads the folder just adopted
      });
    } catch (e) {
      reportError(e, "Switching to the new library archive");
    }
  }

  async function changeArchive() {
    const why = await switchBlockReason();
    if (why) {
      refuse(why);
      return;
    }
    const path = await pickFolderFor("archive");
    if (!path) return;
    if (archiveMove.missing) {
      await repointArchive(path, "missing");
      return;
    }
    try {
      const check = start("Checking the folder"); // a read: it changes nothing
      let pf: ArchivePreflight;
      try {
        pf = await archiveMovePreflight(path);
      } finally {
        check.end();
      }
      if (pf.problems.length) {
        // Nothing to move and the folder clears every path check: offer to use it as the
        // library as it is.
        if (pf.canRepoint) await repointArchive(path, "empty", pf.problems[0]);
        else
          toast({
            message: "Silo can't move your library there.",
            body: problemLines(pf.problems),
            tone: "warn",
            sticky: true,
          });
        return;
      }
      let body =
        "Active mods stay playable. Silo links them into your mods folder" +
        (pf.sameDrive ? "." : " (or copies them if links aren't available).");
      if (!pf.sameDrive && !pf.symlinksAvailable)
        body += " Turn on Windows Developer Mode to link active mods instead of copying them.";
      if (pf.looseMods > 0)
        body += ` That folder already holds ${pf.looseMods === 1 ? "1 loose mod" : `${pf.looseMods} loose mods`}. Silo will offer to file ${pf.looseMods === 1 ? "it" : "them"} in afterwards.`;
      body +=
        " Close Farming Simulator 25 first. You can pause the move to play and resume later, cancel it to put moved mods back, or move the library back afterwards.";
      const facts = [
        { label: "From", value: archiveMove.archive?.path ?? "(unknown)" },
        { label: "To", value: path },
        { label: "Mods", value: String(pf.modCount) },
        { label: "Size", value: fmtSize(pf.bytes) },
      ];
      if (!pf.sameDrive && pf.freeBytes != null)
        facts.push({ label: "Free on that drive", value: fmtSize(pf.freeBytes) });
      if (
        !(await ask({
          title: `Move your library to ${path}?`,
          body,
          facts,
          confirm: "Move library",
        }))
      )
        return;
      archiveMove.startTracking();
      await archiveMove.drive(() => archiveMoveStart(path));
    } catch (e) {
      reportError(e, "Moving your library");
    }
  }

  async function archiveDefault() {
    const why = await switchBlockReason();
    if (why) {
      refuse(why);
      return;
    }
    if (archiveMove.missing) {
      await repointArchive(null, "missing");
      return;
    }
    if (
      !(await ask({
        title: "Move your library back into your mods folder?",
        body: "Mods are moved, not copied, and active mods stay playable. Close Farming Simulator 25 first. You can pause or cancel the move, and move it out again later.",
        facts: [
          { label: "From", value: archiveMove.archive?.path ?? "(unknown)" },
          { label: "To", value: roots[0] ? `Your mods folder (${roots[0]})` : "Your mods folder" },
        ],
        confirm: "Move library",
      }))
    )
      return;
    archiveMove.startTracking();
    await archiveMove.drive(() => archiveUseDefault());
  }

  /** After a move, if the archive folder holds loose mods, offer to file them in. */
  async function offerAdopt() {
    const pv = await archiveAdoptPreview().catch(() => null);
    if (!pv) return;
    const n = pv.duplicates.length;
    const dupNote = n
      ? `${modsCount(n)} in that folder ${n === 1 ? "is" : "are"} already in your library and ${n === 1 ? "was" : "were"} left untouched: ${capList(pv.duplicates)}.`
      : null;
    if (pv.inputs.length === 0) {
      if (dupNote) info(dupNote, { duration: 10_000 });
      return;
    }
    organizePreview = {
      inputs: pv.inputs,
      techNames: pv.inputs.map((m) => m.techName),
      plan: pv.plan,
      loading: false,
      applying: false,
      adopt: { path: archiveMove.archive?.path ?? null, note: dupNote },
    };
  }

  function dismissOverrideNotice() {
    overrideNoticeOpen = false;
    localStorage.setItem("silo.overrideNoticeShown", "1");
  }

  // ── Notices: each condition raises and clears its own key (a rescan touches only `scan`) ──

  // The mods folder: unavailable / blocked / not created yet, FS25 reading another folder, and
  // the one-time "Silo now follows FS25's folder".
  $effect(() => {
    const n = modsFolderNotices(modsStatus, overrideNoticeOpen, {
      retryBlocked: fgLock.reason,
      retry: retryModsFolder,
      chooseAnother: changeModsFolder,
      useGameFolder,
      pointGameHere,
      createFolder: makeModsFolder,
      dismissFollowed: dismissOverrideNotice,
    });
    syncNotice(MODS_FOLDER_KEY, n.modsFolder);
    syncNotice(OVERRIDE_KEY, n.override);
    syncNotice(OVERRIDE_FOLLOWED_KEY, n.followed);
  });

  // A cloud-synced mods folder (copies instead of hardlinks). Dismissing it is remembered.
  $effect(() =>
    syncNotice(
      "cloud",
      cloudNoticeOpen
        ? {
            level: "warning",
            icon: "cloud",
            title: "Your mods folder can't use links, so Silo copies your active mods.",
            body:
              "That works, but it takes more disk space and makes turning many mods on or off slower. This usually means the folder is synced by OneDrive, Google Drive or Proton Drive. Leave your mods folder out of the sync and Silo can link them instead.",
            dismissible: true,
            onDismiss: dismissCloudNotice,
          }
        : null,
    ),
  );

  // An interrupted guided bisection: the active set may be partly applied.
  $effect(() => {
    const lock = writeLock;
    const off = { disabled: lock.blocked, title: lock.reason || undefined };
    syncNotice(
      "bisect-recovery",
      bisectRecovery
        ? {
            level: "blocking",
            title: "Find the problem mod was interrupted.",
            body: "Your active mods may still be one of its test sets, not your usual mods.",
            actions: [
              { label: "Restore my mod set", run: restoreBisect, primary: true, ...off },
              { label: "Discard", run: discardBisect, ...off },
              { label: "Later", run: () => void (bisectRecovery = null), ...off },
            ],
            dismissible: false,
          }
        : null,
    );
  });

  /** Scans skipped behind a foreground write collapse into one rescan when it ends. */
  const scanGate = rescanGate((auto) => void runScan(auto));

  /**
   * Rescan the library. While a foreground library write runs, one rescan is queued for when it
   * ends instead (writes rescan once they're done; the queued one covers startup, folder
   * switches and the like). A background op (the archive move) never holds a scan back. A scan
   * doesn't hold the write class itself, so the controls don't flicker off on every focus
   * rescan; the ones that must wait for a scan check `scanning`.
   * @param auto allow auto-filing afterwards
   * @param label names the scan in the status line (a plain rescan only shows the progress bar)
   * @param heldByCaller the caller holds the write class for this scan (rebuild, bisect
   *   restore): don't wait for it
   */
  async function runScan(auto = true, label?: string, heldByCaller = false) {
    if (scanning) return;
    if (!scanGate.admit(auto, heldByCaller)) return; // queued until the write ends
    const op = start(label ?? "Rescanning your library", { quiet: !label });
    scanOp = op;
    scanning = true;
    scansStarted++;
    // Updates installed before this point are on disk for this scan to read (#402).
    const scanFrom = installSeq;
    progress = { done: 0, total: 0 };
    let scanned = false;
    try {
      // Linking older installs to the catalog (#402) asks it about mod names: only when the
      // user lets Silo check for updates at startup.
      const r = await scanMods(roots.length ? roots : undefined, autoCheckEnabled());
      result = r;
      mods = r.mods;
      releaseInstalled(installedUnscanned, scanFrom);
      libraryScanned = true;
      scanFailed = false;
      activeSet = new Set(r.mods.filter((m) => m.active).map((m) => m.techName));
      scanned = true;
      // Interrupted mod updates Silo finished or rolled back before scanning. The scan owns
      // only the `scan` notice: other producers' notices stay up across rescans.
      if (r.notices?.length) info(r.notices.join(" "), { duration: 10_000 });
      syncNotice(
        SCAN_KEY,
        r.problems?.length
          ? {
              level: "warning",
              title: "Silo couldn't finish an interrupted mod update.",
              body: r.problems.join(" "),
              dismissible: true,
            }
          : null,
      );
      // Filesystem-level check (Rust): files at a managed name that aren't Silo's projection.
      void refreshForeignFiles();
      // Privacy: a scan never sends the library to the catalog. Catalog calls happen only
      // when the user asks (Browse, a mod's details, Updates, integrity check), or with the
      // startup update check on, the catalog-link backfill after the scan (#402).
      // NOTE: the update check is NOT auto-run on launch — on a large library the burst of
      // scan + catalog-tags + a whole-library update pass all landing at once could jank the
      // window right after it opened. The "Needs update" toggle runs it on demand instead.
      // Re-read savegames too, so a Rescan picks up a save created since launch (they're
      // otherwise only loaded once on startup). Fire-and-forget; it handles its own errors.
      void loadSavegames();
    } catch (e) {
      // The library wasn't read: a standing condition until a scan works, so a notice.
      scanFailed = true;
      reportError(e, "Scanning your library", { quiet: true });
      const d = describeError(e, "Scanning your library");
      if (!d.silent)
        syncNotice(SCAN_KEY, {
          level: "warning",
          title: d.title,
          body: d.body,
          actions: [{ label: "Try again", run: () => rescanAnnounced(false), primary: true }],
          dismissible: true,
        });
    } finally {
      scanning = false;
      lastScanAt = Date.now();
      op.end();
      scanOp = null;
    }
    // Auto-file any mods still loose in the flat root (e.g. freshly downloaded),
    // keeping them active so filing is transparent to the game.
    // Never auto-file mid-move: library writes are refused until the move finishes. Never after
    // a failed scan either: `mods` and `activeSet` are still the last scan's (#358).
    if (scanned && auto && autoFileNew && !archiveMove.unfinished) {
      if (mods.some(isFileable)) {
        await autoFile();
      }
    }
  }

  onMount(() => {
    let unlisten: (() => void) | undefined;
    let focusUnlisten: (() => void) | undefined;
    let moveUnlisten: (() => void) | undefined;
    let linkedUnlisten: (() => void) | undefined;
    let closeUnlisten: (() => void) | undefined;
    // Suppress the WebView's built-in right-click menu (reload/inspect junk) app-wide,
    // but keep the native cut/copy/paste menu inside editable fields. Our own row menu
    // (ModListRow / ModCard → Menu) still opens; it sets its own handler on the row.
    const onCtx = (e: MouseEvent) => {
      const t = e.target as HTMLElement | null;
      if (t && (t.isContentEditable || t.closest("input, textarea, select"))) return;
      e.preventDefault();
    };
    window.addEventListener("contextmenu", onCtx);
    // Shared-set links (silo://collection?url=…) are heard before any startup work below, so one
    // that arrives while Silo loads isn't lost; a cold launch from a link is read right after.
    // They open Mod sets → Open a shared set with a preview only.
    const deepLinks = listenForSharedSets(openSharedLink, { onOpenUrl, getCurrent });
    (async () => {
      unlisten = await onScanProgress((p) => {
        progress = p;
        scanOp?.progress(p.done, p.total);
      });
      moveUnlisten = await archiveMove.listen();
      linkedUnlisten = await onCatalogLinked(onCatalogLinkedMods).catch(() => undefined);
      try {
        const rows = await getCuration();
        curationSaved.load(rows.map((r) => [r.techName, r] as const));
        curationMap = Object.fromEntries(rows.map((r) => [r.techName, r]));
        const ovs = await getOverrides();
        const ovRows = ovs.map((o) => [o.techName, { category: o.category, subcategory: o.subcategory }] as const);
        overrideSaved.load(ovRows);
        overrideMap = Object.fromEntries(ovRows);
        await loadTags();
        await loadRepos();
        await loadSources();
        await setStore.load();
        await loadSavegames();
        gameInfo = await detectGame();
        userDir = await userDirPath();
        appVer = await appVersion();
        // Fire-and-forget: never let a slow/absent update endpoint delay startup.
        checkUpdate().then((u) => (update = u));
        settingsModsSet = new Set(await modsWithSettings());
      } catch (e) {
        reportError(e, "Starting Silo", { background: true });
      }
      try {
        modsStatus = await modsRootStatus();
        roots = rootsFor(modsStatus);
        // The folder's FS25 profile is known now (#387): read the game's files from it. The crash
        // check below reads its log.
        if (profileInUse(modsStatus)) await loadGameFiles(false);
        if (
          showOverrideNotice(modsStatus, localStorage.getItem("silo.overrideNoticeShown") === "1")
        ) {
          overrideNoticeOpen = true;
        }
      } catch (e) {
        reportError(e, "Reading the mods folder setting", { retry: retryModsFolder, background: true });
      }
      // Where the library archive lives, and any unfinished move (paused before a restart).
      // Awaited when there is a folder to scan, so the first scan's auto-file guard sees a move
      // left paused before a restart. With nothing to scan (the mods-folder status failed or is
      // unavailable), deep links, bisect recovery and the focus listener don't wait on it.
      const archiveReady = archiveMove.load();
      if (roots.length) await archiveReady;
      // Before the first scan: while the setup flow is up, the auto-filer stays off.
      await decideFirstRun();
      try {
        // e.g. the library database was restored from a backup: worth reading, so it stays up
        // until dismissed.
        const notes = await takeStartupNotices();
        if (notes.length)
          setNotice({ key: STARTUP_KEY, level: "info", title: notes.join(" "), dismissible: true });
      } catch {
        /* non-critical */
      }
      if (roots.length) rescanAnnounced();
      // Checking for mod updates at startup is opt-in (Settings > General, offered once in
      // Updates): it sends mod names to the catalog. It runs after the first scan (see
      // startupCheckPending) and only checks; nothing is installed.
      if (roots.length && autoCheckEnabled()) startupCheckPending = true;
      // Detect a cloud-synced mods folder (OneDrive / Google Drive / Proton Drive): those reject
      // hardlinks, so Silo projects the active set as copies. Warn once so the extra disk use and
      // slower bulk changes aren't a mystery — and so "Optimize links" can explain itself.
      if (roots.length) {
        hardlinkSupport()
          .then((ok) => {
            cloudSynced = !ok;
            if (cloudSynced && localStorage.getItem("silo.cloudNoticeDismissed") !== "1") {
              cloudNoticeOpen = true;
            }
          })
          .catch(() => {});
      }
      // A leftover snapshot means a bisection was interrupted — offer to restore.
      try {
        bisectRecovery = await bisectSnapshotGet();
      } catch {
        /* non-critical */
      }
      // Did the game crash since Silo last looked (e.g. Silo was closed meanwhile)?
      void crashWatch.check();
      // Auto-refresh on window focus: a mod downloaded in a browser lands while Silo is in the
      // background, so pick it up when the user returns instead of waiting for a manual rescan.
      // Throttled (skip during a library write or scan, or scanned <4s ago) so alt-tabbing doesn't thrash; the
      // scan cache (path+mtime+size) makes an unchanged rescan cheap anyway.
      try {
        focusUnlisten = await getCurrentWindow().onFocusChanged(({ payload: focused }) => {
          // Back from the game (or anywhere): did the last run crash? A cheap stat first.
          if (focused) void crashWatch.check();
          if (
            focused &&
            focusRescanAllowed({
              scanning,
              writeBlocked: writeLock.blocked,
              moveRunning: archiveMove.running,
              bisecting,
              switchingFolder: folderSwitches > 0,
              hasRoots: roots.length > 0,
              sinceLastScanMs: Date.now() - lastScanAt,
            })
          ) {
            void runScan(true);
          }
        });
      } catch {
        /* not a desktop window context */
      }
      // Closing while Silo is changing files is held by the backend; ask before quitting.
      try {
        // One question at a time: a second close while the dialog is up is ignored.
        let asking = false;
        closeUnlisten = await onCloseBlocked(async () => {
          if (asking) return;
          asking = true;
          try {
            const quit = await confirmDialog(
              "Silo is still changing files. Quitting now could leave a mod half-moved. Quit anyway?",
              { okLabel: "Quit anyway", cancelLabel: "Keep Silo open" },
            );
            if (quit) await forceQuit();
          } finally {
            asking = false;
          }
        });
      } catch {
        /* not a desktop window context */
      }
    })();
    return () => {
      unlisten?.();
      void deepLinks.then((stop) => stop());
      focusUnlisten?.();
      moveUnlisten?.();
      linkedUnlisten?.();
      closeUnlisten?.();
      window.removeEventListener("contextmenu", onCtx);
    };
  });

  async function restoreBisect() {
    if (!bisectRecovery) return;
    const op = beginWrite("Restoring your mod set");
    if (!op) return;
    // Held through the rescan, so the library shows the restored set before anything else runs.
    try {
      let restored = false;
      try {
        // Clears even if the report lists errors (applyActive shows them): this banner is the
        // escape hatch, and keeping the snapshot would block every new bisection for good.
        await applyActive(bisectRecovery);
        await bisectSnapshotClear();
        bisectRecovery = null;
        restored = true;
      } catch (e) {
        reportError(e, "Restoring your mod set", { retry: restoreBisect });
      }
      if (restored) await runScan(false, undefined, true);
    } finally {
      op.end();
    }
  }

  /** Forget an interrupted bisection without restoring it (asked first: it can't be undone). */
  async function discardBisect() {
    if (!bisectRecovery) return;
    const ok = await ask({
      title: "Forget the interrupted search for the problem mod?",
      body: "Your active mods stay as they are now, and Silo won't offer to restore the earlier set again. This can't be undone.",
      confirm: "Forget it",
      cancel: "Keep it",
      tone: "danger",
    });
    if (!ok) return;
    try {
      await bisectSnapshotClear();
      bisectRecovery = null;
    } catch (e) {
      reportError(e, "Forgetting the interrupted search for the problem mod");
    }
  }

  // ── App shell: header search, status chip, Play, the Library strip and menu (#230) ──

  // A Silo update that's ready: a notice (it was a top-bar button) driven by the same state, with
  // the same action in Settings → About. Dismissed, it stays down until the next launch.
  let updateNoticeDismissed = $state(false);
  $effect(() => {
    syncNotice(
      "self-update",
      update && !updateNoticeDismissed
        ? {
            level: "info",
            icon: "update",
            // A ready update is never folded under "+N more".
            pinned: true,
            title: `Silo ${update.version} is ready`,
            body: updating ? (updatePct != null ? `Updating… ${updatePct}%` : "Updating…") : undefined,
            actions: [
              {
                label: "Restart to update",
                run: applyUpdate,
                primary: true,
                disabled: updating,
                title: updating ? "Silo is updating." : `Download and install Silo ${update.version}, then restart`,
              },
            ],
            dismissible: true,
            onDismiss: () => (updateNoticeDismissed = true),
          }
        : null,
    );
  });

  /** Browse's catalog search text (the header search while Browse is open). */
  let browseQuery = $state("");
  /** The header search shows the current view's search: the catalog on Browse, else the library's. */
  function getHeaderSearch(): string {
    return searchTargetFor(nav.view) === "browse" ? browseQuery : query;
  }
  function setHeaderSearch(v: string) {
    if (searchTargetFor(nav.view) === "browse") browseQuery = v;
    else query = v;
  }
  /** Enter away from Library and Browse takes the search to the Library (the text is applied). */
  function submitHeaderSearch(v: string) {
    if (searchTargetFor(nav.view) !== "library") return;
    query = v;
    go("library");
  }

  // Problems: critical conflicts and active mods that can't load. To review: warning-level
  // conflicts, files Silo left untouched and inactive mods that couldn't load. Info-level
  // findings never count.
  const statusCounts = $derived.by(() => {
    const on = (m: ModEntry) => activeSet.has(m.techName);
    const deps = health.missingDeps.map((d) => d.mod);
    const activeOf = (list: ModEntry[]) => list.filter(on).length;
    const all = deps.length + health.corrupt.length + health.ignored.length;
    const missingDeps = activeOf(deps);
    const corrupt = activeOf(health.corrupt);
    const ignored = activeOf(health.ignored);
    return libraryStatusCounts({
      crash: lastCrash ? 1 : 0,
      criticalConflicts: criticalCount,
      warningConflicts: conflicts.filter((c) => c.severity === "warning").length,
      missingDeps,
      corrupt,
      ignored,
      inactiveFindings: all - missingDeps - corrupt - ignored,
      foreign: health.foreign.length,
    });
  });
  // The chip's counts only mean something once a scan of this folder has succeeded.
  const libraryRead = $derived(libraryReadState({ scanning, scanned: libraryScanned, failed: scanFailed }));

  /** Which mod set the active mods are on ("Harvest 2026", or Custom since it). */
  const setState = $derived(setStore.state);

  // Play keeps the old Launch button's guards: off during a foreground library write or while an
  // archive move is copying (launching would race it).
  const playOffReason = $derived(
    archiveMove.running ? "A library move is running. Pause it to play." : fgLock.reason,
  );

  /** Saves (already loaded) that use mods the library doesn't have. */
  const savesMissingMods = $derived(saveRows.filter((r) => r.st.missing.length > 0));
  const playMenuItems = $derived.by((): PlayMenuItem[] => {
    const items: PlayMenuItem[] = [];
    if (gameInfo) items.push({ id: "game-folder", label: "Open the game folder", icon: "showFile" });
    savesMissingMods.slice(0, 3).forEach(({ s, st }, i) => {
      const n = st.missing.length;
      items.push({
        id: `save:${s.folder}`,
        label: `${s.name} uses ${modsCount(n)} that ${n === 1 ? "isn't" : "aren't"} in your library`,
        icon: "problem",
        separatorBefore: i === 0 && items.length > 0,
      });
    });
    // Custom since a set: keep the change in that set, or save it as a new one.
    if (setState.kind === "custom") {
      // An automatic set is pruned later, so edits only go into a set the user saved.
      if (setStore.current?.kind === "user")
        items.push({
          id: "set-update",
          label: `Update ${setState.name} from current mods`,
          icon: "rescan",
          disabled: !!fgLock.reason,
          separatorBefore: items.length > 0,
        });
      items.push({
        id: "set-save",
        label: "Save current mods as a new set",
        icon: "modSets",
        disabled: !!fgLock.reason,
        separatorBefore: setStore.current?.kind !== "user" && items.length > 0,
      });
    }
    items.push({
      id: "mod-sets",
      label: "Manage mod sets",
      icon: "modSets",
      separatorBefore: items.length > 0 && setState.kind !== "custom",
    });
    return items;
  });
  function playMenuSelect(id: string) {
    if (id === "game-folder")
      openKnownFolder("gameInstall").catch((e) => reportError(e, "Opening the game folder"));
    else if (id.startsWith("save:")) go("modSets", "savegames");
    else if (id === "mod-sets") go("modSets", "sets");
    else if (id === "set-update") {
      const set = setStore.current;
      if (set) void setStore.updateFromCurrent(set);
    } else if (id === "set-save") void setStore.saveCurrent();
  }

  /** Why the old "Organize N" button was off: a library write, a scan, or an unfinished move. */
  const organizeOffReason = $derived(
    fgLock.reason ||
      (scanning ? SCAN_WAIT : "") ||
      (archiveMove.unfinished ? "Finish or cancel the library move first." : ""),
  );
  const libraryStripItems = $derived.by((): StatusStripItem[] => {
    const run: Record<string, () => void> = {
      crash: () => go("diagnose", "lastRun"),
      critical: () => go("diagnose", "conflicts"),
      unfiled: () => (organizeOffReason ? refuse(organizeOffReason) : void organizeNew()),
      updates: () => go("updates"),
    };
    return stripItemSpecs({
      crash: !!lastCrash,
      criticalConflicts: criticalCount,
      // Where the old "Organize N" button showed: loose mods, with auto-file off.
      unfiled: autoFileNew ? 0 : unorganizedCount,
      updates: availableUpdates.length,
    }).map((it) => ({ ...it, run: run[it.id] }));
  });
  const libraryStripLeft = $derived(
    stripLeft({
      scanning,
      scanned: libraryScanned,
      // "Ready to play" only when Play would work: the game is found, Play isn't off and no
      // critical conflict would make it ask first.
      ready: !!gameInfo && !playOffReason && criticalCount === 0,
      active: activeSet.size,
      total: mods.length,
      setName: stripSetName(setState),
    }),
  );

  // The Library's "More" menu: the old Rescan button and Settings' rebuild, with their guards.
  const libraryMenuItems = $derived<MenuItem[]>([
    {
      id: "rescan",
      label: scanning ? "Scanning…" : "Rescan",
      icon: "rescan",
      disabled: scanning || !!fgLock.reason,
      shortcut: "F5",
      keyshortcuts: "F5",
    },
    { id: "rebuild", label: "Re-read every mod", disabled: !!fgLock.reason || scanning },
  ]);
  function libraryMenuSelect(id: string) {
    if (id === "rescan") rescanAnnounced();
    else if (id === "rebuild") rebuildLibrary();
  }

  // ── Keyboard shortcuts (lib/state/shortcuts): one window listener, each key runs the handler
  // its button would. F5 / Ctrl+R take the Library ⋯ Rescan path with the same guard as the menu
  // item (off while scanning or during a foreground change), and never reload the window.
  let appHeader = $state<AppHeader | null>(null);
  let shortcutsOpen = $state(false);

  function runShortcut(a: ShortcutAction) {
    switch (a.kind) {
      case "focusSearch":
        appHeader?.focusSearch();
        break;
      case "rescan": {
        const item = libraryMenuItems.find((i) => i.id === "rescan");
        if (item && !item.disabled) libraryMenuSelect("rescan");
        break;
      }
      case "settings":
        go("settings");
        break;
      case "view":
        go(a.view);
        break;
      case "back":
        back();
        break;
      case "region":
        cycleRegion(document, a.dir, document.activeElement);
        break;
      case "help":
        shortcutsOpen = true;
        break;
    }
  }

  // Shortcuts stand down behind a dialog and behind the overlays App draws itself (they aren't on
  // the overlay stack): the Organize preview, a settings editor, a mod's settings, the row
  // context menu and the category menu. Reload keys are still swallowed there, without a rescan.
  $effect(() =>
    installShortcuts(window, runShortcut, () =>
      shortcutsBlocked(get(modalOpen), [organizePreview, configEditor, settingsMod, rowMenu, editing]),
    ),
  );

  // The mod drawer hands focus back when it closes: to the row of the mod it showed, else to what
  // had focus in the main area when it opened, else to the main area (drawer/drawerFocus).
  returnFocusOnDrawerClose({
    open: () => !!drawerMod && nav.view === "library",
    key: () => (drawerMod ? modKey(drawerMod) : null), // follows the shown mod (Find in Library can swap it)
    focusKey: (k) => libList?.focusKey(k) ?? false,
    keepOpener: (el) => regionOf(el) === "main",
    fallback: () => document.getElementById("main"),
  });
</script>

{#snippet modDrawer()}
  {#if drawerMod}
    {@const dm = drawerMod}
    {@const fgn = foreignFiles.find((f) => f.techName === dm.techName) ?? null}
    <ViewBoundary name="the mod details" onClose={() => (detailMod = null)}>
      <ModDetail
        mod={dm}
        curation={cur(dm.techName)}
        tags={tagsOf(dm.techName)}
        active={activeSet.has(dm.techName)}
        organized={dm.organized}
        hasSettings={settingsModsSet.has(dm.techName)}
        hasUpdate={updatesSet.has(dm.techName)}
        foreign={fgn}
        onAdoptForeign={async () => {
          if (fgn && (await resolveForeign(fgn.fileName, "adopt"))) detailMod = null;
        }}
        onRestoreForeign={async () => {
          if (fgn && (await resolveForeign(fgn.fileName, "restore"))) detailMod = null;
        }}
        {libraryTechNames}
        {conflicts}
        categories={CATEGORIES}
        isOverridden={overrideMap[dm.techName] != null}
        onSetCategory={(c, s) => setCategory(dm.techName, c, s)}
        onResetCategory={() => resetCategory(dm.techName)}
        onClose={() => (detailMod = null)}
        onToggle={(flag) => toggleCuration(dm.techName, flag)}
        onToggleActive={() => toggleActive(dm.techName)}
        onOpenSettings={() =>
          (settingsMod = { techName: dm.techName, title: dm.title ?? dm.techName })}
        onFindInBrowse={() => openInBrowse(dm)}
        onCurationChange={(row) => (curationMap = { ...curationMap, [row.techName]: row })}
        saveCuration={saveCurationRow}
        onTagsChange={(t) => (tagMap = { ...tagMap, [dm.techName]: t })}
        onFilterTag={(t) => {
          libFilters = { ...libFilters, tag: t };
          detailMod = null;
        }}
        repo={repoMap[dm.techName] ?? null}
        onRepoChange={(r) => {
          const next = { ...repoMap };
          if (r) next[dm.techName] = r;
          else delete next[dm.techName];
          repoMap = next;
        }}
        source={sourceMap[dm.techName] ?? null}
        onSourceChange={(row) => setSource(dm.techName, row)}
        onInstalled={() => {
          detailMod = null;
          runScan(false);
        }}
      />
    </ViewBoundary>
  {/if}
{/snippet}

<AppShell
  badges={{ updates: updatesBadge(updateSections, updateCheckedAt != null), diagnose: bisecting ? BISECT_BADGE : undefined }}
  fill={nav.view === "library" || nav.view === "browse"}
  wide={nav.view === "library" || nav.view === "browse"}
  drawer={drawerMod && nav.view === "library" ? modDrawer : undefined}
  drawerLabel={drawerMod ? `Mod details: ${drawerMod.title ?? drawerMod.techName}` : "Mod details"}
>
  {#snippet header()}
    <AppHeader
      bind:this={appHeader}
      bind:search={getHeaderSearch, setHeaderSearch}
      searchPlaceholder={searchPlaceholderFor(nav.view)}
      onsearchsubmit={submitHeaderSearch}
    >
      {#snippet status()}
        {#if bisecting}
          <BisectStatus round={bisectRoundNo} onopen={() => go("diagnose", "lastRun")} />
        {:else}
          <StatusChip
            problems={statusCounts.problems}
            review={statusCounts.review}
            library={libraryRead}
            onclick={() => go("diagnose")}
          />
        {/if}
      {/snippet}
      {#snippet play()}
        {#if gameInfo}
          <PlayButton
            count={activeSet.size}
            disabled={!!playOffReason}
            disabledReason={playOffReason}
            onplay={launch}
            label={testRound ? "Launch test round" : "Play"}
            menuItems={playMenuItems}
            onmenuselect={playMenuSelect}
            activeSetLine={activeSetLine(setState) +
              (profileInUse(modsStatus) ? `. Starts with the FS25 profile in ${profileInUse(modsStatus)}` : "")}
          />
        {/if}
      {/snippet}
    </AppHeader>
  {/snippet}

  {#snippet notices()}
    {#if scanning}
      <div
        class="progress"
        role="progressbar"
        aria-label="Scanning mods"
        aria-valuemin={0}
        aria-valuemax={progress.total || undefined}
        aria-valuenow={progress.total ? progress.done : undefined}
      >
        <div class="bar" style="width: {pct}%"></div>
        <span class="progress-text tnum" aria-hidden="true">{progress.done} / {progress.total}</span>
      </div>
    {/if}
    {#if statusOp}
      <div class="busy" role="status">
        {statusOp.label}…{#if statusOp.progress && statusOp.progress.total > 0}
          <span class="tnum">{statusOp.progress.done}/{statusOp.progress.total}</span>{/if}
      </div>
    {/if}
    <NoticeArea />
  {/snippet}

  <ViewOutlet view={nav.view}>
    {#snippet library()}
      <LibraryView
        stripLeft={libraryStripLeft}
        stripItems={libraryStripItems}
        menuItems={libraryMenuItems}
        onmenuselect={libraryMenuSelect}
      >
        {#snippet filters()}
          <FilterBar
            catTree={libCatTree}
            bind:parentCategory={libCat}
            bind:childCategory={libSub}
            showSearch={false}
            bind:sort={sortBy}
            sortOptions={[
              { value: "added", label: "Recently added" },
              { value: "name", label: "Name" },
              { value: "category", label: "Category" },
              { value: "size", label: "Size" },
              { value: "version", label: "Version" },
              { value: "rating", label: "My rating" },
            ]}
            bind:sortDir
            dirEnabled={true}
            showYear={false}
            showClear={false}
          />

          {#if selection.size > 0}
            <BulkBar
              bind:ref={bulkBarEl}
              count={selection.size}
              shownCount={visible.length}
              allShownSelected={selection.size === visible.length}
              activeCount={selectedMods.filter((m) => activeSet.has(m.techName)).length}
              allFavorite={selectedMods.every((m) => cur(m.techName).favorite)}
              allHidden={selectedMods.every((m) => cur(m.techName).hidden)}
              allSkipUpdates={selectedMods.every((m) => cur(m.techName).skipUpdates)}
              writeReason={writeLock.reason}
              onturnon={() => setActiveForMods(selectedMods, true)}
              onpark={() => setActiveForMods(selectedMods, false)}
              onfavorite={(on) => setCurationFor(selectedMods, "favorite", on)}
              onhide={(on) => setCurationFor(selectedMods, "hidden", on)}
              onskipupdates={(on) => setCurationFor(selectedMods, "skipUpdates", on)}
              onmodsets={() => openMembership(selectedMods, true)}
              onselectall={() => selection.selectAll(visibleKeys)}
              onclear={() => selection.clear()}
            />
          {:else}
            <LibraryToolbar
              bind:this={libToolbar}
              filters={libFilters}
              onchange={(f) => (libFilters = f)}
              onneedsupdate={(on) => toggleNeedsUpdate(on)}
              {updateChecking}
              tags={allTags}
              counts={{
                attention: attentionCount,
                conflicted: conflictedSet.size,
                needsUpdate: availableUpdates.length || undefined,
                maps: stats.maps,
                scripts: stats.scripts,
                unique: stats.unique,
              }}
              shownCount={visible.length}
              bind:libView
            />
          {/if}
          <ActiveFilters
            chips={libChips}
            onremove={removeLibFilter}
            onclear={() => clearLibFilters()}
            onemptied={() => {
              if (!libToolbar?.focusFilters()) libList?.focusCurrent();
            }}
          />
        {/snippet}
        {#snippet list()}
          <!-- svelte-ignore a11y_no_static_element_interactions -->
          <div class="list-body" bind:this={listBodyEl} onkeydown={onListKeydown}>
            {#if mods.length === 0 && !scanning}
              <EmptyState icon="library" title="No mods in this folder yet." level={3}>
                {#snippet actions()}
                  <Button variant="primary" icon="browse" onclick={() => go("browse")}>Browse mods</Button>
                  <Button icon="folder" onclick={openModsFolder}>Open mods folder</Button>
                  <Button onclick={changeModsFolder}>Choose another folder</Button>
                {/snippet}
              </EmptyState>
            {:else if filtered.length === 0 && !scanning}
              <EmptyState icon="search" title="No mods match these filters." level={3}>
                {#snippet actions()}
                  <Button onclick={() => clearLibFilters(true)}>Clear filters</Button>
                {/snippet}
              </EmptyState>
            {:else if libView === "cards"}
              <VirtualGrid
                bind:this={libList}
                items={visible}
                getKey={modKey}
                itemLabel={rowLabel}
                itemHeight={CARD_HEIGHT}
                bottomPad={16}
                label="Mods"
                onopen={(mod) => (detailMod = mod)}
                ontoggle={(mod) => toggleActive(mod.techName)}
                onselect={(mod, intent) => selectMod(modKey(mod), intent)}
                onmenu={(mod, anchor) => openRowMenu(mod, { anchor })}
                onselectall={() => selection.selectAll(visibleKeys)}
              >
                {#snippet item(mod)}
                  {@const c = cur(mod.techName)}
                  <ModCard
                    {mod}
                    active={activeSet.has(mod.techName)}
                    badges={badgesFor(mod)}
                    favorite={c.favorite}
                    flagged={c.broken}
                    rating={c.rating}
                    menuOpen={rowMenuOpen && rowMenu?.mod.path === mod.path}
                    selected={selection.has(modKey(mod))}
                    selectionMode={selection.size > 0}
                    onToggle={() => toggleActive(mod.techName)}
                    onOpen={() => (detailMod = mod)}
                    onMenu={(at) => openRowMenu(mod, at)}
                    onSelect={selectMod}
                  />
                {/snippet}
              </VirtualGrid>
            {:else}
              <VirtualList
                bind:this={libList}
                items={visible}
                getKey={modKey}
                itemLabel={rowLabel}
                rowHeight={ROW_HEIGHTS[libDensity]}
                bottomPad={16}
                label="Mods"
                onopen={(mod) => (detailMod = mod)}
                ontoggle={(mod) => toggleActive(mod.techName)}
                onselect={(mod, intent) => selectMod(modKey(mod), intent)}
                onmenu={(mod, anchor) => openRowMenu(mod, { anchor })}
                onselectall={() => selection.selectAll(visibleKeys)}
              >
                {#snippet row(mod)}
                  {@const c = cur(mod.techName)}
                  <ModListRow
                    {mod}
                    active={activeSet.has(mod.techName)}
                    badges={badgesFor(mod)}
                    density={libDensity}
                    favorite={c.favorite}
                    flagged={c.broken}
                    menuOpen={rowMenuOpen && rowMenu?.mod.path === mod.path}
                    selected={selection.has(modKey(mod))}
                    selectionMode={selection.size > 0}
                    onToggle={() => toggleActive(mod.techName)}
                    onOpen={() => (detailMod = mod)}
                    onMenu={(at) => openRowMenu(mod, at)}
                    onSelect={selectMod}
                  />
                {/snippet}
              </VirtualList>
            {/if}
          </div>

        {/snippet}
      </LibraryView>
    {/snippet}
    {#snippet browse()}
      <BrowseView>
        <ModBrowser
          installed={libraryTechNames}
          updates={updatesSet}
          onInstalled={() => runScan(true)}
          onShowUpdates={() => go("updates")}
          onNeedAuth={() => go("settings", "accounts")}
          onFindInLibrary={openInLibrary}
          seed={browseSeed}
          seedTech={browseSeedTech}
          onSeedConsumed={() => {
            browseSeed = null;
            browseSeedTech = null;
          }}
          showTitle={false}
          bind:query={browseQuery}
        />
      </BrowseView>
    {/snippet}
    {#snippet modSets()}
      <ModSetsView>
        {#snippet setsTab()}
          <ViewBoundary name="My sets">
            <MySetsTab
              sets={setStore.sets}
              active={activeSet}
              library={libraryScanned ? libraryTechNames : undefined}
              state={setState}
              lockReason={fgLock.reason}
              onSwitch={setStore.switchTo}
              onUpdate={setStore.updateFromCurrent}
              onDelete={setStore.remove}
              onShare={openShare}
              onSaveCurrent={setStore.saveCurrent}
              onOpenShared={() => go("modSets", "open")}
            >
              {#snippet after()}
                <SharedLinks active={activeModRefs} refreshKey={sharedLinksKey} />
              {/snippet}
            </MySetsTab>
          </ViewBoundary>
        {/snippet}
        {#snippet openTab()}
          <ViewBoundary name="Open a shared set">
            <OpenSharedTab
              library={mods}
              active={activeSet}
              activeRefs={activeModRefs}
              request={sharedRequest}
              onSaveAsSet={setStore.saveShared}
              onSwitchTo={(id) => {
                const set = setStore.sets.find((l) => l.id === id);
                if (set) void setStore.switchTo(set);
              }}
              onImported={() => {
                void setStore.load();
                void runScan(false);
              }}
              onTurnOn={(t) => setModActive(t, true)}
              onPark={(t) => setModActive(t, false)}
              onFindInBrowse={(t) => findInBrowse(t, mods.find((m) => m.techName === t)?.title ?? null)}
            />
          </ViewBoundary>
        {/snippet}
        {#snippet savegamesTab()}
          <ViewBoundary name="Savegames">
            <SavegamesTab
              rows={saveRows}
              libraryRead={libraryScanned}
              lockReason={fgLock.reason}
              onUse={setStore.useSave}
              onBackup={backupSave}
              onSettings={openSaveConfig}
              onFindInBrowse={findInBrowse}
            />
          </ViewBoundary>
        {/snippet}
      </ModSetsView>
      {#if shareTarget}
        <ShareDialog
          bind:open={shareOpen}
          name={shareTarget.name}
          mods={shareTarget.mods}
          library={mods}
          onConnect={() => go("settings", "accounts")}
          onShared={() => (sharedLinksKey += 1)}
        />
      {/if}
    {/snippet}
    {#snippet diagnose()}
      <!-- Last run's content is the KeepMounted below, docked into this tab's slot. -->
      <DiagnoseView
        conflictsBadge={conflicts.filter((c) => c.severity !== "info").length}
        problemsBadge={healthCount}
        bind:lastRunSlot={diagnoseLastRunSlot}
      >
        {#snippet conflictsTab()}
          <ViewBoundary name="Conflicts" bodyText="Your mods weren't changed.">
            <ConflictsPanel
              {conflicts}
              onOpen={openInLibrary}
              onPark={(t) => void parkMod(t)}
              lockReason={fgReason}
              activeCount={activeSet.size}
            />
          </ViewBoundary>
        {/snippet}
        {#snippet problemsTab()}
          <ViewBoundary name="Problems">
            <ProblemsPanel
              {health}
              isActive={(t) => activeSet.has(t)}
              onFindInBrowse={(t) => findInBrowse(t)}
              onShowFile={(path) => revealInFolder(path).catch((e) => reportError(e, "Showing the file"))}
              onPark={(t) => void parkMod(t)}
              onResolveForeign={resolveForeign}
              lockReason={fgLock.reason}
            />
          </ViewBoundary>
        {/snippet}
      </DiagnoseView>
    {/snippet}
    {#snippet updates()}
      <UpdatesView
        checking={updateChecking}
        checkedAt={updateCheckedAt}
        progress={updateProgress}
        checkDisabled={!!fgLock.reason || !!updateCheckBlocked || updateChecking || mods.length === 0}
        checkDisabledReason={fgLock.reason ||
          updateCheckBlocked ||
          (mods.length === 0 ? "No mods to check yet." : "")}
        catalogError={!!updateCatalogError}
        oncheck={() => checkAllUpdates()}
      >
        <UpdatesPanel
          sections={updateSections}
          checked={updateCheckedAt != null}
          checking={updateChecking}
          catalogError={updateCatalogError}
          rowStates={updateRowStates}
          {updatingAll}
          lockReason={updateInstallLock}
          checkDisabledReason={fgLock.reason ||
            updateCheckBlocked ||
            (mods.length === 0 ? "No mods to check yet." : "")}
          hasLinkedRepos={linkedCount > 0}
          foreign={foreignFiles}
          onCheck={() => checkAllUpdates()}
          onInstall={installFromRow}
          onUpdateAll={updateAllReady}
          onOpenPage={(r) => r.pageUrl && openExternal(r.pageUrl)}
          onUseDropped={(f) => void resolveForeign(f.fileName, "adopt")}
          onOpenSource={(c) => openExternal(c.url)}
          onMarkChecked={markChecked}
          onRecheck={recheckUpdate}
          onCopyDetails={(t) => void copyDetailsText(t)}
          onCheckAgain={(techName) => void toggleCuration(techName, "skipUpdates")}
        />
      </UpdatesView>
    {/snippet}
    {#snippet tools()}
      <ToolsView>
        {#snippet statsTab()}
          <ViewBoundary name="Library stats" bodyText="Your mods weren't changed.">
            <StatsPanel
              modCount={mods.length}
              {organizedCount}
              activeCount={activeSet.size}
              mapsCount={stats.maps}
              {libStats}
            />
          </ViewBoundary>
        {/snippet}
        {#snippet controlsTab()}
          <ViewBoundary name="Bindings" bodyText="Your mods weren't changed.">
            <div class="page-card"><BindingsView /></div>
          </ViewBoundary>
        {/snippet}
        {#snippet bridgeTab()}
          <ViewBoundary name="the filltype bridge">
            <div class="page-card"><BridgeTool onGenerated={() => runScan(true)} /></div>
          </ViewBoundary>
        {/snippet}
        {#snippet graphicsTab()}
          <div class="tool-note">
            <p>
              Tune Farming Simulator 25's graphics settings (presets: Performance / Balanced /
              Quality) without launching the game. Changes take effect the next time you launch it.
            </p>
            {#if userDir}
              <Button icon="modSettings" onclick={openGameGraphics}>Edit graphics settings</Button>
            {:else}
              <p class="muted">Silo couldn't find your Farming Simulator 25 user folder.</p>
            {/if}
          </div>
        {/snippet}
      </ToolsView>
    {/snippet}
    {#snippet settings()}
      <SettingsView>
        {#snippet generalTab()}
          <GeneralSettings
            {theme}
            {uiScale}
            {userDir}
            density={libDensity}
            onSetTheme={setTheme}
            onSetUiScale={setUiScale}
            onSetDensity={setLibDensity}
            onOpenGameGraphics={openGameGraphics}
          />
        {/snippet}
        {#snippet modsFolderTab()}
          <ModsFolderSettings
            {roots}
            {modsStatus}
            {gameInfo}
            lockReason={fgLock.reason}
            {scanning}
            moveUnfinished={archiveMove.unfinished}
            onChangeModsFolder={changeModsFolder}
            onUseGameFolder={useGameFolder}
            onSwitchModsFolder={switchModsFolder}
            onSetGameProfile={setGameProfileOn}
          />
        {/snippet}
        {#snippet libraryTab()}
          <LibrarySettings
            {autoFileNew}
            {organizedCount}
            {unorganizedCount}
            lockReason={fgLock.reason}
            {scanning}
            archive={archiveMove.archive}
            moveUnfinished={archiveMove.unfinished}
            archiveLoadFailed={archiveMove.loadFailed}
            onSetAutoFile={setAutoFile}
            onOrganize={organizeNew}
            onChangeArchive={changeArchive}
            onArchiveDefault={archiveDefault}
          />
        {/snippet}
        {#snippet accountsTab()}
          <AccountSettings />
        {/snippet}
        {#snippet diagnosticsTab()}
          <DiagnosticsSettings onCopyDiagnostics={copyDiagnostics} onExportReport={exportReport} />
        {/snippet}
        {#snippet maintenanceTab()}
          <MaintenanceSettings
            {organizedCount}
            {cloudSynced}
            lockReason={fgLock.reason}
            {scanning}
            moveUnfinished={archiveMove.unfinished}
            onRebuild={rebuildLibrary}
            onOptimizeLinks={optimizeLinks}
            onRestoreVanilla={restoreVanilla}
          />
        {/snippet}
        {#snippet aboutTab()}
          <AboutSettings
            {appVer}
            scanMs={result ? result.tookMs : null}
            {update}
            {updating}
            {updatePct}
            onApplyUpdate={applyUpdate}
          />
        {/snippet}
      </SettingsView>
    {/snippet}
    {#snippet help()}
      <HelpView
        onopen={(url) => openExternal(url).catch((e) => reportError(e, "Opening the link"))}
        oncopydiagnostics={copyDiagnostics}
        onshowshortcuts={() => (shortcutsOpen = true)}
        onshowwhatsnew={() => (whatsNewOpen = true)}
        {appVer}
      />
    {/snippet}
  </ViewOutlet>
  <!-- Diagnose → Last run (log triage and guided bisection). Mounted with its tab; while a
       bisection runs it stays mounted (hidden) when you switch tabs or views, so the run isn't
       cut short, and comes back where it was. -->
  <KeepMounted target={diagnoseLastRunSlot} keep={bisecting}>
    <ViewBoundary
      name="Last run"
      bodyText="Your mods are as they were before this view failed. If Find the problem mod was running, Silo keeps your place."
    >
      <div class="page-card">
        <LogTriage
          gameProfile={profileInUse(modsStatus)}
          activeMods={[...activeSet]}
          activeMaps={mods.filter((m) => activeSet.has(m.techName) && m.isMap).map((m) => m.techName)}
          activeFixed={mods.filter((m) => activeSet.has(m.techName) && !m.organized && m.kind !== "zip").map((m) => m.techName)}
          onBisectPrepare={prepareBisect}
          onApply={bisectRound}
          onBisecting={(v) => void bisectingChanged(v)}
          recoveryPending={bisectRecovery !== null}
          lookup={libraryLookup}
          onPark={(t) => void parkMod(t)}
          onShow={openInLibrary}
          onBisectProgress={(p) => {
            bisectRoundNo = p.round;
            bisectLaunching = p.launching;
          }}
          lockReason={fgReason}
          onBisectFinished={(parked) => {
            lastCrash = null; // the search settled the crash it was started for
            if (parked) success(`Your mods are back, and ${libraryLookup(parked)?.title ?? parked} stays parked. Play again to check.`);
            else success("Your mods are back as they were.");
          }}
          {bisectRequested}
          onBisectRequestTaken={() => (bisectRequested = false)}
          refreshKey={logReadKey}
        />
      </div>
    </ViewBoundary>
  </KeepMounted>
</AppShell>

  {#if settingsMod}
    <ViewBoundary name="Mod settings" overlay onClose={() => (settingsMod = null)}>
      <ModSettings
        modName={settingsMod.techName}
        title={settingsMod.title}
        onClose={() => (settingsMod = null)}
      />
    </ViewBoundary>
  {/if}

  {#if configEditor}
    <ViewBoundary name="the settings editor" overlay onClose={() => (configEditor = null)}>
      <ConfigEditor
        title={configEditor.title}
        path={configEditor.path}
        fields={configEditor.fields}
        presets={configEditor.presets}
        footnote={configEditor.footnote}
        onClose={() => (configEditor = null)}
      />
    </ViewBoundary>
  {/if}

  {#if organizePreview}
    <ViewBoundary name="the Organize dialog" overlay onClose={() => (organizePreview = null)}>
      <OrganizePreview
        plan={organizePreview.plan}
        loading={organizePreview.loading}
        applying={organizePreview.applying}
        progress={organizePreview.progress ?? null}
        failures={organizePreview.failures ?? []}
        filed={organizePreview.filed ?? null}
        problem={organizePreview.problem ?? ""}
        blockedReason={organizeOffReason}
        {cloudSynced}
        activeCount={activeSet.size}
        folderMods={mods.filter((m) => !m.organized && m.kind !== "zip").length}
        autoFile={autoFileNew}
        adopt={organizePreview.adopt ?? null}
        onConfirm={confirmOrganize}
        onCancel={() => (organizePreview = null)}
        onCopyDetails={(t) => void copyDetailsText(t)}
      />
    </ViewBoundary>
  {/if}

  {#if firstRun === "onboarding"}
    <ViewBoundary name="Setup" overlay onClose={() => (firstRun = null)}>
      <Onboarding
        {modsStatus}
        gameFound={!!gameInfo}
        {cloudSynced}
        {scanning}
        scanned={libraryScanned}
        {scanFailed}
        onRescan={() => rescanAnnounced(false)}
        {progress}
        autoFileOn={autoFileNew}
        modCount={mods.length}
        activeCount={activeSet.size}
        folderMods={mods.filter((m) => !m.organized && m.kind !== "zip").length}
        archive={archiveMove.archive}
        sep={pathSep}
        busy={fgLock.reason}
        organizeBlocked={organizeOffReason}
        onChooseModsFolder={changeModsFolder}
        onCreateModsFolder={makeModsFolder}
        onUseGameFolder={useGameFolder}
        onRetry={retryModsFolder}
        onPointGameHere={pointGameHere}
        onChooseArchive={chooseArchiveFirstRun}
        onUseDefaultArchive={useDefaultArchiveFirstRun}
        onPlan={planFirstRun}
        onOrganize={organizeFirstRun}
        onCopyDetails={(t) => void copyDetailsText(t)}
        onfinish={finishOnboarding}
      />
    </ViewBoundary>
  {/if}

  {#if editing}
    {@const ed = editing}
    <Menu
      bind:open={editingOpen}
      anchor={ed.anchor}
      point={ed.anchor ? null : { x: ed.x, y: ed.y }}
      placement="bottom-end"
      items={categoryMenuItems(ed.techName)}
      label="Category for {modTitle(ed.techName)}"
      onselect={(id) =>
        id === CATEGORY_RESET ? resetCategory(ed.techName) : setCategory(ed.techName, id)}
    />
  {/if}

  {#if membership}
    {@const st = setStore.state}
    <SetMembershipDialog
      bind:open={membershipOpen}
      mods={membership.mods}
      sets={setStore.sets}
      currentId={st.kind === "none" ? null : st.id}
      active={activeSet}
      lockReason={fgLock.reason}
      onsave={saveMembership}
    />
  {/if}

  {#if rowMenu && rowMenuSpec}
    <Menu
      bind:open={rowMenuOpen}
      anchor={rowMenu?.at.anchor ?? null}
      point={rowMenu?.at.point ?? null}
      placement="bottom-end"
      items={rowMenuSpec?.items ?? []}
      label="Actions for {rowMenu?.mod.title ?? rowMenu?.mod.techName}"
      onselect={(id) => rowMenuRun?.(id)}
    />
  {/if}

<ViewBoundary name="the dialog" overlay>
  <DialogHost />
</ViewBoundary>
<ShortcutsDialog bind:open={shortcutsOpen} />
<WhatsNew bind:open={whatsNewOpen} onclose={closeWhatsNew} onshowshortcuts={() => (shortcutsOpen = true)} />
<ToastRegion />
<!-- Scan start / finish for screen readers; outside the shell so F6 doesn't treat it as a notice. -->
<p class="sr-only" role="status">{scanSaid}</p>

<style>
  .busy {
    padding: 8px 20px;
    background: color-mix(in srgb, var(--accent) 14%, var(--surface));
    color: var(--accent-text);
    font-size: var(--fs-sm);
    font-weight: 600;
  }
  .progress {
    position: relative;
    height: 4px;
    background: var(--border);
  }
  .bar {
    height: 100%;
    background: var(--accent);
    transition: width 0.1s linear;
  }
  .progress-text {
    position: absolute;
    right: 12px;
    top: 6px;
    font-size: var(--fs-xs);
    color: var(--text-muted);
  }
  /* A tool or panel that was a floating card (log triage, bindings, MP sync, collections, the
     bridge) shown as a page section. Each owns its inner padding. */
  .page-card {
    max-width: 56rem;
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: var(--radius);
  }
  .tool-note {
    max-width: 48rem;
  }
  .tool-note p {
    margin: 0 0 var(--sp-3);
    max-width: 65ch;
  }
  .tool-note .muted {
    color: var(--muted);
  }
  /* ── Library view (LibraryView lays out the column) ── */
  .list-body {
    height: 100%;
    min-height: 0;
  }
</style>
