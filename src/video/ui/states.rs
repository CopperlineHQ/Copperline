// SPDX-License-Identifier: GPL-3.0-or-later

//! The Load State browser: the states folder and the ten quick-save
//! slots as a list of thumbnails, newest first, with the machine and
//! media each state was taken on, so a state can be picked by what it
//! shows rather than by a timestamp in a file dialog.
//!
//! The panel is self-contained: it snapshots the folder when it opens
//! (every entry is a `savestate::peek`, which reads only a file's header
//! and metadata) and keeps its own selection and focus. Up and down walk
//! the list; the focus steps off its foot onto the button row, where left
//! and right pick a button; Return works whatever is focused; Delete asks
//! before removing a file. The pointer does the same by clicking. The
//! window (`app_states.rs`) turns the panel's answers into loads and
//! deletions.

use super::*;
use crate::savestate::{self, StateMeta};
use crate::video::nav::Dir;
use crate::video::window::texture_height;
use std::path::{Path, PathBuf};

/// Pixels of thumbnail shown per row: the saved 240x180 picture at a
/// third, which leaves room for three lines of text beside it.
pub(in crate::video) const STATE_THUMB_W: usize = 80;
pub(in crate::video) const STATE_THUMB_H: usize = 60;
/// Row pitch: the thumbnail plus a little air.
pub(in crate::video) const STATE_ROW_H: usize = STATE_THUMB_H + 8;
/// The folder line under the title bar.
const STATE_HEADER_H: usize = 30;
/// The button row and the status line under the list.
const STATE_FOOTER_H: usize = 58;
const STATE_BUTTON_W: usize = 96;
const STATE_BUTTON_H: usize = 22;
const STATE_BUTTON_GAP: usize = 10;
const STATE_PANEL_W: usize = 660;
/// Rows the panel would like to show; fewer when the display is short.
const STATE_ROWS_WANTED: usize = 5;
/// The margin inside the panel's edge.
const STATE_INSET: usize = 12;

/// The wash behind the selected row, and the frame around the row the
/// pointer is over.
const STATE_ROW_SELECTED: u32 = rgba(0, 85, 170);
const STATE_ROW_HOVER: u32 = rgba(90, 92, 100);
/// A thumbnail-less entry shows a dark plate where the picture would be.
const STATE_THUMB_PLATE: u32 = rgba(12, 13, 15);

/// The footer buttons, in row order; `Button(i)` in [`StatesFocus`]
/// indexes this.
const STATE_BUTTONS: [&str; 3] = ["Load", "Delete", "Browse..."];
/// The confirm strip's buttons.
const STATE_CONFIRM_BUTTONS: [&str; 2] = ["Delete", "Cancel"];

/// A decoded thumbnail, packed RGBA.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StateThumbnail {
    pub pixels: Vec<u32>,
    pub width: usize,
    pub height: usize,
}

/// One row of the browser.
#[derive(Debug, Clone, PartialEq)]
pub struct StateEntry {
    pub path: PathBuf,
    /// `Slot 3`, or the file name.
    pub label: String,
    /// The quick-save slot number (1-based) for a slot row.
    pub slot: Option<usize>,
    /// A slot row whose file has never been written.
    pub empty: bool,
    /// When the state was saved: its metadata's wall clock, or the file's
    /// modification time for a state without metadata.
    pub saved_at_unix: Option<u64>,
    pub emulated_seconds: Option<f64>,
    /// Machine summary: the metadata's line, or the descriptor's.
    pub machine: String,
    /// The media line from the metadata; empty without one.
    pub media: String,
    pub thumbnail: Option<StateThumbnail>,
    /// How the state's machine differs from the running one; `None`
    /// when they match (or the file could not be read at all).
    pub mismatch: Option<String>,
    /// Why the file could not be read, when it could not.
    pub error: Option<String>,
}

impl StateEntry {
    /// Whether Load has anything to load.
    pub fn loadable(&self) -> bool {
        !self.empty && self.error.is_none()
    }

    /// Whether Delete has a file to remove. An unreadable file is still a
    /// file, and the way to be rid of it.
    pub fn deletable(&self) -> bool {
        !self.empty
    }
}

/// Where the focus stands in the panel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatesFocus {
    /// On the list; the selected row is the focused one.
    List,
    /// On a footer button (or, while the confirm strip is up, one of its
    /// two buttons).
    Button(usize),
}

/// What a press asks the window to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatesAction {
    None,
    Load(usize),
    /// Remove the entry's file; the panel has already asked.
    Delete(usize),
    Browse,
}

pub struct StatesPanel {
    pub dir: PathBuf,
    pub entries: Vec<StateEntry>,
    pub selected: usize,
    /// First visible row.
    pub scroll: usize,
    pub focus: StatesFocus,
    /// The confirm strip is up for `selected`.
    pub confirm_delete: bool,
    /// The last thing that happened, shown on the status line.
    pub status: Option<String>,
}

