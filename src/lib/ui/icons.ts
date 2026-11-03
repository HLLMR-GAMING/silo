// One icon per concept (Lucide). Views import names from here, never Lucide directly.
import Library from "@lucide/svelte/icons/library";
import Compass from "@lucide/svelte/icons/compass";
import Layers from "@lucide/svelte/icons/layers";
import Stethoscope from "@lucide/svelte/icons/stethoscope";
import CircleArrowDown from "@lucide/svelte/icons/circle-arrow-down";
import Wrench from "@lucide/svelte/icons/wrench";
import Settings from "@lucide/svelte/icons/settings";
import CircleQuestionMark from "@lucide/svelte/icons/circle-question-mark";
import Bell from "@lucide/svelte/icons/bell";
import Play from "@lucide/svelte/icons/play";
import Search from "@lucide/svelte/icons/search";
import TriangleAlert from "@lucide/svelte/icons/triangle-alert";
import Info from "@lucide/svelte/icons/info";
import CircleCheck from "@lucide/svelte/icons/circle-check";
import ShieldCheck from "@lucide/svelte/icons/shield-check";
import BadgeCheck from "@lucide/svelte/icons/badge-check";
import FileDiff from "@lucide/svelte/icons/file-diff";
import Star from "@lucide/svelte/icons/star";
import EyeOff from "@lucide/svelte/icons/eye-off";
import Eye from "@lucide/svelte/icons/eye";
import Flag from "@lucide/svelte/icons/flag";
import SlidersHorizontal from "@lucide/svelte/icons/sliders-horizontal";
import FolderOpen from "@lucide/svelte/icons/folder-open";
import HardDrive from "@lucide/svelte/icons/hard-drive";
import Share2 from "@lucide/svelte/icons/share-2";
import Download from "@lucide/svelte/icons/download";
import ExternalLink from "@lucide/svelte/icons/external-link";
import Undo2 from "@lucide/svelte/icons/undo-2";
import RefreshCw from "@lucide/svelte/icons/refresh-cw";
import Ellipsis from "@lucide/svelte/icons/ellipsis";
import Keyboard from "@lucide/svelte/icons/keyboard";
import Blend from "@lucide/svelte/icons/blend";
import Cloud from "@lucide/svelte/icons/cloud";
import Copy from "@lucide/svelte/icons/copy";
import Bug from "@lucide/svelte/icons/bug";
import X from "@lucide/svelte/icons/x";
import Trash2 from "@lucide/svelte/icons/trash-2";
import ChevronDown from "@lucide/svelte/icons/chevron-down";
import ChevronRight from "@lucide/svelte/icons/chevron-right";
import ChevronUp from "@lucide/svelte/icons/chevron-up";
import Tag from "@lucide/svelte/icons/tag";
import LayoutGrid from "@lucide/svelte/icons/layout-grid";
import List from "@lucide/svelte/icons/list";
import Menu from "@lucide/svelte/icons/menu";
import GitFork from "@lucide/svelte/icons/git-fork";
import ArrowLeftRight from "@lucide/svelte/icons/arrow-left-right";
import Crosshair from "@lucide/svelte/icons/crosshair";
import ArrowUp from "@lucide/svelte/icons/arrow-up";
import ArrowDown from "@lucide/svelte/icons/arrow-down";
import Check from "@lucide/svelte/icons/check";
import Circle from "@lucide/svelte/icons/circle";
import CircleDot from "@lucide/svelte/icons/circle-dot";
import Link from "@lucide/svelte/icons/link";
import MapIcon from "@lucide/svelte/icons/map";
import FileCode from "@lucide/svelte/icons/file-code";
import Folder from "@lucide/svelte/icons/folder";

export const ICONS = {
  library: Library,
  browse: Compass,
  modSets: Layers,
  diagnose: Stethoscope,
  update: CircleArrowDown,
  tools: Wrench,
  settings: Settings,
  help: CircleQuestionMark,
  activity: Bell,
  play: Play,
  search: Search,
  problem: TriangleAlert,
  info: Info,
  ok: CircleCheck,
  check: Check,
  verifiedBuild: ShieldCheck,
  verifiedAuthor: BadgeCheck,
  modified: FileDiff,
  favorite: Star,
  hidden: EyeOff,
  visible: Eye,
  flagged: Flag,
  modSettings: SlidersHorizontal,
  showFile: FolderOpen,
  archive: HardDrive,
  share: Share2,
  download: Download,
  external: ExternalLink,
  undo: Undo2,
  rescan: RefreshCw,
  more: Ellipsis,
  controls: Keyboard,
  bridge: Blend,
  cloud: Cloud,
  copy: Copy,
  bug: Bug,
  close: X,
  remove: Trash2,
  chevronDown: ChevronDown,
  chevronRight: ChevronRight,
  chevronUp: ChevronUp,
  tag: Tag,
  grid: LayoutGrid,
  list: List,
  menu: Menu,
  fork: GitFork,
  swap: ArrowLeftRight,
  target: Crosshair,
  arrowUp: ArrowUp,
  arrowDown: ArrowDown,
  dot: CircleDot,
  circle: Circle,
  link: Link,
  map: MapIcon,
  script: FileCode,
  folder: Folder,
} as const;

export type IconName = keyof typeof ICONS;
