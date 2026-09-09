// SPDX-License-Identifier: GPL-3.0-or-later

//! The window's side of the Load State browser (`ui/states.rs`): opening
//! it over the states folder, feeding it keys and pad presses, and acting
//! on what it decides -- a load, a deletion, or the file dialog for a
//! state kept somewhere else.

use super::*;
use std::path::Path;

impl App {
    /// The folder the browser lists: the per-user states directory
    /// (`[paths] states`), where named saves and the quick-save slots
    /// live. `None` when the host offers no per-user directory at all.
    fn states_dir(&self) -> Option<PathBuf> {
        crate::savestate::slot_path(1).and_then(|p| p.parent().map(Path::to_path_buf))
    }

    /// Menu row or shortcut: open the browser over the states folder.
    /// With no per-user directory to browse, the file dialog is the only
    /// way to name a state, so that opens instead.
    pub(super) fn open_states_browser(&mut self) {
        let Some(dir) = self.states_dir() else {
            self.load_state_from_dialog(None);
            return;
        };
        self.open_states_browser_at(&dir);
    }

    /// Test seam: the browser over an explicit folder.
    pub(super) fn open_states_browser_at(&mut self, dir: &Path) {
        let panel = ui::StatesPanel::scan(dir, self.emu.machine_descriptor());
        self.ui.panel = Some(Panel::States(Box::new(panel)));
        // The marker that walked the menu here has nothing to stand on
        // in a panel that keeps its own focus.
        self.nav.clear();
        self.request_redraw();
    }

    fn states_panel_mut(&mut self) -> Option<&mut ui::StatesPanel> {
        match self.ui.panel.as_mut() {
            Some(Panel::States(panel)) => Some(panel),
            _ => None,
        }
    }

    /// A pointer click on one of the browser's controls.
    pub(super) fn states_activate(
        &mut self,
        control: UiControl,
        event_loop: Option<&ActiveEventLoop>,
    ) {
        let rows = ui::states_visible_rows();
        let Some(panel) = self.states_panel_mut() else {
            return;
        };
        let action = panel.activate(control, rows);
        self.states_act(action, event_loop);
    }

    /// Keys the open browser answers. Returns true when the key was
    /// consumed; Escape is consumed only while the delete question is up,
    /// so the caller's Escape then closes the panel as it does any other.
    pub(super) fn states_handle_key(
        &mut self,
        code: KeyCode,
        event_loop: Option<&ActiveEventLoop>,
    ) -> bool {
        use crate::video::nav::Dir;
        let rows = ui::states_visible_rows();
        let Some(panel) = self.states_panel_mut() else {
            return false;
        };
        let action = match code {
            KeyCode::ArrowUp => {
                panel.step(Dir::Up, rows);
                ui::StatesAction::None
            }
            KeyCode::ArrowDown => {
                panel.step(Dir::Down, rows);
                ui::StatesAction::None
            }
            KeyCode::ArrowLeft => {
                panel.step(Dir::Left, rows);
                ui::StatesAction::None
            }
            KeyCode::ArrowRight => {
                panel.step(Dir::Right, rows);
                ui::StatesAction::None
            }
            KeyCode::PageUp => {
                panel.page(false, rows);
                ui::StatesAction::None
            }
            KeyCode::PageDown => {
                panel.page(true, rows);
                ui::StatesAction::None
            }
            KeyCode::Home => {
                panel.jump(false, rows);
                ui::StatesAction::None
            }
            KeyCode::End => {
                panel.jump(true, rows);
                ui::StatesAction::None
            }
            KeyCode::Enter | KeyCode::NumpadEnter | KeyCode::Space => panel.press(),
            KeyCode::Delete | KeyCode::Backspace => {
                if panel.confirm_delete {
                    // A second Delete answers the question it asked.
                    panel.focus = ui::StatesFocus::Button(0);
                    panel.press()
                } else {
                    panel.ask_delete()
                }
            }
            KeyCode::Escape if panel.back() => ui::StatesAction::None,
            _ => return false,
        };
        self.states_act(action, event_loop);
        self.request_redraw();
        true
    }

    /// The pad's d-pad while the browser is open: the same walk the arrow
    /// keys make. True when the browser took the direction.
    pub(super) fn states_nav_move(&mut self, dir: crate::video::nav::Dir) -> bool {
        let rows = ui::states_visible_rows();
        let Some(panel) = self.states_panel_mut() else {
            return false;
        };
        panel.step(dir, rows);
        self.request_redraw();
        true
    }

    /// The pad's fire button while the browser is open.
    pub(super) fn states_nav_press(&mut self, event_loop: Option<&ActiveEventLoop>) -> bool {
        let Some(panel) = self.states_panel_mut() else {
            return false;
        };
        let action = panel.press();
        self.states_act(action, event_loop);
        self.request_redraw();
        true
    }

    /// The pad's second button: withdraws the delete question if it is
    /// up. False otherwise, so the caller closes the panel.
    pub(super) fn states_nav_back(&mut self) -> bool {
        let Some(panel) = self.states_panel_mut() else {
            return false;
        };
        let consumed = panel.back();
        if consumed {
            self.request_redraw();
        }
        consumed
    }

    fn states_act(&mut self, action: ui::StatesAction, event_loop: Option<&ActiveEventLoop>) {
        match action {
            ui::StatesAction::None => {}
            ui::StatesAction::Load(index) => {
                let entry = self
                    .states_panel_mut()
                    .and_then(|panel| panel.entries.get(index).cloned());
                let Some(entry) = entry else {
                    return;
                };
                // The panel closes before the load, as picking a file
                // from the dialog does: the restored display is what the
                // eye wants next, not the list. A failed load puts the
                // browser back up with the reason on its status line.
                self.ui.panel = None;
                self.suspend_live_audio_for_host_io();
                let loaded = self.load_state_from_path(&entry.path);
                self.finish_host_io_pause();
                if loaded {
                    self.show_osd(format!("Loaded {}", entry.label));
                    if let Some(event_loop) = event_loop {
                        event_loop.set_control_flow(ControlFlow::Poll);
                    }
                } else {
                    let dir = entry
                        .path
                        .parent()
                        .map(Path::to_path_buf)
                        .or_else(|| self.states_dir());
                    if let Some(dir) = dir {
                        self.open_states_browser_at(&dir);
                        if let Some(panel) = self.states_panel_mut() {
                            panel.select(index, ui::states_visible_rows());
                            panel.status =
                                Some(format!("{} failed to load (see log)", entry.label));
                        }
                    }
                }
            }
            ui::StatesAction::Delete(index) => {
                let running = self.emu.machine_descriptor().clone();
                let Some(panel) = self.states_panel_mut() else {
                    return;
                };
                let Some(entry) = panel.entries.get(index) else {
                    return;
                };
                let (label, path) = (entry.label.clone(), entry.path.clone());
                match std::fs::remove_file(&path) {
                    Ok(()) => {
                        info!("save state deleted: {}", path.display());
                        panel.status = Some(format!("Deleted {label}"));
                    }
                    Err(e) => {
                        warn!("save state delete failed ({}): {e}", path.display());
                        panel.status = Some(format!("Could not delete {label}: {e}"));
                    }
                }
                // The folder as it is now; a deleted slot comes back as an
                // empty one, a deleted named state is gone from the list.
                panel.rescan(&running);
            }
            ui::StatesAction::Browse => {
                self.ui.panel = None;
                self.load_state_from_dialog(event_loop);
            }
        }
    }
}
