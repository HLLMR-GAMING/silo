<script lang="ts">
  // Diagnose → Problems (was Library health): mods that won't load as they are, each with what
  // to do about it.
  //   <ProblemsPanel {health} isActive={(t) => active.has(t)} onFindInBrowse={find}
  //     onShowFile={reveal} onPark={park} onResolveForeign={resolveForeign} />
  // Groups: missing a required mod [Find in Browse], can't be read [Show file] [Park], ignored
  // by the game because the name starts with a digit [Show file], and a different build in the
  // mods folder [Use this version] [Keep Silo's].
  import Button from "../../ui/Button.svelte";
  import { formatCount, plural } from "../../plural";
  import type { ModEntry } from "../../types";
  import { displayVersion } from "../../format";

  interface ForeignFile {
    techName: string;
    fileName: string;
    kind: string;
    flatVersion: string | null;
    managedVersion: string | null;
  }

  interface Props {
    health: {
      missingDeps: { mod: ModEntry; missing: string[] }[];
      corrupt: ModEntry[];
      ignored: ModEntry[];
      foreign: ForeignFile[];
      /** Mods found more than once (#423): the game loads only one copy. */
      duplicates?: { techName: string; copies: ModEntry[] }[];
    };
    /** Whether this copy is the one the game loads now. */
    loadsNow?: (m: ModEntry) => boolean;
    /** Keep `keep` and remove the other copies (App confirms; they go to the Recycle Bin). */
    onKeepCopy?: (keep: ModEntry, others: ModEntry[]) => void;
    /** Whether a mod is active (Park shows only for active mods). */
    isActive?: (techName: string) => boolean;
    onFindInBrowse?: (techName: string) => void;
    onShowFile?: (path: string) => void;
    onPark?: (techName: string) => void;
    onResolveForeign: (fileName: string, action: "adopt" | "restore") => Promise<unknown>;
    /** Why library writes are off right now (Park and the foreign-file choices), or "". */
    lockReason?: string;
  }

  let {
    health,
    isActive = () => false,
    onFindInBrowse,
    onShowFile,
    onPark,
    onResolveForeign,
    lockReason = "",
    loadsNow = () => false,
    onKeepCopy,
  }: Props = $props();
  const duplicates = $derived(health.duplicates ?? []);
  /** Where a copy is, in words. */
  const where = (m: ModEntry) => (m.organized ? "in your library" : "in your mods folder");
  const entry = (m: ModEntry) => m.path.split(/[\\/]/).pop() ?? m.path;

  const total = $derived(
    health.missingDeps.length +
      health.corrupt.length +
      health.ignored.length +
      health.foreign.length +
      (health.duplicates?.length ?? 0),
  );

  /** The file whose choice is running (its buttons are off meanwhile). */
  let pendingFile = $state<string | null>(null);
  async function resolve(fileName: string, action: "adopt" | "restore") {
    pendingFile = fileName;
    try {
      await onResolveForeign(fileName, action);
    } finally {
      pendingFile = null;
    }
  }

  const name = (m: ModEntry) => m.title ?? m.techName;
  const ver = (v: string | null) => (v ? displayVersion(v) : "unknown version");
</script>

