// SPDX-License-Identifier: GPL-3.0-or-later

//! Driving the interface without a pointer.
//!
//! The map of what can be reached is not written down anywhere: it is
//! read off the same hit-testing the mouse uses, by asking it what sits
//! at each point of a coarse grid and collecting what answers. That way
//! there is one description of where things are -- the one the pointer
//! already trusts -- and a control cannot appear under the mouse but be
//! unreachable from the keyboard, which is exactly how such maps rot.
//!
//! What the focus means is deliberately small. A control is either
//! *focused* -- which the surfaces show by lighting it, the way they
//! light what the pointer is over -- or *open*, which only a stepper
//! can be: its arrows light and left and right change its value
//! instead of moving on. How any of that is drawn is the surfaces'
//! own business; this module only says where the focus is.

use super::ui::{UiControl, UiState};
use super::window::statusbar::BarControl;
use super::window::Rect;

/// Somewhere the focus can stand. The interface's surfaces and the
/// status bar are one space to walk: the bar sits under the panels on
/// the screen, so stepping down off the bottom of one reaches it, and
/// stepping back up returns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::video) enum NavTarget {
    Ui(UiControl),
    Bar(BarControl),
}

/// How far apart the probes sit when reading the map. Fine enough to
/// find the smallest control the interface draws (a scroll arrow is
/// nine pixels across), coarse enough that a whole screen is a few
/// thousand questions rather than a million.
const PROBE_STEP: usize = 3;

/// One place the focus can stand: a control, and the box it occupies.
/// A stepper's box takes in both its arrows and the value between
/// them, because the focus stands on the setting rather than on one
/// end of it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::video) struct NavItem {
    pub(in crate::video) target: NavTarget,
    pub(in crate::video) rect: Rect,
}

/// Everything reachable on the surface as it stands, in reading order.
///
/// The two halves of a stepper answer as one place to stand: the focus
/// belongs to the setting, and its arrows are what opening it lights.
pub(in crate::video) fn map(ui: &UiState, width: usize, height: usize) -> Vec<NavItem> {
    // Sampled raw first: a stepper's two arrows are two controls here,
    // and only afterwards become the one place the focus stands.
    let mut raw: Vec<(UiControl, Vec<Rect>)> = Vec::new();
    let mut y = 0;
    while y < height {
        let mut x = 0;
        while x < width {
            if let Some(control) = ui.control_at((x as i32, y as i32)) {
                if reachable(control) && crate::video::ui::control_live(ui, control) {
                    add_sample(&mut raw, control, x, y);
                }
            }
            x += PROBE_STEP;
        }
        y += PROBE_STEP;
    }
    let mut items: Vec<NavItem> = Vec::new();
    for (control, rect) in largest(raw) {
        let key = focus_key(NavTarget::Ui(control));
        match items.iter_mut().find(|i| focus_key(i.target) == key) {
            Some(item) => {
                // The second arrow of a stepper: the box grows to hold
                // both of them and the value they sit either side of.
                let (left, right) = if rect.x < item.rect.x {
                    (rect, item.rect)
                } else {
                    (item.rect, rect)
                };
                item.rect = Rect {
                    x: left.x,
                    y: left.y.min(right.y),
                    w: (right.x + right.w) - left.x,
                    h: left.h.max(right.h),
                };
            }
            None => items.push(NavItem {
                target: NavTarget::Ui(control),
                rect,
            }),
        }
    }
    // Reading order, for a surface with no geometry worth speaking of.
    items.sort_by_key(|i| (i.rect.y, i.rect.x));
    items
}

/// Take a probe into the clusters of the control it found.
///
/// One control can answer for two places at once -- a page reached
/// both from the category column and from the row of sibling pages
/// above the settings -- and a box drawn round both of them would span
/// the width of the window, which is neither where the control is nor
/// anywhere the focus should think it is. So each control keeps its
/// separate clusters, and the largest of them is where it lives.
fn add_sample(clusters: &mut Vec<(UiControl, Vec<Rect>)>, control: UiControl, x: usize, y: usize) {
    let entry = match clusters.iter_mut().find(|(c, _)| *c == control) {
        Some(entry) => entry,
        None => {
            clusters.push((control, Vec::new()));
            clusters.last_mut().expect("just pushed")
        }
    };
    // Near an existing cluster means part of it: probes are a step
    // apart, so anything within a step of the box belongs to it.
    let reach = PROBE_STEP;
    match entry.1.iter_mut().find(|r| {
        x + reach >= r.x && x <= r.x + r.w + reach && y + reach >= r.y && y <= r.y + r.h + reach
    }) {
        Some(rect) => *rect = union(*rect, x, y),
        None => entry.1.push(Rect { x, y, w: 1, h: 1 }),
    }
}

/// Where each control lives, when it answers for more than one place:
/// its largest cluster, which is the control itself rather than some
/// sliver of it.
fn largest(clusters: Vec<(UiControl, Vec<Rect>)>) -> Vec<(UiControl, Rect)> {
    clusters
        .into_iter()
        .filter_map(|(control, rects)| {
            rects
                .into_iter()
                .max_by_key(|r| r.w * r.h)
                .map(|rect| (control, rect))
        })
        .collect()
}

