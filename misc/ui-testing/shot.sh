#!/usr/bin/env bash
# Captures the window of the app started with run_ui.sh into the given png file.
#
#   misc/ui-testing/shot.sh OUT.png
#
# The capture can show the frame before the last change. Make the app draw again first, for example by
# toggling "Focus Mode" twice with atspi.py, and look at the image before trusting it.
set -euo pipefail
wid=""
for id in $(xprop -root _NET_CLIENT_LIST | sed 's/.*# //; s/,//g'); do
    xprop -id "$id" WM_CLASS 2>/dev/null | grep -q '"rnote"' && wid=$id
done
[ -n "$wid" ] || { echo "no rnote window found" >&2; exit 1; }
import -window "$wid" "$1"
echo "$1 (window $wid)"
