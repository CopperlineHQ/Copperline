//! The App side of keyboard and controller navigation: what the focus
//! stands on, what moves it, and what a press means once it is there.
//!
//! The model lives in [`crate::video::nav`], which knows nothing of the
//! window: it derives a map of places to stand by sampling the same
//! hit-test the pointer uses, and answers where a step goes. What is
//! here is everything that needs the running application -- reading the
//! surfaces for that map, spending a step on an open stepper or a list
//! that scrolls, naming the few steps geometry cannot answer, and
//! publishing where the marker is so the drawing can light it.

use super::*;

/// How long one breath of the focus takes, and how far it fades at the
/// bottom of it: the control lights the way the pointer lights it, and
/// breathes between that and its resting face.
const NAV_PULSE_MS: u64 = 1_700;

const NAV_PULSE_FLOOR: f32 = 0.45;

/// How long a pad direction is held before the focus starts walking on
/// its own, and how the gaps close up as it keeps being held.
const PAD_NAV_DELAY: std::time::Duration = std::time::Duration::from_millis(420);

/// The launcher's steppers repeat while they are held. Most settle at a
/// readable pace -- a value a second, so a held arrow walks the list
/// without ever running away with it -- but the few whose ranges are
/// counted in dozens ramp instead, the way the scroll arrows and the
/// synth's own do.
const CYCLE_HOLD_EVERY: std::time::Duration = std::time::Duration::from_millis(1000);

const CYCLE_RAMP_DELAY: std::time::Duration = std::time::Duration::from_millis(400);

/// The pad's own walk: which way it is being held, since when, when the
/// next step falls due, and whether its buttons were down last time --
/// a button fires on its press, not for as long as it is held.
#[derive(Debug, Clone, Copy)]
pub(super) struct PadNav {
    held: Option<crate::video::nav::Dir>,
    since: Instant,
    next: Instant,
    fire: bool,
    back: bool,
}

impl Default for PadNav {
    fn default() -> Self {
        let now = Instant::now();
        Self {
            held: None,
            since: now,
            next: now,
            fire: false,
            back: false,
        }
    }
}

fn pad_nav_every(held: std::time::Duration) -> std::time::Duration {
    let ms = match held.as_millis() {
        0..=1_200 => 180,
        1_201..=2_600 => 110,
        _ => 70,
    };
    std::time::Duration::from_millis(ms)
}

/// Whether a stepper's range is long enough to want the ramp.
pub(super) fn cycle_ramps(field: crate::video::launcher::LauncherField) -> bool {
    use crate::video::launcher::LauncherField as F;
    matches!(
        field,
        F::MouseSensitivity | F::AudioStereoSeparation | F::FloppyVolume
    ) || crate::video::launcher::field_is_bootpri(field)
}

/// How long a held stepper waits before its first repeat.
pub(super) fn cycle_hold_delay(
    field: crate::video::launcher::LauncherField,
) -> std::time::Duration {
    if cycle_ramps(field) {
        CYCLE_RAMP_DELAY
    } else {
        CYCLE_HOLD_EVERY
    }
}

/// The gap to the next repeat, given how long the arrow has been held.
pub(super) fn cycle_hold_every(
    field: crate::video::launcher::LauncherField,
    held_for: std::time::Duration,
) -> std::time::Duration {
    if !cycle_ramps(field) {
        return CYCLE_HOLD_EVERY;
    }
    let ms = match held_for.as_millis() {
        0..=1_800 => 140,
        1_801..=4_000 => 55,
        _ => 25,
    };
    std::time::Duration::from_millis(ms)
}

/// Whether a status-bar control would do anything if it were pressed.
/// A greyed button is not somewhere to stand: the focus walks past a
/// swap with nothing to swap to, and an eject with nothing in the
/// drive, the way the eye does.
pub(super) fn bar_control_live(media: &MediaBar, control: BarControl) -> bool {
    let drive = |idx: usize| media.drives.get(idx).copied().unwrap_or_default();
    match control {
        BarControl::DriveLoad(idx) => drive(idx).connected && !drive(idx).bridged,
        BarControl::DriveSwap(idx) => {
            let d = drive(idx);
            d.connected && d.multi && !d.bridged
        }
        BarControl::DriveEject(idx) => {
            let d = drive(idx);
            d.connected && d.inserted && !d.bridged
        }
        BarControl::CdEject => media.cd == Some(true),
        _ => true,
    }
}

/// Whether a control is one of the buttons along the foot of the
/// configuration screen. They are not settings: walking left along them
/// means walking along them.
pub(super) fn launcher_action_control(control: UiControl) -> bool {
    matches!(
        control,
        UiControl::LauncherLoad
            | UiControl::LauncherSave
            | UiControl::LauncherSaveAs
            | UiControl::LauncherDefaults
            | UiControl::LauncherRun
    )
}

impl App {
    /// Everywhere the focus can stand on the interface as it is now,
    /// read off the pointer's own hit-testing.
    pub(super) fn nav_items(&self) -> Vec<crate::video::nav::NavItem> {
        let mut items = crate::video::nav::map(&self.ui, texture_width(1), texture_height(1));
        // The status bar is part of the same space: it sits under the
        // panels on the screen, so stepping down off the bottom of one
        // reaches it and stepping back up returns. Hidden, it is not
        // there to be walked -- a player build never shows it at all.
        if crate::video::status_bar_hidden() {
            return items;
        }
        let media = self.media_bar();
        let layout = bar_layout(&media);
        items.extend(crate::video::nav::bar_map(
            status_bar_rect(),
            |pos| control_at(pos, &layout).filter(|control| bar_control_live(&media, *control)),
            volume_control_hit_rect(),
        ));
        items
    }