/// The status bar's own controls, sampled the same way. The bar keeps
/// its hit-testing to itself, so the caller hands in the way to ask.
pub(in crate::video) fn bar_map(
    bar: Rect,
    control_at: impl Fn((i32, i32)) -> Option<BarControl>,
    volume: Rect,
) -> Vec<NavItem> {
    let mut items: Vec<NavItem> = Vec::new();
    let mut y = bar.y;
    while y < bar.y + bar.h {
        let mut x = bar.x;
        while x < bar.x + bar.w {
            if let Some(control) = control_at((x as i32, y as i32)) {
                let target = NavTarget::Bar(control);
                match items.iter_mut().find(|i| i.target == target) {
                    Some(item) => item.rect = union(item.rect, x, y),
                    None => items.push(NavItem {
                        target,
                        rect: Rect { x, y, w: 1, h: 1 },
                    }),
                }
            }
            x += PROBE_STEP;
        }
        y += PROBE_STEP;
    }
    // The volume is a slider rather than a button: its whole travel is
    // what the focus stands on, and what the line goes under.
    if let Some(item) = items
        .iter_mut()
        .find(|i| i.target == NavTarget::Bar(BarControl::Volume))
    {
        item.rect = volume;
    }
    items.sort_by_key(|i| (i.rect.y, i.rect.x));
    items
}

/// Whether the focus has any business standing on a control. The body
/// of a panel answers for every point it does not otherwise use -- it
/// is how a click is kept from falling through to the machine -- and
/// the focus must not take that for a control, or the line would be
/// drawn around the whole window and pressing it would do nothing.
fn reachable(control: UiControl) -> bool {
    !matches!(
        control,
        UiControl::PanelBody
            | UiControl::MenuRow { .. }
            // A list's scroll arrows are the pointer's way of doing what
            // up and down already do from a row of it, and they sit at
            // the far right of the box: standing on one puts the marker
            // nowhere the eye is. Coming up out of the buttons under a
            // list found the arrow rather than the list, and coming down
            // into one found the arrow above the first row -- which,
            // greyed at the top of its list, cannot light at all.
            | UiControl::LauncherHostDiskScroll(_)
    ) && !library_scroll(control)
}

/// The game page's own scroll arrows, where that page is built at all.
#[cfg(feature = "game-library")]
fn library_scroll(control: UiControl) -> bool {
    matches!(
        control,
        UiControl::LauncherLibraryScroll(_) | UiControl::LauncherLibraryFavouriteScroll(_)
    )
}

#[cfg(not(feature = "game-library"))]
fn library_scroll(_control: UiControl) -> bool {
    false
}

/// The one place a control stands for, for anything that needs to name
/// the focus rather than move it.
pub(in crate::video) fn normalise(target: NavTarget) -> NavTarget {
    focus_key(target)
}

/// What the focus treats as one place. A stepper's two arrows share a
/// key: the focus lands on the setting, not on one of its ends.
fn focus_key(target: NavTarget) -> NavTarget {
    match target {
        NavTarget::Ui(UiControl::LauncherCycle { field, .. }) => {
            NavTarget::Ui(UiControl::LauncherCycle {
                field,
                forward: false,
            })
        }
        other => other,
    }
}

/// Grow a box to take in another probe. The box holds the probes and
/// nothing more: growing it by the step between them would have it
/// overlap the rows either side, and then neither is beyond the other
/// and the walk stops dead.
fn union(rect: Rect, x: usize, y: usize) -> Rect {
    let x0 = rect.x.min(x);
    let y0 = rect.y.min(y);
    let x1 = (rect.x + rect.w).max(x + 1);
    let y1 = (rect.y + rect.h).max(y + 1);
    Rect {
        x: x0,
        y: y0,
        w: x1 - x0,
        h: y1 - y0,
    }
}

/// Which way the focus is being asked to go.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::video) enum Dir {
    Up,
    Down,
    Left,
    Right,
}

impl Dir {
    /// Whether this direction runs across the screen. An open stepper
    /// spends these on its value rather than on moving the focus.
    pub(in crate::video) fn horizontal(self) -> bool {
        matches!(self, Dir::Left | Dir::Right)
    }

    /// Whether this direction runs down the screen.
    fn vertical(self) -> bool {
        !self.horizontal()
    }
}

/// Whether a control belongs to the page beside the category column,
/// rather than to the column itself, the profiles above it or the
/// buttons and bar below.
pub(in crate::video) fn in_page(target: NavTarget) -> bool {
    matches!(band(target), Band::Pages | Band::Settings)
}

/// The control the focus should move to, or `None` if there is nothing
/// that way.
///
/// The browsers' own heuristic for this (Blink's, by way of the WICG's
/// spatial-navigation work) scores every candidate beyond the focused
/// box on a blend of distances and overlap. It was tried here and it
/// is wrong for this interface, because this interface is not a page:
/// its rows and columns are laid out on a grid the eye can see, and a
/// score that trades alignment off against nearness will, sooner or
/// later, cross the window to find something marginally closer. What
/// works is stricter and simpler -- see the walk below.
pub(in crate::video) fn step(
    items: &[NavItem],
    from: Option<NavTarget>,
    dir: Dir,
) -> Option<NavTarget> {
    let Some(from) = from.and_then(|c| find(items, c)) else {
        // Nowhere yet: the first place in reading order takes it.
        return items.first().map(|i| i.target);
    };
    // Sideways stays in the row the focus is on: left and right mean
    // along this row, so a button on another row is not a candidate
    // however near it looks -- which is what stops a step right from
    // the foot of a page landing in the status bar.
    //
    // Up and down go to the next row, and to whatever in that row is
    // nearest across the screen. Judging them by distance alone reads
    // wrongly on a page where the rows are not all the same shape: the
    // eye sees rows, and expects the one under this one, not whichever
    // control happens to be closest as the crow flies.
    //
    // Nothing that way means nothing happens. The focus stays where it
    // is at the edges of a surface rather than warping to the far
    // corner, which is what "as far as you can go" should mean.
    // Within the band the focus is in first; only when that band has
    // run out does the walk leave it, which is how the settings reach
    // the buttons under them and those reach the status bar.
    let here = band(from.target);
    for same_band in [true, false] {
        let mut best: Option<(i64, i64, NavTarget)> = None;
        for item in items {
            if focus_key(item.target) == focus_key(from.target) {
                continue;
            }
            let there = band(item.target);
            if same_band && there != here {
                continue;
            }
            // Leaving a band vertically stays on its side of the
            // window: down from the row of sibling pages carries on
            // into that page's settings rather than stepping across
            // into the category column, and up from the first setting
            // climbs to the profiles rather than to the category
            // button beside it.
            if dir.vertical()
                && side(here) != Side::Both
                && side(there) != Side::Both
                && side(there) != side(here)
            {
                continue;
            }
            if !beyond(from.rect, item.rect, dir) {
                continue;
            }
            if dir.horizontal() && !shares_row(from.rect, item.rect) {
                continue;
            }
            let (along, across) = spans(from.rect, item.rect, dir);
            // The row it is in comes first, and where it sits in that
            // row second: an item a few pixels further down is in the
            // same row as one directly below, not in a nearer one.
            let rank = (along + ROW_SLACK / 2) / ROW_SLACK;
            if best
                .as_ref()
                .is_none_or(|(r, a, _)| (rank, across) < (*r, *a))
            {
                best = Some((rank, across, item.target));
            }
        }
        if let Some((_, _, target)) = best {
            return Some(foot_landing(items, from.target, dir, target));
        }
    }
    None
}

