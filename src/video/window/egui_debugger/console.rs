// SPDX-License-Identifier: GPL-3.0-or-later

use super::*;

impl Layout {
    pub(super) fn console(
        &mut self,
        root: &mut egui::Ui,
        panel: &mut ui::ConsolePanel,
        actions: &mut Vec<Action>,
    ) {
        let id = egui::Id::new("console_entry");
        let first_pass = root.ctx().current_pass_index() == 0;
        let entering = self.last_tool != Some(ToolPanelKind::Console);
        let mut submit = false;
        let mut scroll_pages = 0;
        if first_pass {
            if entering {
                root.memory_mut(|m| m.request_focus(id));
            }
            for (key, pages) in [(egui::Key::PageUp, -1), (egui::Key::PageDown, 1)] {
                if console_key(root, key, true) {
                    scroll_pages += pages;
                }
            }
            if console_key(root, egui::Key::Escape, false) {
                if let Some(focused) = root.memory(|m| m.focused()) {
                    root.memory_mut(|m| m.surrender_focus(focused));
                } else {
                    actions.push(Action::CloseWorkspace);
                }
            } else if root.memory(|m| m.has_focus(id)) {
                for (key, delta) in [(egui::Key::ArrowUp, -1), (egui::Key::ArrowDown, 1)] {
                    if console_key(root, key, true) {
                        panel.history_step(delta);
                        if let Some(mut state) = egui::TextEdit::load_state(root.ctx(), id) {
                            state
                                .cursor
                                .set_char_range(Some(egui::text::CCursorRange::one(
                                    egui::text::CCursor::new(panel.input.chars().count()),
                                )));
                            state.store(root.ctx(), id);
                        }
                    }
                }
                submit = console_key(root, egui::Key::Enter, false);
            }
        }
        egui::Panel::bottom("console_prompt").show(root, |ui| {
            ui.horizontal(|ui| {
                ui.strong("Command");
                ui.label("Enter executes · Shift+Enter adds a line · Up/Down history");
                submit |= ui.button("Execute").clicked();
            });
            let input_before_edit = panel.input.clone();
            let entry = ui.add(
                egui::TextEdit::multiline(&mut panel.input)
                    .id(id)
                    .font(egui::TextStyle::Monospace)
                    .desired_width(f32::INFINITY)
                    .desired_rows(2)
                    .char_limit(16_384)
                    .return_key(egui::KeyboardShortcut::new(
                        egui::Modifiers::SHIFT,
                        egui::Key::Enter,
                    )),
            );
            if panel.input != input_before_edit {
                panel.history_pos = None;
            }
            if submit && !panel.input.trim().is_empty() {
                actions.push(Action::ConsoleSubmit(std::mem::take(&mut panel.input)));
                self.console_to_end = true;
                if !entry.has_focus() {
                    entry.request_focus();
                }
            }
        });
        egui::CentralPanel::default().show(root, |ui| {
            let mut scroll = ScrollArea::both()
                .id_salt("console_output_scroll")
                .auto_shrink([false, false])
                .stick_to_bottom(true);
            if self.console_to_end {
                scroll = scroll.vertical_scroll_offset(f32::INFINITY);
            }
            scroll.show(ui, |ui| {
                if scroll_pages != 0 {
                    ui.scroll_with_delta(egui::vec2(
                        0.0,
                        -scroll_pages as f32 * ui.clip_rect().height() * 0.9,
                    ));
                }
                let output = panel
                    .output
                    .iter()
                    .map(String::as_str)
                    .collect::<Vec<_>>()
                    .join("\n");
                ui.add(
                    egui::Label::new(RichText::new(output).monospace())
                        .selectable(true)
                        .wrap_mode(egui::TextWrapMode::Extend),
                );
            });
        });
        root.memory_mut(|memory| {
            memory.set_focus_lock_filter(
                id,
                egui::EventFilter {
                    horizontal_arrows: true,
                    vertical_arrows: true,
                    escape: true,
                    ..Default::default()
                },
            )
        });
        self.console_to_end = false;
    }
}

// egui's consume_key deliberately ignores extra Shift/Alt. Here Shift+Enter
// belongs to the editor, and a held Enter must not execute another command.
fn console_key(ui: &mut egui::Ui, key: egui::Key, accept_repeat: bool) -> bool {
    ui.input_mut(|input| {
        let mut pressed = false;
        input.events.retain(|event| {
            if let egui::Event::Key {
                key: actual,
                pressed: true,
                repeat,
                modifiers,
                ..
            } = event
            {
                if *actual == key && modifiers.is_none() {
                    pressed |= accept_repeat || !repeat;
                    return false;
                }
            }
            true
        });
        pressed
    })
}
