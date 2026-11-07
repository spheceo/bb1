import { useEffect, useRef, useState } from "react";
import { ArrowLeft, Gamepad2, Keyboard, Monitor, MousePointer2, Plus, Settings as Gear, X } from "lucide-react";
import { onState, previewUrl, send } from "./bridge";
import type { Computer, ModifierLayout, State, Viewer } from "./types";

export function App() {
  const [state, setState] = useState<State | null>(null);
  const [page, setPage] = useState<"computers" | "settings">(new URLSearchParams(location.search).get("page") === "settings" ? "settings" : "computers");
  useEffect(() => onState(setState), []);
  if (!state) return <div className="app loading" />;
  return (
    <div className="app">
      <Header state={state} page={page} setPage={setPage} />
      <main>{page === "computers" ? <Computers state={state} /> : <SettingsPage state={state} />}</main>
      <StreamBar state={state} />
      {state.stream?.askingCode && <CodeEntry host={state.stream.askingCode} />}
    </div>
  );
}

function Header({ state, page, setPage }: { state: State; page: string; setPage: (p: "computers" | "settings") => void }) {
  const sharing = state.sharing.state === "on" || state.sharing.state === "starting";
  return (
    <header>
      <div className="brand">
        <Mark />
        <span className="name">bb1</span>
        <span className="build">{state.build}</span>
      </div>
      <div className="header-actions">
        <label className="share-switch" title="Let your other computers see and control this one">
          <span>Share this {noun(state)}</span>
          <Toggle on={sharing} onChange={(on) => send({ cmd: "setSharing", on })} />
        </label>
        {page === "computers" ? (
          <button className="icon-button" title="Settings" onClick={() => setPage("settings")}>
            <Gear size={17} />
          </button>
        ) : (
          <button className="pill link" onClick={() => setPage("computers")}>
            <ArrowLeft size={15} /> Your computers
          </button>
        )}
      </div>
    </header>
  );
}

function noun(state: State) {
  return state.platform === "mac" ? "Mac" : state.platform === "windows" ? "PC" : "computer";
}

// MARK: Your computers

function Computers({ state }: { state: State }) {
  const [adding, setAdding] = useState(false);
  const computers = state.computers.filter((c) => c.name.toLowerCase() !== state.me.toLowerCase());
  return (
    <>
      {state.sharing.state !== "off" && <SharingPanel state={state} />}
      <div className="section-title">
        <h1>Your computers</h1>
        <button className="icon-button" title="Connect by address" onClick={() => setAdding(true)}>
          <Plus size={18} />
        </button>
      </div>
      {computers.length === 0 ? (
        <div className="empty">
          <Monitor size={28} strokeWidth={1.5} />
          <div className="empty-title">No other computers yet</div>
          <div className="empty-detail">
            Turn on <b>Share</b> in bb1 on another computer signed in to your Tailscale, or on this home network.
            <br />
            {state.tailscaleRunning ? "Or add one by its address with +." : "Tailscale isn't running here, so only computers on this network show up."}
          </div>
        </div>
      ) : (
        <div className="grid">
          {computers.map((c) => (
            <Tile key={c.name} c={c} state={state} />
          ))}
        </div>
      )}
      {state.lastEnded && (
        <div className="toast" role="status">
          <span>{state.lastEnded}</span>
          <button className="icon-button small" onClick={() => send({ cmd: "dismissEnded" })} title="Dismiss">
            <X size={14} />
          </button>
        </div>
      )}
      {adding && <AddByAddress onClose={() => setAdding(false)} />}
    </>
  );
}

