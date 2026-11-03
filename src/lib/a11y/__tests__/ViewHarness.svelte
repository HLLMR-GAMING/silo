<script lang="ts">
  // Test-only: the app frame (rail, header with status and Play, notices) around one view, filled
  // with that view's real panels and stub data, for the axe suite (#258). `which` picks the view;
  // "libraryDrawer" also opens the mod drawer.
  import AppShell from "../../components/shell/AppShell.svelte";
  import AppHeader from "../../components/shell/AppHeader.svelte";
  import StatusChip from "../../components/shell/StatusChip.svelte";
  import PlayButton from "../../components/shell/PlayButton.svelte";
  import NoticeArea from "../../components/NoticeArea.svelte";
  import LibraryView from "../../views/LibraryView.svelte";
  import UpdatesView from "../../views/UpdatesView.svelte";
  import ModSetsView from "../../views/ModSetsView.svelte";
  import SettingsView from "../../views/SettingsView.svelte";
  import ToolsView from "../../views/ToolsView.svelte";
  import HelpView from "../../views/HelpView.svelte";
  import DiagnoseView from "../../views/DiagnoseView.svelte";
  import BrowseView from "../../views/BrowseView.svelte";
  import VirtualList from "../../components/library/VirtualList.svelte";
  import VirtualGrid from "../../components/library/VirtualGrid.svelte";
  import ModListRow from "../../components/library/ModListRow.svelte";
  import ModCard from "../../components/library/ModCard.svelte";
  import LibraryToolbar from "../../components/panels/LibraryToolbar.svelte";
  import ActiveFilters from "../../components/library/ActiveFilters.svelte";
  import BulkBar from "../../components/library/BulkBar.svelte";
  import FilterBar from "../../components/FilterBar.svelte";
  import UpdatesPanel from "../../components/panels/UpdatesPanel.svelte";
  import StatsPanel from "../../components/panels/StatsPanel.svelte";
  import ConflictsPanel from "../../components/panels/ConflictsPanel.svelte";
  import ProblemsPanel from "../../components/panels/ProblemsPanel.svelte";
  import MySetsTab from "../../components/modsets/MySetsTab.svelte";
  import SavegamesTab from "../../components/modsets/SavegamesTab.svelte";
  import GeneralSettings from "../../components/settings/GeneralSettings.svelte";
  import ModsFolderSettings from "../../components/settings/ModsFolderSettings.svelte";
  import LibrarySettings from "../../components/settings/LibrarySettings.svelte";
  import DiagnosticsSettings from "../../components/settings/DiagnosticsSettings.svelte";
  import MaintenanceSettings from "../../components/settings/MaintenanceSettings.svelte";
  import AboutSettings from "../../components/settings/AboutSettings.svelte";
  import ModDetail from "../../components/ModDetail.svelte";
  import ModBrowser from "../../components/ModBrowser.svelte";
  import { rowBadges } from "../../components/library/rowBadges";
  import { filterChips, NO_FILTERS } from "../../components/library/filters";
  import { NO_SECTIONS } from "../../updates/sections";
  import { saveModStats } from "../../modsets/modSets";
  import type { CurationRow, ModEntry, Savegame } from "../../types";
  import { modEntry } from "../fixtures";

  let { which, mods = [] }: { which: string; mods?: ModEntry[] } = $props();

  const noop = () => {};
  const asyncNoop = async () => {};
  const library = $derived(new Set(mods.map((m) => m.techName)));
  const active = $derived(new Set(mods.filter((m) => m.active).map((m) => m.techName)));
  const label = (m: ModEntry) => `${m.title ?? m.techName}, ${m.active ? "active" : "parked"}`;
  const badges = (m: ModEntry) => rowBadges(m, { hasUpdate: m.techName.endsWith("1"), flagged: m.techName.endsWith("2") });
  const drawerMod = $derived(mods[0] ?? modEntry(0));
  const curation = (t: string): CurationRow => ({ techName: t, favorite: false, hidden: false, broken: false, rating: 0, note: null, skipUpdates: false });
  const save: Savegame = {
    index: 1,
    folder: "savegame1",
    name: "Riverbend",
    mapTitle: "Riverbend Springs",
    saveDate: "2026-10-01",
    mods: [
      { modName: "FS25_Mod1", title: "Mod 1", version: "1.0.0.0", isDlc: false, required: true, fileHash: null },
    ],
  };
  const row = (t: string, extra: Record<string, unknown> = {}) => ({
    techName: t,
    title: t,
    path: `C:/mods/${t}.zip`,
    current: "1.0.0.0",
    latest: "1.1.0.0",
    hasUpdate: true,
    ...extra,
  });
</script>

