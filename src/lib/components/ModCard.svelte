<script lang="ts">
  // One Browse card, rendered inside a VirtualGrid item (which holds the focus and the keys, so
  // the button here is tabindex -1; Enter on the card opens the drawer, which has every action).
  // Cover, title, author, rating and downloads, the sources spelled out with their own versions
  // ("GitHub 1.0.6.0"), at most two badges then "+n", and one primary button whose words follow the
  // install state (installState): Install, Installing… 42%, Show in library, See update, or
  // Open on ModHub for index-only sources. Mods you already have are never dimmed.
  import Icon from "../ui/Icon.svelte";
  import Button from "../ui/Button.svelte";
  import { tooltip } from "../ui/tooltip";
  import BadgeRow from "./library/BadgeRow.svelte";
  import type { BrowseMod } from "../types";
  import type { BadgeSpec } from "./library/rowBadges";
  import {
    fmtCount,
    gatedReason,
    installLabel,
    label,
    loadCatalogImage,
    sourceChip,
    type InstallState,
  } from "../browse";

  interface Props {
    m: BrowseMod;
    state: InstallState;
    badges: BadgeSpec[];
    /** Another install is running: Install waits for it. */
    busy?: boolean;
    /** The primary button (Install / Show in library / See update / Open on ModHub). */
    onPrimary: () => void;
    onOpenDetail: () => void;
  }

  let { m, state: st, badges, busy = false, onPrimary, onOpenDetail }: Props = $props();

  const disabled = $derived(st.kind === "installing" || (st.kind === "install" && busy));
  const hint = $derived.by(() => {
    switch (st.kind) {
      case "install":
        return busy ? "Wait for the current install to finish." : `Install from ${label(st.source.source)}`;
      case "open":
        return gatedReason(st.source.source);
      case "installed":
        return "Open this mod in your library";
      case "update":
        return "A newer version is out. See it in Updates.";
      default:
        return "";
    }
  });
  const icon = $derived(
    st.kind === "install" ? "download" : st.kind === "open" ? "external" : st.kind === "update" ? "update" : st.kind === "installed" ? "library" : "info",
  );

  // Thumbnails: SiloAPI-cached URLs load directly; GitHub ones come through the Rust proxy.
  let thumb = $state("");
  /** The thumbnail element shown now: an error from an earlier one (a previous mod's) is ignored. */
  let thumbEl = $state<HTMLImageElement | null>(null);
  $effect(() => {
    const url = m.imageUrl;
    thumb = "";
    if (!url) return;
    let live = true;
    void loadCatalogImage(url).then((u) => {
      if (live) thumb = u;
    });
    return () => {
      live = false;
    };
  });

  function onclick(e: MouseEvent) {
    // The primary button stops here; anywhere else on the card opens the drawer.
    if ((e.target as HTMLElement | null)?.closest("button")) return;
    onOpenDetail();
  }
</script>

