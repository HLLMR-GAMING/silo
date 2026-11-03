<script lang="ts">
  // What needs the user's attention, under the primary row: a different build in the mods folder
  // (use it or keep Silo's) or a library copy changed since it was put there (use it or keep the
  // mods-folder one), errors in the last game's log, conflicts with active mods. Each has
  // its own action; nothing shows when all is well.
  import Button from "../../ui/Button.svelte";
  import Icon from "../../ui/Icon.svelte";
  import type { ForeignFile } from "../../api";
  import type { Conflict, LogModHealth } from "../../types";
  import { plural, formatCount } from "../../plural";
  import { conflictKindLabel } from "./drawerText";
  import { displayVersion } from "../../format";

  interface Props {
    foreign: ForeignFile | null;
    /** The dropped-in build is newer than Silo's copy. */
    foreignIsNewer: boolean;
    onAdoptForeign: () => void;
    onRestoreForeign: () => void;
    /** This mod's entry in the last game log, when it logged errors. */
    logErrors: LogModHealth | null;
    /** The last run crashed (it then counts as a Problem). */
    crashed: boolean;
    /** The other mods it conflicts with (warning or worse), and those conflicts. */
    partners: string[];
    conflicts: Conflict[];
    /** The mod's own names, to leave out of "with …". */
    selfNames: string[];
    onSeeLastRun: () => void;
    onSeeConflicts: () => void;
  }

  let {
    foreign,
    foreignIsNewer,
    onAdoptForeign,
    onRestoreForeign,
    logErrors,
    crashed,
    partners,
    conflicts,
    selfNames,
    onSeeLastRun,
    onSeeConflicts,
  }: Props = $props();

  const v = (s: string | null) => (s ? displayVersion(s) : "an unknown version");
  const keepLabel = $derived(
    foreign?.managedVersion ? `Keep Silo's ${displayVersion(foreign.managedVersion)}` : "Keep Silo's copy",
  );
  const others = (c: Conflict) => c.mods.filter((m) => !selfNames.includes(m)).join(", ");
</script>

{#if foreign || logErrors || partners.length > 0}
  <div class="banners">
    {#if foreign?.archiveChanged}
      <div class="banner info" role="group" aria-label="The library copy changed">
        <p class="msg">
          <Icon name="info" size={14} />
          <b>The copy in your library changed</b>: it's {v(foreign.managedVersion)} now, but your mods folder still has
          {v(foreign.flatVersion)}, the copy Silo put there before.
        </p>
        <div class="row">
          <Button size="sm" variant="primary" icon="update" onclick={onRestoreForeign}>
            {foreign.managedVersion ? `Use the library's ${displayVersion(foreign.managedVersion)}` : "Use the library's copy"}
          </Button>
          <Button size="sm" onclick={onAdoptForeign}>
            {foreign.flatVersion ? `Keep ${displayVersion(foreign.flatVersion)}` : "Keep the mods-folder copy"}
          </Button>
        </div>
      </div>
    {:else if foreign}
      <div class="banner info" role="group" aria-label="A different build">
        <p class="msg">
          <Icon name="info" size={14} />
          <b>{foreignIsNewer ? "A newer build is in your mods folder" : "A different build is in your mods folder"}</b>:
          {v(foreign.flatVersion)}. Silo keeps {foreign.managedVersion ? displayVersion(foreign.managedVersion) : "its own copy"}. This is how ModHub updates
          arrive: you download the zip into the folder.
        </p>
        <div class="row">
          <Button size="sm" variant="primary" icon="update" onclick={onAdoptForeign}>Use this version</Button>
          <Button size="sm" onclick={onRestoreForeign}>{keepLabel}</Button>
        </div>
      </div>
    {/if}

    {#if logErrors}
      <div class="banner danger" role="group" aria-label="Last game">
        <p class="msg">
          <Icon name="problem" size={14} />
          <b>Logged {formatCount(logErrors.errors)} {plural(logErrors.errors, { one: "error", other: "errors" })} in your last game</b>{crashed
            ? ", and that game crashed."
            : "."}
        </p>
        {#if logErrors.sample}<p class="sample">{logErrors.sample}</p>{/if}
        <div class="row"><Button size="sm" onclick={onSeeLastRun}>See in Diagnose</Button></div>
      </div>
    {/if}

    {#if partners.length > 0}
      <div class="banner warn" role="group" aria-label="Conflicts">
        <p class="msg">
          <Icon name="problem" size={14} />
          <b>Conflicts with {formatCount(partners.length)} active {plural(partners.length, { one: "mod", other: "mods" })}</b>
        </p>
        <ul class="list">
          {#each conflicts as c (c.kind + c.name)}
            <li class:crit={c.severity === "critical"}>
              <span class="sev">{c.severity === "critical" ? "Problem" : "Review"}</span>
              {conflictKindLabel(c.kind)} “{c.name}”, with {others(c)}
            </li>
          {/each}
        </ul>
        <div class="row"><Button size="sm" onclick={onSeeConflicts}>See conflicts</Button></div>
      </div>
    {/if}
  </div>
{/if}

<style>
  .banners {
    display: grid;
    gap: var(--sp-2);
    margin-top: var(--sp-4);
  }
  .banner {
    padding: var(--sp-3);
    border-radius: var(--radius-sm);
    border: 1px solid var(--border);
    font-size: var(--fs-xs);
    line-height: 1.5;
  }
  .banner.info {
    background: var(--info-soft);
  }
  .banner.warn {
    background: var(--warn-soft);
  }
  .banner.danger {
    background: var(--danger-soft);
  }
  .msg {
    margin: 0;
    color: var(--text);
  }
  .msg :global(svg) {
    vertical-align: -2px;
    margin-right: 2px;
  }
  .warn .msg :global(svg) {
    color: var(--warn-text);
  }
  .danger .msg :global(svg) {
    color: var(--danger);
  }
  .sample {
    margin: var(--sp-1) 0 0;
    font-family: var(--font-mono);
    color: var(--text-muted);
    overflow-wrap: anywhere;
  }
  .list {
    margin: var(--sp-1) 0 0;
    padding-left: var(--sp-4);
  }
  .sev {
    font-weight: 700;
    color: var(--warn-text);
    margin-right: 4px;
  }
  .crit .sev {
    color: var(--danger);
  }
  .row {
    display: flex;
    flex-wrap: wrap;
    gap: var(--sp-2);
    margin-top: var(--sp-2);
  }
</style>
