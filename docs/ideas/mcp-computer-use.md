# Idea: bb1 as an MCP computer-use tool

**Status:** idea, not started (2026-10-20)

## The point

While bb1 is sharing a Mac, it also offers that Mac to AI agents as a set of MCP tools. You can then say to any agent (Claude Code, Codex, …): "use my Mac and do X". Watching is optional: connect with bb1 and you see the agent work live.

## Tools

These are the actions computer-use tools generally share:

| Tool | Does |
|---|---|
| `screenshot` | Returns the screen as an image (scaled down for the model) |
| `click` / `double_click` / `right_click` | At x, y |
| `move` / `drag` | Pointer movement and drag paths |
| `scroll` | At x, y, by dx, dy |
| `type` | Types a string |
| `key` | Key combos, e.g. `cmd+t`, `return` |
| `wait` | Pauses for a moment |

Extras bb1 can add because it runs on the Mac:

- `click_element`: click a button by its label using the accessibility tree. More reliable than pixel coordinates.
- `focused_field`: what's focused, and whether it's a password field. This is the hook for the [phone app](phone-app.md).
- `open_app` and `open_url`.

## How

- A small MCP server inside the host process, on localhost (and optionally over Tailscale). It reuses the screen capture and input injection bb1 already has.
- Agents connect through a normal MCP config entry, with a token so only your agents get in.

## Safety

- A visible "an agent is controlling this Mac" badge on the host while the agent acts.
- A hotkey that stops it instantly. Any real mouse or keyboard input on the Mac also pauses the agent.
- Every action logged, with timestamp, tool and arguments. Typed text is logged as a length only.

## Effort

About a day for a first version.