function Tile({ c, state }: { c: Computer; state: State }) {
  const busy = state.stream !== null;
  const connecting = state.stream?.label === c.name;
  const status = c.online
    ? [c.latencyMs !== undefined ? `${Math.round(c.latencyMs)} ms` : null, c.quiet ? "Ready" : "Asks for a code"].filter(Boolean).join(" · ")
    : c.previewAgo
      ? `Offline · seen ${c.previewAgo}`
      : "Offline";
  const connect = () => c.address && !busy && send({ cmd: "connect", address: c.address, label: c.name });
  return (
    <div className={`tile ${c.online ? "" : "offline"}`} onDoubleClick={connect}>
      <div className="screen">
        {c.previewVersion !== undefined ? (
          <img src={previewUrl(state.platform, c.name, c.previewVersion)} alt="" draggable={false} />
        ) : (
          <div className="placeholder">
            <Mark size={34} />
          </div>
        )}
        {connecting && <div className="screen-badge">{state.stream?.connected ? "Connected" : "Connecting…"}</div>}
      </div>
      <div className="tile-footer">
        <div className="tile-text">
          <div className="tile-name">
            <span className={`dot ${c.online ? "on" : ""}`} />
            {c.name}
          </div>
          <div className="tile-status">{status}</div>
        </div>
        {c.online && c.address && (
          <button className="pill primary" disabled={busy} onClick={connect}>
            {c.quiet ? "Connect" : "Pair"}
          </button>
        )}
      </div>
    </div>
  );
}

function SharingPanel({ state }: { state: State }) {
  const s = state.sharing;
  if (s.state === "starting") {
    return (
      <div className="card sharing">
        <div className="sharing-head">
          <span className="dot pulse" /> Starting to share…
        </div>
        <p className="muted">
          {state.platform === "linux"
            ? "Allow sharing the screen and remote control in the desktop's dialog. bb1 asks only once."
            : "Waiting for permission to share the screen."}
        </p>
      </div>
    );
  }
  if (s.state === "failed") {
    return (
      <div className="card sharing failed">
        <div className="sharing-head">Couldn't share this {noun(state)}</div>
        <p className="muted">{s.error}</p>
        <button className="pill secondary" onClick={() => send({ cmd: "setSharing", on: true })}>
          Try again
        </button>
      </div>
    );
  }
  return (
    <div className="card sharing">
      <div className="sharing-head">
        <span className="dot on" /> Sharing this {noun(state)}
        <span className="muted"> · {s.viewers.length === 0 ? "nobody's watching" : `${s.viewers.length} watching`}</span>
      </div>
      {s.pairing && (
        <div className="pairing">
          <div>
            <b>{s.pairing.device}</b> wants to connect. Type this code on it:
          </div>
          <div className="code">{s.pairing.code}</div>
        </div>
      )}
      {s.viewers.map((v) => (
        <ViewerRow key={v.id} v={v} />
      ))}
    </div>
  );
}

function ViewerRow({ v }: { v: Viewer }) {
  const chip = (what: "mouse" | "keyboard" | "controller", on: boolean, icon: React.ReactNode, label: string) => (
    <button className={`chip ${on ? "on" : ""}`} title={`${label}: ${on ? "allowed" : "off"}`} onClick={() => send({ cmd: "setViewerAccess", id: v.id, what, on: !on })}>
      {icon}
      {label}
    </button>
  );
  return (
    <div className="viewer">
      <div className="viewer-name">{v.name}</div>
      <div className="viewer-actions">
        {chip("mouse", v.mouse, <MousePointer2 size={13} />, "Mouse")}
        {chip("keyboard", v.keyboard, <Keyboard size={13} />, "Keyboard")}
        {chip("controller", v.controller, <Gamepad2 size={13} />, "Controller")}
        <button className="pill secondary small" onClick={() => send({ cmd: "disconnectViewer", id: v.id })}>
          Disconnect
        </button>
      </div>
    </div>
  );
}

// MARK: Connecting

function StreamBar({ state }: { state: State }) {
  const st = state.stream;
  if (!st) return null;
  return (
    <div className="stream-bar">
      <span className={`dot ${st.connected ? "on" : "pulse"}`} />
      <span className="stream-text">
        <b>{st.label}</b> <span className="muted">· {st.status}</span>
      </span>
      <button className="pill secondary small" onClick={() => send({ cmd: "disconnect" })}>
        {st.connected ? "Disconnect" : "Cancel"}
      </button>
    </div>
  );
}