<!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
<div class="card" {onclick}>
  <div class="thumb">
    {#if thumb}
      <!-- Image host unreachable: the letter placeholder, not a broken-image icon (#428). -->
      <img src={thumb} alt="" loading="lazy" bind:this={thumbEl}
        onerror={(e) => e.currentTarget === thumbEl && (thumb = "")} />
    {:else}
      <div class="thumb-fallback" aria-hidden="true">{(m.title || "?").slice(0, 1)}</div>
    {/if}
  </div>
  <div class="body">
    <div class="title" title={m.title}>{m.title}</div>
    <div class="author">{m.author ?? "Unknown author"}</div>
    <div class="stats tnum">
      {#if m.rating != null}
        <span class="stat" use:tooltip={"Rating"}>
          <Icon name="favorite" size={12} filled label="Rating" />
          {m.rating.toFixed(1)}{#if m.ratingCount}<span class="sub">&nbsp;({fmtCount(m.ratingCount)})</span>{/if}
        </span>
      {/if}
      {#if m.downloads != null}
        <span class="stat" use:tooltip={"Downloads"}>
          <Icon name="download" size={12} label="Downloads" />
          {fmtCount(m.downloads)}
        </span>
      {/if}
    </div>
    <div class="chips">
      {#each m.sources as s (s.source + s.sourceUrl)}
        <span class="chip tnum">{sourceChip(s)}</span>
      {:else}
        <span class="chip none">No sources listed</span>
      {/each}
    </div>
    <div class="badges"><BadgeRow {badges} max={2} /></div>
    <div class="primary" use:tooltip={hint}>
      {#if st.kind === "installing"}
        <div
          class="progress"
          role="progressbar"
          aria-label="Installing {m.title}"
          aria-valuemin={0}
          aria-valuemax={100}
          aria-valuenow={st.pct ?? undefined}
        >
          <div class="fill" class:indet={st.pct === null} style:width={st.pct === null ? undefined : `${st.pct}%`}></div>
          <span class="progress-text tnum">{installLabel(st)}</span>
        </div>
      {:else}
        <Button
          size="sm"
          variant={st.kind === "install" ? "primary" : "secondary"}
          {icon}
          {disabled}
          tabindex={-1}
          aria-label="{installLabel(st)}: {m.title}"
          onclick={onPrimary}
          class="primary-btn"
        >
          {installLabel(st)}
        </Button>
      {/if}
    </div>
  </div>
</div>

<style>
  .card {
    height: 100%;
    box-sizing: border-box;
    display: flex;
    flex-direction: column;
    overflow: hidden;
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: var(--r-md);
    box-shadow: var(--shadow-1);
    cursor: pointer;
    transition: box-shadow 0.15s;
  }
  .card:hover {
    box-shadow: var(--shadow-2);
  }
  .thumb {
    flex: none;
    height: 124px;
    background: var(--sunken);
    overflow: hidden;
  }
  .thumb img {
    width: 100%;
    height: 100%;
    object-fit: cover;
    display: block;
  }
  .thumb-fallback {
    width: 100%;
    height: 100%;
    display: grid;
    place-items: center;
    font-family: var(--font-display);
    font-size: 2.4rem;
    color: var(--green-300);
    background: linear-gradient(135deg, var(--green-700), var(--green-900));
  }
  .body {
    flex: 1 1 auto;
    min-height: 0;
    display: flex;
    flex-direction: column;
    gap: var(--sp-1);
    padding: var(--sp-2) var(--sp-3) var(--sp-3);
  }
  .title {
    flex: none;
    font-weight: 600;
    color: var(--text);
    line-height: 1.25;
    height: 2.5em;
    display: -webkit-box;
    -webkit-line-clamp: 2;
    line-clamp: 2;
    -webkit-box-orient: vertical;
    overflow: hidden;
  }
  .author {
    font-size: var(--fs-sm);
    color: var(--muted);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .stats {
    display: flex;
    gap: var(--sp-3);
    align-items: center;
    min-height: 1.25rem;
    font-size: var(--fs-sm);
    color: var(--muted);
  }
  .stat {
    display: inline-flex;
    align-items: center;
    gap: 3px;
    white-space: nowrap;
  }
  .sub {
    color: var(--faint, var(--muted));
  }
  .chips {
    display: flex;
    gap: var(--sp-1);
    min-height: 1.25rem;
    overflow: hidden;
  }
  .chip {
    flex: 0 1 auto;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    font-size: var(--fs-xs);
    line-height: 1.25rem;
    padding: 0 var(--sp-2);
    border-radius: var(--r-pill);
    border: 1px solid var(--border);
    color: var(--text);
    white-space: nowrap;
  }
  .chip.none {
    color: var(--muted);
    border-style: dashed;
  }
  /* Two badges then "+n" (the tooltip names the rest), on up to two lines so a narrow card
     never hides one. */
  .badges {
    display: flex;
    max-height: calc(2.5rem + var(--sp-1));
    overflow: hidden;
  }
  .badges :global(.badge-row) {
    flex-wrap: wrap;
    row-gap: var(--sp-1);
  }
  .primary {
    margin-top: auto;
    display: flex;
  }
  .primary :global(.primary-btn) {
    flex: 1 1 auto;
  }
  .progress {
    position: relative;
    flex: 1 1 auto;
    height: 28px;
    border-radius: var(--r-sm);
    background: var(--sunken);
    overflow: hidden;
  }
  .fill {
    height: 100%;
    background: color-mix(in srgb, var(--primary) 35%, transparent);
    transition: width 0.2s ease;
  }
  .fill.indet {
    width: 35%;
    animation: indet 1.1s ease-in-out infinite;
  }
  .progress-text {
    position: absolute;
    inset: 0;
    display: grid;
    place-items: center;
    font-size: var(--fs-sm);
    font-weight: 600;
    color: var(--text);
  }
  @keyframes indet {
    from {
      margin-left: -35%;
    }
    to {
      margin-left: 100%;
    }
  }
  @media (prefers-reduced-motion: reduce) {
    .fill.indet {
      animation: none;
      width: 100%;
      opacity: 0.5;
    }
  }
</style>
