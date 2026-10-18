#!/bin/sh
# Prüft, dass der NAS die Version aus der .exe des PC-Dienstes lesen kann:
# die erste Marke, auf die eine Version folgt (nas-proxy/src/pc_setup.rs).
# Die ID nach dem + ist der Anfang der Build-ID (crates/pc-agent/build.rs
# kürzt einen Commit-Hash auf 7 Zeichen).
# Aufruf: check-exe-version.sh <exe> <build-id>
set -eu
exe=$1
build_id=$2
found="$(LC_ALL=C grep -ao 'strata-pc-agent-version:[!-~]\{1,63\}$' "$exe" | head -n1)"
id="${found##*+}"
case "$found" in *+?*) ;; *) id="" ;; esac
case "$build_id" in
    "$id"*) [ -n "$id" ] && exit 0 ;;
esac
echo "Keine Version mit ${build_id} in $exe, gefunden: '$found'" >&2
exit 1