function CodeEntry({ host }: { host: string }) {
  const [digits, setDigits] = useState("");
  const input = useRef<HTMLInputElement>(null);
  useEffect(() => input.current?.focus(), []);
  const submit = (code: string) => send({ cmd: "sendCode", code });
  return (
    <div className="modal-backdrop">
      <div className="modal">
        <h2>Pair with {host}</h2>
        <p className="muted">It shows a 6-digit code. Type it here; after that it connects by itself.</p>
        <div className="code-boxes" onClick={() => input.current?.focus()}>
          {Array.from({ length: 6 }, (_, i) => (
            <div key={i} className={`code-box ${i === digits.length ? "current" : ""}`}>
              {digits[i] ?? ""}
            </div>
          ))}
          <input
            ref={input}
            className="code-input"
            inputMode="numeric"
            autoComplete="one-time-code"
            value={digits}
            onChange={(e) => {
              const d = e.target.value.replace(/\D/g, "").slice(0, 6);
              setDigits(d);
              if (d.length === 6) submit(d);
            }}
            onKeyDown={(e) => e.key === "Escape" && submit("")}
          />
        </div>
        <div className="modal-actions">
          <button className="pill secondary" onClick={() => submit("")}>
            Cancel
          </button>
          <button className="pill primary" disabled={digits.length < 6} onClick={() => submit(digits)}>
            Pair
          </button>
        </div>
      </div>
    </div>
  );
}

function AddByAddress({ onClose }: { onClose: () => void }) {
  const [address, setAddress] = useState("");
  const go = () => {
    if (address.trim()) {
      send({ cmd: "connect", address: address.trim(), label: "" });
      onClose();
    }
  };
  return (
    <div className="modal-backdrop" onMouseDown={(e) => e.target === e.currentTarget && onClose()}>
      <div className="modal">
        <h2>Connect by address</h2>
        <p className="muted">A Tailscale IP or name (100.80.66.124, my-mac), or a home-network address (192.168.1.20, my-mac.local).</p>
        <input
          className="text"
          autoFocus
          placeholder="Address"
          value={address}
          onChange={(e) => setAddress(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter") go();
            if (e.key === "Escape") onClose();
          }}
        />
        <div className="modal-actions">
          <button className="pill secondary" onClick={onClose}>
            Cancel
          </button>
          <button className="pill primary" disabled={!address.trim()} onClick={go}>
            Connect
          </button>
        </div>
      </div>
    </div>
  );
}

// MARK: Settings