    /// Move the focus, or spend the direction on an open stepper.
    pub(super) fn nav_move(
        &mut self,
        dir: crate::video::nav::Dir,
        event_loop: Option<&ActiveEventLoop>,
    ) -> bool {
        use crate::video::nav::{Dir, NavTarget};
        // The menu keeps its own cursor -- it is a tree, not a page --
        // so while it is open the focus is that cursor, whichever hand
        // is moving it.
        if self.ui.menu_open {
            let ui = &mut self.ui;
            match dir {
                // The menu does not cycle: the top of a level is the
                // top. Walking off the foot of it leaves the menu
                // altogether, landing on the button it hangs from, so
                // the bar can be walked from there.
                Dir::Up => {
                    ui.menu_nav.step_within(&ui.menu_rows, false);
                }
                Dir::Down if ui.menu_nav.cursor().is_none() => {
                    ui.menu_nav.step(&ui.menu_rows, true);
                }
                Dir::Down => {
                    if !ui.menu_nav.step_within(&ui.menu_rows, true) && ui.menu_nav.depth() == 0 {
                        self.close_menu();
                        self.nav_show(Some(crate::video::nav::NavTarget::Bar(BarControl::Menu)));
                    }
                }
                Dir::Right => {
                    ui.menu_nav.descend(&ui.menu_rows);
                }
                Dir::Left => {
                    ui.menu_nav.ascend();
                }
            }
            self.request_redraw();
            return true;
        }
        let items = self.nav_items();
        if items.is_empty() {
            return false;
        }
        self.nav.settle(&items, self.nav_home());
        // A list longer than its box scrolls under the focus: stepping
        // off the last row it can show brings the next one into view
        // rather than leaving the list for the buttons under it.
        if !dir.horizontal() && self.nav_library_step(dir) {
            return true;
        }
        if !dir.horizontal() {
            if let Some(next) = self.nav_host_disk_scroll(dir) {
                self.nav_show(Some(next));
                self.request_redraw();
                return true;
            }
        }
        // A dialog is walked in the order it reads, not in the order it
        // is laid out: its picture is a tall block beside the boxes
        // rather than above the first of them, so what the eye calls
        // the next thing down is not what is under it.
        if !dir.horizontal() {
            if let Some(next) = self.nav_dialog_step(&items, dir) {
                self.nav_show(Some(next));
                self.nav_sync_dialog();
                self.request_redraw();
                return true;
            }
        }
        // An open stepper spends left and right on its value; up and
        // down close it and move on, so the focus is never trapped.
        if self.nav.open() {
            if dir.horizontal() {
                if let Some(arrow) = self
                    .nav
                    .focus()
                    .and_then(|t| crate::video::nav::stepper_arrow(t, dir))
                {
                    self.activate_nav_target(arrow, event_loop);
                    self.request_redraw();
                    return true;
                }
                // The volume has no arrows of its own: the direction is
                // the step, and holding it gathers speed like every
                // other held control here.
                if let Some(NavTarget::Bar(BarControl::Volume)) = self.nav.focus() {
                    let steps = if dir == Dir::Right { 1 } else { -1 };
                    self.adjust_output_volume(steps * VOLUME_STEP_PERCENT);
                    self.request_redraw();
                    return true;
                }
            }
            self.nav.close();
        }
        let mut next = crate::video::nav::step(&items, self.nav.focus(), dir);
        // The two rules the column follows, which geometry alone gets
        // wrong on pages whose settings do not begin beside it: right
        // from a category opens its page at the first setting, and left
        // from anywhere in that page comes back to the category. The
        // page you are on is the button you came in by.
        match dir {
            Dir::Right => {
                if let Some(first) = self.nav_first_setting(&items) {
                    next = Some(first);
                } else if next.is_none() {
                    next = self.nav_row_across(&items, dir);
                }
            }
            Dir::Left => {
                // Left walks the row it is on first -- from Clear back
                // to the Browse beside it, from one image format to the
                // one before it -- and only leaves the page when that
                // row has run out.
                if let Some(row) = self.nav_row_across(&items, dir) {
                    next = Some(row);
                } else if let Some(letters) = self.nav_library_letters(&items) {
                    next = Some(letters);
                } else if !next.is_some_and(crate::video::nav::in_page) {
                    if let Some(home) = self.nav_column_home(&items) {
                        next = Some(home);
                    }
                }
            }
            _ => {}
        }
        if next.is_some() || !self.nav.showing() {
            self.nav_show(next.or_else(|| self.nav.focus()));
        }
        self.nav_sync_library();
        self.nav_sync_dialog();
        self.request_redraw();
        true
    }