/// Where a walk that comes down onto the row of buttons at the foot
/// lands.
///
/// Each track ends on its own end of that row: the category column on
/// the first button, the page beside it on the last. Nearest-across
/// lands wherever the page's bottom setting happens to sit over, which
/// is Defaults from one page and Run from the next, and reads as
/// random -- the row is one thing, not four things under four columns.
fn foot_landing(items: &[NavItem], from: NavTarget, dir: Dir, target: NavTarget) -> NavTarget {
    if dir != Dir::Down || band(from) == Band::Foot || band(target) != Band::Foot {
        return target;
    }
    let feet = items.iter().filter(|item| band(item.target) == Band::Foot);
    let end = if side(band(from)) == Side::Column {
        feet.min_by_key(|item| item.rect.x)
    } else {
        feet.max_by_key(|item| item.rect.x)
    };
    end.map_or(target, |item| item.target)
}

/// How far apart two rows have to be before they are different rows.
/// Rows of controls are rarely aligned to the pixel, and a page mixes
/// tall boxes with short ones.
const ROW_SLACK: i64 = 14;

/// The gaps between two boxes, along the direction and across it.
fn spans(from: Rect, to: Rect, dir: Dir) -> (i64, i64) {
    let dx = gap(from.x, from.w, to.x, to.w);
    let dy = gap(from.y, from.h, to.y, to.h);
    if dir.horizontal() {
        (dx, dy)
    } else {
        (dy, dx)
    }
}

/// The band of the screen a control belongs to. The configuration
/// screen is built of them: a row of machine profiles, a row of
/// sibling pages, a column of categories, the settings themselves, the
/// buttons along the foot, and the status bar under everything.
///
/// Up and down keep to the band they start in wherever they can. Two
/// bands can interleave down the screen -- the category column and the
/// settings beside it are the obvious pair -- and a walk down one of
/// them that hopped into the other whenever its rows happened to fall
/// between would be unusable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Band {
    Close,
    Profiles,
    Pages,
    Column,
    Settings,
    Foot,
    Bar,
}

/// Which of the two tracks running down the window a band belongs to.
///
/// The category column on the left and the page beside it interleave
/// all the way down, so a walk down one that fell into the other
/// whenever a row happened to line up would be unusable. Bands that
/// span the whole width -- the profiles above, the status bar below --
/// belong to both, and are where a track ends.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Side {
    Column,
    Page,
    Both,
}

fn side(band: Band) -> Side {
    match band {
        Band::Column => Side::Column,
        Band::Pages | Band::Settings => Side::Page,
        // The buttons along the foot run under both tracks -- Load sits
        // beneath the column, Run beneath the page -- so each track ends
        // on whichever of them it comes down onto.
        Band::Close | Band::Profiles | Band::Foot | Band::Bar => Side::Both,
    }
}

fn band(target: NavTarget) -> Band {
    match target {
        NavTarget::Bar(_) => Band::Bar,
        // The close gadget belongs to the window's corner, not to the
        // settings it sits over: left in with them, a walk up the
        // settings would find it above everything and stop there.
        NavTarget::Ui(UiControl::PanelClose) => Band::Close,
        NavTarget::Ui(UiControl::LauncherModel(_)) => Band::Profiles,
        NavTarget::Ui(UiControl::LauncherNavTab(_)) => Band::Pages,
        NavTarget::Ui(UiControl::LauncherTab(_)) => Band::Column,
        NavTarget::Ui(
            UiControl::LauncherLoad
            | UiControl::LauncherSave
            | UiControl::LauncherSaveAs
            | UiControl::LauncherDefaults
            | UiControl::LauncherRun,
        ) => Band::Foot,
        NavTarget::Ui(_) => Band::Settings,
    }
}

/// Whether two boxes share any of the same rows of the screen.
fn shares_row(a: Rect, b: Rect) -> bool {
    a.y < b.y + b.h && b.y < a.y + a.h
}

/// Whether a candidate lies beyond the focused control's edge in the
/// direction asked for.
fn beyond(from: Rect, to: Rect, dir: Dir) -> bool {
    // A probe's worth of slack: boxes read off a grid are a pixel or
    // two out, and two rows that touch should still be beyond one
    // another.
    let slack = PROBE_STEP;
    match dir {
        Dir::Up => to.y + to.h <= from.y + slack,
        Dir::Down => to.y + slack >= from.y + from.h,
        Dir::Left => to.x + to.w <= from.x + slack,
        Dir::Right => to.x + slack >= from.x + from.w,
    }
}

