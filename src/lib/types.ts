// Mirrors the Rust `scan::ModEntry` / `scan::ScanResult` (serde camelCase).

export interface ModEntry {
  techName: string;
  path: string;
  kind: "zip" | "dir";
  size: number;
  mtimeMs: number;

  title: string | null;
  author: string | null;
  version: string | null;
  descVersion: number | null;
  iconFilename: string | null;

  isMap: boolean;
  mapTitle: string | null;
  category: string;
  subcategory: string | null;
  organized: boolean;
  active: boolean;

  dependencies: string[];
  scriptCount: number;
  registrationCount: number;
  uniqueType: string | null;
  storeItemCount: number;
  mpSupported: boolean;

  ignoredDigitPrefix: boolean;
  error: string | null;

  /** Detected by looking inside the mod (not just the runtime modSettings/ folder). */
  hasSettings: boolean;
  /** How settings were detected: "shipped" (editable now) · "runtime" (after first run) · "none". */
  settingsSource: "none" | "shipped" | "runtime";
}

export interface ScanResult {
  mods: ModEntry[];
  roots: string[];
  tookMs: number;
  total: number;
  /** Interrupted mod updates finished or rolled back before the scan. */
  notices: string[];
  /** What that recovery couldn't fix. */
  problems: string[];
}

/** Mirrors the Rust `UpdateOutcome` returned by `download_update`. */
export interface UpdateOutcome {
  /** Where the previous version is kept, or null if it couldn't be moved there. */
  backup: string | null;
  /** What went wrong after the new version was in place (the update itself stands). */
  problems: string[];
}

export interface ScanProgress {
  done: number;
  total: number;
}

export interface CurationRow {
  techName: string;
  favorite: boolean;
  hidden: boolean;
  broken: boolean;
  rating: number;
  note: string | null;
  /** Left out of update checks and Update all (#333). */
  skipUpdates: boolean;
}

export interface TagRow {
  techName: string;
  tag: string;
}

export interface CategoryOverride {
  techName: string;
  category: string;
  subcategory: string | null;
}

export interface ModInput {
  techName: string;
  fileName: string;
  kind: string;
  category: string;
  subcategory: string | null;
}

/** What removing mods from the library did (#429). */
export interface RemoveReport {
  /** The mods whose file or folder went to the Recycle Bin, as asked for. */
  removed: { techName: string; path: string }[];
  /** Why the others didn't, one line each ("FS25_X: …"). */
  errors: string[];
  /** Anything else to tell (where parking kept an edited copy). */
  notices: string[];
}

export interface OrganizeReport {
  changed: number;
  skipped: number;
  errors: string[];
  /** Not errors, but worth telling the user (e.g. where an edited folder copy was kept). */
  notices: string[];
  /** set_active only: names left parked because another copy of the name is in the mods folder (#421). */
  otherCopies?: string[];
  /** set_active with `touched` only: problems with mods the change didn't touch (#427). */
  standing?: string[];
}

/** Provenance verdict of comparing an installed mod to its canonical build. */
export type VerifyStatus = "verified" | "modified" | "unverified";
export interface VerifyResult {
  status: VerifyStatus;
  /** "exact" (whole-zip) or "content" (manifest) when verified. */
  how: string | null;
  /** Local files absent from the canonical build. */
  added: string[];
  /** Canonical files missing locally. */
  removed: string[];
  /** Files present in both with different content. */
  changed: string[];
  matchedVersion: string | null;
  note: string | null;
}

/** One planned move from a dry-run `plan_organize` (read-only preview). */
export interface PlannedMove {
  techName: string;
  fileName: string;
  category: string;
  relFrom: string;
  relTo: string;
}

export interface Loadout {
  id: number;
  name: string;
  mods: string[];
  /** "user": a set the user saved; "auto": a "Before …" snapshot Silo took before a switch
   *  (only the newest 5 are kept). */
  kind: "user" | "auto";
  /** When it was created (unix ms); null for sets saved before Silo 1.0. */
  createdAt: number | null;
}

export interface SaveMod {
  modName: string;
  title: string | null;
  version: string | null;
  required: boolean;
  fileHash: string | null;
  isDlc: boolean;
}

