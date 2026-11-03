// The drawer's ⋯ menu: the per-mod actions that aren't on the primary row. Pure, so the labels
// (which follow the mod's curation state, like the Library row menu) unit-test.
import type { MenuItem } from "../../ui/overlayTypes";
import type { CurationRow } from "../../types";

export type DrawerMenuId = "favorite" | "broken" | "hidden" | "category" | "showFile" | "browse" | "remove";

/**
 * The menu items for one mod.
 * @param c the mod's favorite / hidden / flag state
 */
export function drawerMenuItems(c: Pick<CurationRow, "favorite" | "hidden" | "broken">): MenuItem[] {
  return [
    { id: "favorite", label: c.favorite ? "Remove favorite" : "Favorite", icon: "favorite" },
    { id: "broken", label: c.broken ? "Remove flag" : "Flag", icon: "flagged", danger: !c.broken },
    { id: "hidden", label: c.hidden ? "Unhide" : "Hide", icon: c.hidden ? "visible" : "hidden" },
    { id: "category", label: "Change category…", icon: "tag", separatorBefore: true },
    { id: "showFile", label: "Show file", icon: "showFile" },
    { id: "browse", label: "Find in Browse", icon: "search" },
    // #429: the file goes to the Recycle Bin, behind a confirmation.
    { id: "remove", label: "Remove from library…", icon: "remove", danger: true, separatorBefore: true },
  ];
}
