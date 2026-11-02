// Every launch from Silo (Play, a Find the problem mod round) goes through here (#415): the
// backend refuses while a paused library move left mods FS25 can't open (MOVE_LINKS_BROKEN,
// naming them). Silo then asks, and starts the game only on "Play anyway".
import { launchGame } from "./api";
import { errorCode, toAppError } from "./errors";
import { ask } from "./ui";

/**
 * Start FS25, asking first when a paused library move left mods FS25 can't open.
 * @param expectedProfile as for `launchGame`
 * @returns true when the game was started, false when the user chose not to play
 * @throws whatever else refused the launch (the second try's refusal too)
 */
export async function launchAsking(expectedProfile?: string | null): Promise<boolean> {
  try {
    await launchGame(expectedProfile);
    return true;
  } catch (e) {
    if (errorCode(e) !== "MOVE_LINKS_BROKEN") throw e;
    const go = await ask({
      title: "Play without some of your mods?",
      body: toAppError(e).message,
      confirm: "Play anyway",
      // Starting the game is outside Silo: Enter keeps the safe answer.
      focus: "cancel",
    });
    if (!go) return false;
    await launchGame(expectedProfile, { playAnyway: true });
    return true;
  }
}