/// When a state was saved: the time in its metadata, or the file's own
/// modification time. Zero there is the metadata's "unknown" (a browser
/// build has no clock to stamp with), not midnight in 1970, so it falls
/// back rather than dating the state to the epoch and sorting it there.
fn saved_at(meta: Option<&StateMeta>, modified: Option<u64>) -> Option<u64> {
    meta.map(|m| m.saved_at_unix)
        .filter(|&at| at != 0)
        .or(modified)
}

impl StatesPanel {
    /// Snapshot `dir`: the ten quick-save slots first, then every other
    /// `.clstate` in the folder, newest first. `running` is the machine
    /// the window has now, for flagging states taken on another.
    pub fn scan(dir: &Path, running: &crate::config::MachineDescriptor) -> Self {
        let mut entries: Vec<StateEntry> = (1..=savestate::SLOT_COUNT)
            .map(|slot| {
                let path = savestate::slot_path_in(dir, slot).expect("slot in range");
                if path.exists() {
                    let mut entry = read_entry(&path, running);
                    entry.label = format!("Slot {slot}");
                    entry.slot = Some(slot);
                    entry
                } else {
                    StateEntry {
                        path,
                        label: format!("Slot {slot}"),
                        slot: Some(slot),
                        empty: true,
                        saved_at_unix: None,
                        emulated_seconds: None,
                        machine: String::new(),
                        media: String::new(),
                        thumbnail: None,
                        mismatch: None,
                        error: None,
                    }
                }
            })
            .collect();
        let slot_paths: Vec<PathBuf> = entries.iter().map(|e| e.path.clone()).collect();
        let mut named: Vec<StateEntry> = std::fs::read_dir(dir)
            .map(|dir| {
                dir.filter_map(Result::ok)
                    .map(|entry| entry.path())
                    .filter(|path| {
                        path.extension()
                            .is_some_and(|ext| ext.eq_ignore_ascii_case("clstate"))
                            && !slot_paths.contains(path)
                    })
                    .map(|path| read_entry(&path, running))
                    .collect()
            })
            .unwrap_or_default();
        // Newest first; ties (and undated files) by name so the order is
        // stable from one open to the next.
        named.sort_by(|a, b| {
            b.saved_at_unix
                .cmp(&a.saved_at_unix)
                .then_with(|| a.label.cmp(&b.label))
        });
        entries.extend(named);
        Self {
            dir: dir.to_path_buf(),
            entries,
            selected: 0,
            scroll: 0,
            focus: StatesFocus::List,
            confirm_delete: false,
            status: None,
        }
    }

    /// Re-read the folder after a change, keeping the selection where it
    /// was as far as the list allows.
    pub fn rescan(&mut self, running: &crate::config::MachineDescriptor) {
        let selected = self.selected;
        let status = self.status.take();
        *self = Self::scan(&self.dir, running);
        self.selected = selected.min(self.entries.len().saturating_sub(1));
        self.status = status;
        self.scroll_to_selected(STATE_ROWS_WANTED);
    }

    pub fn selected_entry(&self) -> Option<&StateEntry> {
        self.entries.get(self.selected)
    }

    /// Bring the selected row into the visible window of `rows` rows.
    pub fn scroll_to_selected(&mut self, rows: usize) {
        let rows = rows.max(1);
        if self.selected < self.scroll {
            self.scroll = self.selected;
        } else if self.selected >= self.scroll + rows {
            self.scroll = self.selected + 1 - rows;
        }
        let max_scroll = self.entries.len().saturating_sub(rows);
        self.scroll = self.scroll.min(max_scroll);
    }

    /// Select a row (a pointer hover or click), scrolling it into view.
    pub fn select(&mut self, index: usize, rows: usize) {
        if index < self.entries.len() {
            self.selected = index;
            self.focus = StatesFocus::List;
            self.scroll_to_selected(rows);
        }
    }

    /// A cursor key or d-pad direction. `rows` is how many rows the panel
    /// shows at once, for scrolling and paging.
    pub(in crate::video) fn step(&mut self, dir: Dir, rows: usize) {
        if self.confirm_delete {
            // Only the two confirm buttons are live while the question
            // is up.
            self.focus = match (self.focus, dir) {
                (StatesFocus::Button(_), Dir::Left) => StatesFocus::Button(0),
                (StatesFocus::Button(_), Dir::Right) => StatesFocus::Button(1),
                (focus, _) => focus,
            };
            return;
        }
        let last = self.entries.len().saturating_sub(1);
        match (self.focus, dir) {
            (StatesFocus::List, Dir::Up) => {
                self.selected = self.selected.saturating_sub(1);
            }
            (StatesFocus::List, Dir::Down) => {
                if self.selected < last {
                    self.selected += 1;
                } else {
                    self.focus = StatesFocus::Button(0);
                }
            }
            // Left and right on the list go nowhere: a row is the whole
            // width, and the buttons are below, not beside.
            (StatesFocus::List, Dir::Left | Dir::Right) => {}
            (StatesFocus::Button(_), Dir::Up) => self.focus = StatesFocus::List,
            (StatesFocus::Button(i), Dir::Left) => {
                self.focus = StatesFocus::Button(i.saturating_sub(1));
            }
            (StatesFocus::Button(i), Dir::Right) => {
                self.focus = StatesFocus::Button((i + 1).min(STATE_BUTTONS.len() - 1));
            }
            (StatesFocus::Button(_), Dir::Down) => {}
        }
        self.scroll_to_selected(rows);
    }

