// SPDX-License-Identifier: GPL-3.0-or-later

//! Game-library list and artwork rendering.

use super::*;

#[cfg(feature = "game-library")]
pub(in crate::video::ui) fn draw_library_page(
    frame: &mut [u8],
    rect: Rect,
    state: &LauncherState,
    hover: Option<UiControl>,
    scale: usize,
) {
    let entries = state.library.games.entries();
    let whdload_entry = state.setup.whdload_enabled();
    let games = library_table_rect(rect, whdload_entry);
    // A scan running greys both buttons: neither of them can start a
    // second one while the first is going.
    let busy = matches!(
        state.status.as_ref().map(|s| s.kind),
        Some(launcher::StatusKind::Busy)
    );

    // The shortcut row shares the label's line, so it costs no height.
    if entries.len() >= LIBRARY_AZ_MIN_GAMES {
        let present = state.az_buckets_present();
        for (bucket, at) in library_az_rects(rect, whdload_entry)
            .into_iter()
            .enumerate()
        {
            let live = present.get(bucket).copied().unwrap_or(false);
            let hovered = if live {
                lit(hover, UiControl::LauncherLibraryJump(bucket))
            } else {
                0.0
            };
            draw_az_button(frame, at, launcher::az_label(bucket), live, hovered, scale);
        }
    }

    draw_panel_text(
        frame,
        games.x,
        games.y.saturating_sub(14),
        "Games:",
        PANEL_TEXT_DIM,
        1,
        scale,
    );
    draw_library_box(frame, games, scale);
    for (at, title) in [
        (LIBRARY_COL_NAME, "Game"),
        (library_col_favourite(rect, whdload_entry), "Favourite"),
    ] {
        draw_panel_text(
            frame,
            games.x + 4 + at,
            games.y + 5,
            title,
            PANEL_TEXT_DIM,
            1,
            scale,
        );
    }

    if entries.is_empty() {
        // What is wrong, then what to do about it, broken where it reads:
        // the launcher panel is a fixed size, so these lines are the same
        // lines on every machine. Each is still put through the wrap, which
        // does nothing while they fit and catches them if the box is ever
        // made narrower than the words in it.
        let lines = [
            "No games found!",
            "",
            "Update the \"Game library\" directory",
            "under WHDLoad -> Settings...",
        ];
        let lines: Vec<String> = lines
            .into_iter()
            .flat_map(|line| match line.is_empty() {
                true => vec![String::new()],
                false => wrap_balanced(line, games.w.saturating_sub(16)),
            })
            .collect();
        for (line, text) in lines.into_iter().enumerate() {
            if text.is_empty() {
                continue;
            }
            draw_panel_text(
                frame,
                games.x + 8,
                games.y + LIBRARY_HEADER_H + 6 + line * 14,
                &text,
                PANEL_TEXT_DIM,
                1,
                scale,
            );
        }
    }

    for drawn in 0..library_visible_rows(rect, whdload_entry) {
        let Some(entry) = entries.get(state.library.scroll + drawn) else {
            break;
        };
        let row = library_row_rect(rect, whdload_entry, drawn);
        let chosen = state.library.focus == launcher::LibraryFocus::Games
            && state.library.scroll + drawn == state.library.selected;
        if chosen {
            fill_rect(frame, scale_rect(row, scale), MENU_HILIGHT_BG, scale);
        } else if let Some(face) =
            row_light(ENTRY_BG, lit(hover, UiControl::LauncherLibraryPick(drawn)))
        {
            fill_rect(frame, scale_rect(row, scale), face, scale);
        }
        let colour = if chosen {
            MENU_HILIGHT_TEXT
        } else {
            PANEL_TEXT
        };
        // Clipped at the Favourite column, so a long title stops rather
        // than running under the tick.
        draw_panel_text(
            frame,
            row.x + 4 + LIBRARY_COL_NAME,
            row.y + 3,
            &truncate_to_width(
                entry.title(),
                library_col_favourite(rect, whdload_entry).saturating_sub(LIBRARY_COL_NAME + 12),
            ),
            colour,
            1,
            scale,
        );
        let tick = library_favourite_box(rect, whdload_entry, drawn);
        draw_tick_box(
            frame,
            tick.x,
            tick.y,
            state.library.db.is_favourite(&entry.relative),
            TICK_GREEN,
            scale,
        );
        if let Some(edge) = tick_outline(lit(hover, UiControl::LauncherLibraryFavourite(drawn))) {
            draw_outline(frame, tick, edge, scale);
        }
    }

    let visible = library_visible_rows(rect, whdload_entry);
    if entries.len() > visible {
        for (control, at) in library_arrow_rects(rect, whdload_entry) {
            let up = matches!(control, UiControl::LauncherLibraryScroll(d) if d < 0);
            let live = match up {
                true => state.library.scroll > 0,
                false => state.library.scroll + visible < entries.len(),
            };
            draw_scroll_arrow(frame, at, up, live, lit(hover, control), scale);
        }
    }

    // The favourites, which are the same games under a shorter heading.
    let favourites = library_favourites_rect(rect, whdload_entry);
    draw_panel_text(
        frame,
        favourites.x,
        favourites.y.saturating_sub(14),
        "Favourites:",
        PANEL_TEXT_DIM,
        1,
        scale,
    );
    draw_library_box(frame, favourites, scale);
    for (x, title) in [
        (favourites.x + 4 + LIBRARY_COL_NAME, "Game"),
        (library_remove_heading_x(rect, whdload_entry), "Remove"),
    ] {
        draw_panel_text(frame, x, favourites.y + 5, title, PANEL_TEXT_DIM, 1, scale);
    }
    // From the database rather than from the library, so a favourite whose
    // package has been deleted is still listed -- and can still be taken
    // off, which is most of the reason its Remove tick is there.
    let favourite_rows = library_favourite_rows(rect, whdload_entry);
    for (drawn, (key, name)) in state
        .library
        .db
        .favourites()
        .skip(state.library.favourite_scroll)
        .take(favourite_rows)
        .enumerate()
    {
        let row = library_favourite_row_rect(rect, whdload_entry, drawn);
        let chosen = state.library.focus == launcher::LibraryFocus::Favourites
            && state.library.favourite_scroll + drawn == state.library.favourite_selected;
        if chosen {
            fill_rect(frame, scale_rect(row, scale), MENU_HILIGHT_BG, scale);
        } else if let Some(face) = row_light(
            ENTRY_BG,
            lit(hover, UiControl::LauncherLibraryFavouritePick(drawn)),
        ) {
            fill_rect(frame, scale_rect(row, scale), face, scale);
        }
        // One no longer in the library is dimmed: still listed, still
        // removable, but there is nothing to launch.
        let present = entries.iter().any(|entry| entry.relative == key);
        let colour = match (chosen, present) {
            (true, _) => MENU_HILIGHT_TEXT,
            (false, true) => PANEL_TEXT,
            (false, false) => PANEL_TEXT_DIM,
        };
        draw_panel_text(
            frame,
            row.x + 4 + LIBRARY_COL_NAME,
            row.y + 3,
            &truncate_to_width(
                name,
                library_col_favourite(rect, whdload_entry).saturating_sub(LIBRARY_COL_NAME + 12),
            ),
            colour,
            1,
            scale,
        );
        let tick = library_remove_box(rect, whdload_entry, drawn);
        draw_tick_box(frame, tick.x, tick.y, false, TICK_GREEN, scale);
        if let Some(edge) =
            tick_outline(lit(hover, UiControl::LauncherLibraryFavouriteRemove(drawn)))
        {
            draw_outline(frame, tick, edge, scale);
        }
    }

    let starred = state.library.db.favourite_count();
    if starred > favourite_rows {
        for (control, at) in library_favourite_arrow_rects(rect, whdload_entry) {
            let up = matches!(control, UiControl::LauncherLibraryFavouriteScroll(d) if d < 0);
            let live = match up {
                true => state.library.favourite_scroll > 0,
                false => state.library.favourite_scroll + favourite_rows < starred,
            };
            draw_scroll_arrow(frame, at, up, live, lit(hover, control), scale);
        }
    }

    draw_library_cover(frame, rect, state, scale);

    // The two buttons that say when work happens, in the gap between the
    // lists. A third slot is left beside them: the row is sized for three
    // so gaining one later does not move the two that are here.
    let buttons = library_button_rects(rect, whdload_entry);
    for (at, (label, control, enabled)) in [
        ("Refresh", UiControl::LauncherLibraryRefresh, !busy),
        // Nothing to look up until the folder has been read, so Scan waits
        // for a Refresh that found something.
        (
            "Scan",
            UiControl::LauncherLibraryUpdate,
            !busy && !entries.is_empty(),
        ),
        (
            "Update",
            UiControl::LauncherLibraryEdit,
            !busy && state.library_selection().is_some(),
        ),
    ]
    .into_iter()
    .enumerate()
    {
        draw_text_button(
            frame,
            buttons[at],
            label,
            enabled,
            lit(hover, control),
            scale,
        );
    }
}

