<script lang="ts">
  // About: what Silo's catalog says about the mod (cover, summary, where it's available). Hidden
  // when the mod isn't in the catalog. Read more opens the full description in a dialog.
  import Button from "../../ui/Button.svelte";
  import Dialog from "../../ui/Dialog.svelte";
  import Icon from "../../ui/Icon.svelte";
  import type { CatalogModDetail } from "../../types";
  import { label as sourceLabel, loadCatalogImage } from "../../browse";
  import { displayVersion } from "../../format";
  import { openExternal } from "../../api";

  let { catalog, title }: { catalog: CatalogModDetail | null; title: string } = $props();

  const uid = $props.id();
  let showFull = $state(false);
  let cover = $state("");
  /** The cover element shown now: an error from an earlier one (a previous mod's) is ignored. */
  let coverEl = $state<HTMLImageElement | null>(null);
  $effect(() => {
    const url = catalog?.imageUrl;
    cover = "";
    showFull = false;
    if (url)
      loadCatalogImage(url)
        .then((u) => {
          if (catalog?.imageUrl === url) cover = u;
        })
        .catch(() => {}); // no cover is fine
  });

  const summary = $derived((catalog?.descriptionFull || catalog?.description || "").trim());
  const hasMore = $derived(summary.length > 420);
</script>

{#if catalog && (cover || summary || catalog.sources?.length)}
  <section class="sec" aria-labelledby="{uid}-about">
    <h3 id="{uid}-about" class="h">About</h3>
    {#if cover}<img class="cover" src={cover} alt="" bind:this={coverEl}
        onerror={(e) => e.currentTarget === coverEl && (cover = "")} />{/if}
    {#if summary}
      <p class="summary">{summary}</p>
      {#if hasMore}
        <Button variant="link" size="sm" onclick={() => (showFull = true)}>Read more</Button>
      {/if}
    {/if}
    {#if catalog.sources?.length}
      <div class="sources">
        <span class="src-label">Available on</span>
        {#each catalog.sources as s (s.source + s.sourceUrl)}
          <button class="src" onclick={() => openExternal(s.sourceUrl)}>
            {sourceLabel(s.source)}{s.version ? ` · ${displayVersion(s.version)}` : ""}
            <Icon name="external" size={12} />
            <span class="vh">(opens in your browser)</span>
          </button>
        {/each}
      </div>
    {/if}
  </section>
{/if}

{#if catalog}
  <Dialog bind:open={showFull} {title} size="lg">
    <div class="full">{catalog.descriptionFull || catalog.description}</div>
    {#snippet footer()}
      <Button variant="primary" onclick={() => (showFull = false)}>Close</Button>
    {/snippet}
  </Dialog>
{/if}

<style>
  .sec {
    padding: var(--sp-3) 0;
    border-top: 1px solid var(--border);
    display: flex;
    flex-direction: column;
    gap: var(--sp-2);
    align-items: flex-start;
  }
  .h {
    margin: 0;
    font-size: var(--fs-xs);
    text-transform: uppercase;
    letter-spacing: 0.04em;
    color: var(--text-muted);
    font-weight: 700;
  }
  .cover {
    width: 100%;
    border-radius: var(--radius-sm);
    border: 1px solid var(--border);
    display: block;
  }
  .summary {
    margin: 0;
    font-size: var(--fs-xs);
    line-height: 1.55;
    color: var(--text);
    white-space: pre-wrap;
    display: -webkit-box;
    -webkit-line-clamp: 20;
    line-clamp: 20;
    -webkit-box-orient: vertical;
    overflow: hidden;
  }
  .sources {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 6px;
  }
  .src-label {
    font-size: var(--fs-xs);
    color: var(--text-muted);
    font-weight: 600;
  }
  .src {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    border: 1px solid var(--border);
    background: var(--bg);
    color: var(--text);
    padding: 4px 9px;
    border-radius: var(--radius-sm);
    font: inherit;
    font-size: var(--fs-xs);
    cursor: pointer;
  }
  .src:hover {
    border-color: color-mix(in srgb, var(--primary) 40%, var(--border));
  }
  .full {
    font-size: var(--fs-sm);
    line-height: 1.6;
    white-space: pre-wrap;
    color: var(--text);
  }
  .vh {
    position: absolute;
    width: 1px;
    height: 1px;
    overflow: hidden;
    clip-path: inset(50%);
    white-space: nowrap;
  }
</style>
