<script lang="ts">
  // Takes the toolbar's place while any mod is selected:
  //   12 selected · [Turn on] [Park] [Favorite] [Hide] [Skip updates] [Mod sets…] · [Select all 140 shown] · Clear
  //   <BulkBar count={sel.size} shownCount={visible.length} onturnon={...} onpark={...}
  //     onfavorite={...} onhide={...} onmodsets={...} onselectall={...} onclear={() => sel.clear()} />
  // Turn on / Park are off (with the reason as the tooltip) while another library change runs.
  // Favorite / Hide flip to Remove favorite / Unhide when every selected mod already is one.
  // A toolbar: one Tab stop, ←/→ (and Home/End) move between its buttons, skipping turned-off
  // ones. `bind:ref` gives the bar's element (App hands focus on when the bar goes away).
  import { tick } from "svelte";
  import Button from "../../ui/Button.svelte";
  import { tooltip } from "../../ui/tooltip";

  interface Props {
    count: number;
    shownCount: number;
    /** Every shown mod is selected (Select all has nothing to add). */
    allShownSelected?: boolean;
    /** Every selected mod is a favorite: the button removes the favorite instead. */
    allFavorite?: boolean;
    /** Every selected mod has update checks off (#333): the button switches them back on. */
    allSkipUpdates?: boolean;
    /** Every selected mod is hidden: the button unhides instead. */
    allHidden?: boolean;
    /** How many of the selected mods are active: Turn on is the primary action when none is,
     *  Park when all are, neither for a mix. */
    activeCount?: number;
    /** Why Turn on / Park can't run now ("Silo is moving mods."), or "". */
    writeReason?: string;
    onturnon: () => void;
    onpark: () => void;
    onfavorite: (on: boolean) => void;
    onhide: (on: boolean) => void;
    /** Switch update checks off (true) or back on (false) for the selection. */
    onskipupdates?: (on: boolean) => void;
    /** Add the selected mods to, or remove them from, saved sets (the "Mod sets…" dialog). */
    onmodsets: () => void;
    /** Remove the selected mods from the library (#429): App confirms first. */
    onremove?: () => void;
    onselectall: () => void;
    onclear: () => void;
    /** The bar's element (bindable). */
    ref?: HTMLElement | null;
  }

  let {
    count,
    shownCount,
    allShownSelected = false,
    allFavorite = false,
    allHidden = false,
    activeCount = 0,
    writeReason = "",
    onturnon,
    onpark,
    onfavorite,
    onhide,
    onskipupdates,
    allSkipUpdates = false,
    onmodsets,
    onremove,
    onselectall,
    onclear,
    ref = $bindable(null),
  }: Props = $props();

  /** The buttons that can take focus, in order. */
  function buttons(): HTMLButtonElement[] {
    return [...(ref?.querySelectorAll<HTMLButtonElement>("button") ?? [])].filter((b) => !b.disabled);
  }
  /** Make `stop` (else the current stop, else the first button) the one Tab stop. */
  function rove(stop?: Element | null) {
    const bs = buttons();
    const cur = bs.find((b) => b === stop) ?? bs.find((b) => b.tabIndex === 0) ?? bs[0];
    for (const b of bs) b.tabIndex = b === cur ? 0 : -1;
  }
  // Buttons turn on and off (a write starts, everything gets selected): keep one stop on.
  $effect(() => {
    void [writeReason, allShownSelected, allFavorite, allHidden, ref];
    void tick().then(() => rove());
  });
  function onkeydown(e: KeyboardEvent) {
    if (e.altKey || e.ctrlKey || e.metaKey) return;
    // Esc in the selection bar clears the selection, as it does in the list.
    if (e.key === "Escape" && !e.defaultPrevented) {
      e.preventDefault();
      onclear();
      return;
    }
    const bs = buttons();
    const i = bs.findIndex((b) => b === document.activeElement);
    if (i < 0 || bs.length === 0) return;
    let to: number;
    if (e.key === "ArrowRight") to = (i + 1) % bs.length;
    else if (e.key === "ArrowLeft") to = (i - 1 + bs.length) % bs.length;
    else if (e.key === "Home") to = 0;
    else if (e.key === "End") to = bs.length - 1;
    else return;
    e.preventDefault();
    rove(bs[to]);
    bs[to].focus();
  }
</script>

<div
  class="bulk"
  role="toolbar"
  aria-label="Selected mods"
  tabindex="-1"
  bind:this={ref}
  {onkeydown}
  onfocusin={(e) => rove(e.target as Element)}
>
  <span class="count tnum" aria-live="polite">{count} selected</span>
  <span class="sep" aria-hidden="true">·</span>
  <span class="group" use:tooltip={writeReason || null}>
    <Button size="sm" variant={activeCount === 0 ? "primary" : "secondary"} disabled={!!writeReason} onclick={onturnon}
      >Turn on</Button
    >
    <Button size="sm" variant={activeCount === count ? "primary" : "secondary"} disabled={!!writeReason} onclick={onpark}
      >Park</Button
    >
  </span>
  <Button size="sm" icon="favorite" onclick={() => onfavorite(!allFavorite)}>
    {allFavorite ? "Remove favorite" : "Favorite"}
  </Button>
  <Button size="sm" icon={allHidden ? "visible" : "hidden"} onclick={() => onhide(!allHidden)}>
    {allHidden ? "Unhide" : "Hide"}
  </Button>
  {#if onskipupdates}
    <Button size="sm" icon="update" onclick={() => onskipupdates(!allSkipUpdates)}>
      {allSkipUpdates ? "Check updates" : "Skip updates"}
    </Button>
  {/if}
  <Button size="sm" icon="modSets" onclick={onmodsets}>Mod sets…</Button>
  {#if onremove}
    <span use:tooltip={writeReason || null}>
      <Button size="sm" icon="remove" disabled={!!writeReason} onclick={onremove}>Remove…</Button>
    </span>
  {/if}
  <span class="sep" aria-hidden="true">·</span>
  <Button size="sm" variant="ghost" disabled={allShownSelected} onclick={onselectall}>
    Select all {shownCount} shown
  </Button>
  <span class="sep" aria-hidden="true">·</span>
  <Button size="sm" variant="link" onclick={onclear}>Clear</Button>
</div>

<style>
  .bulk {
    flex: 0 0 auto;
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: var(--sp-2);
    min-height: 2.5rem;
    margin: 0 0 8px;
    padding: 4px 10px;
    border: 1px solid color-mix(in srgb, var(--primary) 40%, var(--border));
    border-radius: var(--radius);
    background: color-mix(in srgb, var(--primary) 8%, var(--surface));
  }
  .count {
    font-size: var(--fs-sm);
    font-weight: 600;
  }
  .group {
    display: inline-flex;
    gap: var(--sp-2);
  }
  .sep {
    color: var(--text-muted);
  }
</style>