    /// Work whatever the focus stands on: a stepper opens, everything
    /// else is pressed. The first press only shows the focus, so the
    /// interface never acts on a control nobody could see was chosen.
    pub(super) fn nav_press(&mut self, event_loop: Option<&ActiveEventLoop>) -> bool {
        if self.ui.menu_open {
            self.activate_menu_row(event_loop);
            if let Some(event_loop) = event_loop {
                self.ensure_tool_windows_for_open_panels(event_loop);
            }
            self.request_redraw();
            return true;
        }
        let items = self.nav_items();
        if items.is_empty() {
            return false;
        }
        self.nav.settle(&items, self.nav_home());
        let Some(focus) = self.nav.focus() else {
            self.nav_show(items.first().map(|i| i.target));
            self.request_redraw();
            return true;
        };
        if !self.nav.showing() {
            self.nav_show(Some(focus));
            self.request_redraw();
            return true;
        }
        if crate::video::nav::is_stepper(focus) {
            self.nav.toggle_open();
        } else {
            self.activate_nav_target(focus, event_loop);
            if let Some(event_loop) = event_loop {
                self.ensure_tool_windows_for_open_panels(event_loop);
            }
            // A button that changes the page is the way into it, and so
            // is the way back out: going back from anywhere in the page
            // returns to it rather than closing the launcher.
            if matches!(
                focus,
                crate::video::nav::NavTarget::Ui(
                    UiControl::LauncherTab(_) | UiControl::LauncherNavTab(_)
                )
            ) {
                self.nav_entered_from = Some(focus);
            }
            // Pressing a category button changes the page under the
            // focus; what it paints has to follow.
            let items = self.nav_items();
            // A tick is marked and left: what was being looked at is the
            // game, not its tick, so the focus goes back to the row it
            // came across from. Right again returns to the tick.
            if let Some(row) = Self::nav_row_beside(focus) {
                if crate::video::nav::find(&items, row).is_some() {
                    self.nav.show(Some(row));
                }
            }
            // A button that opened a page has gone with the page it
            // opened. The focus belongs on the new page, not back at the
            // top of the column.
            if crate::video::nav::find(&items, focus).is_none() {
                if let Some(entry) = self.nav_page_entry(&items) {
                    self.nav.show(Some(entry));
                }
            }
            self.nav.settle(&items, self.nav_home());
            self.nav_sync_library();
            self.nav_sync_dialog();
        }
        self.request_redraw();
        true
    }

    /// Step back out: an open stepper closes, and otherwise the surface
    /// itself goes -- which is what the second button means everywhere
    /// else it is pressed.
    pub(super) fn nav_back(&mut self) {
        if self.nav.open() {
            self.nav.close();
            self.request_redraw();
            return;
        }
        if self.ui.menu_open {
            if !self.ui.menu_nav.ascend() {
                self.close_menu();
            }
            self.request_redraw();
            return;
        }
        // Back out of the page before back out of the launcher: the
        // focus returns to the button it came in by, and only a second
        // press closes the panel.
        if let Some(entered) = self.nav_entered_from {
            let items = self.nav_items();
            let inside = self.nav.focus().is_some_and(crate::video::nav::in_page)
                && self.nav.focus() != Some(entered);
            if inside && crate::video::nav::find(&items, entered).is_some() {
                self.nav_entered_from = None;
                self.nav_show(Some(entered));
                self.request_redraw();
                return;
            }
        }
        if self.ui.panel.is_some() {
            self.close_panel();
        }
    }

    /// Put the focus somewhere and start its breath afresh, so a step
    /// always begins bright rather than wherever the last one left the
    /// pulse.
    fn nav_show(&mut self, target: Option<crate::video::nav::NavTarget>) {
        self.nav_shown_at = Instant::now();
        self.nav.show(target);
    }

    /// Work whatever the focus is standing on, whichever surface it
    /// belongs to.
    fn activate_nav_target(
        &mut self,
        target: crate::video::nav::NavTarget,
        event_loop: Option<&ActiveEventLoop>,
    ) {
        match target {
            crate::video::nav::NavTarget::Ui(control) => {
                self.activate_ui_control_with_event_loop(control, event_loop);
            }
            crate::video::nav::NavTarget::Bar(control) => self.activate_bar_control(control),
        }
    }

    /// Walk the interface with the arrows, and work it with Return.
    ///
    /// A stepper opens rather than firing: its arrows light, left and
    /// right change the value, and Return closes it again. Everything
    /// else is simply pressed, which is what a single-choice control
    /// means.
    pub(super) fn handle_nav_key(
        &mut self,
        code: KeyCode,
        event_loop: Option<&ActiveEventLoop>,
    ) -> bool {
        use crate::video::nav::Dir;
        let dir = match code {
            KeyCode::ArrowUp => Some(Dir::Up),
            KeyCode::ArrowDown => Some(Dir::Down),
            KeyCode::ArrowLeft => Some(Dir::Left),
            KeyCode::ArrowRight => Some(Dir::Right),
            _ => None,
        };
        if let Some(dir) = dir {
            return self.nav_move(dir, event_loop);
        }
        if matches!(code, KeyCode::Enter | KeyCode::NumpadEnter | KeyCode::Space) {
            return self.nav_press(event_loop);
        }
        // With a surface open Escape has already been spent closing it
        // by the time this is reached, so what is left is a marker
        // standing on the bar over a running machine: put it away and
        // the keyboard is the guest's again.
        if code == KeyCode::Escape && self.nav.showing() {
            self.nav.clear();
            self.request_redraw();
            return true;
        }
        false
    }

