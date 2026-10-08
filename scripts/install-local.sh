#!/bin/sh
set -eu
binary=${1:?Usage: install-local.sh /path/to/shardrop [destination-directory]}
destination=${2:-"$HOME/.local/bin"}
[ -f "$binary" ] || { echo "Executable not found: $binary" >&2; exit 1; }
mkdir -p "$destination"
install -m 755 "$binary" "$destination/shardrop"
printf 'Installed %s/shardrop\nAdd %s to PATH if needed.\n' "$destination" "$destination"