/// How far apart two boxes are along one axis: nothing if they overlap
/// on it, and the gap between their nearest edges otherwise.
fn gap(a0: usize, a_len: usize, b0: usize, b_len: usize) -> i64 {
    let (a1, b1) = ((a0 + a_len) as i64, (b0 + b_len) as i64);
    let (a0, b0) = (a0 as i64, b0 as i64);
    if b0 >= a1 {
        b0 - a1
    } else if a0 >= b1 {
        a0 - b1
    } else {
        0
    }
}

/// The item a control stands for, matching a stepper by its setting.
pub(in crate::video) fn find(items: &[NavItem], target: NavTarget) -> Option<NavItem> {
    let key = focus_key(target);
    items.iter().copied().find(|i| focus_key(i.target) == key)
}

/// Whether a control is a stepper -- the one kind that opens for
/// editing rather than simply being pressed.
pub(in crate::video) fn is_stepper(target: NavTarget) -> bool {
    matches!(
        target,
        NavTarget::Ui(UiControl::LauncherCycle { .. }) | NavTarget::Bar(BarControl::Volume)
    )
}

/// The stepper's own arrow for a direction, for the press an open
/// setting turns into.
pub(in crate::video) fn stepper_arrow(target: NavTarget, dir: Dir) -> Option<NavTarget> {
    match target {
        NavTarget::Ui(UiControl::LauncherCycle { field, .. }) => {
            Some(NavTarget::Ui(UiControl::LauncherCycle {
                field,
                forward: dir == Dir::Right,
            }))
        }
        _ => None,
    }
}

/// Where the focus stands, and how it is being shown.
#[derive(Debug, Default, Clone)]
pub(in crate::video) struct Nav {
    /// The control the focus is on. Kept across a mouse click so
    /// coming back to the keyboard resumes where the hand left off.
    focus: Option<NavTarget>,
    /// Whether the focus is being shown. Set by a key or a pad, cleared
    /// by the pointer: a line under a control means the keyboard is
    /// driving, and the moment the mouse is used it is not.
    showing: bool,
    /// A stepper standing open: its arrows are lit, and left and right
    /// change the value instead of moving on.
    open: bool,
}

impl Nav {
    /// The control the focus is on, whether or not it is being shown.
    pub(in crate::video) fn focus(&self) -> Option<NavTarget> {
        self.focus
    }

    /// The control the focus is on while it is being shown -- what the
    /// surfaces light. `None` while the pointer is driving.
    pub(in crate::video) fn shown(&self) -> Option<NavTarget> {
        self.showing.then_some(self.focus).flatten()
    }

    /// Whether the focused stepper stands open.
    pub(in crate::video) fn open(&self) -> bool {
        self.open && self.focus.is_some_and(is_stepper)
    }

    pub(in crate::video) fn showing(&self) -> bool {
        self.showing
    }

    /// Put the focus somewhere and show it.
    pub(in crate::video) fn show(&mut self, target: Option<NavTarget>) {
        self.focus = target;
        self.showing = true;
        self.open = false;
    }

    /// Remember where the pointer last pressed, so the keyboard picks
    /// up from there rather than from the top of the page.
    pub(in crate::video) fn follow_pointer(&mut self, target: NavTarget) {
        self.focus = Some(target);
        self.showing = false;
        self.open = false;
    }

    /// Open or close the focused stepper.
    pub(in crate::video) fn toggle_open(&mut self) {
        if self.focus.is_some_and(is_stepper) {
            self.open = !self.open;
        }
    }

    pub(in crate::video) fn close(&mut self) {
        self.open = false;
    }

    /// Forget everything: the surface underneath has gone.
    /// Put the focus somewhere without saying whether it is shown.
    ///
    /// A surface that has just opened has a place to start from, and
    /// whichever hand opened it keeps the marker as it was: opening the
    /// launcher from the menu carries the keyboard's marker straight
    /// onto the page, opening it with the mouse leaves it unmarked.
    pub(in crate::video) fn park(&mut self, target: Option<NavTarget>) {
        self.focus = target;
        self.open = false;
    }

    pub(in crate::video) fn clear(&mut self) {
        *self = Self::default();
    }