    /// Page the list by `rows` rows in either direction.
    pub fn page(&mut self, down: bool, rows: usize) {
        if self.confirm_delete || self.entries.is_empty() {
            return;
        }
        let last = self.entries.len() - 1;
        self.selected = if down {
            (self.selected + rows.max(1)).min(last)
        } else {
            self.selected.saturating_sub(rows.max(1))
        };
        self.focus = StatesFocus::List;
        self.scroll_to_selected(rows);
    }

    /// Jump to the first or last row.
    pub fn jump(&mut self, to_end: bool, rows: usize) {
        if self.confirm_delete || self.entries.is_empty() {
            return;
        }
        self.selected = if to_end { self.entries.len() - 1 } else { 0 };
        self.focus = StatesFocus::List;
        self.scroll_to_selected(rows);
    }

    /// Return, Space, or the pad's fire button: work whatever is focused.
    pub fn press(&mut self) -> StatesAction {
        if self.confirm_delete {
            return match self.focus {
                StatesFocus::Button(0) => {
                    self.confirm_delete = false;
                    self.focus = StatesFocus::List;
                    StatesAction::Delete(self.selected)
                }
                _ => {
                    self.cancel_delete();
                    StatesAction::None
                }
            };
        }
        match self.focus {
            StatesFocus::List | StatesFocus::Button(0) => self.load_selected(),
            StatesFocus::Button(1) => self.ask_delete(),
            StatesFocus::Button(_) => StatesAction::Browse,
        }
    }

    /// The pointer on one of the panel's controls.
    pub fn activate(&mut self, control: UiControl, rows: usize) -> StatesAction {
        match control {
            UiControl::StateRow(index) => {
                if self.confirm_delete {
                    // The question is about one row; a click elsewhere
                    // withdraws it rather than answering it.
                    self.cancel_delete();
                    return StatesAction::None;
                }
                self.select(index, rows);
                self.load_selected()
            }
            UiControl::StateLoad => {
                self.focus = StatesFocus::Button(0);
                self.load_selected()
            }
            UiControl::StateDelete => {
                self.focus = StatesFocus::Button(1);
                self.ask_delete()
            }
            UiControl::StateBrowse => StatesAction::Browse,
            UiControl::StateConfirmDelete => {
                self.confirm_delete = false;
                self.focus = StatesFocus::List;
                StatesAction::Delete(self.selected)
            }
            UiControl::StateCancelDelete => {
                self.cancel_delete();
                StatesAction::None
            }
            _ => StatesAction::None,
        }
    }

    /// The Delete key: the same question the Delete button asks.
    pub fn ask_delete(&mut self) -> StatesAction {
        match self.selected_entry() {
            Some(entry) if entry.deletable() => {
                self.confirm_delete = true;
                // Cancel is the safe answer, so it is where the focus
                // lands: a second Return withdraws the question.
                self.focus = StatesFocus::Button(1);
            }
            Some(entry) => self.status = Some(format!("{} is empty", entry.label)),
            None => {}
        }
        StatesAction::None
    }

    /// Escape or the pad's second button. `true` when it withdrew the
    /// confirm strip; `false` when there was nothing to step back out of
    /// and the panel itself should close.
    pub fn back(&mut self) -> bool {
        if self.confirm_delete {
            self.cancel_delete();
            return true;
        }
        false
    }

    fn cancel_delete(&mut self) {
        self.confirm_delete = false;
        self.focus = StatesFocus::List;
    }

    fn load_selected(&mut self) -> StatesAction {
        match self.selected_entry() {
            Some(entry) if entry.loadable() => StatesAction::Load(self.selected),
            Some(entry) if entry.empty => {
                self.status = Some(format!("{} is empty", entry.label));
                StatesAction::None
            }
            Some(entry) => {
                self.status = Some(format!("{} cannot be read", entry.label));
                StatesAction::None
            }
            None => StatesAction::None,
        }
    }
}