export interface Savegame {
  index: number;
  folder: string;
  name: string;
  mapTitle: string | null;
  /** When the game last saved it, as written in the save ("2026-08-02"), or null. */
  saveDate: string | null;
  mods: SaveMod[];
  /** Why careerSavegame.xml couldn't be read (cut off, damaged, empty): then `mods` is empty. */
  error?: string | null;
}

export interface ConflictInput {
  techName: string;
  title: string | null;
  path: string;
  kind: string;
}

export interface Conflict {
  /** Stable identity from Rust (kind, name, sorted tech names): use it as the list key. */
  id: string;
  /** Wire values; the UI says Problem / Review / Note (`conflictSeverityLabel`). */
  severity: "critical" | "warning" | "info";
  kind: string;
  name: string;
  explanation: string;
  /** Display labels of the mods involved. */
  mods: string[];
  /** The same mods' tech names, in the same order as `mods`. */
  techNames: string[];
}

export interface GameInfo {
  appId: string;
  /** The executable; on macOS, the `.app` bundle. */
  exe: string;
  installDir: string;
  /** `steam` launches via Steam; `standalone` (macOS App Store / GIANTS) opens the bundle. */
  store: "steam" | "standalone";
}

export interface SettingsField {
  id: number;
  label: string;
  kind: "bool" | "int" | "float" | "string";
  value: string;
}

export interface SettingsFile {
  path: string;
  name: string;
  fields: SettingsField[];
  raw: string;
}

export interface SettingsEdit {
  id: number;
  value: string;
}

export interface RepoRow {
  techName: string;
  owner: string;
  repo: string;
}

/** The web page a mod came from, saved by the user. Silo never requests it. */
export interface SourceRow {
  techName: string;
  url: string;
  /** When the user last said they checked the page (unix seconds). */
  lastChecked: number | null;
}

export interface ReleaseInfo {
  tag: string;
  name: string | null;
  publishedAt: string | null;
  htmlUrl: string | null;
  assetUrl: string | null;
  assetName: string | null;
}

export interface UpdateInfo {
  hasUpdate: boolean;
  current: string;
  release: ReleaseInfo;
}

export interface GhStatus {
  clientId: string | null;
  user: string | null;
  builtin: boolean;
  /** True when the stored token can star/watch (public_repo OAuth grant or a PAT). */
  canWrite: boolean;
  /** True when the stored token can create/read gists (the `gist` grant) — needed to share a Collection. */
  canGist: boolean;
}

/** Live public signals for a GitHub source card. `youStarred`/`youWatching` are
 *  null unless a token is connected. */
export interface RepoStats {
  fullName: string;
  htmlUrl: string;
  stars: number;
  forks: number;
  watchers: number;
  openIssues: number;
  archived: boolean;
  pushedAt: string | null;
  youStarred: boolean | null;
  youWatching: boolean | null;
}

export interface DeviceCode {
  deviceCode: string;
  userCode: string;
  verificationUri: string;
  interval: number;
  expiresIn: number;
}

export interface PollResult {
  /** "cancelled": a newer sign-in, a cancel or a Disconnect ended the flow; nothing was stored. */
  status: "ok" | "pending" | "slow_down" | "expired" | "denied" | "error" | "cancelled";
  error: string | null;
}

// ── SiloAPI (mod browser) ──

/** One place a mod can be got from. The API decides `installable` — ModHub's CDN
 *  blocks direct downloads, so those open their site instead. */
export interface ModSourceOption {
  source: string;
  sourceUrl: string;
  version: string | null;
  installable: boolean;
  downloadUrl: string | null;
  // Raw per-source popularity signals (see SiloAPI docs/ENRICHMENT.md). Each source
  // only fills what it natively reports; null until the server backfill lands.
  downloads?: number | null;
  rating?: number | null;
  stars?: number | null;
}

/** Who claimed a catalog mod (SiloAPI's `verifiedAuthor`). */
export interface VerifiedAuthor {
  login: string;
  avatarUrl?: string | null;
  profileUrl?: string | null;
}

