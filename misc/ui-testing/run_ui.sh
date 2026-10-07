#!/usr/bin/env bash
# Starts the dev build of the app for testing, without touching the user's settings or data.
#
#   misc/ui-testing/run_ui.sh [FILE.rnote ...] &
#
# - Settings live in memory only, starting from the defaults.
# - XDG_DATA_HOME (search index) is tmp/ui-test/data, or what UI_TEST_DATA says.
# - The X11 backend is used, so the window can be captured with shot.sh on a Wayland desktop.
# - The log goes to tmp/ui-test/log/ui_run.log.
set -euo pipefail
# A running Rnote with the same app id would get the files and the clicks instead of the test instance
if pgrep -x rnote >/dev/null; then
    echo "Rnote is running already (pid $(pgrep -x rnote | tr '\n' ' ')). Close it first." >&2
    exit 1
fi
cd "$(dirname "$0")/../.."
dir=tmp/ui-test
mkdir -p "$dir/schemas" "$dir/log" "${UI_TEST_DATA:-$dir/data}"
cp _mesonbuild/crates/rnote-ui/data/*.gschema.xml "$dir/schemas/"
glib-compile-schemas "$dir/schemas"
exec env GDK_BACKEND=x11 GTK_A11Y=atspi GSETTINGS_BACKEND=memory GSETTINGS_SCHEMA_DIR="$PWD/$dir/schemas" \
    XDG_DATA_HOME="$PWD/${UI_TEST_DATA:-$dir/data}" RUST_LOG=rnote=debug \
    _mesonbuild/target/debug/rnote "$@" >"$dir/log/ui_run.log" 2>&1