/// Read one file's header and metadata into a row. A file that cannot be
/// read is still a row, saying so, rather than a file that silently is
/// not there.
fn read_entry(path: &Path, running: &crate::config::MachineDescriptor) -> StateEntry {
    let label = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string());
    let modified = std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs());
    match savestate::peek_path(path) {
        Ok(peeked) => {
            let mismatch = {
                let diffs = running.differences(&peeked.descriptor);
                (!diffs.is_empty()).then(|| diffs.join(", "))
            };
            let (thumbnail, meta_fields) = match &peeked.meta {
                Some(meta) => (decode_thumbnail(meta), Some(meta)),
                None => (None, None),
            };
            StateEntry {
                path: path.to_path_buf(),
                label,
                slot: None,
                empty: false,
                saved_at_unix: saved_at(meta_fields, modified),
                emulated_seconds: meta_fields.map(|m| m.emulated_seconds),
                machine: meta_fields
                    .map(|m| m.machine.clone())
                    .unwrap_or_else(|| peeked.descriptor.short_summary()),
                media: meta_fields.map(|m| m.media.summary()).unwrap_or_default(),
                thumbnail,
                mismatch,
                error: None,
            }
        }
        Err(e) => StateEntry {
            path: path.to_path_buf(),
            label,
            slot: None,
            empty: false,
            saved_at_unix: modified,
            emulated_seconds: None,
            machine: String::new(),
            media: String::new(),
            thumbnail: None,
            mismatch: None,
            error: Some(format!("{e:#}")),
        },
    }
}

fn decode_thumbnail(meta: &StateMeta) -> Option<StateThumbnail> {
    match meta.thumbnail_pixels() {
        Ok(Some((pixels, width, height))) => Some(StateThumbnail {
            pixels,
            width,
            height,
        }),
        Ok(None) => None,
        Err(e) => {
            log::warn!("state browser: thumbnail not shown: {e:#}");
            None
        }
    }
}

/// Emulated time as `m:ss.t`, or `h:mm:ss` past an hour.
pub(in crate::video) fn format_emulated(seconds: f64) -> String {
    let total = seconds.max(0.0);
    let hours = (total / 3600.0).floor() as u64;
    let minutes = ((total % 3600.0) / 60.0).floor() as u64;
    let secs = total % 60.0;
    if hours > 0 {
        format!("{hours}:{minutes:02}:{:02}", secs.floor() as u64)
    } else {
        format!("{minutes}:{secs:04.1}")
    }
}

// ---------------------------------------------------------------------------
// Layout
// ---------------------------------------------------------------------------

/// The panel's size: wide enough for a thumbnail and three lines of text,
/// as many rows as the display has room for.
pub(super) fn states_panel_dims() -> (usize, usize) {
    let fixed = TITLE_H + STATE_HEADER_H + STATE_FOOTER_H;
    let rows = states_visible_rows_for(present_height());
    (STATE_PANEL_W, fixed + rows * STATE_ROW_H)
}

/// Rows that fit a display `height` tall, at least one.
fn states_visible_rows_for(height: usize) -> usize {
    let fixed = TITLE_H + STATE_HEADER_H + STATE_FOOTER_H;
    (height.saturating_sub(fixed + 8) / STATE_ROW_H).clamp(1, STATE_ROWS_WANTED)
}

/// Rows the open panel shows at once.
pub(in crate::video) fn states_visible_rows() -> usize {
    states_visible_rows_for(present_height())
}

fn states_list_rect(rect: Rect) -> Rect {
    Rect {
        x: rect.x + STATE_INSET,
        y: rect.y + TITLE_H + STATE_HEADER_H,
        w: rect.w - 2 * STATE_INSET,
        h: states_visible_rows() * STATE_ROW_H,
    }
}

/// The visible rows' rects with their controls (absolute entry index).
pub(super) fn states_row_rects(rect: Rect, panel: &StatesPanel) -> Vec<(UiControl, Rect)> {
    let list = states_list_rect(rect);
    let rows = states_visible_rows();
    (panel.scroll..panel.entries.len().min(panel.scroll + rows))
        .enumerate()
        .map(|(row, index)| {
            (
                UiControl::StateRow(index),
                Rect {
                    x: list.x,
                    y: list.y + row * STATE_ROW_H,
                    w: list.w,
                    h: STATE_ROW_H,
                },
            )
        })
        .collect()
}

fn states_footer_top(rect: Rect) -> usize {
    rect.y + rect.h - STATE_FOOTER_H
}

/// The footer buttons, or the confirm strip's two while it is up.
pub(super) fn states_button_rects(rect: Rect, panel: &StatesPanel) -> Vec<(UiControl, Rect)> {
    let y = states_footer_top(rect) + 8;
    let labels: &[&str] = if panel.confirm_delete {
        &STATE_CONFIRM_BUTTONS
    } else {
        &STATE_BUTTONS
    };
    let controls: &[UiControl] = if panel.confirm_delete {
        &[UiControl::StateConfirmDelete, UiControl::StateCancelDelete]
    } else {
        &[
            UiControl::StateLoad,
            UiControl::StateDelete,
            UiControl::StateBrowse,
        ]
    };
    // The confirm buttons sit to the right of the question; the regular
    // row starts at the left inset.
    let x0 = if panel.confirm_delete {
        rect.x + rect.w - STATE_INSET - labels.len() * (STATE_BUTTON_W + STATE_BUTTON_GAP)
            + STATE_BUTTON_GAP
    } else {
        rect.x + STATE_INSET
    };
    controls
        .iter()
        .enumerate()
        .map(|(i, control)| {
            (
                *control,
                Rect {
                    x: x0 + i * (STATE_BUTTON_W + STATE_BUTTON_GAP),
                    y,
                    w: STATE_BUTTON_W,
                    h: STATE_BUTTON_H,
                },
            )
        })
        .collect()
}