    /// Where the focus starts when whatever it was standing on has
    /// gone -- a page change, most often. The configuration screen's
    /// own answer is the first of its category buttons, which is where
    /// the eye starts on it too.
    pub(super) fn nav_home(&self) -> Option<crate::video::nav::NavTarget> {
        use crate::video::launcher::LauncherTab;
        let Some(Panel::Launcher(state)) = self.ui.panel.as_ref() else {
            return None;
        };
        // A dialog over the page is the only thing being answered while
        // it is up, so it is where the focus starts: on the first of the
        // three things the Save dialog offers, and on the one thing the
        // confirm asks.
        if state.save_dialog {
            return Some(crate::video::nav::NavTarget::Ui(
                crate::video::ui::SAVE_ACTIONS[0],
            ));
        }
        if state.confirm_reset {
            return Some(crate::video::nav::NavTarget::Ui(
                UiControl::LauncherConfirmReset,
            ));
        }
        if let Some(first) = self.nav_dialog_order().first() {
            return Some(*first);
        }
        Some(crate::video::nav::NavTarget::Ui(UiControl::LauncherTab(
            LauncherTab::System,
        )))
    }

    /// Where the focus stands when a page opens.
    ///
    /// Right off a category button, and again when a button that
    /// changed the page has gone with it: pressing one of the rows
    /// across the top of a page opens a page whose own row is a Back
    /// button, so the thing that was being stood on no longer exists.
    /// Without this the focus fell back to the first category and the
    /// page that had just been opened was left behind.
    fn nav_page_entry(
        &self,
        items: &[crate::video::nav::NavItem],
    ) -> Option<crate::video::nav::NavTarget> {
        use crate::video::nav::NavTarget;
        let column = self
            .launcher_state()
            .map(|state| state.tab.parent_tab().unwrap_or(state.tab))
            .and_then(|tab| {
                crate::video::nav::find(items, NavTarget::Ui(UiControl::LauncherTab(tab)))
            });
        let right_of_column = column.map_or(0, |item| item.rect.x + item.rect.w);
        // The game page is a list with a strip of letters over it, and
        // that is what it is opened for: right off its button goes to
        // the letters, or straight into the list when there are too few
        // games for the strip to be drawn.
        #[cfg(feature = "game-library")]
        {
            let library = items
                .iter()
                .filter(|item| {
                    matches!(
                        item.target,
                        NavTarget::Ui(UiControl::LauncherLibraryJump(_))
                    )
                })
                .min_by_key(|item| item.rect.x)
                .or_else(|| {
                    items
                        .iter()
                        .filter(|item| {
                            matches!(
                                item.target,
                                NavTarget::Ui(UiControl::LauncherLibraryPick(_))
                            )
                        })
                        .min_by_key(|item| item.rect.y)
                });
            if let Some(item) = library {
                return Some(item.target);
            }
        }
        // A page with sibling pages opens on that row: it is the first
        // thing across the top of the page, and the settings under it
        // belong to whichever of them is chosen.
        let pages = items
            .iter()
            .filter(|item| {
                matches!(
                    item.target,
                    crate::video::nav::NavTarget::Ui(UiControl::LauncherNavTab(_))
                )
            })
            .min_by_key(|item| (item.rect.y, item.rect.x));
        if let Some(item) = pages {
            return Some(item.target);
        }
        items
            .iter()
            .filter(|item| {
                let NavTarget::Ui(control) = item.target else {
                    return false;
                };
                // The settings themselves: not the rows the page hangs
                // under, nor the buttons along its foot.
                !matches!(
                    control,
                    UiControl::LauncherTab(_)
                        | UiControl::LauncherNavTab(_)
                        | UiControl::LauncherModel(_)
                        | UiControl::PanelClose
                ) && !launcher_action_control(control)
                    && item.rect.x >= right_of_column
            })
            .min_by_key(|item| (item.rect.y, item.rect.x))
            .map(|item| item.target)
    }

    /// The first setting of the page, for stepping right out of the
    /// category column: the top-left of the settings, wherever that
    /// happens to sit. Pages whose settings start well below the column
    /// button, or well to the right of it, have nothing beside the
    /// button at all, and geometry alone leaves the focus where it was.
    fn nav_first_setting(
        &self,
        items: &[crate::video::nav::NavItem],
    ) -> Option<crate::video::nav::NavTarget> {
        use crate::video::nav::NavTarget;
        let NavTarget::Ui(UiControl::LauncherTab(_)) = self.nav.focus()? else {
            return None;
        };
        self.nav_page_entry(items)
    }

    fn nav_column_home(
        &self,
        items: &[crate::video::nav::NavItem],
    ) -> Option<crate::video::nav::NavTarget> {
        use crate::video::nav::NavTarget;
        let NavTarget::Ui(control) = self.nav.focus()? else {
            return None;
        };
        if matches!(
            control,
            UiControl::LauncherTab(_) | UiControl::LauncherNavTab(_) | UiControl::LauncherModel(_)
        ) || launcher_action_control(control)
        {
            return None;
        }
        let state = self.launcher_state()?;
        let column = state.tab.parent_tab().unwrap_or(state.tab);
        let home = NavTarget::Ui(UiControl::LauncherTab(column));
        let seat = crate::video::nav::find(items, home)?;
        let here = crate::video::nav::find(items, NavTarget::Ui(control))?;
        // Only from the right of the column: a control already in it,
        // or left of it, has somewhere else to be.
        (here.rect.x >= seat.rect.x + seat.rect.w).then_some(home)
    }

