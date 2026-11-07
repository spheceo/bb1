// The UI's line to the app it runs in.
//
// UI → app: one JSON `Command` per message, through
//   - wry (Linux, Windows): window.ipc.postMessage(json)
//   - WKWebView (Mac):     window.webkit.messageHandlers.bb1.postMessage(json)
// App → UI: the app calls window.bb1Receive({ type: "state", state }).
//
// In a plain browser (bun run dev) there's no app: a mock with sample computers stands in, so the
// UI can be designed without building anything.

import type { Command, State } from "./types";
import { startMock } from "./mock";

type Listener = (s: State) => void;

declare global {
  interface Window {
    ipc?: { postMessage(message: string): void };
    webkit?: { messageHandlers?: { bb1?: { postMessage(message: string): void } } };
    bb1Receive?: (message: { type: "state"; state: State }) => void;
  }
}

let listener: Listener | null = null;
let latest: State | null = null;
let mockSend: ((c: Command) => void) | null = null;

window.bb1Receive = (message) => {
  if (message.type === "state") {
    latest = message.state;
    listener?.(message.state);
  }
};

export function onState(l: Listener) {
  listener = l;
  if (latest) l(latest);
}

export function send(c: Command) {
  const json = JSON.stringify(c);
  if (window.ipc) window.ipc.postMessage(json);
  else if (window.webkit?.messageHandlers?.bb1) window.webkit.messageHandlers.bb1.postMessage(json);
  else mockSend?.(c);
}

/** Where a computer's latest screenshot is served (the app answers this URL itself). */
export function previewUrl(platform: State["platform"], name: string, version: number) {
  const path = `preview/${encodeURIComponent(name)}?v=${version}`;
  if (platform === "windows") return `http://bb1.localhost/${path}`;
  if (platform === "mac" || platform === "linux") return `bb1://localhost/${path}`;
  return path;
}

export function start() {
  if (!window.ipc && !window.webkit?.messageHandlers?.bb1) {
    mockSend = startMock((s) => window.bb1Receive?.({ type: "state", state: s }));
  }
  send({ cmd: "ready" });
}