/// Hit-test the panel's own controls.
pub(super) fn states_control_at(
    rect: Rect,
    panel: &StatesPanel,
    pos: (i32, i32),
) -> Option<UiControl> {
    for (control, button) in states_button_rects(rect, panel) {
        if button.contains(pos) {
            return Some(control);
        }
    }
    if panel.confirm_delete {
        // While the question is up the list is not for clicking on: a
        // click there withdraws the question (the window routes any row
        // to `activate`, which does exactly that).
    }
    for (control, row) in states_row_rects(rect, panel) {
        if row.contains(pos) {
            return Some(control);
        }
    }
    None
}

/// Whether a button is live, for greying: Load and Delete need a usable
/// selection.
fn states_button_enabled(panel: &StatesPanel, control: UiControl) -> bool {
    match control {
        UiControl::StateLoad => panel.selected_entry().is_some_and(StateEntry::loadable),
        UiControl::StateDelete => panel.selected_entry().is_some_and(StateEntry::deletable),
        _ => true,
    }
}

// ---------------------------------------------------------------------------
// Drawing
// ---------------------------------------------------------------------------

pub(super) fn draw_states_panel(
    frame: &mut [u8],
    rect: Rect,
    panel: &StatesPanel,
    hover: Option<UiControl>,
    scale: usize,
) {
    // Header: the folder, clipped from the left so the end that varies
    // (the folder's own name) stays visible.
    let max_chars = (rect.w - 2 * STATE_INSET) / font::GLYPH_W;
    let folder = clip_path_to_chars(
        &panel.dir.display().to_string(),
        max_chars.saturating_sub(8),
    );
    draw_panel_text(
        frame,
        rect.x + STATE_INSET,
        rect.y + TITLE_H + 8,
        &format!("Folder: {folder}"),
        PANEL_TEXT_DIM,
        1,
        scale,
    );
    let count = panel.entries.iter().filter(|e| !e.empty).count();
    let count_text = format!("{count} states");
    draw_panel_text(
        frame,
        rect.x + rect.w - STATE_INSET - count_text.chars().count() * font::GLYPH_W,
        rect.y + TITLE_H + 8,
        &count_text,
        PANEL_TEXT_DIM,
        1,
        scale,
    );

    let list = states_list_rect(rect);
    fill_rect(frame, scale_rect(list, scale), ENTRY_BG, scale);
    if panel.entries.is_empty() {
        draw_panel_text(
            frame,
            list.x + 8,
            list.y + 8,
            "No states in this folder",
            PANEL_TEXT_DIM,
            1,
            scale,
        );
    }
    for (control, row) in states_row_rects(rect, panel) {
        let UiControl::StateRow(index) = control else {
            continue;
        };
        let entry = &panel.entries[index];
        let selected = index == panel.selected;
        if selected {
            let strength = if panel.focus == StatesFocus::List && !panel.confirm_delete {
                0.55
            } else {
                0.3
            };
            fill_rect_blend(
                frame,
                scale_rect(row, scale),
                STATE_ROW_SELECTED,
                strength,
                scale,
            );
        }
        if hover == Some(control) && !panel.confirm_delete {
            draw_outline(frame, scale_rect(row, scale), STATE_ROW_HOVER, scale);
        }
        draw_state_row(frame, row, entry, scale);
    }
    // Scroll cues: which rows are above and below the window.
    let rows = states_visible_rows();
    if panel.scroll > 0 {
        draw_panel_text(
            frame,
            list.x + list.w - 3 * font::GLYPH_W,
            list.y + 2,
            "^",
            PANEL_TEXT_DIM,
            1,
            scale,
        );
    }
    if panel.scroll + rows < panel.entries.len() {
        draw_panel_text(
            frame,
            list.x + list.w - 3 * font::GLYPH_W,
            list.y + list.h - font::GLYPH_H - 2,
            "v",
            PANEL_TEXT_DIM,
            1,
            scale,
        );
    }

    // Footer: the buttons, or the question with its two answers.
    let footer_y = states_footer_top(rect);
    if panel.confirm_delete {
        let name = panel
            .selected_entry()
            .map(|e| e.label.clone())
            .unwrap_or_default();
        let question = format!("Delete {name}?");
        let room = (rect.w - 2 * STATE_INSET) / font::GLYPH_W;
        let question = clip_chars(&question, room.saturating_sub(28));
        draw_panel_text(
            frame,
            rect.x + STATE_INSET,
            footer_y + 8 + (STATE_BUTTON_H - font::GLYPH_H) / 2,
            &question,
            PANEL_TEXT_ACCENT,
            1,
            scale,
        );
    }
    for (i, (control, button)) in states_button_rects(rect, panel).into_iter().enumerate() {
        let label = if panel.confirm_delete {
            STATE_CONFIRM_BUTTONS[i]
        } else {
            STATE_BUTTONS[i]
        };
        let focused = panel.focus == StatesFocus::Button(i);
        let light = lit(hover, control).max(if focused { 1.0 } else { 0.0 });
        draw_text_button(
            frame,
            button,
            label,
            states_button_enabled(panel, control),
            light,
            scale,
        );
        if focused {
            draw_outline(frame, scale_rect(button, scale), PANEL_TEXT_HILIGHT, scale);
        }
    }
    let hint = match &panel.status {
        Some(status) => status.clone(),
        None if panel.confirm_delete => "Return answers - Esc keeps the file".to_string(),
        None => "Up/Down select - Return loads - Delete removes - Esc closes".to_string(),
    };
    draw_panel_text(
        frame,
        rect.x + STATE_INSET,
        footer_y + STATE_FOOTER_H - font::GLYPH_H - 8,
        &clip_chars(&hint, (rect.w - 2 * STATE_INSET) / font::GLYPH_W),
        if panel.status.is_some() {
            PANEL_TEXT
        } else {
            PANEL_TEXT_DIM
        },
        1,
        scale,
    );
}

