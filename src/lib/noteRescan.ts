// One more scan after a foreign-file check noted a file as not Silo's copy (#436). The scan
// before the check still showed the library's copy at that name; the next one shows the file as
// itself. The check after that rescan finds the note already there, so it stops at one.

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
