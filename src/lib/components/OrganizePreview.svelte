<script lang="ts">
  // The Organize dialog (#250): a summary of the dry run, the files grouped by category behind
  // "See every file", "File new mods automatically from now on", progress in the footer while
  // filing, and the mods that couldn't be moved (with Copy details) when some fail.
  //   <OrganizePreview {plan} {loading} {applying} {progress} {failures} activeCount={142}
  //     onConfirm={(autoFile) => ...} onCancel={...} onCopyDetails={(text) => ...} />
  // Adopting loose mods already in a custom library archive (#166) uses the same dialog without
  // the auto-file choice.
  import Button from "../ui/Button.svelte";
  import Checkbox from "../ui/Checkbox.svelte";
  import Dialog from "../ui/Dialog.svelte";
  import ProgressBar from "../ui/ProgressBar.svelte";
  import OrganizeSummary from "./organize/OrganizeSummary.svelte";
  import OrganizeFiles from "./organize/OrganizeFiles.svelte";
  import type { PlannedMove } from "../types";
  import { failureDetails, filingProgress, nMods, type FilingFailure } from "../firstRun/firstRun";
  import { formatCount } from "../plural";

  let {
    plan,
    loading,
    applying,
    progress = null,
    failures = [],
    filed: filedCount = null,
    problem = "",
    blockedReason = "",
    cloudSynced = false,
    activeCount = 0,
    folderMods = 0,
    autoFile = false,
    onConfirm,
    onCancel,
    onCopyDetails,
    adopt = null,
  }: {
    plan: PlannedMove[];
    loading: boolean;
    applying: boolean;
    /** Filing progress (mods done of total) while `applying`. */
    progress?: { done: number; total: number } | null;
    /** Mods that couldn't be filed: the dialog stays open and lists them. */
    failures?: FilingFailure[];
    /** How many were filed, with `failures` (else plan length minus failures). */
    filed?: number | null;
    /** Why the last try filed nothing (the game is running…), shown above the buttons. */
    problem?: string;
    /** Why filing can't start right now (a scan, another change), or "". */
    blockedReason?: string;
    cloudSynced?: boolean;
    activeCount?: number;
    folderMods?: number;
    /** Auto-file is on already (the checkbox starts ticked). */
    autoFile?: boolean;
    /** `autoFile`: the user ticked "File new mods automatically from now on". */
    onConfirm: (autoFile: boolean) => void;
    onCancel: () => void;
    onCopyDetails?: (text: string) => void;
    /** Adopting loose mods already sitting in a custom library archive (#166): `path` is that
     *  folder, `note` an info line (e.g. duplicates left untouched). */
    adopt?: { path: string | null; note: string | null } | null;
  } = $props();

  // plan_organize's relTo is `archive/<Category>/<file>`: right for the default archive, but an
  // adopted mod lands in a custom archive folder, so show the path inside that folder instead.
  const dest = (mv: PlannedMove) => (adopt ? mv.relTo.replace(/^archive\//, "") : mv.relTo);

  // svelte-ignore state_referenced_locally
  let autoFileNext = $state(autoFile);
  let open = $state(true);

  const failed = $derived(new Set(failures.map((f) => f.techName).filter(Boolean)));
  const filed = $derived(filedCount ?? Math.max(0, plan.length - failures.length));
  // The file lists render only while their disclosure is open (hundreds of rows).
  let filesOpen = $state(false);
  const title = $derived(adopt ? `File ${nMods(plan.length)} into your library` : "Organize new mods");
  const fileLabel = $derived(`File ${nMods(plan.length)}`);
</script>

<Dialog
  bind:open
  {title}
  size="lg"
  dismissible={!applying}
  onclose={() => onCancel()}
>
  {#if loading}
    <p class="muted">Working out what would move…</p>
  {:else if failures.length > 0}
    <div class="failed" role="alert">
      <p class="lead">
        Silo filed {formatCount(filed)} of {nMods(plan.length)}. {formatCount(failures.length)}
        couldn't be moved:
      </p>
      <ul class="fail-list">
        {#each failures as f, i (i)}
          <li><span class="fn">{f.fileName || f.techName || "A mod"}</span>: {f.message}</li>
        {/each}
      </ul>
      <p class="muted">The rest are in your library. Silo moved none of the ones above.</p>
    </div>
    <details class="files" bind:open={filesOpen}>
      <summary>See every file</summary>
      {#if filesOpen}<OrganizeFiles {plan} {failed} {dest} />{/if}
    </details>
  {:else if plan.length === 0}
    <p class="muted">Nothing to organize. Every loose mod is already filed.</p>
  {:else if adopt}
    <p class="lead">
      Your library archive{#if adopt.path}{" "}(<code>{adopt.path}</code>){/if} already holds loose
      mod zips. Silo can file them into its category folders there so they show up in your library
      (parked, so switch them on as usual). Nothing is overwritten, and nothing has moved yet.
    </p>
    {#if adopt.note}<p class="muted">{adopt.note}</p>{/if}
    <OrganizeFiles {plan} {dest} />
  {:else}
    <OrganizeSummary {plan} {activeCount} {folderMods} {cloudSynced} />
    <details class="files" bind:open={filesOpen}>
      <summary>See every file</summary>
      {#if filesOpen}<OrganizeFiles {plan} {dest} />{/if}
    </details>
    <Checkbox
      class="auto"
      bind:checked={autoFileNext}
      disabled={applying}
      label="File new mods automatically from now on"
    />
  {/if}
  {#if !loading && failures.length === 0 && (problem || blockedReason)}
    <p class="problem" role={problem ? "alert" : "status"}>{problem || blockedReason}</p>
  {/if}

  {#snippet footer()}
    {#if applying}
      {@const bar = filingProgress(progress?.done ?? 0, progress?.total ?? 0)}
      <ProgressBar class="foot-progress" value={bar.value} max={bar.max} label={bar.label} />
    {:else if failures.length > 0}
      <Button
        icon="copy"
        onclick={() => onCopyDetails?.(failureDetails(failures, filed, plan.length))}>Copy details</Button
      >
      <Button variant="primary" onclick={() => (open = false)}>Close</Button>
    {:else}
      <Button variant="ghost" onclick={() => (open = false)}>Cancel</Button>
      <Button
        variant="primary"
        disabled={loading || plan.length === 0 || !!blockedReason}
        title={blockedReason || undefined}
        onclick={() => onConfirm(!adopt && autoFileNext)}>{fileLabel}</Button
      >
    {/if}
  {/snippet}
</Dialog>

<style>
  .lead {
    margin: 0 0 var(--sp-3);
    line-height: 1.55;
  }
  .lead code {
    font-family: var(--font-mono);
    font-size: var(--fs-xs);
    background: var(--bg);
    padding: 1px 4px;
    border-radius: 4px;
  }
  .muted {
    margin: 0;
    color: var(--muted);
  }
  .files {
    margin: var(--sp-3) 0;
  }
  .files summary {
    cursor: pointer;
    color: var(--primary-text);
    font-weight: 600;
    font-size: var(--fs-sm);
    margin-bottom: var(--sp-2);
  }
  .fail-list {
    margin: 0 0 var(--sp-2);
    padding-left: var(--sp-5);
    max-height: 12rem;
    overflow-y: auto;
    font-size: var(--fs-sm);
  }
  .fn {
    font-weight: 600;
  }
  .problem {
    margin: var(--sp-3) 0 0;
    padding: var(--sp-2) var(--sp-3);
    border-radius: var(--r-md);
    background: var(--warn-soft);
    color: var(--warn-text);
    font-size: var(--fs-sm);
  }
  :global(.foot-progress) {
    flex: 1 1 auto;
  }
</style>