/// One row: the thumbnail, then the label and date, the times and
/// machine, and the media -- or the reason the file is unreadable.
fn draw_state_row(frame: &mut [u8], row: Rect, entry: &StateEntry, scale: usize) {
    let thumb = Rect {
        x: row.x + 4,
        y: row.y + 4,
        w: STATE_THUMB_W,
        h: STATE_THUMB_H,
    };
    match &entry.thumbnail {
        Some(picture) => draw_thumbnail(frame, thumb, picture, scale),
        None => {
            fill_rect(frame, scale_rect(thumb, scale), STATE_THUMB_PLATE, scale);
            let text = if entry.empty { "empty" } else { "no image" };
            draw_panel_text(
                frame,
                thumb.x + (thumb.w - text.len() * font::GLYPH_W) / 2,
                thumb.y + (thumb.h - font::GLYPH_H) / 2,
                text,
                PANEL_TEXT_DIM,
                1,
                scale,
            );
        }
    }
    let text_x = thumb.x + thumb.w + 10;
    let room = (row.x + row.w).saturating_sub(text_x + 4) / font::GLYPH_W;
    let line = |frame: &mut [u8], n: usize, text: &str, color: u32| {
        draw_panel_text(
            frame,
            text_x,
            row.y + 6 + n * (font::GLYPH_H + 6),
            &clip_chars(text, room),
            color,
            1,
            scale,
        );
    };
    if entry.empty {
        line(frame, 0, &entry.label, PANEL_TEXT_DIM);
        line(frame, 1, "empty", PANEL_TEXT_DIM);
        return;
    }
    let date = entry
        .saved_at_unix
        .map(crate::timestamp::readable)
        .unwrap_or_default();
    let head = if date.is_empty() {
        entry.label.clone()
    } else {
        // The date right-aligned after the label, when both fit.
        let gap = room.saturating_sub(entry.label.chars().count() + date.chars().count());
        if gap >= 2 {
            format!("{}{}{date}", entry.label, " ".repeat(gap))
        } else {
            format!("{}  {date}", entry.label)
        }
    };
    line(frame, 0, &head, PANEL_TEXT);
    if let Some(error) = &entry.error {
        line(frame, 1, "cannot be read", PANEL_TEXT_ACCENT);
        line(frame, 2, error, PANEL_TEXT_DIM);
        return;
    }
    let mut second = String::new();
    if let Some(seconds) = entry.emulated_seconds {
        second.push_str(&format!("at {}  ", format_emulated(seconds)));
    }
    second.push_str(&entry.machine);
    if entry.mismatch.is_some() {
        line(frame, 1, &second, PANEL_TEXT_ACCENT);
        let flag = "different machine: ";
        line(
            frame,
            2,
            &format!("{flag}{}", entry.mismatch.as_deref().unwrap_or_default()),
            PANEL_TEXT_ACCENT,
        );
    } else {
        line(frame, 1, &second, PANEL_TEXT);
        let media = if entry.media.is_empty() {
            "no media".to_string()
        } else {
            entry.media.clone()
        };
        line(frame, 2, &media, PANEL_TEXT_DIM);
    }
}

/// Paint a thumbnail into `rect`, sampled nearest at texture resolution:
/// at 2x the 80x60 box shows 160x120 texels of the 240x180 picture, so a
/// sharper display gets a sharper picture rather than a scaled-up blur.
pub(in crate::video) fn draw_thumbnail(
    frame: &mut [u8],
    rect: Rect,
    picture: &StateThumbnail,
    scale: usize,
) {
    if picture.width == 0
        || picture.height == 0
        || picture.pixels.len() < picture.width * picture.height
    {
        return;
    }
    let dest = scale_rect(rect, scale);
    let (tw, th) = (texture_width(scale), texture_height(scale));
    for dy in 0..dest.h {
        let y = dest.y + dy;
        if y >= th {
            break;
        }
        let sy = (dy * picture.height / dest.h).min(picture.height - 1);
        for dx in 0..dest.w {
            let x = dest.x + dx;
            if x >= tw {
                break;
            }
            let sx = (dx * picture.width / dest.w).min(picture.width - 1);
            let px = picture.pixels[sy * picture.width + sx];
            let at = (y * tw + x) * 4;
            frame[at..at + 4].copy_from_slice(&px.to_le_bytes());
        }
    }
}

