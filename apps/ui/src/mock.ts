// A pretend app for designing the UI in a browser (bun run dev): sample computers, a connection
// that asks for a code, sharing with a viewer.

import type { Command, State } from "./types";

export function startMock(push: (s: State) => void): (c: Command) => void {
  let s: State = {
    platform: "linux",
    me: "mandalorian",
    build: "nightly-38",
    tailscaleRunning: true,
    computers: [
      { name: "Siphesihle’s MacBook Air", address: "100.80.66.124", online: true, quiet: true, latencyMs: 7 },
      { name: "Siphesihle’s MacBook Air M4", address: "192.168.68.103:47800", online: true, quiet: false, latencyMs: 3 },
      { name: "PC", online: false, quiet: true, previewAgo: "2 days ago" },
    ],
    stream: null,
    sharing: { state: "off", viewers: [] },
    settings: { sound: true, clipboard: true, modifierLayout: "standard", allowTailscale: true, allowPairing: true, sharing: false, nightly: true },
    logDir: "~/.local/state/bb1",
  };
  // ?demo=sharing / ?demo=code: start in that state (for screenshots).
  const demo = new URLSearchParams(location.search).get("demo");
  if (demo === "sharing") {
    s.settings.sharing = true;
    s.sharing = {
      state: "on",
      pairing: { device: "Siphesihle’s MacBook Air M4", code: "482913" },
      viewers: [{ id: 1, name: "Siphesihle’s MacBook Air", mouse: true, keyboard: true, controller: false }],
    };
    s.stream = { label: "Siphesihle’s MacBook Air", status: "Connected · 60 fps · 7 ms", connected: true };
  }
  if (demo === "code") s.stream = { label: "Siphesihle’s MacBook Air M4", status: "Waiting for the code", connected: false, askingCode: "Siphesihle’s MacBook Air M4" };
  const update = (f: (s: State) => State) => {
    s = f(s);
    push(s);
  };
  setTimeout(() => push(s), 50);
  return (c) => {
    switch (c.cmd) {
      case "connect":
        update((s) => ({ ...s, lastEnded: undefined, stream: { label: c.label || c.address, status: "Connecting…", connected: false } }));
        setTimeout(() => {
          const quiet = s.computers.find((x) => x.name === c.label)?.quiet ?? false;
          update((s) => ({
            ...s,
            stream: s.stream && (quiet ? { ...s.stream, status: `Connected to ${s.stream.label}`, connected: true } : { ...s.stream, status: "Waiting for the code", askingCode: s.stream.label }),
          }));
        }, 900);
        break;
      case "sendCode":
        update((s) => ({ ...s, stream: s.stream && { ...s.stream, askingCode: undefined, status: `Connected to ${s.stream.label}`, connected: true } }));
        break;
      case "disconnect":
        update((s) => ({ ...s, stream: null }));
        break;
      case "setSharing":
        update((s) => ({ ...s, settings: { ...s.settings, sharing: c.on }, sharing: { state: c.on ? "starting" : "off", viewers: [] } }));
        if (c.on) {
          setTimeout(() => update((s) => ({ ...s, sharing: { state: "on", viewers: [] } })), 1200);
          setTimeout(() => update((s) => ({ ...s, sharing: { ...s.sharing, pairing: { device: "Siphesihle’s MacBook Air M4", code: "482 913" } } })), 3000);
          setTimeout(
            () =>
              update((s) =>
                s.sharing.state === "on"
                  ? { ...s, sharing: { state: "on", viewers: [{ id: 1, name: "Siphesihle’s MacBook Air M4", mouse: true, keyboard: true, controller: false }] } }
                  : s,
              ),
            6000,
          );
        }
        break;
      case "setViewerAccess":
        update((s) => ({ ...s, sharing: { ...s.sharing, viewers: s.sharing.viewers.map((v) => (v.id === c.id ? { ...v, [c.what]: c.on } : v)) } }));
        break;
      case "disconnectViewer":
        update((s) => ({ ...s, sharing: { ...s.sharing, viewers: s.sharing.viewers.filter((v) => v.id !== c.id) } }));
        break;
      case "setSettings":
        update((s) => ({ ...s, settings: { ...s.settings, ...c.settings } }));
        break;
      case "dismissEnded":
        update((s) => ({ ...s, lastEnded: undefined }));
        break;
      default:
        break;
    }
  };
}
