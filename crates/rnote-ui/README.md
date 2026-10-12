# rnote-ui

The app: GTK4 and libadwaita, with `.ui` templates in `data/ui/` and one stylesheet, `data/ui/style.css`.

## Driving the app from scripts

Dev builds (meson profile `devel`, which builds with `debug_assertions`) have test hooks in `src/testhooks.rs`. Release
builds do not have them. When `RNOTE_TEST_COMMANDS` names a file, the app reads it twice a second, runs the commands
in it, one per line, and deletes it.

| Command | Does |
|---|---|
| `open PATH` | Opens a file or note folder as the app's own Open does. A picture or Pdf is imported into the open document |
| `action NAME` | Activates an action of the window, for example `save-doc`, `undo`, `active-tab-close`, `open-settings`. The names are in `src/appwindow/actions.rs` |
| `screenshot PATH` | Writes what the window shows as a Png, drawn by the app itself, so it works on Wayland and X11. Popovers and dialogs are separate windows and are not in it |

Each command is logged at `debug` level (`Test command: ...`), and a failed one at `error` level.

The scripts in `misc/ui-testing/` go with it:

```bash
misc/ui-testing/run_ui.sh &                    # dev build with its own settings and data, hooks on
misc/ui-testing/cmd.sh "open $PWD/tmp/note.rnoted" "action save-doc"
misc/ui-testing/cmd.sh "screenshot tmp/ui-test/shot.png"
python3 misc/ui-testing/atspi.py dump          # the widgets as accessibility sees them
python3 misc/ui-testing/atspi.py click button "Save Document"
grep "Test command" tmp/ui-test/log/ui_run.log
```

- The user's own Rnote has the same app id: a file or a click meant for the test instance would go to it. `run_ui.sh`
  refuses to start while any `rnote` process runs. Stop only the pid the launcher started.
- `run_ui.sh` copies the settings schema from `_mesonbuild/`. After adding a settings key, regenerate it there
  (`meson compile -C _mesonbuild`), or the app aborts on start because the key is missing.
- `atspi.py` cannot press keys or drag on the canvas. What needs that gets a new hook command.
