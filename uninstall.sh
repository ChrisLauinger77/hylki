#!/usr/bin/env bash
#
# Uninstall Vireo for the current user: removes everything install.sh placed
# into the XDG user prefix (binary, icons, desktop entry, translations), and
# the icon override the app itself writes there when an app icon is chosen
# in Settings. Your accounts, mail cache and settings stay unless asked.
#
# Usage:  ./uninstall.sh              # removes from ~/.local
#         ./uninstall.sh --purge      # also removes settings, cache and data
#         PREFIX=/usr ./uninstall.sh  # system-wide (run with sudo)
set -euo pipefail

APP_ID="co.hyprlab.Vireo"
PREFIX="${PREFIX:-$HOME/.local}"
PURGE=0
for arg in "$@"; do
    case "$arg" in
        --purge) PURGE=1 ;;
        *) echo "unknown option: $arg" >&2; exit 2 ;;
    esac
done

echo "==> Removing binary"
rm -f "$PREFIX/bin/vireo"

echo "==> Removing icons"
for size in 256x256 512x512; do
    rm -f "$PREFIX/share/icons/hicolor/$size/apps/$APP_ID.png"
done
rm -f "$PREFIX/share/icons/hicolor/scalable/apps/$APP_ID.svg"
# The app icon chosen in Settings lives beside the shipped one under its own
# name (`co.hyprlab.Vireo-<choice>.png`), in the user's icon directory.
rm -f "$HOME"/.local/share/icons/hicolor/*/apps/"$APP_ID"-*.png

echo "==> Removing desktop entry"
rm -f "$PREFIX/share/applications/$APP_ID.desktop"

echo "==> Removing translations"
for mo in "$PREFIX"/share/locale/*/LC_MESSAGES/vireo.mo; do
    [ -e "$mo" ] || continue
    rm -f "$mo"
    rmdir --ignore-fail-on-non-empty "$(dirname "$mo")" 2>/dev/null || true
done

echo "==> Updating caches"
gtk-update-icon-cache -f -t "$PREFIX/share/icons/hicolor" 2>/dev/null || true
update-desktop-database "$PREFIX/share/applications" 2>/dev/null || true

if [ "$PURGE" = 1 ]; then
    echo "==> Removing settings, cache and data"
    rm -rf "${XDG_CONFIG_HOME:-$HOME/.config}/vireo" \
           "${XDG_CACHE_HOME:-$HOME/.cache}/vireo" \
           "${XDG_DATA_HOME:-$HOME/.local/share}/vireo"
    echo "    Account passwords stay in the system keyring (Passwords and Keys, entries named Vireo)."
fi

echo "==> Done. Vireo has been removed from $PREFIX."
if [ "$PURGE" = 0 ]; then
    echo "    Settings (~/.config/vireo), the mail cache (~/.cache/vireo) and data (~/.local/share/vireo)"
    echo "    were left in place; run with --purge to remove them too."
fi