{#snippet header()}
  <AppHeader searchPlaceholder="Search your library">
    {#snippet status()}<StatusChip problems={1} review={2} />{/snippet}
    {#snippet play()}
      <PlayButton
        count={active.size}
        onplay={noop}
        menuItems={[{ id: "folder", label: "Open the game folder", icon: "showFile" }]}
        onmenuselect={noop}
        activeSetLine="Harvest 2026, unchanged"
      />
    {/snippet}
  </AppHeader>
{/snippet}

{#snippet notices()}<NoticeArea />{/snippet}

{#snippet drawer()}
  <ModDetail
    mod={drawerMod}
    curation={curation(drawerMod.techName)}
    tags={["harvest"]}
    active={true}
    organized={false}
    hasSettings={false}
    libraryTechNames={library}
    conflicts={[]}
    categories={["Tools", "Tractors"]}
    isOverridden={false}
    onSetCategory={noop}
    onResetCategory={noop}
    onClose={noop}
    onToggle={noop}
    onToggleActive={noop}
    onOpenSettings={noop}
    onFindInBrowse={noop}
    onCurationChange={noop}
    onTagsChange={noop}
    onFilterTag={noop}
    repo={null}
    onRepoChange={noop}
    source={null}
    onSourceChange={noop}
    onInstalled={noop}
    foreign={null}
    onAdoptForeign={noop}
    onRestoreForeign={noop}
    onRemove={noop}
  />
{/snippet}

{#snippet sets()}
  <MySetsTab
    sets={[
      { id: 1, name: "Harvest 2026", mods: ["FS25_Mod1", "FS25_Mod2"], kind: "user", createdAt: null },
      { id: 2, name: "Before Harvest 2026", mods: ["FS25_Mod1"], kind: "auto", createdAt: 1_760_000_000_000 },
    ]}
    {active}
    {library}
    state={{ kind: "set", id: 1, name: "Harvest 2026" }}
    onSwitch={noop}
    onUpdate={noop}
    onDelete={noop}
    onShare={noop}
    onSaveCurrent={noop}
    onOpenShared={noop}
  />
{/snippet}

{#snippet savegames()}
  <SavegamesTab rows={[{ s: save, st: saveModStats(save, library) }]} onUse={noop} onBackup={noop} onSettings={noop} onFindInBrowse={noop} />
{/snippet}

{#snippet general()}
  <GeneralSettings theme="system" uiScale={1} userDir="C:/Users/me/Documents/My Games/FarmingSimulator2025" density="comfortable" onSetTheme={noop} onSetUiScale={noop} onSetDensity={noop} onOpenGameGraphics={noop} />
{/snippet}
{#snippet modsFolder()}
  <ModsFolderSettings roots={["C:/mods"]} modsStatus={null} gameInfo={null} lockReason="" scanning={false} moveUnfinished={false} onChangeModsFolder={noop} onUseGameFolder={noop} onSwitchModsFolder={noop} />
{/snippet}
{#snippet librarySettings()}
  <LibrarySettings autoFileNew={true} organizedCount={10} unorganizedCount={2} lockReason="" scanning={false} archive={null} moveUnfinished={false} archiveLoadFailed={false} onSetAutoFile={noop} onOrganize={noop} onChangeArchive={noop} onArchiveDefault={noop} />
{/snippet}
{#snippet diagnostics()}
  <DiagnosticsSettings onCopyDiagnostics={noop} onExportReport={noop} />
{/snippet}
{#snippet maintenance()}
  <MaintenanceSettings organizedCount={10} cloudSynced={false} lockReason="" scanning={false} moveUnfinished={false} onRebuild={noop} onOptimizeLinks={noop} onRestoreVanilla={noop} />
{/snippet}
{#snippet about()}
  <AboutSettings appVer="0.11.0" scanMs={420} />
{/snippet}
{#snippet empty()}<p>Nothing here.</p>{/snippet}

{#snippet stats()}
  <StatsPanel
    modCount={mods.length}
    organizedCount={2}
    activeCount={active.size}
    mapsCount={0}
    libStats={{ totalSize: 1e9, cats: [{ name: "Tools", count: 3, size: 3e6 }], maxCatSize: 3e6, largest: mods.slice(0, 2), rated: 0, avgRating: 0, tagged: 0 }}
  />
{/snippet}

{#snippet conflictsTab()}
  <ConflictsPanel
    conflicts={[
      {
        id: "uniqueType:seedSlot:FS25_A,FS25_B",
        severity: "critical",
        kind: "uniqueType",
        name: "seedSlot",
        explanation: "These mods use the same exclusive slot.",
        mods: ["Seeder A", "Seeder B"],
        techNames: ["FS25_A", "FS25_B"],
      },
    ]}
    activeCount={active.size}
    onOpen={() => {}}
    onPark={() => {}}
  />
{/snippet}
{#snippet problemsTab()}
  <ProblemsPanel
    health={{ missingDeps: mods.length ? [{ mod: mods[0], missing: ["FS25_gone"] }] : [], corrupt: [], ignored: [], foreign: [] }}
    onFindInBrowse={() => {}}
    onResolveForeign={asyncNoop}
  />
{/snippet}

<AppShell
  {header}
  {notices}
  badges={{ updates: 2 }}
  drawer={which === "libraryDrawer" ? drawer : undefined}
  drawerLabel="Mod details"
  fill={which.startsWith("library") || which === "browse"}
>
  {#if which.startsWith("library")}
    <LibraryView stripLeft="{mods.length} mods, {active.size} active" stripItems={[{ id: "u", text: "2 updates", action: "Review", run: noop }]} menuItems={[{ id: "rescan", label: "Rescan" }]} onmenuselect={noop}>
      {#snippet filters()}
        <FilterBar catTree={[{ name: "Tools", count: 3, children: [] }]} showSearch={false} sortOptions={[{ value: "name", label: "Name" }, { value: "added", label: "Recently added" }]} sort="added" dirEnabled showYear={false} showClear={false} />
        {#if which === "librarySelecting"}
          <BulkBar count={2} shownCount={mods.length} onturnon={noop} onpark={noop} onfavorite={noop} onhide={noop} onmodsets={noop} onselectall={noop} onclear={noop} />
        {:else}
          <LibraryToolbar filters={{ ...NO_FILTERS }} onchange={noop} onneedsupdate={noop} tags={["harvest"]} counts={{ attention: 2 }} shownCount={mods.length} libView={which === "libraryCards" ? "cards" : "list"} />
        {/if}
        <ActiveFilters chips={filterChips({ ...NO_FILTERS, favorites: true }, "Tools")} onremove={noop} onclear={noop} />
      {/snippet}
      {#snippet list()}
        {#if which === "libraryCards"}
          <VirtualGrid items={mods} getKey={(m) => m.path} itemLabel={label} itemHeight={260} label="Mods">
            {#snippet item(m)}
              <ModCard mod={m} active={m.active} badges={badges(m)} rating={3} onToggle={noop} onOpen={noop} onMenu={noop} />
            {/snippet}
          </VirtualGrid>
        {:else}
          <VirtualList items={mods} getKey={(m) => m.path} itemLabel={label} rowHeight={56} label="Mods">
            {#snippet row(m)}
              <ModListRow
                mod={m}
                active={m.active}
                badges={badges(m)}
                selected={which === "librarySelecting" && m.techName === "FS25_Mod1"}
                selectionMode={which === "librarySelecting"}
                onToggle={noop}
                onOpen={noop}
                onMenu={noop}
              />
            {/snippet}
          </VirtualList>
        {/if}
      {/snippet}
    </LibraryView>
  {:else if which === "updates"}
    <UpdatesView checking={false} checkedAt={1_760_000_000_000} checkDisabled={false} oncheck={noop}>
      <UpdatesPanel
        sections={{
          ...NO_SECTIONS,
          ready: [row("FS25_Ready", { assetUrl: "https://example.invalid/a.zip" })],
          modhub: [row("FS25_Hub", { pageUrl: "https://example.invalid/mod" })],
          failed: [row("FS25_Fail", { hasUpdate: null, error: "Rate limited" })],
          upToDate: [row("FS25_Fine", { hasUpdate: false })],
        }}
        checked={true}
        checking={false}
        catalogError={null}
        rowStates={{}}
        updatingAll={false}
        lockReason=""
        checkDisabledReason=""
        foreign={[]}
        onCheck={noop}
        onInstall={noop}
        onUpdateAll={noop}
        onOpenPage={noop}
        onUseDropped={noop}
        onOpenSource={noop}
        onMarkChecked={noop}
        onRecheck={noop}
        onCopyDetails={noop}
      />
    </UpdatesView>
  {:else if which === "modSets"}
    <ModSetsView setsTab={sets} openTab={empty} savegamesTab={savegames} />
  {:else if which === "savegames"}
    <ModSetsView setsTab={empty} openTab={empty} savegamesTab={savegames} />
  {:else if which === "settings"}
    <SettingsView generalTab={general} modsFolderTab={modsFolder} libraryTab={librarySettings} accountsTab={empty} diagnosticsTab={diagnostics} maintenanceTab={maintenance} aboutTab={about} />
  {:else if which === "tools"}
    <ToolsView statsTab={stats} controlsTab={empty} bridgeTab={empty} graphicsTab={empty} />
  {:else if which === "help"}
    <HelpView onopen={noop} oncopydiagnostics={noop} onshowshortcuts={noop} onshowwhatsnew={noop} appVer="0.11.0" />
  {:else if which === "diagnose"}
    <DiagnoseView lastRunTab={empty} {conflictsTab} {problemsTab} conflictsBadge={0} problemsBadge={1} />
  {:else if which === "browse"}
    <BrowseView>
      <ModBrowser installed={library} onInstalled={noop} showTitle={false} seed="tractor" />
    </BrowseView>
  {/if}
</AppShell>
