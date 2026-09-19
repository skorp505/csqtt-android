#!/usr/bin/env bash
set -euo pipefail

if [[ $# -ne 2 ]]; then
    echo "usage: $0 <version> <output-dir>" >&2
    exit 2
fi

version=$1
output_dir=$2
script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
luci_dir="$script_dir/luci-app-csqtt"
work_dir=$(mktemp -d)
trap 'rm -rf -- "$work_dir"' EXIT

test -f "$luci_dir/Makefile"
test -d "$luci_dir/files"
mkdir -p "$work_dir/control" "$work_dir/data" "$output_dir"

cp -a "$luci_dir/files/root/." "$work_dir/data/"
cp -a "$luci_dir/files/htdocs/." "$work_dir/data/htdocs/"

installed_size=$(du -k -s "$work_dir/data" | awk '{print $1}')
cat >"$work_dir/control/control" <<EOF
Package: luci-app-csqtt
Version: $version
Architecture: all
Maintainer: skorp505 <skorp505@gmail.com>
Depends: luci-base, csqtt-client
Installed-Size: $installed_size
Section: luci
Priority: optional
Description: LuCI web panel for the CSQTT OpenWrt client
EOF

printf '2.0\n' >"$work_dir/debian-binary"
tar -C "$work_dir/control" -czf "$work_dir/control.tar.gz" .
tar -C "$work_dir/data" -czf "$work_dir/data.tar.gz" .
package="$output_dir/luci-app-csqtt_${version}_all.ipk"
ar rcs "$package" "$work_dir/debian-binary" "$work_dir/control.tar.gz" "$work_dir/data.tar.gz"
echo "$package"