    /// The first of the letters over the game list.
    ///
    /// Left off a game goes back up to the strip it was jumped to by,
    /// which is the way back out of a list that fills its box: there is
    /// nothing beside a row of it to step onto.
    #[cfg(feature = "game-library")]
    fn nav_library_letters(
        &self,
        items: &[crate::video::nav::NavItem],
    ) -> Option<crate::video::nav::NavTarget> {
        use crate::video::nav::NavTarget;
        if !matches!(
            self.nav.focus(),
            Some(NavTarget::Ui(UiControl::LauncherLibraryPick(_)))
        ) {
            return None;
        }
        items
            .iter()
            .filter(|item| {
                matches!(
                    item.target,
                    NavTarget::Ui(UiControl::LauncherLibraryJump(_))
                )
            })
            .min_by_key(|item| item.rect.x)
            .map(|item| item.target)
    }

    #[cfg(not(feature = "game-library"))]
    fn nav_library_letters(
        &self,
        _items: &[crate::video::nav::NavItem],
    ) -> Option<crate::video::nav::NavTarget> {
        None
    }

    /// Where a step across a row of one of the lists goes.
    ///
    /// A row is as wide as its box and its cells are drawn inside it,
    /// so neither is beyond the other and the geometry has nothing to
    /// find either side: these steps have to be named. Right walks the
    /// cells of the row and then leaves the list -- to the buttons under
    /// the games, which up off them comes back from; to Run off a
    /// favourite, which is what a favourite is chosen for; and to Mount
    /// off a host disk. Left walks back through them to the row.
    fn nav_row_across(
        &self,
        items: &[crate::video::nav::NavItem],
        dir: crate::video::nav::Dir,
    ) -> Option<crate::video::nav::NavTarget> {
        use crate::video::nav::{Dir, NavTarget};
        let focus = self.nav.focus()?;
        if dir == Dir::Left {
            let row = Self::nav_row_beside(focus)?;
            return crate::video::nav::find(items, row).map(|item| item.target);
        }
        if dir != Dir::Right {
            return None;
        }
        let NavTarget::Ui(control) = focus else {
            return None;
        };
        let beside = |tick: UiControl| {
            crate::video::nav::find(items, NavTarget::Ui(tick)).map(|item| item.target)
        };
        let mount = || {
            crate::video::nav::find(items, NavTarget::Ui(UiControl::LauncherHostDiskMount))
                .map(|item| item.target)
        };
        #[cfg(feature = "game-library")]
        {
            let buttons = || {
                items
                    .iter()
                    .filter(|item| {
                        matches!(
                            item.target,
                            NavTarget::Ui(
                                UiControl::LauncherLibraryRefresh
                                    | UiControl::LauncherLibraryUpdate
                                    | UiControl::LauncherLibraryEdit
                            )
                        )
                    })
                    .min_by_key(|item| item.rect.x)
                    .map(|item| item.target)
            };
            let run = || {
                crate::video::nav::find(items, NavTarget::Ui(UiControl::LauncherRun))
                    .map(|item| item.target)
            };
            match control {
                UiControl::LauncherLibraryPick(at) => {
                    return beside(UiControl::LauncherLibraryFavourite(at)).or_else(buttons)
                }
                UiControl::LauncherLibraryFavourite(_) => return buttons(),
                UiControl::LauncherLibraryFavouritePick(at) => {
                    return beside(UiControl::LauncherLibraryFavouriteRemove(at)).or_else(run)
                }
                UiControl::LauncherLibraryFavouriteRemove(_) => return run(),
                _ => {}
            }
        }
        match control {
            UiControl::LauncherHostDiskSelect(at) => beside(UiControl::LauncherHostDiskAttach(at))
                .or_else(|| beside(UiControl::LauncherHostDiskWritable(at)))
                .or_else(mount),
            UiControl::LauncherHostDiskAttach(at) => {
                beside(UiControl::LauncherHostDiskWritable(at)).or_else(mount)
            }
            UiControl::LauncherHostDiskWritable(at) => {
                beside(UiControl::LauncherHostDiskEnable(at)).or_else(mount)
            }
            UiControl::LauncherHostDiskEnable(_) => mount(),
            _ => None,
        }
    }

    /// What sits left of a cell drawn inside a row, if this is one.
    fn nav_row_beside(
        target: crate::video::nav::NavTarget,
    ) -> Option<crate::video::nav::NavTarget> {
        use crate::video::nav::NavTarget;
        let NavTarget::Ui(control) = target else {
            return None;
        };
        Some(NavTarget::Ui(match control {
            #[cfg(feature = "game-library")]
            UiControl::LauncherLibraryFavourite(at) => UiControl::LauncherLibraryPick(at),
            #[cfg(feature = "game-library")]
            UiControl::LauncherLibraryFavouriteRemove(at) => {
                UiControl::LauncherLibraryFavouritePick(at)
            }
            UiControl::LauncherHostDiskAttach(at) => UiControl::LauncherHostDiskSelect(at),
            UiControl::LauncherHostDiskWritable(at) => UiControl::LauncherHostDiskAttach(at),
            UiControl::LauncherHostDiskEnable(at) => UiControl::LauncherHostDiskWritable(at),
            _ => return None,
        }))
    }

    /// Spend a step on the game list the focus is standing in.
    ///
    /// Up and down in a list are what scrolling is, and they belong to
    /// the focus rather than to the keyboard: a pad walking the same
    /// list means the same thing by them. At an end of the list the
    /// step is not the list's -- it walks off instead, up to the letters
    /// over the games and down to the buttons under them, rather than
    /// pressing against a row that cannot move.
    #[cfg(feature = "game-library")]
    fn nav_library_step(&mut self, dir: crate::video::nav::Dir) -> bool {
        use crate::video::nav::Dir;
        if !self.nav_focus_in_library() {
            return false;
        }
        let step = match dir {
            Dir::Up => -1,
            Dir::Down => 1,
            _ => return false,
        };
        if self.library_at_end(step) {
            return false;
        }
        self.step_library_list(step);
        true
    }

