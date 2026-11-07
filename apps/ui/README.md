# bb1's window (shared UI)

The main window of every bb1 app — your computers, pairing codes, sharing this computer, settings —
written once in React. The stream window itself stays native in each app (SDL3, Direct3D, Metal).

```sh
bun install
bun run dev     # in a browser, with sample computers (src/mock.ts): ?demo=sharing, ?demo=code, ?page=settings
bun run build   # → dist/index.html, one self-contained page (scripts, styles, font) the apps embed
```

Each app shows the page in the system's web view and talks to it the same way (`src/bridge.ts`):
the app sends its whole state (`State` in `src/types.ts`) through `window.bb1Receive`, the page sends
`Command`s back as JSON. Change `src/types.ts` and the apps' bridges together.

| App | Web view | Bridge |
|---|---|---|
| Linux | WebKitGTK (wry + tao) | `apps/windows/src/linux/ui.rs` |
| Windows | WebView2 (wry + tao) | not yet: the Windows app still draws its own window |
| Mac | WKWebView | not yet: the Mac app still uses its AppKit dashboard |

Live editing inside the Linux app: `bun run dev` here, then `BB1_UI_URL=http://localhost:5173 bb1`.
