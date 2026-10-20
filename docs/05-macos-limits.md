# macOS limits

Things macOS imposes that no app can fully get around. bb1 handles them as well as it can and says clearly when it can't.

## Permissions

| Side | Permission | Why |
|---|---|---|
| Host | Screen & System Audio Recording | Capture |
| Host | Accessibility | Inject mouse and keyboard |
| Client | Accessibility (or Input Monitoring) | Capture Cmd+Tab and other system shortcuts while focused |
| Host | Local Network | Bonjour and LAN connections |

- Each is granted by hand once in System Settings.
- macOS ties these permissions to the app's code signature. **Unsigned or ad-hoc builds lose their permissions on every rebuild.** We sign every build with a local self-signed certificate made by `scripts/signing-identity.sh`, so permissions survive rebuilds. Full Xcode isn't needed; see [07-building-and-running](07-building-and-running.md).
- Recent macOS versions sometimes ask again for Screen Recording. The host detects lost permission, tells the client, and shows a notification on the host.

## Sleep

- While a client is connected, the host keeps the system and display awake (see [04](04-reliability.md)).
- When no one is connected, the host can sleep. To reach it after that, enable **Wake for network access** in System Settings → Energy. Waking from sleep can take a few seconds; the client keeps trying.
- If the host is a laptop with the lid closed, it needs external power and a display (or a dummy HDMI plug) to stay awake. A desktop Mac doesn't have this problem.

## Lock screen and login window

- bb1 runs inside your logged-in session. It **can** see and type into the lock screen (you can type your password to unlock), because the session is still running.
- It **cannot** reach the login window after a reboot or log-out. Nothing in your session is running yet.
- With **FileVault**, after a reboot the Mac waits for your password before macOS even starts. No app can help with that.

Workarounds:

- For planned restarts: `sudo fdesetup authrestart` reboots and unlocks FileVault once.
- Turn on automatic login (requires FileVault off), or accept that after an unplanned reboot someone at home has to log in.
- Fixing this properly would need a system-level helper outside the user session. Out of scope for v1.

## Display

- If no monitor is attached, macOS may run a tiny default resolution or none at all. A real monitor or an HDMI dummy plug avoids this. A virtual display is possible later but not in v1.
