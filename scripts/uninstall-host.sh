#!/bin/zsh
# Stop the bb1 host and remove it from login. Keeps the pairing key and logs.
set -euo pipefail
launchctl bootout "gui/$UID/com.bb1.host" 2>/dev/null || true
pkill -x bb1-host 2>/dev/null || true
pkill -x bb1 2>/dev/null || true
rm -f "$HOME/Library/LaunchAgents/com.bb1.host.plist"
rm -rf "$HOME/Applications/BB1 Host.app" "$HOME/Applications/BB1.app" "$HOME/Applications/bb1.app"
echo "bb1 removed. Pairing key kept in ~/Library/Application Support/bb1/host-key."