/// A list box, sunk into the panel like an entry field so it reads as
/// something to look into rather than a raised control.
#[cfg(feature = "game-library")]
pub(in crate::video::ui) fn draw_library_box(frame: &mut [u8], at: Rect, scale: usize) {
    fill_rect(frame, scale_rect(at, scale), ENTRY_BG, scale);
    draw_outline(frame, at, BUTTON_EDGE_LIGHT, scale);
    draw_rect_bevel(
        frame,
        scale_rect(
            Rect {
                x: at.x + 1,
                y: at.y + 1,
                w: at.w.saturating_sub(2),
                h: at.h.saturating_sub(2),
            },
            scale,
        ),
        BUTTON_EDGE_DARK,
        BUTTON_EDGE_LIGHT,
        scale,
    );
}

/// The cover art box, and what the database says under it.
#[cfg(feature = "game-library")]
pub(in crate::video::ui) fn draw_library_cover(
    frame: &mut [u8],
    rect: Rect,
    state: &LauncherState,
    scale: usize,
) {
    let whdload_entry = state.setup.whdload_enabled();
    // The frame is the size the layout reserves, whatever shape the picture
    // in it turns out to be: it is where the eye expects the art to be, and
    // the writing under it starts on the same line for every game.
    // Amiga box art is portrait almost without exception, so the frame is
    // cut for that and the rare landscape scan is letterboxed into it --
    // black above and below beats a frame that changes shape and drags the
    // metadata down the page with it.
    let widest = library_cover_rect(rect, whdload_entry);
    let entry = state.library_selection();
    let art = entry
        .and_then(|entry| entry.game.as_ref())
        .and_then(|game| game.front_sha1.as_deref())
        .and_then(|sha1| state.library.covers.get(sha1));
    let (frame_rect, box_rect) = (widest, library_art_rect(rect, whdload_entry));

    // The mount: a button-faced border raised out of the panel, with the
    // picture recessed into it. Two bevels facing opposite ways is what
    // makes the frame read as having thickness.
    fill_rect(frame, scale_rect(frame_rect, scale), BUTTON_FACE, scale);
    draw_rect_bevel(
        frame,
        scale_rect(frame_rect, scale),
        BUTTON_EDGE_LIGHT,
        BUTTON_EDGE_DARK,
        scale,
    );
    fill_rect(frame, scale_rect(box_rect, scale), ENTRY_BG, scale);
    if let Some(art) = art {
        draw_cover_art(frame, box_rect, art, scale);
    }
    draw_rect_bevel(
        frame,
        scale_rect(box_rect, scale),
        BUTTON_EDGE_DARK,
        BUTTON_EDGE_LIGHT,
        scale,
    );

    let Some(entry) = entry else {
        return;
    };
    // Two different nothings: a package the catalogue has never heard of,
    // and one it knows but has no picture for.
    let missing: &[&str] = match (&entry.game, art.is_some()) {
        (_, true) => &[],
        (None, _) => &["not in the", "database"],
        (Some(_), _) => &["No cover art"],
    };
    {
        for (line, text) in missing.iter().copied().enumerate() {
            let w = text.chars().count() * font::GLYPH_W;
            draw_panel_text(
                frame,
                box_rect.x + box_rect.w.saturating_sub(w) / 2,
                box_rect.y + box_rect.h / 2 - 8 * missing.len() + line * 12,
                text,
                PANEL_TEXT_DIM,
                1,
                scale,
            );
        }
    }

    // Under the art: what the database knows, each label dimmed above its
    // value, and a value too long for the column wrapped rather than cut.
    // It starts under the frame, which is one size for every game, so it
    // starts on the same line each time whatever shape the picture is.
    // The block stops above the action bar: a developer credited to nine
    // people would otherwise run down over the Run button and off the
    // panel. Each value is held to two lines as well, so one long field
    // cannot crowd out the ones under it -- what is cut is only what is
    // drawn, never what is stored.
    let floor = launcher_action_y(rect).saturating_sub(6);
    let mut y = widest.y + widest.h + 8;
    let game = entry.game.as_ref();
    let mut show = |label: &str, value: Option<&str>, y: &mut usize| {
        let Some(value) = value.filter(|v| !v.is_empty()) else {
            return;
        };
        // Label and one line at least, or there is no point starting.
        if *y + 24 > floor {
            return;
        }
        draw_panel_text(frame, widest.x, *y, label, PANEL_TEXT_DIM, 1, scale);
        *y += 12;
        let mut lines = wrap_to_width(value, widest.w);
        let over = lines.len() > LIBRARY_FIELD_LINES;
        lines.truncate(LIBRARY_FIELD_LINES);
        let last = lines.len().saturating_sub(1);
        for (at, line) in lines.into_iter().enumerate() {
            if *y + 12 > floor {
                break;
            }
            // The panel marks a cut with a tilde, so a credit that goes
            // on does not read as one that stopped. Room is made for the
            // mark rather than hoping the line is short enough.
            let line = match over && at == last {
                true => {
                    let mut cut = line;
                    while cut.chars().count() * font::GLYPH_W + font::GLYPH_W > widest.w {
                        cut.pop();
                    }
                    format!("{cut}~")
                }
                false => line,
            };
            draw_panel_text(frame, widest.x, *y, &line, PANEL_TEXT, 1, scale);
            *y += 12;
        }
        *y += 4;
    };
    show("Year", game.and_then(|g| g.year.as_deref()), &mut y);
    show(
        "Publisher",
        game.and_then(|g| g.publisher.as_deref()),
        &mut y,
    );
    show(
        "Developer",
        game.and_then(|g| g.developer.as_deref()),
        &mut y,
    );
    show("Players", game.and_then(|g| g.players.as_deref()), &mut y);

    // Which release this is. Shown only when there is something to say:
    // what somebody typed, or -- where the library holds this game under
    // one title more than once -- the package's own name, since nothing
    // else separates `CannonFodder2_v1.11_0104` from `_v1.12_Fr_2578`.
    // Without the extension: it is the same on both and says nothing about
    // which release either is.
    //
    // A game held once and never edited has no version and no row, and
    // neither has one the catalogue has never heard of -- two packages the
    // scan could not name are two rows that say nothing already, and a
    // file name under them is not the answer to which release they are.
    let version = game
        .and_then(|g| g.version.as_deref())
        .filter(|v| !v.is_empty())
        .or_else(|| (entry.duplicated && game.is_some()).then_some(entry.file_name.as_str()));
    if let Some(version) = version.filter(|_| y + 24 <= floor) {
        draw_panel_text(frame, widest.x, y, "Version", PANEL_TEXT_DIM, 1, scale);
        y += 12;
        // Two lines, because a package name is longer than the column and
        // both ends of it matter: `CannonFodder2_v1.11_0104.lha` says
        // which game at the front and which release at the back. Anything
        // past that is cut, which nothing typed here should reach -- the
        // editor stops at what these two lines hold.
        for line in wrap_to_width(version, widest.w)
            .into_iter()
            .take(LIBRARY_VERSION_LINES)
        {
            if y + 12 > floor {
                break;
            }
            draw_panel_text(frame, widest.x, y, &line, PANEL_TEXT, 1, scale);
            y += 12;
        }
    }
}