    /// Keep the focus honest when the page changes under it: a control
    /// that is no longer there hands the focus to the first thing that
    /// is.
    pub(in crate::video) fn settle(&mut self, items: &[NavItem], home: Option<NavTarget>) {
        // Somewhere it cannot be -- the page changed under it -- or
        // nowhere at all, which is where a surface that has just opened
        // starts.
        if !self.focus.is_some_and(|t| find(items, t).is_some()) {
            // Home if the surface offers one -- the page the eye starts
            // on -- and otherwise the first place in reading order.
            self.focus = home
                .filter(|t| find(items, *t).is_some())
                .or_else(|| items.first().map(|i| i.target));
            self.open = false;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    /// One page's map, as the window sees it.
    fn page(tab: crate::video::launcher::LauncherTab) -> Vec<NavItem> {
        page_of(tab, |_| {})
    }

    /// The same, with the page's own contents put there first: a page
    /// that is mostly list has none of the things a walk across it must
    /// find until something is in the list.
    fn page_of(
        tab: crate::video::launcher::LauncherTab,
        fill: impl FnOnce(&mut crate::video::launcher::LauncherState),
    ) -> Vec<NavItem> {
        use crate::video::launcher::{LauncherState, MachineSetup};
        use crate::video::menu::MenuNav;
        use crate::video::ui::Panel;
        let mut state = LauncherState::new(MachineSetup::default());
        state.tab = tab;
        fill(&mut state);
        let ui = UiState {
            menu_open: false,
            menu_rows: Vec::new(),
            menu_nav: MenuNav::default(),
            panel: Some(Panel::Launcher(Box::new(state))),
        };
        map(
            &ui,
            crate::video::window::texture_width(1),
            crate::video::window::texture_height(1),
        )
    }

    /// Every page offers the focus somewhere to go when it steps right
    /// out of the category column, and the rows across the top of a
    /// page lead back up to the machine profiles rather than to the
    /// corner of the window.
    #[test]
    fn every_page_can_be_entered_and_left() {
        use crate::config::MachineModel;
        use crate::video::launcher::{LauncherState, LauncherTab, MachineSetup};
        use crate::video::menu::MenuNav;
        use crate::video::ui::Panel;

        for tab in [
            LauncherTab::System,
            LauncherTab::Cpu,
            LauncherTab::Memory,
            LauncherTab::Rom,
            LauncherTab::Floppy,
            LauncherTab::Storage,
            LauncherTab::Input,
            LauncherTab::IoPorts,
            LauncherTab::Zorro,
            LauncherTab::WhdloadLibrary,
            LauncherTab::AvAudio,
        ] {
            let mut state = LauncherState::new(MachineSetup::default());
            state.tab = tab;
            let ui = UiState {
                menu_open: false,
                menu_rows: Vec::new(),
                menu_nav: MenuNav::default(),
                panel: Some(Panel::Launcher(Box::new(state))),
            };
            let items = map(
                &ui,
                crate::video::window::texture_width(1),
                crate::video::window::texture_height(1),
            );
            // Something to enter: either a row of sibling pages, or a
            // setting of its own. The window picks between them; the map
            // has to offer at least one.
            let column = find(&items, NavTarget::Ui(UiControl::LauncherTab(tab)))
                .unwrap_or_else(|| panic!("{tab:?} has no category button"));
            let right_of_column = column.rect.x + column.rect.w;
            assert!(
                items.iter().any(|item| {
                    item.rect.x >= right_of_column
                        && matches!(band(item.target), Band::Pages | Band::Settings)
                }),
                "{tab:?} offers the focus nothing to the right of its button"
            );
            // And a page with sibling pages climbs from them to the
            // profiles above, not to the close gadget in the corner.
            if let Some(pages) = items
                .iter()
                .find(|item| band(item.target) == Band::Pages)
                .map(|item| item.target)
            {
                assert!(
                    matches!(
                        step(&items, Some(pages), Dir::Up),
                        Some(NavTarget::Ui(UiControl::LauncherModel(_)))
                    ),
                    "{tab:?} does not climb from its sibling pages to the profiles"
                );
            }
            // Above the top row of profiles there is only the corner of
            // the window: climbing must never drop back into the page.
            assert!(
                matches!(
                    step(
                        &items,
                        Some(NavTarget::Ui(UiControl::LauncherModel(MachineModel::A1000))),
                        Dir::Up
                    ),
                    None | Some(NavTarget::Ui(UiControl::PanelClose))
                ),
                "{tab:?} climbs off the top row back into the page"
            );
        }
    }

    /// The buttons along the foot end both tracks: the category column
    /// comes down onto them, and so does the page beside it.
    #[test]
    fn the_foot_ends_both_tracks() {
        use crate::video::launcher::LauncherTab;
        let items = page(LauncherTab::Rom);
        assert_eq!(
            step(
                &items,
                Some(NavTarget::Ui(UiControl::LauncherTab(LauncherTab::AvAudio))),
                Dir::Down
            ),
            Some(NavTarget::Ui(UiControl::LauncherLoad)),
            "the last category comes down onto the button under it"
        );
        // And the page's own track ends there too, rather than stepping
        // sideways into the column on its way down.
        assert_eq!(
            step(
                &items,
                Some(NavTarget::Ui(UiControl::LauncherBrowse(
                    crate::video::launcher::LauncherField::ExtendedRom
                ))),
                Dir::Down
            ),
            Some(NavTarget::Ui(UiControl::LauncherRun)),
            "the last setting comes down onto the buttons, not the column"
        );
        // Climbing off the first setting of a page with no row of
        // sibling pages goes to the profiles above, not across into the
        // category button beside it.
        assert!(
            matches!(
                step(
                    &items,
                    Some(NavTarget::Ui(UiControl::LauncherBrowse(
                        crate::video::launcher::LauncherField::Rom
                    ))),
                    Dir::Up
                ),
                Some(NavTarget::Ui(UiControl::LauncherModel(_)))
            ),
            "the first setting climbs to the machines above it"
        );
    }

    /// A row of choices is walked both ways. Coming back left through
    /// one used to leave the page at the first step, because leaving it
    /// was tried before staying on it.
    #[test]
    fn a_row_of_choices_walks_both_ways() {
        use crate::video::launcher::{FsFamily, LauncherField, LauncherTab};
        let items = page(LauncherTab::CreateFloppy);
        let family = |family| {
            NavTarget::Ui(UiControl::LauncherFsFamily {
                field: LauncherField::NewFloppyFs,
                family,
            })
        };
        assert_eq!(
            step(&items, Some(family(FsFamily::Ffs)), Dir::Left),
            Some(family(FsFamily::Ofs)),
            "left goes back through the choices"
        );
        assert_eq!(
            step(&items, Some(family(FsFamily::Ofs)), Dir::Left),
            Some(family(FsFamily::Unformatted)),
            "and keeps going"
        );
        // Only then has the row run out. What is left of it is the
        // category column, which the window turns into the button that
        // opened the page.
        assert!(
            !step(&items, Some(family(FsFamily::Unformatted)), Dir::Left).is_some_and(in_page),
            "the first choice in the row leaves the page"
        );
    }

    /// The game page is a list with a strip of letters over it and a
    /// second list under the buttons, and every walk across it has been
    /// got wrong at least once.
    #[test]
    fn the_game_page_is_a_list_to_walk() {
        use crate::video::launcher::LauncherTab;
        let items = page_of(LauncherTab::WhdloadLibrary, |state| {
            state.library.games =
                crate::gamelib::Library::of_titles((0..40).map(|i| format!("Game {i:03}")));
            for i in 0..3 {
                let title = format!("Game {i:03}");
                state.library.db.toggle_favourite(&title, &title);
            }
        });
        let go = |from: UiControl, dir: Dir| step(&items, Some(NavTarget::Ui(from)), dir);
        let at = |control: UiControl| Some(NavTarget::Ui(control));
        use UiControl::{
            LauncherLibraryFavourite as Tick, LauncherLibraryFavouritePick as Starred,
            LauncherLibraryJump as Letter, LauncherLibraryPick as Game,
            LauncherLibraryRefresh as Refresh,
        };

        // The letters are a strip: along it either way, and no further
        // than its ends.
        assert_eq!(go(Letter(0), Dir::Right), at(Letter(1)));
        assert_eq!(go(Letter(1), Dir::Left), at(Letter(0)));
        assert_eq!(go(Letter(27), Dir::Right), None, "Z is the end of it");
        assert!(
            !go(Letter(0), Dir::Left).is_some_and(in_page),
            "and left off the first leaves the page, for the button that opened it"
        );
        // Above them is the row of sibling pages; below them, the list.
        assert_eq!(
            go(Letter(0), Dir::Up),
            at(UiControl::LauncherNavTab(LauncherTab::WhdloadLibrary))
        );
        assert_eq!(go(Letter(0), Dir::Down), at(Game(0)));

        // A row's tick is drawn inside the row's own box, so neither
        // is beyond the other and the geometry has nothing to offer
        // across a list. The window names those steps itself.
        assert_eq!(go(Game(0), Dir::Right), None);
        assert_ne!(
            go(Tick(0), Dir::Left),
            at(Game(0)),
            "the geometry cannot find a row from the tick drawn inside it"
        );

        // Under the list are its buttons, and under those the
        // favourites, which is the only way down to them.
        assert_eq!(go(Refresh, Dir::Down), at(Starred(0)));
        assert!(
            matches!(go(Refresh, Dir::Up), Some(NavTarget::Ui(Game(_)))),
            "up off the buttons comes back to the list, got {:?}",
            go(Refresh, Dir::Up)
        );
    }

    /// The Back button on a sub-page leads down into the page, and
    /// where the page has nothing to lead into, past the buttons that
    /// cannot be pressed to the one that can.
    #[test]
    fn a_sub_page_is_entered_from_its_back_button() {
        use crate::video::launcher::LauncherTab;
        let back = Some(NavTarget::Ui(UiControl::LauncherNavTab(
            LauncherTab::Storage,
        )));
        let full = page_of(LauncherTab::HostDisk, |state| {
            state.setup.fake_host_disks(4);
        });
        assert_eq!(
            step(&full, back, Dir::Down),
            Some(NavTarget::Ui(UiControl::LauncherHostDiskSelect(0))),
            "down off Back is the first disk in the list"
        );
        assert_eq!(
            step(
                &full,
                Some(NavTarget::Ui(UiControl::LauncherHostDiskSelect(3))),
                Dir::Down
            ),
            Some(NavTarget::Ui(UiControl::LauncherHostDiskRefresh)),
            "and the last row comes down onto the button that can be pressed"
        );
        // A list long enough to scroll grows a pair of arrows in the
        // corner of its box, above the first row and below the last.
        // They are the pointer's, and greyed at the end of the list they
        // cannot light: down off Back found one and the marker vanished.
        let long = page_of(LauncherTab::HostDisk, |state| {
            state.setup.fake_host_disks(20);
        });
        assert_eq!(
            step(&long, back, Dir::Down),
            Some(NavTarget::Ui(UiControl::LauncherHostDiskSelect(0))),
            "down off Back is the first disk, not the scroll arrow over it"
        );
        // Across a row: the attach column only once the disk is ticked,
        // then its two ticks. The Enable tick is a place of its own so
        // the focus can stand on the box it ticks.
        assert_eq!(
            step(
                &full,
                Some(NavTarget::Ui(UiControl::LauncherHostDiskSelect(0))),
                Dir::Right
            ),
            Some(NavTarget::Ui(UiControl::LauncherHostDiskWritable(0))),
            "an unticked disk has no attach cell to stand on"
        );
        assert_eq!(
            step(
                &full,
                Some(NavTarget::Ui(UiControl::LauncherHostDiskWritable(0))),
                Dir::Right
            ),
            Some(NavTarget::Ui(UiControl::LauncherHostDiskEnable(0))),
        );
        // With nothing to mount, Mount and Unmount are dead: the focus
        // steps over them to Refresh, which only ever looks.
        let empty = page(LauncherTab::HostDisk);
        assert_eq!(
            step(&empty, back, Dir::Down),
            Some(NavTarget::Ui(UiControl::LauncherHostDiskRefresh)),
            "an empty list drops past the dead buttons"
        );
    }

    /// A panel with nothing on it but its close gadget still offers that:
    /// a surface the focus cannot enter is a surface Escape is the only
    /// way out of.
    #[test]
    fn every_panel_offers_its_close_gadget() {
        use crate::video::menu::MenuNav;
        use crate::video::ui::{Panel, UiState};
        for panel in [Panel::About, Panel::Shortcuts] {
            let ui = UiState {
                menu_open: false,
                menu_rows: Vec::new(),
                menu_nav: MenuNav::default(),
                panel: Some(panel),
            };
            let items = map(
                &ui,
                crate::video::window::texture_width(1),
                crate::video::window::texture_height(1),
            );
            assert!(
                find(&items, NavTarget::Ui(UiControl::PanelClose)).is_some(),
                "the close gadget is a place to stand"
            );
        }
    }

    /// A page's map, printed, for working out why a step goes where it
    /// does.
    #[test]
    #[ignore = "a tool for reading a page, not a check"]
    fn dump_page() {
        use crate::video::launcher::LauncherTab;
        for tab in [LauncherTab::BootPriority] {
            println!("=== {tab:?}");
            for i in page_of(tab, |_| {}) {
                println!(
                    "{:>4},{:>4} {:>4}x{:<4} {:?}",
                    i.rect.x, i.rect.y, i.rect.w, i.rect.h, i.target
                );
            }
        }
    }

    /// The bar's own controls reach the map: the focus can walk down
    /// off the foot of a surface onto them.
    #[test]
    fn the_bar_is_in_the_map() {
        use crate::video::window::statusbar::{bar_layout, control_at, status_bar_rect, MediaBar};
        // The bar as it stands with a bare machine: no drives, no CD.
        let layout = bar_layout(&MediaBar {
            drives: Default::default(),
            cd: None,
        });
        let bar = status_bar_rect();
        println!("bar rect {:?}", (bar.x, bar.y, bar.w, bar.h));
        let items = bar_map(
            bar,
            |pos| control_at(pos, &layout),
            crate::video::window::statusbar::volume_control_hit_rect(),
        );
        for i in &items {
            println!(
                "{:>4},{:>4} {:>4}x{:<4} {:?}",
                i.rect.x, i.rect.y, i.rect.w, i.rect.h, i.target
            );
        }
        assert!(!items.is_empty(), "the bar has controls to stand on");

        // And the focus can walk down onto them from the foot of a
        // surface, which is the only way to reach the bar without a
        // pointer.
        use crate::video::launcher::{LauncherState, LauncherTab, MachineSetup};
        use crate::video::menu::MenuNav;
        use crate::video::ui::Panel;
        let mut state = LauncherState::new(MachineSetup::default());
        state.tab = LauncherTab::IoPorts;
        let ui = UiState {
            menu_open: false,
            menu_rows: Vec::new(),
            menu_nav: MenuNav::default(),
            panel: Some(Panel::Launcher(Box::new(state))),
        };
        let mut all = map(
            &ui,
            crate::video::window::texture_width(1),
            crate::video::window::texture_height(1),
        );
        all.extend(items);
        let down = step(&all, Some(NavTarget::Ui(UiControl::LauncherRun)), Dir::Down);
        assert!(
            matches!(down, Some(NavTarget::Bar(_))),
            "down off the last button reaches the bar, got {down:?}"
        );
        // And back up again.
        let up = step(&all, down, Dir::Up);
        assert!(
            matches!(up, Some(NavTarget::Ui(_))),
            "and up returns to the surface, got {up:?}"
        );
    }

    /// The walk across a real page, against the real map. Directions mean
    /// what the eye means by them: down from a machine profile lands on
    /// the box directly below it, sideways along the row of sibling
    /// pages stays in that row, and the buttons along the foot reach
    /// each other rather than climbing into the settings above.
    #[test]
    fn the_walk_follows_the_eye() {
        use crate::config::MachineModel;
        use crate::video::launcher::{LauncherState, LauncherTab, MachineSetup};
        use crate::video::menu::MenuNav;
        use crate::video::ui::Panel;

        let mut state = LauncherState::new(MachineSetup::default());
        state.tab = LauncherTab::IoPorts;
        let ui = UiState {
            menu_open: false,
            menu_rows: Vec::new(),
            menu_nav: MenuNav::default(),
            panel: Some(Panel::Launcher(Box::new(state))),
        };
        let items = map(
            &ui,
            crate::video::window::texture_width(1),
            crate::video::window::texture_height(1),
        );
        let go = |from: UiControl, dir: Dir| step(&items, Some(NavTarget::Ui(from)), dir);
        let at = |control: UiControl| Some(NavTarget::Ui(control));

        assert_eq!(
            go(UiControl::LauncherModel(MachineModel::A3000), Dir::Down),
            at(UiControl::LauncherNavTab(LauncherTab::IoPorts)),
            "down lands on what is directly below"
        );
        assert_eq!(
            go(
                UiControl::LauncherNavTab(LauncherTab::IoParallel),
                Dir::Left
            ),
            at(UiControl::LauncherNavTab(LauncherTab::IoPorts)),
            "left takes the page beside it, not the profile above"
        );
        assert_eq!(
            go(UiControl::LauncherSave, Dir::Right),
            at(UiControl::LauncherDefaults),
            "right along the foot reaches the buttons to the right of it"
        );
        assert_eq!(
            go(UiControl::LauncherDefaults, Dir::Left),
            at(UiControl::LauncherSave),
        );
        assert_eq!(
            go(UiControl::LauncherTab(LauncherTab::Cpu), Dir::Down),
            at(UiControl::LauncherTab(LauncherTab::Memory)),
            "the category column is a column"
        );
        // Down goes to the row below, and to what in that row is
        // nearest across the screen -- not to whatever is closest as
        // the crow flies.
        assert_eq!(
            go(UiControl::LauncherLoad, Dir::Right),
            at(UiControl::LauncherSave),
        );
        assert_eq!(
            go(UiControl::LauncherRun, Dir::Left),
            at(UiControl::LauncherDefaults),
            "left walks back along the foot of the page"
        );

        // A page whose settings do not line up under one another: down
        // still takes the next setting rather than diving to the
        // buttons along the foot, which happen to line up exactly.
        let mut state = LauncherState::new(MachineSetup::default());
        state.tab = LauncherTab::Floppy;
        let ui = UiState {
            menu_open: false,
            menu_rows: Vec::new(),
            menu_nav: MenuNav::default(),
            panel: Some(Panel::Launcher(Box::new(state))),
        };
        let floppy = map(
            &ui,
            crate::video::window::texture_width(1),
            crate::video::window::texture_height(1),
        );
        let down = step(
            &floppy,
            Some(NavTarget::Ui(UiControl::LauncherBrowse(
                crate::video::launcher::LauncherField::Df0Image,
            ))),
            Dir::Down,
        );
        assert!(
            !matches!(down, Some(NavTarget::Ui(UiControl::LauncherDefaults))),
            "down from Browse takes the settings under it, not the foot of the page"
        );
        // The row under the drive-speed stepper is the one holding the
        // Browse button, so that is where down goes -- there is no
        // other way to reach it.
        assert_eq!(
            step(
                &floppy,
                Some(NavTarget::Ui(UiControl::LauncherCycle {
                    field: crate::video::launcher::LauncherField::FloppySpeed,
                    forward: false,
                })),
                Dir::Down,
            ),
            Some(NavTarget::Ui(UiControl::LauncherBrowse(
                crate::video::launcher::LauncherField::Df0Image
            ))),
            "down takes the row below, whatever is in it"
        );
        // And the row of sibling pages can always climb back to the
        // machine profiles above it.
        let mut state = LauncherState::new(MachineSetup::default());
        state.tab = LauncherTab::WhdloadLibrary;
        let ui = UiState {
            menu_open: false,
            menu_rows: Vec::new(),
            menu_nav: MenuNav::default(),
            panel: Some(Panel::Launcher(Box::new(state))),
        };
        let whdload = map(
            &ui,
            crate::video::window::texture_width(1),
            crate::video::window::texture_height(1),
        );
        assert_eq!(
            step(
                &whdload,
                Some(NavTarget::Ui(UiControl::LauncherNavTab(
                    LauncherTab::WhdloadLibrary
                ))),
                Dir::Up
            ),
            at(UiControl::LauncherModel(MachineModel::A3000)),
            "up from the sibling row reaches the profile above it"
        );
    }

    use crate::video::launcher::LauncherField;

    fn item(control: UiControl, x: usize, y: usize, w: usize, h: usize) -> NavItem {
        NavItem {
            target: NavTarget::Ui(control),
            rect: Rect { x, y, w, h },
        }
    }

    fn ui(control: UiControl) -> NavTarget {
        NavTarget::Ui(control)
    }

    /// A walk down a column of settings stays in its column, even when
    /// something in the next column sits closer as the crow flies.
    #[test]
    fn a_walk_down_a_column_stays_in_it() {
        let tab = UiControl::LauncherTab(crate::video::launcher::LauncherTab::System);
        let rows: Vec<NavItem> = vec![
            item(
                UiControl::LauncherToggle(LauncherField::FloppyVolume),
                200,
                100,
                90,
                12,
            ),
            item(
                UiControl::LauncherToggle(LauncherField::MouseSensitivity),
                200,
                130,
                90,
                12,
            ),
            // Nearer in a straight line, but off to one side -- and
            // sharing the row, as a category button beside a setting
            // does.
            item(tab, 120, 100, 40, 12),
        ];
        let from = rows[0].target;
        assert_eq!(
            step(&rows, Some(from), Dir::Down),
            Some(rows[1].target),
            "down takes the row below, not the nearer neighbour beside it"
        );
        assert_eq!(
            step(&rows, Some(from), Dir::Left),
            Some(ui(tab)),
            "and left crosses to it"
        );
        assert_eq!(step(&rows, Some(from), Dir::Up), None, "nothing above");
    }

    /// A stepper's two arrows are one place to stand: the focus belongs
    /// to the setting, and its ends are what opening it works.
    #[test]
    fn a_steppers_arrows_are_one_place() {
        let back = UiControl::LauncherCycle {
            field: LauncherField::FloppyVolume,
            forward: false,
        };
        let forward = UiControl::LauncherCycle {
            field: LauncherField::FloppyVolume,
            forward: true,
        };
        let rows = vec![
            item(back, 200, 100, 12, 12),
            item(forward, 260, 100, 12, 12),
        ];
        assert!(
            find(&rows, ui(forward)).is_some_and(|i| i.target == ui(back)),
            "either end finds the same place"
        );
        assert_eq!(
            step(&rows, Some(ui(back)), Dir::Right),
            None,
            "and right does not walk from one end of it to the other"
        );
        assert_eq!(stepper_arrow(ui(back), Dir::Right), Some(ui(forward)));
        assert_eq!(stepper_arrow(ui(back), Dir::Left), Some(ui(back)));
        assert!(is_stepper(ui(back)) && !is_stepper(ui(UiControl::PanelClose)));
    }

    /// The body of a panel is not somewhere the focus can stand. It
    /// answers for every point the panel does not otherwise use, so
    /// taking it for a control drew the line around the whole window
    /// and left nothing to press.
    #[test]
    fn the_panel_body_is_not_a_place_to_stand() {
        let ui = UiState {
            menu_open: false,
            menu_rows: Vec::new(),
            menu_nav: crate::video::menu::MenuNav::default(),
            panel: Some(crate::video::ui::Panel::About),
        };
        let items = map(&ui, 720, 300);
        assert!(
            items
                .iter()
                .any(|i| i.target == NavTarget::Ui(UiControl::PanelClose)),
            "the close gadget is reachable"
        );
        assert!(
            !items
                .iter()
                .any(|i| i.target == NavTarget::Ui(UiControl::PanelBody)),
            "and the body it sits on is not"
        );
    }

    /// The focus survives a page it still exists on, and moves home when
    /// it does not.
    #[test]
    fn the_focus_settles_when_the_page_changes() {
        let close = UiControl::PanelClose;
        let body = UiControl::PanelBody;
        let mut nav = Nav::default();
        nav.show(Some(ui(close)));
        assert_eq!(nav.shown(), Some(ui(close)));
        nav.settle(&[item(close, 0, 0, 8, 8)], None);
        assert_eq!(nav.focus(), Some(ui(close)), "still there, still focused");
        nav.settle(&[item(body, 0, 0, 8, 8)], None);
        assert_eq!(nav.focus(), Some(ui(body)), "gone: the focus goes home");
        // The pointer puts the line out but remembers the place.
        nav.follow_pointer(ui(close));
        assert_eq!(nav.shown(), None);
        assert_eq!(nav.focus(), Some(ui(close)));
    }
}