function SettingsPage({ state }: { state: State }) {
  const s = state.settings;
  const set = (settings: Partial<State["settings"]>) => send({ cmd: "setSettings", settings });
  const sharing = state.sharing.state === "on" || state.sharing.state === "starting";
  const superName = state.platform === "windows" ? "Windows key" : "Super";
  return (
    <div className="settings">
      <h1>Settings</h1>
      <Section title="Sharing" detail={`Let your other computers see and control this ${noun(state)}.`}>
        <Row title={`Share this ${noun(state)}`} detail={sharing ? (state.platform === "linux" ? "On while bb1 is open" : "On") : "Off"}>
          <Toggle on={sharing} onChange={(on) => send({ cmd: "setSharing", on })} />
        </Row>
      </Section>
      <Section title="Who can connect" detail="Computers you've paired, or on your Tailscale account, get in without asking.">
        <Row title="Tailscale sign-in" detail="Computers on your Tailscale account">
          <Toggle on={s.allowTailscale} onChange={(on) => set({ allowTailscale: on })} />
        </Row>
        <Row title="Pairing code" detail="A new computer types a 6-digit code shown here">
          <Toggle on={s.allowPairing} onChange={(on) => set({ allowPairing: on })} />
        </Row>
      </Section>
      {state.platform !== "mac" && (
        <Section title="Connecting" detail="When you use another computer from this one. Applies to the next connection.">
          <Row title="Sound" detail="Play the other computer's sound here">
            <Toggle on={s.sound} onChange={(on) => set({ sound: on })} />
          </Row>
          <Row title="Clipboard" detail="Copy on one, paste on the other (not password-manager copies)">
            <Toggle on={s.clipboard} onChange={(on) => set({ clipboard: on })} />
          </Row>
          <Row title="Mac's ⌘ key" detail={layoutDetail(s.modifierLayout, superName)}>
            <Segmented<ModifierLayout>
              value={s.modifierLayout}
              options={[
                ["standard", superName],
                ["mac", "Alt"],
                ["ctrl", "Ctrl"],
              ]}
              onChange={(v) => set({ modifierLayout: v })}
            />
          </Row>
          <Row title="Shortcuts" detail="In the stream window: Ctrl+Alt+Enter full screen · Ctrl+Alt+← / → the other screen" />
        </Section>
      )}
      <Section title="Updates" detail={state.platform === "linux" ? "On Linux, bb1 is built from source for now." : "New builds arrive inside the app."}>
        <Row title="bb1" detail={state.build} />
        {state.platform !== "linux" && (
          <Row title="Channel" detail={s.nightly ? "Every nightly, as soon as it's out" : "Releases only"}>
            <Segmented<"stable" | "nightly">
              value={s.nightly ? "nightly" : "stable"}
              options={[
                ["stable", "Stable"],
                ["nightly", "Nightly"],
              ]}
              onChange={(v) => set({ nightly: v === "nightly" })}
            />
          </Row>
        )}
      </Section>
      <Section title="Help" detail="For when something isn't right.">
        <Row title="Logs" detail={state.logDir}>
          <button className="pill secondary small" onClick={() => send({ cmd: "openLogs" })}>
            Open folder
          </button>
        </Row>
      </Section>
    </div>
  );
}

function layoutDetail(l: ModifierLayout, superName: string) {
  if (l === "mac") return "Alt is ⌘ and the key next to it ⌥, where they sit on a Mac keyboard";
  if (l === "ctrl") return `Ctrl is ⌘, so Ctrl+C copies; ${superName} is ⌃`;
  return `${superName} is ⌘, Alt is ⌥, Ctrl is ⌃`;
}

function Section({ title, detail, children }: { title: string; detail: string; children: React.ReactNode }) {
  return (
    <section>
      <h2>{title}</h2>
      <p className="muted">{detail}</p>
      <div className="group">{children}</div>
    </section>
  );
}

function Row({ title, detail, children }: { title: string; detail?: string; children?: React.ReactNode }) {
  return (
    <div className="row">
      <div className="row-text">
        <div className="row-title">{title}</div>
        {detail && <div className="row-detail">{detail}</div>}
      </div>
      {children && <div className="row-control">{children}</div>}
    </div>
  );
}

// MARK: Controls

function Toggle({ on, onChange }: { on: boolean; onChange: (on: boolean) => void }) {
  return <button role="switch" aria-checked={on} className={`toggle ${on ? "on" : ""}`} onClick={() => onChange(!on)} />;
}

function Segmented<T extends string>({ value, options, onChange }: { value: T; options: [T, string][]; onChange: (v: T) => void }) {
  return (
    <div className="segmented">
      {options.map(([v, label]) => (
        <button key={v} className={v === value ? "on" : ""} onClick={() => onChange(v)}>
          {label}
        </button>
      ))}
    </div>
  );
}

/** bb1's mark (as in its icon): a rounded screen with the left part filled. */
function Mark({ size = 20 }: { size?: number }) {
  return (
    <svg width={size} height={size * 0.7} viewBox="0 0 40 28" aria-hidden>
      <rect x="1.5" y="1.5" width="37" height="25" rx="7" fill="none" stroke="currentColor" strokeWidth="3" />
      <path d="M23 1.5H8.5a7 7 0 0 0-7 7v11a7 7 0 0 0 7 7H23z" fill="currentColor" />
    </svg>
  );
}
