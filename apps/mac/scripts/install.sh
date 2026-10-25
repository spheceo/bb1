#!/bin/zsh
# Development: build bb1 and install it into /Applications, replacing the copy there, then open it.
# The app does the rest itself: it quits older copies, points the login item at itself, and turns
# on "start at login" when you share the Mac. (Everyone else installs from the .dmg.)
set -euo pipefail
root=${0:A:h:h}
cd "$root"
scripts/bundle.sh

dest="/Applications/bb1.app"
# Quit a running bb1 without launchd restarting it (it only restarts after a crash).
launchctl bootout "gui/$UID/com.bb1.host" 2>/dev/null || true
pkill -x bb1 2>/dev/null || true
sleep 1
rm -rf "$dest"
ditto build/bb1.app "$dest"
open "$dest"
echo "==> installed $dest and opened it"
