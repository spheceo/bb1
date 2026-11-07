// What every bb1 app (Linux and Windows in Rust, the Mac in Swift) tells the UI, and what the UI
// asks of it. The app sends the whole `State` whenever something changes; the UI sends `Command`s.
// Keep this file and the apps' bridges in step (apps/windows/src/linux/ui.rs).

export type Platform = "linux" | "windows" | "mac";

export interface Computer {
  /** The name it announces (also its key). */
  name: string;
  /** Where to connect; absent while offline. */
  address?: string;
  online: boolean;
  /** Paired, or on this Tailscale account: connects without a code. */
  quiet: boolean;
  latencyMs?: number;
  /** Bumps with every new screenshot; fetch `bb1://…/preview/<name>?v=<version>` (see `previewUrl`). */
  previewVersion?: number;
  /** "2 min ago": when the screenshot was taken, for offline computers. */
  previewAgo?: string;
  /** It's paired with this one, so it can be forgotten (it pairs again next time). */
  canForget?: boolean;
  /** It runs an older bb1 than the newest: offer to update it (Mac). */
  canUpdate?: boolean;
  /** Mac: how to reach it when both work, and whether it can be renamed here. */
  route?: "home" | "tailscale";
  routes?: ("home" | "tailscale")[];
  canRename?: boolean;
  /** Being updated right now (0…1). */
  updating?: number;
}

export interface Stream {
  label: string;
  status: string;
  connected: boolean;
  /** The other computer shows a 6-digit code to type here. */
  askingCode?: string;
}

export interface Viewer {
  id: number;
  name: string;
  mouse: boolean;
  keyboard: boolean;
  controller: boolean;
}

export interface Sharing {
  state: "off" | "starting" | "on" | "failed";
  error?: string;
  /** What sharing needs here ("Needs an NVIDIA graphics card"). */
  note?: string;
  /** This computer's addresses, to connect by hand. */
  addresses?: string;
  /** Someone is pairing: show them this code. */
  pairing?: { device: string; code: string };
  viewers: Viewer[];
}

export type ModifierLayout = "standard" | "mac" | "ctrl";

export interface Settings {
  sound: boolean;
  clipboard: boolean;
  modifierLayout: ModifierLayout;
  allowTailscale: boolean;
  allowPairing: boolean;
  sharing: boolean;
  nightly: boolean;
}

/** The update button at the top: "Update to nightly-41", "Updating · 40%", "Updated to …". */
export interface UpdatePill {
  kind: "offer" | "busy" | "done" | "failed";
  label: string;
  progress?: number;
  /** Also updates these computers first (they run an older bb1). */
  also?: string[];
}

export interface MutedApp {
  id: string;
  name: string;
  muted: boolean;
}

export interface State {
  platform: Platform;
  /** This computer's name. */
  me: string;
  /** "nightly-38" */
  build: string;
  tailscaleRunning: boolean;
  computers: Computer[];
  stream: Stream | null;
  /** Why the last connection ended (not shown for a plain disconnect). */
  lastEnded?: string;
  sharing: Sharing;
  settings: Settings;
  logDir: string;
  update?: UpdatePill;
  /** Settings → Updates: "Up to date", "nightly-41 is ready", … */
  updateStatus?: { text: string; canCheck: boolean };
  /** Computers that sign in here without a code (Settings → Forget all). */
  pairedCount?: number;
  /** Apps whose sound isn't shared (Windows, Mac). */
  mutedApps?: MutedApp[];
  /** Windows: the ViGEmBus driver friends' controllers need. */
  controllerDriver?: { state: "ready" | "missing" | "downloading" | "installing" | "failed"; error?: string };
  /** Mac: what sharing needs from System Settings. */
  permissions?: { screenRecording: boolean; accessibility: boolean };
  /** Mac: how bb1 sits on this computer. */
  app?: { openAtLogin: boolean; keepRunning: boolean; dock: "always" | "open" | "never" };
}

export type Command =
  | { cmd: "connect"; address: string; label: string }
  | { cmd: "disconnect" }
  | { cmd: "sendCode"; code: string }
  | { cmd: "setSharing"; on: boolean }
  | { cmd: "setViewerAccess"; id: number; what: "mouse" | "keyboard" | "controller"; on: boolean }
  | { cmd: "disconnectViewer"; id: number }
  | { cmd: "setSettings"; settings: Partial<Settings> }
  | { cmd: "openLogs" }
  | { cmd: "dismissEnded" }
  | { cmd: "ready" }
  // Updates
  | { cmd: "update" }
  | { cmd: "updateComputer"; name: string }
  | { cmd: "checkUpdates" }
  | { cmd: "dismissUpdate" }
  // A computer's menu
  | { cmd: "forgetComputer"; name: string }
  | { cmd: "renameComputer"; name: string; newName: string }
  | { cmd: "setRoute"; name: string; route: "home" | "tailscale" }
  // Sharing
  | { cmd: "cancelPairing" }
  | { cmd: "forgetPaired" }
  | { cmd: "setMuted"; id: string; muted: boolean }
  | { cmd: "installControllerDriver" }
  | { cmd: "fixPermission"; which: "screenRecording" | "accessibility" }
  | { cmd: "setApp"; app: Partial<NonNullable<State["app"]>> };