export interface BrowseMod {
  id: string;
  techName: string | null;
  title: string;
  author: string | null;
  category: string | null;
  description: string | null;
  // Full body + changelog (SiloAPI docs/ENRICHMENT.md), separate from the short
  // `description` summary. Null until the server ingests them.
  descriptionFull?: string | null;
  changelog?: string | null;
  imageUrl: string | null;
  latestVersion: string | null;
  trustScore: number | null;
  /** Silo's catalog hashed the published build from its source (SiloAPI provenance), so an
   *  installed copy can be checked byte for byte. Null/absent when it hasn't. */
  verified?: boolean | null;
  /** The author claimed this mod through SiloAPI's author program (A7). Null/absent when
   *  unclaimed, or on a catalog that predates claims. */
  verifiedAuthor?: VerifiedAuthor | null;
  updatedAt: string | null;
  // Aggregated popularity (see SiloAPI docs/ENRICHMENT.md). `downloads` is a
  // cross-source sum, `rating` (0–5) from the rating-bearing source, `popularity` the
  // composite sort key. All null until the server backfill lands — render null-safe.
  downloads?: number | null;
  rating?: number | null;
  ratingCount?: number | null;
  popularity?: number | null;
  sources: ModSourceOption[];
  pageUrl: string | null;
  /** Semantic facet tags (brand/theme/region/realism/era), highest-confidence first.
   *  Populated on detail; empty on list results. */
  tags?: ModTag[];
  /** Real-world production-year range for identifiable machines (drives availableBy). */
  yearFrom?: number | null;
  yearTo?: number | null;
}

/** One semantic facet tag (SiloAPI docs/TAGGING.md). `confidence` lets the client threshold. */
export interface ModTag {
  namespace: string;
  value: string;
  confidence?: number | null;
  source?: string | null;
}

/** A page of catalog results + how many match the filter overall. */
export interface BrowsePage {
  mods: BrowseMod[];
  total: number;
}

export interface SiloStats {
  mods: number;
  sources: number;
}

export interface InstallProgress {
  id: string;
  done: number;
  total: number | null;
}

export interface CatalogUpdate {
  techName: string;
  latest: string | null;
  hasUpdate: boolean;
  downloadUrl: string | null;
  source: string | null;
  /** Where to get it by hand when there's no direct download (a ModHub page). Older Silo
   *  builds' catalog rows don't carry it. */
  pageUrl?: string | null;
  /** Why this mod couldn't be checked (its catalog link or entry couldn't be read, #386). */
  error?: string | null;
}

/** The detail record is a BrowseMod plus its registrations; sources are the same shape. */
export type CatalogModDetail = BrowseMod;

export interface CategoryCount {
  category: string;
  count: number;
}

// ── Crash / log triage ──
export interface LogModHealth {
  modName: string;
  errors: number;
  warnings: number;
  benign: number;
  sample: string;
  /** The sample's line in log.txt; null for a line from the end of a log too big to read whole (#351). */
  sampleLine: number | null;
}

/** `log_status`: where log.txt is, when it changed, whether FS25 runs, and what Silo last saw. */
export interface LogStatus {
  path: string | null;
  /** Epoch ms, or null when there's no log yet. */
  modifiedMs: number | null;
  gameRunning: boolean;
  /** The change time Silo last looked at (stored in the DB), or null if never. */
  lastSeenMs: number | null;
}

export interface LogReport {
  engineVersion: string | null;
  modCount: number;
  cleanExit: boolean;
  crashed: boolean;
  mods: LogModHealth[];
  unattributed: number;
  totalFindings: number;
  /** The game ran out of ground-pile slots: the cap it reported (e.g. 63), else null (#339). */
  groundPileCap: number | null;
  /** What turned itself off for lack of ground-pile slots, as the game names it. */
  groundPileDisabled: string[];
  /** The game hit its fill-type limit: the limit it reported (e.g. 255), else null (#341). */
  fillTypeCap: number | null;
  /** The fill types it skipped because of that limit. */
  droppedFillTypes: string[];
  /** The log was too big to read whole: only its start and its end were read (#351). */
  truncated: boolean;
  /** Silo's own folders in mods (archive, backups) the game tried to open as mods; harmless (#385). */
  siloFolders: string[];
}

