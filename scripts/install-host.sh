#!/bin/zsh
# Install bb1.app as the host on this Mac: copy to ~/Applications, set Host mode,
# and start it at login (restarting it if it crashes).
set -euo pipefail

root=${0:A:h:h}
cd "$root"
scripts/bundle.sh

dest="$HOME/Applications/bb1.app"
agent="$HOME/Library/LaunchAgents/com.bb1.host.plist"
label="com.bb1.host"
settings="$HOME/Library/Application Support/bb1/settings.json"

launchctl bootout "gui/$UID/$label" 2>/dev/null || true
pkill -x bb1-host 2>/dev/null || true
pkill -x bb1 2>/dev/null || true

mkdir -p "$HOME/Applications" "$HOME/Library/LaunchAgents" "${settings:h}"
rm -rf "$dest" "$HOME/Applications/BB1.app" "$HOME/Applications/BB1 Host.app"   # older names
cp -R "build/bb1.app" "$dest"

# Host mode, keeping any other settings.
/usr/bin/python3 - "$settings" <<'PY'
import json, sys, os
p = sys.argv[1]
s = json.load(open(p)) if os.path.exists(p) else {}
s["mode"] = "host"
json.dump(s, open(p, "w"), indent=2)
PY

sed "s#__APP__#$dest#g" launchd/com.bb1.host.plist > "$agent"
launchctl bootstrap "gui/$UID" "$agent"

echo
echo "Installed: $dest (Host mode)"
echo "It starts at login and restarts within seconds if it crashes. Quit from the menu bar stays quit until next login."
echo "Pairing key: $("$dest/Contents/MacOS/bb1" host --show-key)"
