<script lang="ts">
  import Icon from "../ui/Icon.svelte";
  import { displayVersion } from "../format";
  import type { CatalogModDetail, ModSourceOption } from "../types";
  import { resizable } from "../resize";
  import GithubCard from "./GithubCard.svelte";
  import ModHubCard from "./ModHubCard.svelte";
  import {
    label,
    gatedReason,
    fmtCount,
    parseRepo,
    canExpand,
    loadCatalogImage,
    browseBadges,
    progressPct,
    type InstallProgressEntry,
  } from "../browse";
  import { tooltip } from "../ui/tooltip";
  import Skeleton from "../ui/Skeleton.svelte";
  import BadgeRow from "./library/BadgeRow.svelte";

  let {
    detail,
    detailLoading,
    installingId,
    progress,
    busy = false,
    takeFocus = false,
    gh,
    installed,
    updates,
    onClose,
    onUseSource,
    onOpenDesc,
    onNeedAuth,
    onFindInLibrary,
    onShowUpdates,
  }: {
    detail: CatalogModDetail | null;
    detailLoading: boolean;
    installingId: string | null;
    /** Download progress for this mod's install, while it runs. */
    progress?: InstallProgressEntry;
    /** Some install is running: Install waits for it. */
    busy?: boolean;
    /** Opened from the keyboard: move focus to the title once the details are in. */
    takeFocus?: boolean;
    gh: { connected: boolean; canWrite: boolean };
    installed: ReadonlySet<string>;
    /** Tech names with a known newer version. */
    updates?: ReadonlySet<string>;
    onClose: () => void;
    onUseSource: (d: CatalogModDetail, s: ModSourceOption) => void;
    onOpenDesc: (d: CatalogModDetail) => void;
    onNeedAuth?: () => void;
    onFindInLibrary?: (techName: string) => void;
    onShowUpdates?: () => void;
  } = $props();

  function hasLocally(d: CatalogModDetail): boolean {
    return d.techName != null && installed.has(d.techName);
  }
  const pct = $derived(progressPct(progress));
  const badges = $derived(detail ? browseBadges(detail, { installed, updates }) : []);

  // A keyboard open lands on the title (Tab goes on into the actions); a mouse open leaves focus.
  let titleEl = $state<HTMLElement>();
  let focusedFor: string | null = null;
  $effect(() => {
    const id = detail?.id ?? null;
    if (!takeFocus || !id || !titleEl || focusedFor === id) return;
    focusedFor = id;
    titleEl.focus();
  });

  // The drawer is non-modal (clicks fall through to the grid), so Escape is the click-away close.
  $effect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== "Escape" || e.defaultPrevented) return; // a menu or dialog took this Esc
      onClose();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  });

  // Cover loads through the Rust proxy (data: URL) — the CDN needs a referer.
  let cover = $state("");
  /** The cover element shown now: an error from an earlier one (a previous mod's) is ignored. */
  let coverEl = $state<HTMLImageElement | null>(null);
  $effect(() => {
    const url = detail?.imageUrl;
    cover = "";
    if (url) loadCatalogImage(url).then((u) => { if (detail?.imageUrl === url) cover = u; });
  });
</script>