// ── Multiplayer mod-set sync ──
export interface MpModRef {
  techName: string;
  path: string;
  kind: string;
  version: string | null;
}
export interface MpManifestEntry {
  techName: string;
  version: string | null;
  /** "" when Silo has no fingerprint for it (an unpacked folder, or a zip it couldn't read). */
  hash: string;
  size: number;
}
/** A mod Silo couldn't compare, because a copy of it has no fingerprint. */
export interface MpUnverifiable {
  techName: string;
  /** The server's check file has no fingerprint for it. */
  server: boolean;
  /** Silo couldn't fingerprint your copy (an unpacked folder, or an unreadable zip). */
  yours: boolean;
}
export interface MpMismatch {
  techName: string;
  expected: string;
  got: string;
}
export interface MpVerifyReport {
  ok: boolean;
  missing: MpManifestEntry[];
  versionMismatch: MpMismatch[];
  hashMismatch: MpMismatch[];
  /** Same version, but a copy has no fingerprint: never counted as a match. */
  unverifiable: MpUnverifiable[];
  extra: string[];
}
/** A saved Multiplayer check file. */
export interface MpExportResult {
  count: number;
  /** Packaged mods whose zip Silo couldn't read: listed without a fingerprint. */
  unreadable: string[];
}

// ── Collections ──
export interface CollectionExportResult {
  /** The shareable gist URL. */
  url: string;
  /** How many mods went into the collection. */
  count: number;
  /** Dev/unpacked (directory) mods left out — no stable bytes to pin. */
  omitted: string[];
  /** Packaged mods listed without the curator's fingerprint (their zip couldn't be read). */
  unpinned: string[];
  /** The link works, but something around it didn't (e.g. the README couldn't be written). */
  warning: string | null;
}
export interface ImportPlanRow {
  techName: string;
  version: string | null;
  source: string | null;
  /** For versionDrift rows: the version already in the library. */
  installedVersion: string | null;
}
export interface ImportPlan {
  name: string;
  description: string | null;
  author: string | null;
  /** Silo can download these directly (a GitHub-style installable source). */
  willInstall: ImportPlanRow[];
  /** In the catalog but download-gated (ModHub) — get them from their page. */
  openPage: ImportPlanRow[];
  /** Already in your library at a matching (or unpinned) version. */
  alreadyPresent: ImportPlanRow[];
  /** Installed, but a different version than the collection pins. */
  versionDrift: ImportPlanRow[];
  /** Not found in the catalog — can't be resolved automatically. */
  unresolved: ImportPlanRow[];
  /** Hash of the collection file and what the catalog matched for it, as previewed; apply
   *  refuses if either changed. */
  fingerprint: string;
}
export interface ApplyRow {
  techName: string;
  filename: string | null;
  /** "installed" | "present" | "skipped" | "failed". */
  status: string;
  /** Provenance verdict for what landed: "verified" | "modified" | "unverified". */
  verdict: string | null;
  detail: string | null;
}
export interface ApplyReport {
  /** Null when Silo stayed busy and couldn't add the downloads (their rows say why). */
  loadoutId: number | null;
  installed: number;
  failed: number;
  rows: ApplyRow[];
}
export interface CollectionProgress {
  done: number;
  total: number;
  current: string;
}

// ── Filltype bridge ──
export interface BridgeSpec {
  techName: string;
  title: string;
  fillTypes: string[];
  categories: string[];
}

// ── Input bindings map ──
export interface Bind {
  action: string;
  input: string;
}
export interface SharedInput {
  input: string;
  actions: string[];
}
export interface DeviceBindings {
  device: string;
  bindings: Bind[];
  shared: SharedInput[];
}
export interface BindingReport {
  totalActions: number;
  totalBindings: number;
  devices: DeviceBindings[];
}

// ── Update checking (library-wide, catalog + linked GitHub repos) ──
export interface UpdateRow {
  techName: string;
  title: string;
  path: string;
  current?: string;
  latest?: string;
  hasUpdate?: boolean;
  assetUrl?: string | null;
  source?: string;
  /** The mod's page, for updates Silo can't download itself (ModHub). */
  pageUrl?: string | null;
  error?: string;
  /** The text behind "Copy details" for `error`. */
  errorDetail?: string;
}

// ── Guided bisection ──
export type BisectStep =
  | { kind: "split"; test: string[]; rest: string[]; roundsLeft: number }
  | { kind: "culprit"; modName: string }
  | { kind: "inconclusive" };