/// Draw a cover into `into`, scaled to fit and centred, keeping its shape.
///
/// Nearest-neighbour, like everything else the panel draws: the launcher
/// renders at one scale and is blown up whole, so smoothing here would be
/// undone by the magnification above it anyway.
#[cfg(feature = "game-library")]
pub(in crate::video::ui) fn draw_cover_art(
    frame: &mut [u8],
    into: Rect,
    art: &crate::gamelib::cover::Image,
    scale: usize,
) {
    let Some(at) = fit_within(art.width, art.height, into) else {
        return;
    };
    for y in 0..at.h {
        let from_y = y * art.height / at.h;
        for x in 0..at.w {
            let from = (from_y * art.width + x * art.width / at.w) * 4;
            let Some(px) = art.pixels.get(from..from + 4) else {
                continue;
            };
            // Drawn opaque: cover art has no transparency worth honouring,
            // and one that does reads better over the box's own fill.
            let colour = rgba(px[0] as u32, px[1] as u32, px[2] as u32);
            fill_rect(
                frame,
                scale_rect(
                    Rect {
                        x: at.x + x,
                        y: at.y + y,
                        w: 1,
                        h: 1,
                    },
                    scale,
                ),
                colour,
                scale,
            );
        }
    }
}

/// The largest rectangle of `w` by `h`'s shape that fits inside `into`,
/// centred. `None` if either is empty, which is nothing to draw.
///
/// Covers are portrait and the box is close to square, so a picture drawn
/// to the box's own shape would be visibly stretched.
#[cfg(feature = "game-library")]
pub(in crate::video::ui) fn fit_within(w: usize, h: usize, into: Rect) -> Option<Rect> {
    if w == 0 || h == 0 || into.w == 0 || into.h == 0 {
        return None;
    }
    // Whichever side runs out first sets the scale; the other is left a
    // margin, split evenly.
    let (fit_w, fit_h) = if w * into.h >= h * into.w {
        (into.w, (into.w * h / w).clamp(1, into.h))
    } else {
        ((into.h * w / h).clamp(1, into.w), into.h)
    };
    Some(Rect {
        x: into.x + (into.w - fit_w) / 2,
        y: into.y + (into.h - fit_h) / 2,
        w: fit_w,
        h: fit_h,
    })
}

