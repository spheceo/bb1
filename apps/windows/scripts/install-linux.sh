#!/usr/bin/env bash
# Builds bb1 for Linux and installs it for this user: ~/.local/bin/bb1, an entry in the app list
# (with its icon), and optionally sharing at login.
#
#   scripts/install-linux.sh                    build and install
#   scripts/install-linux.sh --share-at-login   also share this computer whenever you log in
#   scripts/install-linux.sh --uninstall        remove it (settings in ~/.config/bb1 stay)
#
# Needs (Ubuntu 25.04 or later / Debian 13; names differ on other distributions):
#   sudo apt install libsdl3-dev libavcodec-dev libavformat-dev libavutil-dev libswscale-dev \
#                    libswresample-dev libavfilter-dev libavdevice-dev libpipewire-0.3-dev libclang-dev clang
set -euo pipefail
cd "$(dirname "$0")/.."

BIN="$HOME/.local/bin"
APPS="${XDG_DATA_HOME:-$HOME/.local/share}/applications"
ICONS="${XDG_DATA_HOME:-$HOME/.local/share}/icons/hicolor/256x256/apps"
AUTOSTART="${XDG_CONFIG_HOME:-$HOME/.config}/autostart"

if [[ "${1:-}" == "--uninstall" ]]; then
  rm -f "$BIN/bb1" "$APPS/bb1.desktop" "$ICONS/bb1.png" "$AUTOSTART/bb1-host.desktop"
  echo "bb1 removed. Your pairings and settings are still in ~/.config/bb1."
  exit 0
fi

missing=()
for lib in sdl3 libavcodec libswscale libpipewire-0.3; do
  pkg-config --exists "$lib" || missing+=("$lib")
done
if (( ${#missing[@]} )); then
  echo "Missing development libraries: ${missing[*]}"
  echo "On Ubuntu: sudo apt install libsdl3-dev libavcodec-dev libavformat-dev libavutil-dev libswscale-dev libswresample-dev libavfilter-dev libavdevice-dev libpipewire-0.3-dev libclang-dev clang"
  exit 1
fi

cargo build --release --bin bb1
mkdir -p "$BIN" "$APPS" "$ICONS"
install -m 755 target/release/bb1 "$BIN/bb1.new" && mv -f "$BIN/bb1.new" "$BIN/bb1"
install -m 644 res/bb1.png "$ICONS/bb1.png"
cat > "$APPS/bb1.desktop" <<EOF
[Desktop Entry]
Type=Application
Name=bb1
Comment=Use your other computers, or share this one
Exec=$BIN/bb1
Icon=bb1
Terminal=false
Categories=Network;RemoteAccess;
StartupWMClass=bb1
EOF
if [[ "${1:-}" == "--share-at-login" ]]; then
  mkdir -p "$AUTOSTART"
  cat > "$AUTOSTART/bb1-host.desktop" <<EOF
[Desktop Entry]
Type=Application
Name=bb1 sharing
Comment=Share this computer with bb1
Exec=$BIN/bb1 --host
Icon=bb1
Terminal=false
X-GNOME-Autostart-enabled=true
EOF
  echo "This computer is shared whenever you log in (remove $AUTOSTART/bb1-host.desktop to stop)."
fi
command -v update-desktop-database >/dev/null && update-desktop-database "$APPS" >/dev/null 2>&1 || true
command -v gtk-update-icon-cache >/dev/null && gtk-update-icon-cache -q "${XDG_DATA_HOME:-$HOME/.local/share}/icons/hicolor" >/dev/null 2>&1 || true
echo "Installed: $BIN/bb1 ($("$BIN/bb1" --version)). Open bb1 from your apps, or run bb1."
