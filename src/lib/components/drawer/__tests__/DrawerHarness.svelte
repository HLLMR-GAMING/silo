<script lang="ts">
  // Test-only: a library list and the mod drawer beside it, wired as App wires them: Enter on a
  // row opens the drawer, and focus goes back to the row by key when it closes.
  import VirtualList from "../../library/VirtualList.svelte";
  import ModDetail from "../../ModDetail.svelte";
  import { returnFocusOnDrawerClose } from "../drawerFocus.svelte";
  import type { CurationRow, ModEntry } from "../../../types";

  let { mods }: { mods: ModEntry[] } = $props();

  let open = $state<ModEntry | null>(null);
  let list = $state<{ focusKey: (key: string) => boolean }>();
  const noop = () => {};
  const curation = (techName: string): CurationRow => ({
    techName,
    favorite: false,
    hidden: false,
    broken: false,
    rating: 0,
    note: null,
    skipUpdates: false,
  });

  returnFocusOnDrawerClose({
    open: () => !!open,
    key: () => open?.path ?? null,
    focusKey: (k) => list?.focusKey(k) ?? false,
    keepOpener: () => false,
    fallback: () => null,
  });

  export function focusKey(key: string): boolean {
    return list?.focusKey(key) ?? false;
  }
</script>

<div style="height: 600px">
  <VirtualList bind:this={list} items={mods} getKey={(m) => m.path} rowHeight={56} label="Mods" onopen={(m) => (open = m)}>
    {#snippet row(m)}<span>{m.title}</span>{/snippet}
  </VirtualList>
</div>
<button type="button">Outside</button>
{#if open}
  {@const m = open}
  <aside aria-label="Mod details">
    <ModDetail
      mod={m}
      curation={curation(m.techName)}
      tags={[]}
      active={false}
      organized={false}
      hasSettings={false}
      libraryTechNames={new Set()}
      conflicts={[]}
      categories={["Tools", "Vehicles"]}
      isOverridden={false}
      onSetCategory={noop}
      onResetCategory={noop}
      onClose={() => (open = null)}
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
  </aside>
{/if}