/// The same as [`wrap_to_width`], but with the lines evened up.
///
/// A greedy wrap fills each line to the brim and leaves whatever is left
/// on the last one, which for a sentence a little wider than its box means
/// a full line and a single trailing word. It takes the same number of
/// lines to say it with the break in a sensible place, so the column is
/// narrowed a character at a time for as long as the line count holds.
#[cfg(feature = "game-library")]
pub(in crate::video::ui) fn wrap_balanced(text: &str, width: usize) -> Vec<String> {
    let mut best = wrap_to_width(text, width);
    if best.len() < 2 {
        return best;
    }
    let lines = best.len();
    let mut narrow = width;
    while narrow > font::GLYPH_W {
        narrow -= font::GLYPH_W;
        let tried = wrap_to_width(text, narrow);
        if tried.len() != lines {
            break;
        }
        best = tried;
    }
    best
}

/// Break text into lines that fit `width`, at spaces where there are any.
/// A single word longer than the column is broken across lines rather than
/// left to run off the panel.
#[cfg(feature = "game-library")]
pub(in crate::video::ui) fn wrap_to_width(text: &str, width: usize) -> Vec<String> {
    let per_line = (width / font::GLYPH_W).max(1);
    let mut lines = Vec::new();
    let mut line = String::new();
    for word in text.split_whitespace() {
        let would_be = if line.is_empty() {
            word.chars().count()
        } else {
            line.chars().count() + 1 + word.chars().count()
        };
        if would_be > per_line && !line.is_empty() {
            lines.push(std::mem::take(&mut line));
        }
        if word.chars().count() > per_line {
            // Nothing to break at, so it is broken anyway -- across as
            // many lines as it needs. A package name is one long word and
            // taking only its first line would drop the part that says
            // which release it is.
            if !line.is_empty() {
                lines.push(std::mem::take(&mut line));
            }
            let mut rest: Vec<char> = word.chars().collect();
            while rest.len() > per_line {
                lines.push(rest.drain(..per_line).collect());
            }
            line = rest.into_iter().collect();
            continue;
        }
        if !line.is_empty() {
            line.push(' ');
        }
        line.push_str(word);
    }
    if !line.is_empty() {
        lines.push(line);
    }
    lines
}
