// The foreign-file check after each scan, and the one more scan it can ask for (#436). The check
// notes a file it finds isn't Silo's copy; the scan before it still showed the library's copy at
// that name, so the next scan shows the file as itself. The check after that rescan finds the
// note already there, so it stops at one.

/**
 * The one more scan, started only once no scan cycle runs: a cycle is a scan and the auto-filing
 * after it, whose own scan (a nested cycle) shows what it filed. A rescan started in the middle
 * could show the library from before the filing, with nothing after it to correct that.
 * @param rescan start a scan
 */
export function noteRescanner(rescan: () => void) {
  let due = false;
  let cycles = 0;
  return {
    /** A foreign-file check finished with `found`: rescan when it noted a file just now. */
    checked(found: readonly { newlyNoted: boolean }[]): void {
      if (!found.some((f) => f.newlyNoted)) return;
      if (cycles > 0) due = true;
      else rescan();
    },
    /** A scan cycle started (a scan was let run). */
    scanStarted(): void {
      cycles++;
    },
    /** A scan cycle ended, its auto-filing included: the last one starts a rescan asked for meanwhile. */
    scanEnded(): void {
      cycles = Math.max(0, cycles - 1);
      if (cycles > 0 || !due) return;
      due = false;
      rescan();
    },
  };
}

interface CheckerOptions<T> {
  /** Run the check in this mods folder. */
  detect: (root: string | undefined) => Promise<T[]>;
  /** The mods folder shown now. */
  root: () => string | undefined;
  rescan: () => void;
  /** A check started: the last answer no longer matches the scan. */
  started: () => void;
  /** The latest check of the folder shown answered (`checked` is false when it failed). */
  answered: (found: T[], checked: boolean) => void;
}

/**
 * Foreign-file checks of the folder shown. Only the latest check's answer is shown, and none
 * from a folder switched away from (#358). A superseded check's notes were still written, so its
 * request for a rescan still counts: the newer check finds those notes already there and asks
 * for none.
 */
export function foreignChecker<T extends { newlyNoted: boolean }>(o: CheckerOptions<T>) {
  const notes = noteRescanner(o.rescan);
  let gen = 0;
  return {
    async refresh(): Promise<void> {
      const mine = ++gen;
      const root = o.root();
      o.started();
      let checked = true;
      const found = await o.detect(root).catch(() => ((checked = false), [] as T[]));
      if (o.root() !== root) return;
      notes.checked(found);
      if (mine === gen) o.answered(found, checked);
    },
    /** The folder changed: a check still running answers nothing. */
    drop(): void {
      gen++;
    },
    scanStarted: (): void => notes.scanStarted(),
    scanEnded: (): void => notes.scanEnded(),
  };
}
