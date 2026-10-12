//! Hooks for driving the app from scripts while testing, see `crates/rnote-ui/README.md`. Only in debug builds,
//! which is what the `devel` profile builds.
//!
//! When `RNOTE_TEST_COMMANDS` names a file, the app reads it twice a second, runs the commands in it, one per line,
//! and deletes it. Commands:
//!
//! - `open PATH`: opens the file or note folder as if it was opened from the app, a picture or Pdf is imported
//!   into the open document.
//! - `action NAME`: activates the action of the window, such as `save-doc` or `active-tab-close`.
//! - `screenshot PATH`: writes what the window shows as Png. Popovers and dialogs are windows of their own and
//!   are not in it.
//!
//! What a command did is logged with `debug!`, errors with `error!`.

// Imports
use crate::RnAppWindow;
use anyhow::Context;
use gtk4::{gdk, glib, graphene, prelude::*};
use std::path::Path;
use std::time::Duration;
use tracing::{debug, error};

const ENV_VAR: &str = "RNOTE_TEST_COMMANDS";

/// Starts reading commands, when the environment asks for it.
pub(crate) fn install(appwindow: &RnAppWindow) {
    let Ok(cmd_file) = std::env::var(ENV_VAR) else {
        return;
    };
    debug!("Reading test commands from {cmd_file:?}");
    glib::timeout_add_local(
        Duration::from_millis(500),
        glib::clone!(
            #[weak]
            appwindow,
            #[upgrade_or]
            glib::ControlFlow::Break,
            move || {
                if let Ok(commands) = std::fs::read_to_string(&cmd_file) {
                    let _ = std::fs::remove_file(&cmd_file);
                    for command in commands.lines().map(str::trim).filter(|l| !l.is_empty()) {
                        run(&appwindow, command);
                    }
                }
                glib::ControlFlow::Continue
            }
        ),
    );
}

fn run(appwindow: &RnAppWindow, command: &str) {
    debug!("Test command: {command}");
    let (name, arg) = command.split_once(' ').unwrap_or((command, ""));
    let result = match name {
        "open" => {
            let file = gtk4::gio::File::for_path(arg);
            glib::spawn_future_local(glib::clone!(
                #[weak]
                appwindow,
                async move { appwindow.open_file_w_dialogs(file, None, true).await }
            ));
            Ok(())
        }
        "action" if appwindow.has_action(arg) => {
            gtk4::gio::prelude::ActionGroupExt::activate_action(appwindow, arg, None);
            Ok(())
        }
        "action" => Err(anyhow::anyhow!("The window has no such action")),
        "screenshot" => screenshot(appwindow, Path::new(arg)),
        _ => Err(anyhow::anyhow!("Unknown command")),
    };
    if let Err(e) = result {
        error!("Test command `{command}` failed, Err: {e:?}");
    }
}

/// Writes what the window shows into a Png.
fn screenshot(appwindow: &RnAppWindow, path: &Path) -> anyhow::Result<()> {
    let (width, height) = (appwindow.width() as f32, appwindow.height() as f32);
    let snapshot = gtk4::Snapshot::new();
    gtk4::WidgetPaintable::new(Some(appwindow)).snapshot(&snapshot, width as f64, height as f64);
    let node = snapshot.to_node().context("The window drew nothing.")?;
    let renderer = appwindow
        .renderer()
        .context("The window has no renderer.")?;
    let texture: gdk::Texture =
        renderer.render_texture(&node, Some(&graphene::Rect::new(0.0, 0.0, width, height)));
    texture.save_to_png(path)?;
    debug!("Screenshot written to {path:?}");
    Ok(())
}
