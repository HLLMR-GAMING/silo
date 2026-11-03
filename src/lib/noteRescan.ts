// The foreign-file check after each scan, and the one more scan it can ask for (#436). The check
// notes a file it finds isn't Silo's copy; the scan before it still showed the library's copy at
// that name, so the next scan shows the file as itself. The check after that rescan finds the
// note already there, so it stops at one.

/**
 * @param isScanning whether a scan is running now (it is then followed by one more)
 * @param rescan start a scan
 */
export function noteRescanner(isScanning: () => boolean, rescan: () => void) {
  let due = false;
  return {
    /** A foreign-file check finished with `found`: rescan when it noted a file just now. */
    checked(found: readonly { newlyNoted: boolean }[]): void {
      if (!found.some((f) => f.newlyNoted)) return;
      if (isScanning()) due = true;
      else rescan();
    },
    /** A scan ended: start the one a check asked for while it ran. */
    scanEnded(): void {
      if (!due) return;
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
  isScanning: () => boolean;
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
  const notes = noteRescanner(o.isScanning, o.rescan);
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
    scanEnded: (): void => notes.scanEnded(),
  };
}