    #[cfg(not(feature = "game-library"))]
    fn nav_library_step(&mut self, _dir: crate::video::nav::Dir) -> bool {
        false
    }

    /// Move the game list's own selection, scrolling it into view, and
    /// bring the marker with it. A held step gathers speed the way the
    /// list's scroll arrows do; the jumps to either end are absolute.
    #[cfg(feature = "game-library")]
    pub(super) fn step_library_list(&mut self, step: isize) {
        let whdload_entry = self
            .launcher_state()
            .is_some_and(|state| state.setup.whdload_enabled());
        let visible = crate::video::ui::launcher_panel_rect(&self.ui)
            .map(|rect| crate::video::ui::library_visible_rows(rect, whdload_entry))
            .unwrap_or(1);
        if let Some(state) = self.launcher_state_mut() {
            let rows = match step {
                isize::MIN | isize::MAX => step,
                _ => {
                    let rate = state.library.scroll_rate.rows_for_step(Instant::now());
                    step * rate.max(1) as isize
                }
            };
            state.step_library_focus(rows, visible);
        }
        // The row the list chose is the row the focus stands on.
        self.nav_sync_library();
        self.request_redraw();
    }

    /// Scroll the host-disk list under the focus, where the step would
    /// otherwise leave a list that has more to show.
    ///
    /// Only the rows in view are places to stand -- they are what the
    /// hit-test answers for -- so without this a list of twenty disks
    /// could only ever be walked as far as the eighth.
    fn nav_host_disk_scroll(
        &mut self,
        dir: crate::video::nav::Dir,
    ) -> Option<crate::video::nav::NavTarget> {
        use crate::video::nav::{Dir, NavTarget};
        use crate::video::ui::HOST_DISK_VISIBLE_ROWS;
        let NavTarget::Ui(control) = self.nav.focus()? else {
            return None;
        };
        let at = Self::host_disk_row_of(control)?;
        let state = self.launcher_state()?;
        let disks = state.setup.host_disks().len();
        let scroll = state.setup.host_disk_scroll();
        let (next, step) = match dir {
            Dir::Down if at + 1 < disks && at + 1 >= scroll + HOST_DISK_VISIBLE_ROWS => (at + 1, 1),
            Dir::Up if at > 0 && at - 1 < scroll => (at - 1, -1),
            _ => return None,
        };
        self.launcher_state_mut()?
            .setup
            .scroll_host_disks(step, HOST_DISK_VISIBLE_ROWS);
        Some(NavTarget::Ui(Self::host_disk_row_at(control, next)))
    }

    /// Which row of the host-disk list a control belongs to.
    fn host_disk_row_of(control: UiControl) -> Option<usize> {
        match control {
            UiControl::LauncherHostDiskSelect(at)
            | UiControl::LauncherHostDiskAttach(at)
            | UiControl::LauncherHostDiskWritable(at)
            | UiControl::LauncherHostDiskEnable(at) => Some(at),
            _ => None,
        }
    }

    /// The same control on another row, so a scroll keeps the focus in
    /// the column it was walking down.
    fn host_disk_row_at(control: UiControl, at: usize) -> UiControl {
        match control {
            UiControl::LauncherHostDiskAttach(_) => UiControl::LauncherHostDiskAttach(at),
            UiControl::LauncherHostDiskWritable(_) => UiControl::LauncherHostDiskWritable(at),
            UiControl::LauncherHostDiskEnable(_) => UiControl::LauncherHostDiskEnable(at),
            _ => UiControl::LauncherHostDiskSelect(at),
        }
    }

    /// The order a dialog is walked in, top to bottom.
    ///
    /// Empty unless one is up, which is what makes this the walk: a
    /// dialog is the only thing being answered while it is there, so
    /// its own order is the only one that matters.
    #[cfg(feature = "game-library")]
    fn nav_dialog_order(&self) -> Vec<crate::video::nav::NavTarget> {
        use crate::video::launcher::{LoginField, MetaField};
        use crate::video::nav::NavTarget::Ui;
        let Some(state) = self.launcher_state() else {
            return Vec::new();
        };
        if state.meta.is_some() {
            let mut order = vec![Ui(UiControl::MetaArt)];
            order.extend(MetaField::ALL.iter().map(|&f| Ui(UiControl::MetaField(f))));
            order.extend([
                Ui(UiControl::MetaSave),
                Ui(UiControl::MetaClear),
                Ui(UiControl::MetaCancel),
            ]);
            return order;
        }
        if state.login.is_some() {
            return vec![
                Ui(UiControl::LoginField(LoginField::User)),
                Ui(UiControl::LoginField(LoginField::Pass)),
                Ui(UiControl::LoginOk),
                Ui(UiControl::LoginCancel),
            ];
        }
        Vec::new()
    }

    #[cfg(not(feature = "game-library"))]
    fn nav_dialog_order(&self) -> Vec<crate::video::nav::NavTarget> {
        Vec::new()
    }

