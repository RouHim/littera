#!/bin/sh
# Rewrites the crate version in Cargo.toml and the matching Cargo.lock package
# entry. semantic-release runs this as its prepare step; the release job then
# commits both files together with CHANGELOG.md.
#
# Both files carry the version twice (Cargo.toml `[package] version`, the
# `[[package]] name = "littera"` block in Cargo.lock), so the rewrite is scoped
# to exactly those two places rather than every `version = "…"` line.
set -eu

version=${1:-}
case $version in
    '' | *[!0-9A-Za-z.+-]* | [!0-9]*)
        echo "usage: $0 <semver>" >&2
        exit 2
        ;;
esac

root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)

sed -E -i "0,/^version = \"[^\"]*\"\$/s//version = \"$version\"/" "$root/Cargo.toml"

awk -v version="$version" '
    /^name = "littera"$/ { inside = 1; print; next }
    inside && /^version = "/ {
        sub(/"[^"]*"/, "\"" version "\"")
        inside = 0
    }
    { print }
' "$root/Cargo.lock" > "$root/Cargo.lock.tmp"
mv "$root/Cargo.lock.tmp" "$root/Cargo.lock"

grep -q "^version = \"$version\"\$" "$root/Cargo.toml"
