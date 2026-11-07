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
  | { cmd: "ready" };