    /// The next place along that order, or `None` where there is no
    /// dialog up or no further to go.
    fn nav_dialog_step(
        &self,
        items: &[crate::video::nav::NavItem],
        dir: crate::video::nav::Dir,
    ) -> Option<crate::video::nav::NavTarget> {
        use crate::video::nav::Dir;
        let order: Vec<_> = self
            .nav_dialog_order()
            .into_iter()
            .filter(|target| crate::video::nav::find(items, *target).is_some())
            .collect();
        if order.is_empty() {
            return None;
        }
        let Some(focus) = self.nav.focus() else {
            return order.first().copied();
        };
        let at = order.iter().position(|target| *target == focus)?;
        match dir {
            Dir::Down => order.get(at + 1).copied(),
            Dir::Up => at.checked_sub(1).and_then(|at| order.get(at).copied()),
            _ => None,
        }
    }

    /// Whether a key pressed in a dialog belongs to the focus rather
    /// than to the box being typed into.
    #[cfg(feature = "game-library")]
    pub(super) fn nav_dialog_key_is_focus(&self, code: KeyCode, buttons: &[UiControl]) -> bool {
        use crate::video::nav::NavTarget;
        if matches!(code, KeyCode::ArrowUp | KeyCode::ArrowDown) {
            return true;
        }
        let on_button =
            matches!(self.nav.focus(), Some(NavTarget::Ui(control)) if buttons.contains(&control));
        on_button
            && matches!(
                code,
                KeyCode::ArrowLeft
                    | KeyCode::ArrowRight
                    | KeyCode::Enter
                    | KeyCode::NumpadEnter
                    | KeyCode::Space
            )
    }

    /// Put the dialog's own caret in the box the focus is standing on.
    ///
    /// A dialog keeps its own focus -- it is what the caret blinks in
    /// and what typing goes into -- and the marker walking it is a
    /// second one. On the same dialog they must be the same box.
    #[cfg(feature = "game-library")]
    pub(super) fn nav_sync_dialog(&mut self) {
        use crate::video::nav::NavTarget;
        let Some(NavTarget::Ui(control)) = self.nav.focus() else {
            return;
        };
        match control {
            UiControl::LoginField(field) => {
                if let Some(login) = self.launcher_login_mut() {
                    login.focus_on(field);
                }
            }
            UiControl::MetaField(field) => {
                if let Some(meta) = self.launcher_meta_mut() {
                    meta.focus_on(field);
                }
            }
            _ => {}
        }
    }

    #[cfg(not(feature = "game-library"))]
    pub(super) fn nav_sync_dialog(&mut self) {}

    /// Put the focus on the row the list has chosen.
    ///
    /// A list keeps its own selection -- it is what scrolls, and what
    /// Return launches -- and the marker walking about the page is a
    /// second one. On the same list they must be the same row, or the
    /// box shows two chosen games and stepping across to a tick takes
    /// the tick of the wrong one.
    #[cfg(feature = "game-library")]
    pub(super) fn nav_sync_library(&mut self) {
        use crate::video::launcher::LibraryFocus;
        use crate::video::nav::NavTarget;
        let Some(NavTarget::Ui(control)) = self.nav.focus() else {
            return;
        };
        let games = matches!(
            control,
            UiControl::LauncherLibraryPick(_) | UiControl::LauncherLibraryFavourite(_)
        );
        let starred = matches!(
            control,
            UiControl::LauncherLibraryFavouritePick(_)
                | UiControl::LauncherLibraryFavouriteRemove(_)
        );
        if !games && !starred {
            return;
        }
        let Some(state) = self.launcher_state_mut() else {
            return;
        };
        // Standing in a list is what gives that list the focus, which is
        // what draws its chosen row: the other list shows none.
        state.library.focus = if games {
            LibraryFocus::Games
        } else {
            LibraryFocus::Favourites
        };
        let drawn = if games {
            state.library.selected.saturating_sub(state.library.scroll)
        } else {
            state
                .library
                .favourite_selected
                .saturating_sub(state.library.favourite_scroll)
        };
        let at = match control {
            UiControl::LauncherLibraryPick(_) => UiControl::LauncherLibraryPick(drawn),
            UiControl::LauncherLibraryFavourite(_) => UiControl::LauncherLibraryFavourite(drawn),
            UiControl::LauncherLibraryFavouritePick(_) => {
                UiControl::LauncherLibraryFavouritePick(drawn)
            }
            _ => UiControl::LauncherLibraryFavouriteRemove(drawn),
        };
        self.nav.park(Some(NavTarget::Ui(at)));
    }

    #[cfg(not(feature = "game-library"))]
    pub(super) fn nav_sync_library(&mut self) {}

    /// Whether the list the focus is standing in has run out this way.
    #[cfg(feature = "game-library")]
    pub(super) fn library_at_end(&self, step: isize) -> bool {
        use crate::video::launcher::LibraryFocus;
        self.launcher_state().is_some_and(|state| {
            let (at, len) = match state.library.focus {
                LibraryFocus::Games => (state.library.selected, state.library.games.len()),
                LibraryFocus::Favourites => (
                    state.library.favourite_selected,
                    state.library.db.favourite_count(),
                ),
            };
            len == 0 || if step < 0 { at == 0 } else { at + 1 >= len }
        })
    }

    /// Whether the focus is standing on the game list, whose own
    /// arrows scroll it rather than moving the focus.
    #[cfg(feature = "game-library")]
    pub(super) fn nav_focus_in_library(&self) -> bool {
        use crate::video::nav::NavTarget;
        matches!(
            self.nav.focus(),
            Some(NavTarget::Ui(
                UiControl::LauncherLibraryPick(_) | UiControl::LauncherLibraryFavouritePick(_)
            ))
        )
    }