/// Cut `text` to `max` glyphs with a `..` tail.
fn clip_chars(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let mut cut: String = text.chars().take(max.saturating_sub(2)).collect();
    cut.push_str("..");
    cut
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(label: &str, empty: bool) -> StateEntry {
        StateEntry {
            path: PathBuf::from(format!("/states/{label}.clstate")),
            label: label.to_string(),
            slot: None,
            empty,
            saved_at_unix: Some(1_699_956_800),
            emulated_seconds: Some(83.4),
            machine: "A500 / M68000 / Ocs / Pal / chip 512K".into(),
            media: "DF0: game.adf".into(),
            thumbnail: None,
            mismatch: None,
            error: None,
        }
    }

    fn panel(n: usize) -> StatesPanel {
        StatesPanel {
            dir: PathBuf::from("/states"),
            entries: (0..n).map(|i| entry(&format!("s{i}"), false)).collect(),
            selected: 0,
            scroll: 0,
            focus: StatesFocus::List,
            confirm_delete: false,
            status: None,
        }
    }

    #[test]
    fn the_focus_walks_the_list_then_the_buttons() {
        let mut p = panel(3);
        p.step(Dir::Up, 5);
        assert_eq!((p.selected, p.focus), (0, StatesFocus::List));
        p.step(Dir::Down, 5);
        p.step(Dir::Down, 5);
        assert_eq!(p.selected, 2);
        // Off the foot of the list onto the first button, right along the
        // row and no further, up back to the list.
        p.step(Dir::Down, 5);
        assert_eq!(p.focus, StatesFocus::Button(0));
        p.step(Dir::Right, 5);
        p.step(Dir::Right, 5);
        p.step(Dir::Right, 5);
        assert_eq!(p.focus, StatesFocus::Button(2));
        p.step(Dir::Left, 5);
        assert_eq!(p.focus, StatesFocus::Button(1));
        p.step(Dir::Up, 5);
        assert_eq!((p.selected, p.focus), (2, StatesFocus::List));
    }

    #[test]
    fn the_list_scrolls_to_keep_the_selection_visible() {
        let mut p = panel(12);
        for _ in 0..7 {
            p.step(Dir::Down, 5);
        }
        assert_eq!(p.selected, 7);
        assert_eq!(p.scroll, 3, "row 7 is the last of rows 3..8");
        for _ in 0..7 {
            p.step(Dir::Up, 5);
        }
        assert_eq!((p.selected, p.scroll), (0, 0));
        p.page(true, 5);
        assert_eq!((p.selected, p.scroll), (5, 1));
        p.jump(true, 5);
        assert_eq!((p.selected, p.scroll), (11, 7));
        p.jump(false, 5);
        assert_eq!((p.selected, p.scroll), (0, 0));
    }

    #[test]
    fn return_loads_the_selection_and_delete_asks_first() {
        let mut p = panel(2);
        p.step(Dir::Down, 5);
        assert_eq!(p.press(), StatesAction::Load(1));
        // Delete: the question comes up with Cancel focused, so a reflex
        // Return keeps the file; Left then Return removes it.
        assert_eq!(p.ask_delete(), StatesAction::None);
        assert!(p.confirm_delete);
        assert_eq!(p.focus, StatesFocus::Button(1));
        assert_eq!(p.press(), StatesAction::None);
        assert!(!p.confirm_delete);
        p.ask_delete();
        p.step(Dir::Left, 5);
        assert_eq!(p.press(), StatesAction::Delete(1));
        assert!(!p.confirm_delete);
        // Escape withdraws the question; with none up it closes the panel.
        p.ask_delete();
        assert!(p.back());
        assert!(!p.confirm_delete);
        assert!(!p.back());
    }

    #[test]
    fn empty_slots_neither_load_nor_delete() {
        let mut p = panel(1);
        p.entries[0] = entry("Slot 4", true);
        assert_eq!(p.press(), StatesAction::None);
        assert_eq!(p.status.as_deref(), Some("Slot 4 is empty"));
        assert_eq!(p.ask_delete(), StatesAction::None);
        assert!(!p.confirm_delete);
        // Unreadable files load nothing but can still be deleted.
        p.entries[0] = entry("broken", false);
        p.entries[0].error = Some("not a Copperline save state".into());
        p.status = None;
        assert_eq!(p.press(), StatesAction::None);
        assert_eq!(p.status.as_deref(), Some("broken cannot be read"));
        p.ask_delete();
        assert!(p.confirm_delete);
    }

    #[test]
    fn a_click_on_a_row_selects_and_loads_it() {
        let mut p = panel(3);
        assert_eq!(p.activate(UiControl::StateRow(2), 5), StatesAction::Load(2));
        assert_eq!(p.selected, 2);
        assert_eq!(p.activate(UiControl::StateDelete, 5), StatesAction::None);
        assert!(p.confirm_delete);
        // A click on a row while the question is up withdraws it.
        assert_eq!(p.activate(UiControl::StateRow(0), 5), StatesAction::None);
        assert!(!p.confirm_delete);
        assert_eq!(p.selected, 2);
        p.activate(UiControl::StateDelete, 5);
        assert_eq!(
            p.activate(UiControl::StateConfirmDelete, 5),
            StatesAction::Delete(2)
        );
        assert_eq!(p.activate(UiControl::StateBrowse, 5), StatesAction::Browse);
    }

    #[test]
    fn emulated_time_reads_as_minutes_and_tenths() {
        assert_eq!(format_emulated(0.0), "0:00.0");
        assert_eq!(format_emulated(83.44), "1:23.4");
        assert_eq!(format_emulated(3725.9), "1:02:05");
        assert_eq!(format_emulated(-3.0), "0:00.0");
    }

    #[test]
    fn scan_lists_slots_first_then_named_states_newest_first() {
        let dir = std::env::temp_dir().join(format!(
            "copperline-states-scan-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        // Two named states, one plain file, and slot 2: the named ones are
        // not real states, so they show as unreadable rows -- which is the
        // point: a folder is listed as it is, not as it should be.
        std::fs::write(dir.join("older.clstate"), b"x").unwrap();
        std::fs::write(dir.join("notes.txt"), b"x").unwrap();
        std::fs::write(dir.join("newer.CLSTATE"), b"x").unwrap();
        std::fs::write(savestate::slot_path_in(&dir, 2).unwrap(), b"x").unwrap();
        let running = crate::config::MachineDescriptor::default();
        let panel = StatesPanel::scan(&dir, &running);
        assert_eq!(panel.entries.len(), savestate::SLOT_COUNT + 2);
        assert_eq!(panel.entries[0].label, "Slot 1");
        assert!(panel.entries[0].empty);
        assert_eq!(panel.entries[1].slot, Some(2));
        assert!(!panel.entries[1].empty);
        assert!(panel.entries[1].error.is_some());
        let named: Vec<&str> = panel.entries[savestate::SLOT_COUNT..]
            .iter()
            .map(|e| e.label.as_str())
            .collect();
        // Same second, so the tie falls to the name.
        assert_eq!(named, ["newer.CLSTATE", "older.clstate"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_unknown_save_time_falls_back_to_the_file() {
        let mut meta = StateMeta {
            thumbnail_png: Vec::new(),
            thumbnail_width: 0,
            thumbnail_height: 0,
            emulated_seconds: 0.0,
            emulated_frames: 0,
            saved_at_unix: 1_699_956_800,
            machine: String::new(),
            media: Default::default(),
        };
        assert_eq!(saved_at(Some(&meta), Some(42)), Some(1_699_956_800));
        // Zero means the host had no clock, not the epoch.
        meta.saved_at_unix = 0;
        assert_eq!(saved_at(Some(&meta), Some(42)), Some(42));
        assert_eq!(saved_at(Some(&meta), None), None);
        assert_eq!(saved_at(None, Some(42)), Some(42));
    }

    #[test]
    fn rows_and_buttons_lay_out_inside_the_panel() {
        let mut p = panel(12);
        p.scroll = 2;
        let rect = Rect {
            x: 20,
            y: 10,
            w: STATE_PANEL_W,
            h: states_panel_dims().1,
        };
        let rows = states_row_rects(rect, &p);
        assert_eq!(rows.len(), states_visible_rows());
        assert_eq!(rows[0].0, UiControl::StateRow(2));
        for (_, row) in &rows {
            assert!(row.x >= rect.x && row.x + row.w <= rect.x + rect.w);
            assert!(row.y >= rect.y && row.y + row.h <= rect.y + rect.h);
        }
        let buttons = states_button_rects(rect, &p);
        assert_eq!(buttons.len(), 3);
        assert_eq!(buttons[2].0, UiControl::StateBrowse);
        for (_, b) in &buttons {
            assert!(b.y + b.h <= rect.y + rect.h);
        }
        let last_row = rows.last().unwrap().1;
        assert!(buttons[0].1.y >= last_row.y + last_row.h);
        // Hit-testing finds a row and a button, and nothing in the gap.
        let (rx, ry) = (rows[1].1.x as i32 + 5, rows[1].1.y as i32 + 5);
        assert_eq!(
            states_control_at(rect, &p, (rx, ry)),
            Some(UiControl::StateRow(3))
        );
        let b = buttons[1].1;
        assert_eq!(
            states_control_at(rect, &p, (b.x as i32 + 1, b.y as i32 + 1)),
            Some(UiControl::StateDelete)
        );
        p.confirm_delete = true;
        let confirm = states_button_rects(rect, &p);
        assert_eq!(confirm.len(), 2);
        assert_eq!(confirm[1].0, UiControl::StateCancelDelete);
    }
}