<section class="problems-panel" aria-labelledby="problems-head">
  <div class="lp-head">
    <h3 id="problems-head" class="lp-title tnum">
      <!-- Not "problems": the status chip counts only active mods as problems, so this says what it is. -->
      <strong>{total === 0 ? "Nothing needs attention" : `${formatCount(total)} ${plural(total, { one: "mod needs", other: "mods need" })} attention`}</strong>
    </h3>
  </div>
  {#if lockReason}<p class="lock">{lockReason}</p>{/if}

  {#if total === 0}
    <p class="lp-empty">Every mod in your library can load. Nothing needs fixing.</p>
  {/if}

  {#if health.missingDeps.length > 0}
    <h4 class="hz-group">Missing a required mod ({formatCount(health.missingDeps.length)})</h4>
    <ul class="rows">
      {#each health.missingDeps as d (d.mod.techName)}
        <li class="hz-row">
          <div class="hz-name">{name(d.mod)}</div>
          <p class="hz-detail">
            Needs {d.missing.join(", ")}, which {d.missing.length === 1 ? "isn't" : "aren't"} in your library. The game
            won't load it without {d.missing.length === 1 ? "it" : "them"}.
          </p>
          {#if onFindInBrowse}
            <div class="hz-actions">
              {#each d.missing as dep (dep)}
                <Button size="sm" icon="browse" onclick={() => onFindInBrowse(dep)}>
                  Find {d.missing.length > 1 ? dep : "it"} in Browse
                </Button>
              {/each}
            </div>
          {/if}
        </li>
      {/each}
    </ul>
  {/if}

  {#if duplicates.length > 0}
    <h4 class="hz-group">More than one copy of a mod ({formatCount(duplicates.length)})</h4>
    <ul class="rows">
      {#each duplicates as g (g.techName)}
        <li class="hz-row">
          <div class="hz-name">{name(g.copies[0])}</div>
          <p class="hz-detail">
            The game loads only one copy of a mod. Keep the one you want; the other {g.copies.length === 2
              ? "copy goes"
              : "copies go"} to the Recycle Bin.
          </p>
          <ul class="copies">
            {#each g.copies as c (c.path)}
              <li class="copy">
                <span class="copy-what">
                  <code>{entry(c)}</code>, {where(c)}, {ver(c.version)}{#if loadsNow(c)}<span class="loads">
                      · the game loads this one</span
                    >{/if}
                </span>
                {#if onKeepCopy}
                  <Button
                    size="sm"
                    disabled={!!lockReason}
                    onclick={() => onKeepCopy(c, g.copies.filter((o) => o.path !== c.path))}>Keep this one</Button
                  >
                {/if}
              </li>
            {/each}
          </ul>
        </li>
      {/each}
    </ul>
  {/if}

  {#if health.corrupt.length > 0}
    <h4 class="hz-group">Can't be read ({formatCount(health.corrupt.length)})</h4>
    <ul class="rows">
      {#each health.corrupt as m (m.techName)}
        <li class="hz-row">
          <div class="hz-name">{name(m)}</div>
          <p class="hz-detail">Silo couldn't open this file, so the game probably can't either. {m.error}</p>
          <div class="hz-actions">
            {#if onShowFile}<Button size="sm" icon="showFile" onclick={() => onShowFile(m.path)}>Show file</Button>{/if}
            {#if onPark && isActive(m.techName)}
              <Button size="sm" disabled={!!lockReason} onclick={() => onPark(m.techName)}>Park</Button>
            {/if}
          </div>
        </li>
      {/each}
    </ul>
  {/if}

  {#if health.ignored.length > 0}
    <h4 class="hz-group">Ignored by the game: the name starts with a number ({formatCount(health.ignored.length)})</h4>
    <ul class="rows">
      {#each health.ignored as m (m.techName)}
        <li class="hz-row">
          <div class="hz-name">{name(m)}</div>
          <p class="hz-detail">
            The game skips any mod whose file name starts with a number, like <code>{m.techName}</code>. Renaming the file
            would change its Mod ID, so ask its author for a fixed build.
          </p>
          {#if onShowFile}
            <div class="hz-actions">
              <Button size="sm" icon="showFile" onclick={() => onShowFile(m.path)}>Show file</Button>
            </div>
          {/if}
        </li>
      {/each}
    </ul>
  {/if}

  {#if health.foreign.length > 0}
    <h4 class="hz-group">A different build is in your mods folder ({formatCount(health.foreign.length)})</h4>
    <ul class="rows">
      {#each health.foreign as f (f.fileName)}
        <li class="hz-row">
          <div class="hz-name"><code>{f.fileName}</code></div>
          <p class="hz-detail">
            The file in your mods folder ({ver(f.flatVersion)}) isn't the one Silo keeps ({ver(f.managedVersion)}). Pick
            which build to keep. The other is moved to a backup folder, so nothing is lost.
          </p>
          <div class="hz-actions">
            <Button
              size="sm"
              variant="primary"
              disabled={pendingFile === f.fileName || !!lockReason}
              onclick={() => resolve(f.fileName, "adopt")}
            >
              Use this version
            </Button>
            <Button size="sm" disabled={pendingFile === f.fileName || !!lockReason} onclick={() => resolve(f.fileName, "restore")}>
              Keep Silo's copy
            </Button>
          </div>
        </li>
      {/each}
    </ul>
  {/if}
</section>

<style>
  .problems-panel {
    max-width: 48rem;
    background: var(--surface-raised);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    padding: 10px;
  }
  .lp-head {
    padding: 6px 8px 10px;
  }
  .lp-title {
    margin: 0;
    font-family: var(--font-display);
    font-size: 1rem;
    font-weight: 500;
  }
  .lock {
    margin: 0 8px 8px;
    color: var(--text-muted);
    font-size: var(--fs-sm);
  }
  .lp-empty {
    margin: 0;
    padding: 10px 8px 14px;
    font-size: var(--fs-sm);
    color: var(--text-muted);
    line-height: 1.5;
  }
  .hz-group {
    margin: 12px 8px 6px;
    font-size: var(--fs-xs);
    font-weight: 600;
    text-transform: uppercase;
    letter-spacing: 0.05em;
    color: var(--text-muted);
  }
  .rows {
    list-style: none;
    margin: 0;
    padding: 0;
  }
  .hz-row {
    padding: 9px 10px 11px;
    border-radius: var(--radius-sm);
    border-left: 3px solid var(--danger);
    background: color-mix(in srgb, var(--danger) 5%, transparent);
    margin-bottom: 8px;
  }
  .hz-name {
    font-weight: 600;
    font-family: var(--font-display);
  }
  .hz-detail {
    margin: 4px 0 0;
    font-size: var(--fs-sm);
    color: var(--text-muted);
    line-height: 1.5;
  }
  .hz-actions {
    display: flex;
    gap: 6px;
    flex-wrap: wrap;
    margin-top: 8px;
  }
  .copies {
    list-style: none;
    margin: var(--space-2, 8px) 0 0;
    padding: 0;
    display: grid;
    gap: var(--space-2, 8px);
  }
  .copy {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--space-3, 12px);
  }
  .copy-what {
    min-width: 0;
    overflow-wrap: anywhere;
  }
  .loads {
    color: var(--text-muted, inherit);
  }
</style>