    /// The line under the focused control, and the light on an open
    /// stepper's arrows. Drawn over the interface rather than inside
    /// it: what the focus is doing belongs to the window, not to the
    /// surface it is walking.
    /// What the surfaces need to know before they draw: where the focus
    /// stands and how far through its breath it is.
    pub(super) fn nav_light(&self) -> (Option<UiControl>, f32) {
        let Some(target) = self.nav.shown() else {
            return (None, 0.0);
        };
        // A stepper's two arrows are one place: the surface is told the
        // one, whichever end the map happened to meet first.
        match crate::video::nav::normalise(target) {
            crate::video::nav::NavTarget::Ui(control) => (Some(control), self.nav_mix()),
            crate::video::nav::NavTarget::Bar(_) => (None, 0.0),
        }
    }

    /// The bar's half of the same answer: it draws itself, so it is
    /// told separately.
    pub(super) fn nav_bar_light(&self) -> (Option<BarControl>, f32) {
        match self.nav.shown() {
            Some(crate::video::nav::NavTarget::Bar(control)) => (Some(control), self.nav_mix()),
            _ => (None, 0.0),
        }
    }

    /// How lit the focused control stands: breathing while it is merely
    /// chosen, full while it stands open for changing.
    pub(super) fn nav_mix(&self) -> f32 {
        if self.nav.open() {
            1.0
        } else {
            self.nav_pulse()
        }
    }

    /// Where the focus's breath stands: dim at the bottom of it, full
    /// at the top. A pure function of the clock, so it never depends on
    /// how often the window happens to redraw.
    fn nav_pulse(&self) -> f32 {
        let ms = self.nav_shown_at.elapsed().as_millis() as u64 % NAV_PULSE_MS;
        let phase = ms as f32 / NAV_PULSE_MS as f32 * std::f32::consts::TAU;
        let swing = (1.0 - NAV_PULSE_FLOOR) / 2.0;
        NAV_PULSE_FLOOR + swing * (1.0 + phase.sin())
    }

    /// Walk the interface with the pad: the stick and the hat move the
    /// focus, the fire button works what it is standing on, and the
    /// second button steps back out -- closing an open stepper, then
    /// the surface itself.
    ///
    /// A held direction repeats, slowly at first and then faster, the
    /// way every other held control in this interface does; the buttons
    /// fire once each on the press, because a button that repeated
    /// would open and close a setting under your thumb.
    pub(super) fn pad_drives_interface(
        &mut self,
        pad: crate::gamepad::JoystickState,
        event_loop: Option<&ActiveEventLoop>,
    ) {
        use crate::video::nav::Dir;
        let now = Instant::now();
        let dir = if pad.up {
            Some(Dir::Up)
        } else if pad.down {
            Some(Dir::Down)
        } else if pad.left {
            Some(Dir::Left)
        } else if pad.right {
            Some(Dir::Right)
        } else {
            None
        };
        match dir {
            None => self.pad_nav.held = None,
            Some(dir) => {
                let fresh = self.pad_nav.held != Some(dir);
                if fresh {
                    self.pad_nav.held = Some(dir);
                    self.pad_nav.since = now;
                    self.pad_nav.next = now + PAD_NAV_DELAY;
                    self.nav_move(dir, event_loop);
                } else if now >= self.pad_nav.next {
                    let held = now.saturating_duration_since(self.pad_nav.since);
                    self.pad_nav.next = now + pad_nav_every(held);
                    self.nav_move(dir, event_loop);
                }
            }
        }
        if pad.fire && !self.pad_nav.fire {
            self.nav_press(event_loop);
        }
        self.pad_nav.fire = pad.fire;
        if pad.button2 && !self.pad_nav.back {
            self.nav_back();
        }
        self.pad_nav.back = pad.button2;
    }

    /// The pad asking for the interface, or asking to be rid of it: the
    /// menu opens with the focus already on its first row, and closes
    /// again the same way. Whatever else is open closes first, so one
    /// button always leads back to the machine.
    pub(super) fn toggle_pad_interface(&mut self) {
        if self.ui.menu_open {
            self.close_menu();
            self.nav.clear();
        } else if self.ui.panel.is_some() {
            self.close_panel();
        } else {
            // The menu is the way in. It opens with its own cursor on
            // the foot of its list -- the menu is a tree, and keeps that
            // cursor rather than a place on a map -- and the pad walks
            // from there into the bar and the panels beyond it.
            self.toggle_menu();
        }
        self.request_redraw();
    }

    /// Step a held launcher stepper. The pointer must still be on the
    /// arrow, the way a held scroll arrow works: a button that kept
    /// firing from under the pointer would be one you cannot get away
    /// from.
    pub(super) fn repeat_held_cycle(&mut self) {
        let Some((control, due, started)) = self.cycle_hold else {
            return;
        };
        if self.cursor_pos.and_then(|p| self.main_ui_control_at(p)) != Some(control) {
            self.cycle_hold = None;
            return;
        }
        let now = Instant::now();
        if now < due {
            return;
        }
        let UiControl::LauncherCycle { field, .. } = control else {
            self.cycle_hold = None;
            return;
        };
        self.cycle_hold = Some((
            control,
            now + cycle_hold_every(field, now - started),
            started,
        ));
        self.activate_ui_control_with_event_loop(control, None);
        self.request_redraw();
    }
}
