#!/bin/sh
# Installs Littera's desktop entry and hicolor icon set under the user's XDG data
# directories (FR-008/FR-009). Re-runnable, never needs root; it only creates
# missing directories and copies files.
set -eu

here=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
data=${XDG_DATA_HOME:-$HOME/.local/share}
app=$here/target/release/littera

if [ ! -x "$app" ]; then
    echo "error: $app is missing — build it first: cargo build --release" >&2
    exit 1
fi

install -d "$data/applications" "$data/icons"
cp -R "$here/assets/icon/hicolor" "$data/icons/"

escaped=$(printf '%s' "$app" | sed 's/\\/\\\\/g; s/"/\\"/g')
sed "s|^Exec=.*|Exec=\"$escaped\"|" "$here/assets/icon/littera.desktop" \
    > "$data/applications/littera.desktop.tmp"
mv "$data/applications/littera.desktop.tmp" "$data/applications/littera.desktop"

echo "installed $data/applications/littera.desktop"
echo "installed $data/icons/hicolor/*/apps/littera.png and hicolor/scalable/apps/littera.svg"