<!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
<div class="drawer-backdrop" onclick={onClose}></div>
<aside class="drawer" aria-label={detail ? `Mod details: ${detail.title}` : "Mod details"} use:resizable>
  {#if detailLoading}
    <div class="loading" aria-busy="true" aria-label="Loading the mod's details">
      <Skeleton width="70%" height="1.4em" />
      <Skeleton width="45%" />
      <Skeleton variant="block" height="160px" />
      <Skeleton width="90%" />
      <Skeleton width="80%" />
    </div>
  {:else if detail}
    {@const d = detail}
    <div class="drawer-head">
      <div class="dh-info">
        <h2 class="dh-title" tabindex="-1" bind:this={titleEl}>{d.title}</h2>
        {#if d.author || d.latestVersion}
          <div class="dh-sub">
            {#if d.author}{d.author}{/if}{#if d.author && d.latestVersion}
              · {/if}{#if d.latestVersion}v{d.latestVersion.replace(/^v/i, "")}{/if}
          </div>
        {/if}
        {#if d.techName}<div class="dh-tech tnum">{d.techName}</div>{/if}
        {#if badges.length}<div class="dh-badges"><BadgeRow {badges} max={4} /></div>{/if}
      </div>
      <button class="drawer-x" use:tooltip={"Close details"} aria-label="Close details" onclick={onClose}><Icon name="close" /></button>
    </div>
    <div class="drawer-body">
      {#if cover}
        <img class="drawer-img" src={cover} alt="" bind:this={coverEl} onerror={(e) => e.currentTarget === coverEl && (cover = "")} />
      {/if}
      <dl class="facts">
        {#if d.category}<dt>Category</dt><dd>{d.category}</dd>{/if}
        {#if d.rating != null}
          <dt>Rating</dt>
          <dd class="tnum">
            <Icon name="favorite" size={13} filled label="rating" /> {d.rating.toFixed(1)}{#if d.ratingCount}
              &nbsp;({fmtCount(d.ratingCount)}){/if}
          </dd>
        {/if}
        {#if d.downloads != null}
          <dt>Downloads</dt><dd class="tnum">{fmtCount(d.downloads)}</dd>
        {/if}
      </dl>

      {#if d.description}
        <p class="drawer-desc clamped">{d.description}</p>
        {#if canExpand(d)}
          <button class="read-more" onclick={() => onOpenDesc(d)}>Read more</button>
        {/if}
      {/if}

      {#if d.tags?.length}
        <div class="drawer-tags">
          {#each d.tags as t (t.namespace + t.value)}
            <span
              class="dtag"
              title="{t.namespace}{t.confidence != null ? ` · ${Math.round(t.confidence * 100)}% confidence` : ''}{t.source ? ` · ${t.source}` : ''}"
            >{t.value}</span>
          {/each}
        </div>
      {/if}

      <div class="drawer-sec">Available from</div>
      {#if d.sources.length === 0}
        <p class="drawer-none">No sources recorded.</p>
      {:else}
        <ul class="srcs">
          {#each d.sources as s (s.source + s.sourceUrl)}
            <li>
              <div class="src-head">
                <span class="src-name">{label(s.source)}</span>
                {#if s.version}<span class="src-ver tnum">{displayVersion(s.version)}</span>{/if}
                <button
                  class="src-action"
                  class:can-install={s.installable}
                  disabled={s.installable && !hasLocally(d) && busy}
                  use:tooltip={s.installable && busy && installingId !== d.id
                    ? "Wait for the current install to finish."
                    : ""}
                  onclick={() =>
                    s.installable && hasLocally(d) && d.techName
                      ? onFindInLibrary?.(d.techName)
                      : onUseSource(d, s)}
                >
                  {#if s.installable && installingId === d.id}
                    Installing…{#if pct !== null}&nbsp;<span class="tnum">{pct}%</span>{/if}
                  {:else if s.installable && hasLocally(d)}
                    Show in library
                  {:else if s.installable}
                    Install <Icon name="download" size={13} />
                  {:else}
                    Open on {label(s.source)} <Icon name="external" size={13} />
                  {/if}
                </button>
              </div>
              {#if !s.installable}
                <p class="src-why">{gatedReason(s.source)}</p>
              {/if}
            </li>
          {/each}
        </ul>
      {/if}

      {#if d.sources.some((s) => (s.source === "github" && parseRepo(s.sourceUrl)) || s.source === "modhub")}
        {@const ghSrcs = d.sources.filter((s) => s.source === "github" && parseRepo(s.sourceUrl))}
        {@const mhSrcs = d.sources.filter((s) => s.source === "modhub")}
        <div class="drawer-sec">Interact</div>
        <div class="src-cards">
          {#each ghSrcs as s (s.sourceUrl)}
            {@const r = parseRepo(s.sourceUrl)}
            {#if r}
              <GithubCard
                owner={r.owner}
                repo={r.repo}
                connected={gh.connected}
                canWrite={gh.canWrite}
                onConnect={() => onNeedAuth?.()}
              />
            {/if}
          {/each}
          {#each mhSrcs as s (s.sourceUrl)}
            <ModHubCard
              rating={d.rating}
              ratingCount={d.ratingCount}
              sourceUrl={s.sourceUrl}
            />
          {/each}
        </div>
      {/if}

      {#if !hasLocally(d) && d.sources.length > 0 && !d.sources.some((s) => s.installable)}
        <!-- Nothing here is directly installable — tell the user exactly what to do,
             and that Silo will still take it from there. -->
        <p class="drawer-hint">
          None of these sources allow apps to download for you. Grab the .zip from a
          source above and drop it in your mods folder. Silo adds it to your library
          on the next scan.
        </p>
      {/if}
      {#if hasLocally(d)}
        <div class="drawer-owned">
          <span>Already in your library</span>
          {#if d.techName && updates?.has(d.techName) && onShowUpdates}
            <button class="find-lib-btn" onclick={() => onShowUpdates?.()}>
              <Icon name="update" size={13} /> See update
            </button>
          {/if}
          {#if d.techName && onFindInLibrary}
            <button class="find-lib-btn" onclick={() => onFindInLibrary?.(d.techName!)}>
              <Icon name="library" size={13} /> Show in library
            </button>
          {/if}
        </div>
      {/if}
    </div>
  {/if}
</aside>

<style>
  .loading {
    display: flex;
    flex-direction: column;
    gap: var(--sp-3);
    padding: var(--sp-5);
  }
  /* ── Detail drawer ── */
  .drawer-backdrop {
    position: fixed;
    inset: 0;
    top: var(--header-h);
    background: rgba(0, 0, 0, 0.15);
    /* Non-modal: let clicks fall through to the catalog grid so clicking another card switches
       the drawer to that mod. Close via the ✕ or Escape instead of clicking away. */
    pointer-events: none;
    z-index: 40;
  }
  .drawer {
    position: fixed;
    top: var(--header-h);
    right: 0;
    bottom: 0;
    width: min(420px, 92vw);
    background: var(--surface);
    border-left: 1px solid var(--border);
    box-shadow: var(--shadow-2);
    z-index: 41;
    display: flex;
    flex-direction: column;
    overflow: hidden;
  }
  .drawer-head {
    display: flex;
    align-items: flex-start;
    gap: 8px;
    padding: 14px 16px;
    border-bottom: 1px solid var(--border);
  }
  .dh-info {
    flex: 1 1 auto;
    min-width: 0;
  }
  .dh-title {
    font-family: var(--font-display);
    font-size: var(--fs-xl);
    font-weight: 600;
    line-height: 1.2;
    color: var(--text);
    margin: 0;
  }
  .dh-title:focus {
    outline: none;
  }
  .dh-title:focus-visible {
    outline: 2px solid var(--focus);
    outline-offset: 2px;
  }
  .dh-sub {
    font-size: var(--fs-xs);
    color: var(--text-muted);
    margin-top: 2px;
  }
  .dh-tech {
    font-size: var(--fs-xs);
    color: var(--text-muted);
    opacity: 0.75;
    margin-top: 2px;
  }
  .dh-badges {
    display: flex;
    margin-top: var(--sp-2);
  }
  .drawer-x {
    border: none;
    background: transparent;
    color: var(--text-muted);
    font-size: 1rem;
    cursor: pointer;
    padding: 4px 6px;
    border-radius: var(--radius-sm);
  }
  .drawer-x:hover {
    background: var(--bg);
    color: var(--text);
  }
  .drawer-body {
    padding: 14px 16px 24px;
    overflow-y: auto;
  }
  .drawer-img {
    width: 100%;
    border-radius: var(--radius-sm);
    border: 1px solid var(--border);
    margin-bottom: 12px;
    display: block;
  }
  /* Match the Library drawer's .d-meta spec sheet: label left, value right. */
  .facts {
    display: grid;
    grid-template-columns: auto 1fr;
    gap: 6px 12px;
    margin: 0 0 12px;
    padding: 10px 0;
    border-top: 1px solid var(--border);
    font-size: var(--fs-xs);
  }
  .facts dt {
    color: var(--text-muted);
  }
  .facts dd {
    margin: 0;
    color: var(--text);
    text-align: right;
  }
  .drawer-desc {
    color: var(--text);
    font-size: 0.88rem;
    line-height: 1.5;
    margin: 0 0 4px;
  }
  .drawer-desc.clamped {
    display: -webkit-box;
    -webkit-line-clamp: 4;
    line-clamp: 4;
    -webkit-box-orient: vertical;
    overflow: hidden;
  }
  .read-more {
    border: none;
    background: transparent;
    color: var(--info);
    font-size: var(--fs-sm);
    font-weight: 600;
    padding: 0;
    margin: 0 0 14px;
    cursor: pointer;
  }
  .read-more:hover {
    text-decoration: underline;
  }
  /* Match the Library drawer's section labels so both read as one system. */
  .drawer-sec {
    font-size: var(--fs-xs);
    text-transform: uppercase;
    letter-spacing: 0.04em;
    font-weight: 700;
    color: var(--text-muted);
    margin-bottom: 8px;
  }
  .drawer-tags {
    display: flex;
    flex-wrap: wrap;
    gap: 5px;
    margin: 0 0 14px;
  }
  .dtag {
    font-size: var(--fs-xs);
    padding: 2px 8px;
    border-radius: 999px;
    border: 1px solid var(--border);
    background: var(--bg);
    color: var(--text-muted);
    text-transform: capitalize;
  }
  .src-cards {
    display: flex;
    flex-direction: column;
    gap: 8px;
    margin-bottom: 14px;
  }
  .drawer-none {
    color: var(--text-muted);
    font-size: 0.85rem;
    margin: 0 0 14px;
  }
  .srcs {
    list-style: none;
    padding: 0;
    margin: 0 0 16px;
    display: flex;
    flex-direction: column;
    gap: 6px;
  }
  .srcs li {
    padding: 8px 10px;
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
    background: var(--surface-raised);
  }
  .src-head {
    display: flex;
    align-items: center;
    gap: 8px;
  }
  .src-name {
    font-weight: 600;
    font-size: 0.85rem;
    color: var(--text);
  }
  .src-ver {
    font-size: var(--fs-xs);
    color: var(--text-muted);
  }
  .src-action {
    margin-left: auto;
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
    background: var(--surface);
    color: var(--text-muted);
    font: inherit;
    font-size: var(--fs-xs);
    padding: 4px 9px;
    cursor: pointer;
    white-space: nowrap;
  }
  .src-action.can-install {
    background: var(--primary);
    border-color: transparent;
    color: var(--on-primary);
    font-weight: 600;
  }
  .src-action:hover:not(:disabled) {
    filter: brightness(1.06);
  }
  .src-action:disabled {
    opacity: 0.5;
    cursor: default;
  }
  /* Why this source can't be installed directly — names who's responsible. */
  .src-why {
    margin: 6px 0 0;
    font-size: var(--fs-xs);
    line-height: 1.4;
    color: var(--text-muted);
  }
  .drawer-hint {
    font-size: var(--fs-xs);
    line-height: 1.5;
    color: var(--text-muted);
    background: var(--bg);
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
    padding: 9px 11px;
    margin: 0 0 12px;
  }
  .drawer-owned {
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 8px;
    text-align: center;
    color: var(--text-muted);
    font-size: 0.85rem;
    padding: 8px;
  }
  .find-lib-btn {
    border: 1px solid color-mix(in srgb, var(--primary) 45%, var(--border));
    background: var(--surface);
    color: var(--primary-text);
    font: inherit;
    font-weight: 600;
    padding: 7px 14px;
    border-radius: var(--radius-sm);
    cursor: pointer;
  }
  .find-lib-btn:hover {
    background: color-mix(in srgb, var(--primary) 12%, transparent);
    border-color: var(--primary);
  }
</style>
