#!/bin/sh
# Installs Littera's desktop entry and hicolor icon set under the user's XDG
# data directories (FR-008/FR-009). It works from a checkout and from an
# extracted release tarball, creates missing directories, and is safe to
# re-run. Never needs root.
set -eu

here=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
data=${XDG_DATA_HOME:-$HOME/.local/share}

if [ -f "$here/littera.desktop" ] && [ -d "$here/icons/hicolor" ]; then
    entry=$here/littera.desktop
    icons=$here/icons/hicolor
elif [ -f "$here/assets/icon/littera.desktop" ] && [ -d "$here/assets/icon/hicolor" ]; then
    entry=$here/assets/icon/littera.desktop
    icons=$here/assets/icon/hicolor
else
    echo "error: littera.desktop and icons/hicolor were not found next to $0" >&2
    echo "run this script from a checkout or from the extracted release tarball" >&2
    exit 1
fi

install -d "$data/icons" "$data/applications"
# Icons first: a failed copy must not leave a launcher entry whose icon is
# unresolved (the embedded tray icon would still hide the mistake).
cp -R "$icons" "$data/icons/"
install -Dm644 "$entry" "$data/applications/littera.desktop"

echo "installed $data/applications/littera.desktop"
echo "installed $data/icons/hicolor/<size>/apps/littera.png and $data/icons/hicolor/scalable/apps/littera.svg"
