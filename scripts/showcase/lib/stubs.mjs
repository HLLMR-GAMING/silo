// The demo profile's canned answers for every command that would go online (src-tauri/src/
// profile.rs reads them from <profile>/stubs.json). Built from the staged library, so Updates,
// Browse, provenance and the Share dialog show believable rows. No real account or token: the
// "connected" GitHub user is a made-up name, and every link points at a placeholder address.
//   writeFileSync("stubs.json", JSON.stringify(buildStubs(lib)));
import { iconPng } from "./png.mjs";
import { rng } from "./catalog.mjs";

const DEMO_USER = "silo-demo";
const OFF = "The demo profile is offline: this needs the internet.";
// Made-up gist ids (32 hex characters, like GitHub's).
const SHARED_GIST = "5f1c0de5a1b04c7e9d2f8a6b3c1e7d40";
const NEW_GIST = "9a3e7c41d2b8f05e6a1c7d3b9e2f4a68";
const sharePage = (id) => `https://silo.hllmr.com/c/?g=${id}`;

/** "1.2.0.0" -> "1.3.0.0" (minor bump) or "1.2.1.0" (patch). */
function bump(v, patch) {
  const p = v.split(".").map(Number);
  if (patch) p[2] += 1;
  else {
    p[1] += 1;
    p[2] = 0;
  }
  return p.join(".");
}

const modhubPage = (n) => `https://www.farming-simulator.com/mod.php?mod_id=${310000 + n}&title=fs2025`;
const ghRelease = (m, v) => `https://github.com/${m.gh.owner}/${m.gh.repo}/releases/download/v${v}/${m.techName}.zip`;

/**
 * @param {{ mods: object[], roles: Record<string, string> }} lib the staged library
 * @returns {Record<string, object>} stubs.json content
 */
