#!/usr/bin/env bash
# Sends commands to the app started with run_ui.sh, one per argument, and waits until the app has read them.
#
#   misc/ui-testing/cmd.sh "open $PWD/tmp/some.rnoted" "action save-doc" "screenshot tmp/ui-test/shot.png"
#
# The commands are in crates/rnote-ui/README.md. Paths are relative to the repository root.
set -euo pipefail
cd "$(dirname "$0")/../.."
file=${RNOTE_TEST_COMMANDS:-tmp/ui-test/cmd}
[ -e "$file" ] && { echo "$file is still waiting to be read, is the app running?" >&2; exit 1; }
printf '%s\n' "$@" > "$file"
# The app reads the file twice a second and then deletes it
for _ in $(seq 50); do [ -e "$file" ] || exit 0; sleep 0.2; done
echo "The app did not read $file within 10 s. Is it a dev build started with run_ui.sh?" >&2
exit 1