export function buildStubs(lib) {
  const r = rng(99);
  const byTech = new Map(lib.mods.map((m) => [m.techName, m]));
  const { roles } = lib;

  // ── Updates: most mods tracked; a few GitHub releases ready, a few ModHub pages newer. ──
  const updatable = new Set();
  const gh = lib.mods.filter((m) => m.gh && m.active && m.techName !== roles.conflictB).slice(0, 3);
  const mh = lib.mods.filter((m) => m.source === "modhub" && m.active && !m.isMap && ![roles.verified, roles.modified, roles.culprit].includes(m.techName)).filter((_, i) => i % 31 === 7).slice(0, 4);
  for (const m of [...gh, ...mh]) updatable.add(m.techName);
  const updates = lib.mods
    .filter((m) => m.source !== "other")
    .map((m, i) => {
      const has = updatable.has(m.techName);
      const latest = has ? bump(m.version, i % 2 === 0) : m.version;
      const isGh = !!m.gh;
      return {
        techName: m.techName,
        latest,
        hasUpdate: has,
        downloadUrl: isGh && has ? ghRelease(m, latest) : null,
        source: isGh ? "github" : "modhub",
        pageUrl: isGh ? `https://github.com/${m.gh.owner}/${m.gh.repo}` : modhubPage(i),
      };
    });

  // ── Browse: a catalog page mixing installed, updatable and new mods. ──
  // Images are inline data: URLs, so the webview never fetches anything.
  const card = (m, i, installedVersion) => {
    const url = `data:image/png;base64,${iconPng(m.glyph, m.hue, 480, 240).toString("base64")}`;
    const latest = installedVersion && updatable.has(m.techName) ? bump(m.version, i % 2 === 0) : m.version;
    const sources = [];
    if (m.gh) sources.push({ source: "github", sourceUrl: `https://github.com/${m.gh.owner}/${m.gh.repo}`, version: latest, installable: true, downloadUrl: ghRelease(m, latest), stars: 20 + Math.floor(r() * 400) });
    if (!m.gh || r() < 0.3) sources.push({ source: "modhub", sourceUrl: modhubPage(i), version: latest, installable: false, downloadUrl: null, downloads: 2000 + Math.floor(r() * 90000), rating: Math.round((3.6 + r() * 1.4) * 10) / 10 });
    return {
      id: `demo-${i}`,
      techName: m.techName,
      title: m.title,
      author: m.author,
      category: m.store ?? (m.isMap ? "maps" : "gameplay"),
      description: `${m.title} by ${m.author}.`,
      imageUrl: url,
      latestVersion: latest,
      trustScore: null,
      verified: !!m.gh,
      verifiedAuthor: null,
      updatedAt: `2026-${String(1 + Math.floor(r() * 9)).padStart(2, "0")}-${String(1 + Math.floor(r() * 27)).padStart(2, "0")}T12:00:00Z`,
      downloads: 1500 + Math.floor(r() * 120000),
      rating: Math.round((3.5 + r() * 1.5) * 10) / 10,
      ratingCount: 10 + Math.floor(r() * 900),
      popularity: r(),
      sources,
      pageUrl: sources[0].sourceUrl,
      tags: [],
    };
  };
  // Catalog-only mods (not in the library) are new fictional titles of the same kinds.
  const fresh = ["Ostreg 6M 175 Pack", "Lindqvist Seed Drill 8m", "Riverside Grain Elevator", "Corvane Bale Trailer 12", "Selden Mulcher 3m", "Holmgard Forestry Trailer", "Valmora Stone Picker", "Pellham Liquid Manure Pack", "Greyhurst Wheel Loader 950", "Old Mill Farmhouse", "Brannock Hay Rake 7m", "Aldervik Compact Tractor 25"].map((title, i) => ({
    techName: `FS25_${title.replace(/[^A-Za-z0-9]/g, "")}`,
    title,
    author: ["Copperleaf", "Grainline", "Westfold Agri", "Thistle Modding"][i % 4],
    version: ["1.0.0.0", "1.1.0.0", "1.0.2.0"][i % 3],
    glyph: ["tractor", "implement", "building", "trailer", "implement", "trailer", "implement", "trailer", "loader", "building", "implement", "tractor"][i],
    hue: (i * 47) % 360,
    store: null,
    gh: i % 3 === 0 ? { owner: "grainline-mods", repo: title.replace(/[^A-Za-z0-9]/g, "") } : null,
  }));
  const installedPicks = lib.mods.filter((m) => !m.isMap && m.source !== "other" && !updatable.has(m.techName)).filter(() => r() < 0.05).slice(0, 18);
  const browseMods = [];
  installedPicks.forEach((m, i) => {
    browseMods.push(card(m, browseMods.length, m.version));
    if (i < fresh.length) browseMods.push(card(fresh[i], browseMods.length, null));
  });
  // Mods with an update, spread through the page rather than bunched together.
  [...gh, ...mh].filter((m) => !browseMods.some((b) => b.techName === m.techName)).forEach((m, i) => browseMods.splice(3 + i * 5, 0, card(m, browseMods.length, m.version)));

  const verifiedMod = byTech.get(roles.verified);
  const modifiedMod = byTech.get(roles.modified);
  const base = (m) => m.techName.replace(/^FS25_/, "");

  return {
    // GitHub: a fake connected account (no token exists anywhere).
    gh_status: { value: { clientId: "demo", user: DEMO_USER, builtin: true, canWrite: true, canGist: true } },
    secret_storage_secure: { value: true },
    gh_repo_stats: { value: { fullName: `${DEMO_USER}/demo`, htmlUrl: "https://github.com/", stars: 142, forks: 9, watchers: 11, openIssues: 3, archived: false, pushedAt: "2026-10-02T09:00:00Z", youStarred: false, youWatching: false } },
    gh_star: { value: null },
    gh_watch: { value: null },
    gh_device_start: { error: OFF },
    gh_device_poll: { error: OFF },
    gh_set_pat: { error: OFF },
    check_mod_update: { value: { hasUpdate: false, current: "1.0.0.0", release: { tag: "v1.0.0.0", name: null, publishedAt: null, htmlUrl: null, assetUrl: null, assetName: null } } },
    set_mod_repo: { error: OFF },
    download_update: { error: OFF },
    install_remote_mod: { error: OFF },
    // Catalog.
    siloapi_status: { value: "https://silo-api.hllmr.com" },
    siloapi_stats: { value: { mods: 4186, sources: 5310 } },
    siloapi_facets: { value: { facets: {} } },
    siloapi_categories: { value: [["tractorsM", 412], ["trailers", 388], ["productionPoints", 241], ["cultivators", 204], ["trucks", 167], ["maps", 96]].map(([category, count]) => ({ category, count })) },
    browse_mods: { value: { mods: browseMods, total: 4186 } },
    siloapi_mod_detail: { byArg: "id", cases: Object.fromEntries(browseMods.map((b) => [b.id, b])), value: browseMods[0] },
    catalog_detail_by_tech: { byArg: "techName", cases: Object.fromEntries(browseMods.filter((b) => byTech.has(b.techName)).map((b) => [b.techName, b])), value: null },
    catalog_image: { error: OFF },
    catalog_check_updates: { value: updates },
    // Provenance: one build matches the catalog's hash, one was re-packed.
    verify_mod: {
      byArg: "techName",
      cases: {
        [verifiedMod.techName]: { status: "verified", how: "content", added: [], removed: [], changed: [], matchedVersion: verifiedMod.version, note: null },
        [modifiedMod.techName]: { status: "modified", how: null, added: [`textures/${base(modifiedMod)}_decal.dds`], removed: [], changed: [`xml/${base(modifiedMod)}.xml`, `textures/${base(modifiedMod)}_diffuse.dds`], matchedVersion: modifiedMod.version, note: null },
      },
      value: { status: "unverified", how: null, added: [], removed: [], changed: [], matchedVersion: null, note: "Silo's catalog hasn't hashed this build yet." },
    },
    // Sharing: placeholder gists, shaped like the real thing. Silo always hands out its handoff
    // page, `https://silo.hllmr.com/c/?g=<gist id>` (collection::gist_page_url), never the gist,
    // and a gist id is 20 to 40 hex characters (github::valid_gist_id).
    collections_list: { value: { items: [{ kind: "gist", reference: SHARED_GIST, name: "Marlow Flats Grain", modCount: lib.sets[1].mods.length, createdAt: "2026-10-12T18:20:00Z", pageUrl: sharePage(SHARED_GIST), sourceUrl: `https://gist.github.com/${DEMO_USER}/${SHARED_GIST}`, canDelete: true }], warning: null } },
    collection_export: { value: { url: sharePage(NEW_GIST), count: lib.mods.filter((m) => m.active).length, omitted: [] } },
    collection_update: { error: OFF },
    collection_delete: { error: OFF },
    collection_import_preview: { error: OFF },
    collection_apply: { error: OFF },
  };
}
