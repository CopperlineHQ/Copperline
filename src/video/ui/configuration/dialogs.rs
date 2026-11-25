// SPDX-License-Identifier: GPL-3.0-or-later

//! Modal dialogs over the configuration panel.

use super::*;

/// The "are you sure" over Reset default, centred on the panel.
pub(in crate::video::ui) fn launcher_confirm_rect(rect: Rect) -> Rect {
    // Its own width, which its title bar and two buttons decide, but the
    // Save dialog's height exactly: they are the same window asking two
    // things, and one being shorter than the other made them look like two
    // unrelated boxes that happened to open in the same place.
    let (w, h) = (268, launcher_save_dialog_rect(rect).h);
    Rect {
        x: rect.x + rect.w.saturating_sub(w) / 2,
        y: rect.y + rect.h.saturating_sub(h) / 2,
        w,
        h,
    }
}

/// Its two buttons, as (yes, cancel). Cancel is the rightmost, where a
/// dialog's least destructive answer usually sits.
pub(in crate::video::ui) fn launcher_confirm_button_rects(rect: Rect) -> (Rect, Rect) {
    let dialog = launcher_confirm_rect(rect);
    let (w, h) = (66, SAVE_DIALOG_BUTTON.1);
    let y = dialog.y + dialog.h - SAVE_DIALOG_MARGIN - h;
    (
        Rect {
            x: dialog.x + dialog.w - 2 * w - 20,
            y,
            w,
            h,
        },
        Rect {
            x: dialog.x + dialog.w - w - 12,
            y,
            w,
            h,
        },
    )
}

pub(in crate::video::ui) fn draw_launcher_confirm(
    canvas: &mut dyn Canvas,
    rect: Rect,
    state: &LauncherState,
    hover: Option<UiControl>,
) {
    if !state.confirm_reset {
        return;
    }
    canvas.blend(rect, SCRIM, SCRIM_ALPHA);
    let dialog = launcher_confirm_rect(rect);
    canvas.fill(dialog, PANEL_BG);
    canvas.bevel(dialog, BUTTON_EDGE_LIGHT, BUTTON_EDGE_DARK);
    title_bar(
        canvas,
        dialog,
        "Reset default",
        lit(hover, UiControl::LauncherDialogClose),
    );
    // The title bar has already said which default, and the buttons say
    // what the answers are. Anything more here is a paragraph nobody
    // reads standing between somebody and a decision they have made.
    canvas.text(
        dialog.x + SAVE_DIALOG_MARGIN,
        dialog.y + TITLE_H + SAVE_DIALOG_MARGIN,
        "Are you sure?",
        PANEL_TEXT,
        1,
    );
    let (yes, cancel) = launcher_confirm_button_rects(rect);
    text_button(
        canvas,
        yes,
        "Yes",
        true,
        lit(hover, UiControl::LauncherConfirmReset),
    );
    text_button(
        canvas,
        cancel,
        "Cancel",
        true,
        lit(hover, UiControl::LauncherCancelReset),
    );
}

/// The Save dialog, centred on the panel like the confirm.
pub(in crate::video::ui) fn launcher_save_dialog_rect(rect: Rect) -> Rect {
    let (bw, bh) = SAVE_DIALOG_BUTTON;
    let buttons = SAVE_ACTIONS.len();
    let (w, h) = (
        2 * SAVE_DIALOG_MARGIN + buttons * bw + (buttons - 1) * SAVE_DIALOG_GAP,
        TITLE_H
            + 2 * SAVE_DIALOG_MARGIN
            + SAVE_DIALOG_HELP_LINES * SAVE_DIALOG_LINE_H
            + SAVE_DIALOG_HELP_GAP
            + bh,
    );
    Rect {
        x: rect.x + rect.w.saturating_sub(w) / 2,
        y: rect.y + rect.h.saturating_sub(h) / 2,
        w,
        h,
    }
}

/// The buttons in it.
pub(in crate::video::ui) fn launcher_save_dialog_rects(
    rect: Rect,
) -> [(UiControl, Rect); SAVE_ACTIONS.len()] {
    let dialog = launcher_save_dialog_rect(rect);
    let (w, h) = SAVE_DIALOG_BUTTON;
    std::array::from_fn(|i| {
        let item = Rect {
            x: dialog.x + SAVE_DIALOG_MARGIN + i * (w + SAVE_DIALOG_GAP),
            // Along the bottom, under the line that says what they do.
            y: dialog.y + dialog.h - SAVE_DIALOG_MARGIN - h,
            w,
            h,
        };
        (SAVE_ACTIONS[i], item)
    })
}

pub(in crate::video::ui) fn draw_launcher_save_dialog(
    canvas: &mut dyn Canvas,
    rect: Rect,
    state: &LauncherState,
    hover: Option<UiControl>,
) {
    if !state.save_dialog {
        return;
    }
    canvas.blend(rect, SCRIM, SCRIM_ALPHA);
    let dialog = launcher_save_dialog_rect(rect);
    canvas.fill(dialog, PANEL_BG);
    canvas.bevel(dialog, BUTTON_EDGE_LIGHT, BUTTON_EDGE_DARK);
    // The close gadget in its title bar is how this is dismissed. There is
    // no Cancel among the four because none of them answers a question --
    // they are four things you might do, and not doing any of them is
    // closing the window rather than choosing a fifth.
    title_bar(
        canvas,
        dialog,
        "Save configuration...",
        lit(hover, UiControl::LauncherDialogClose),
    );
    for (control, item) in launcher_save_dialog_rects(rect) {
        text_button(
            canvas,
            item,
            launcher_action_label(control),
            true,
            lit(hover, control),
        );
    }
    // Above the row, where a dialog's own words go, and never blank: with
    // neither hand on any of the four it says what the dialog is for.
    // The marker is asked first, as the lighting asks it -- what the
    // keyboard is standing on is what the line should be about.
    let chars = (dialog.w - 2 * SAVE_DIALOG_MARGIN) / font::GLYPH_W;
    let help = save_dialog_help(
        nav_target().or(hover).unwrap_or(UiControl::LauncherSaveAs),
        state.config_file.as_deref(),
        chars,
    );
    for (i, line) in wrap_text(&help, chars, chars)
        .into_iter()
        .take(SAVE_DIALOG_HELP_LINES)
        .enumerate()
    {
        canvas.text(
            dialog.x + SAVE_DIALOG_MARGIN,
            dialog.y + TITLE_H + SAVE_DIALOG_MARGIN + i * SAVE_DIALOG_LINE_H,
            &line,
            PANEL_TEXT,
            1,
        );
    }
}

/// Hit-test the Save dialog, which is over everything while it is up. A
/// click anywhere else -- its close gadget, its own frame, the panel
/// behind it -- puts it away without doing anything, so it can never be a
/// mode you are stuck in.
pub(in crate::video::ui) fn launcher_save_dialog_hit(
    rect: Rect,
    pos: (i32, i32),
) -> Option<UiControl> {
    launcher_save_dialog_rects(rect)
        .into_iter()
        .find_map(|(control, item)| item.contains(pos).then_some(control))
}

/// The metadata editor.
#[cfg(feature = "game-library")]
pub(in crate::video::ui) fn draw_meta_dialog(
    canvas: &mut dyn Canvas,
    rect: Rect,
    state: &LauncherState,
    hover: Option<UiControl>,
) {
    let Some(meta) = &state.meta else { return };
    canvas.blend(rect, SCRIM, SCRIM_ALPHA);
    let dialog = meta_rect(rect);
    canvas.fill(dialog, PANEL_BG);
    canvas.bevel(dialog, BUTTON_EDGE_LIGHT, BUTTON_EDGE_DARK);
    title_bar(
        canvas,
        dialog,
        "Update metadata",
        lit(hover, UiControl::MetaCancel),
    );

    // The art, drawn the way the Library page draws it, and clickable.
    let art = meta_art_rect(rect);
    canvas.fill(art, BUTTON_FACE);
    canvas.bevel(art, BUTTON_EDGE_LIGHT, BUTTON_EDGE_DARK);
    let inner = Rect {
        x: art.x + LIBRARY_COVER_BEZEL,
        y: art.y + LIBRARY_COVER_BEZEL,
        w: art.w - 2 * LIBRARY_COVER_BEZEL,
        h: art.h - 2 * LIBRARY_COVER_BEZEL,
    };
    canvas.fill(inner, ENTRY_BG);
    let picture = meta
        .art
        .as_deref()
        .and_then(|key| state.library.covers.get(key));
    match picture {
        Some(picture) => draw_cover_art(canvas, inner, picture),
        None => {
            for (line, text) in ["Click to", "choose art"].into_iter().enumerate() {
                let w = text.len() * font::GLYPH_W;
                canvas.text(
                    inner.x + inner.w.saturating_sub(w) / 2,
                    inner.y + inner.h / 2 - 12 + line * 12,
                    text,
                    PANEL_TEXT_DIM,
                    1,
                );
            }
        }
    }
    canvas.bevel(inner, BUTTON_EDGE_DARK, BUTTON_EDGE_LIGHT);
    // The same green edge a tick box takes, breathing under the focus:
    // the art is answered by choosing a picture, so it is a thing to
    // press rather than a value to change.
    if let Some(edge) = tick_outline(lit(hover, UiControl::MetaArt)) {
        outline(canvas, art, edge);
    }

    for field in launcher::MetaField::ALL {
        let box_rect = meta_field_rect(rect, field);
        canvas.text(
            art.x + art.w + 12,
            box_rect.y + 5,
            field.label(),
            PANEL_TEXT_DIM,
            1,
        );
        canvas.fill(box_rect, ENTRY_BG);
        outline(
            canvas,
            box_rect,
            if meta.focus == field {
                PANEL_TEXT_HILIGHT
            } else {
                BUTTON_EDGE_DARK
            },
        );
        // The focused box carries the caret, and the window on the text
        // follows it: metadata is amended more often than typed fresh, so
        // the middle of a value has to be reachable.
        let value = meta.value(field);
        if meta.focus == field {
            draw_edit_line(
                canvas,
                box_rect.x + 5,
                box_rect.y + 5,
                value,
                meta.caret.at(),
                PANEL_TEXT,
                ENTRY_BG,
                box_rect.w.saturating_sub(10),
            );
        } else {
            canvas.text(
                box_rect.x + 5,
                box_rect.y + 5,
                &truncate_to_width(value, box_rect.w.saturating_sub(10)),
                PANEL_TEXT,
                1,
            );
        }
    }

    for (at, (label, control)) in [
        ("Save", UiControl::MetaSave),
        ("Clear", UiControl::MetaClear),
        ("Cancel", UiControl::MetaCancel),
    ]
    .into_iter()
    .enumerate()
    {
        text_button(
            canvas,
            meta_button_rects(rect)[at],
            label,
            true,
            lit(hover, control),
        );
    }
}

/// The OpenRetro sign-in dialog.
///
/// The password is drawn as a run of asterisks, one per character typed:
/// the [`crate::gamelib::Secret`] behind it is never turned into display
/// text, so what is on screen cannot be a second copy of it.
#[cfg(feature = "game-library")]
pub(in crate::video::ui) fn draw_login_dialog(
    canvas: &mut dyn Canvas,
    rect: Rect,
    state: &LauncherState,
    hover: Option<UiControl>,
) {
    use launcher::LoginField;
    let Some(login) = &state.login else { return };
    // Everything behind it is dimmed rather than merely covered: a dialog
    // that only overlaps the page still looks like part of it.
    canvas.blend(rect, SCRIM, SCRIM_ALPHA);
    let dialog = login_rect(rect);
    canvas.fill(dialog, PANEL_BG);
    canvas.bevel(dialog, BUTTON_EDGE_LIGHT, BUTTON_EDGE_DARK);
    // Its own title bar, the same as the panel's: a window over a window
    // should look like one, close gadget included.
    title_bar(
        canvas,
        dialog,
        "Log in to OpenRetro",
        lit(hover, UiControl::LoginCancel),
    );
    for field in [LoginField::User, LoginField::Pass] {
        let box_rect = login_field_rect(rect, field);
        let (label, shown) = match field {
            LoginField::User => ("Username", login.user.clone()),
            LoginField::Pass => ("Password", "*".repeat(login.pass.chars())),
        };
        canvas.text(dialog.x + 12, box_rect.y + 5, label, PANEL_TEXT_DIM, 1);
        canvas.fill(box_rect, ENTRY_BG);
        outline(
            canvas,
            box_rect,
            if login.focus == field {
                PANEL_TEXT_HILIGHT
            } else {
                BUTTON_EDGE_DARK
            },
        );
        // The focused box carries the caret, and the window on the text
        // follows it. The mask is one asterisk a character, so the caret
        // steps through a password exactly as it does through a name.
        if login.focus == field {
            draw_edit_line(
                canvas,
                box_rect.x + 5,
                box_rect.y + 5,
                &shown,
                login.caret.at(),
                PANEL_TEXT,
                ENTRY_BG,
                box_rect.w.saturating_sub(10),
            );
        } else {
            canvas.text(
                box_rect.x + 5,
                box_rect.y + 5,
                &truncate_to_width(&shown, box_rect.w.saturating_sub(10)),
                PANEL_TEXT,
                1,
            );
        }
    }
    let (ok, cancel) = login_button_rects(rect);
    text_button(
        canvas,
        ok,
        "OK",
        !login.sending,
        lit(hover, UiControl::LoginOk),
    );
    text_button(
        canvas,
        cancel,
        "Cancel",
        true,
        lit(hover, UiControl::LoginCancel),
    );
}

/// The metadata editor: the art on the left at the shape a cover is, the
/// fields down the right, the buttons along the bottom.
#[cfg(feature = "game-library")]
pub(in crate::video::ui) fn meta_rect(rect: Rect) -> Rect {
    let (w, h) = (440, TITLE_H + META_ART.1 + 56);
    Rect {
        x: rect.x + rect.w.saturating_sub(w) / 2,
        y: rect.y + rect.h.saturating_sub(h) / 2,
        w,
        h,
    }
}

#[cfg(feature = "game-library")]
pub(in crate::video::ui) fn meta_field_rect(rect: Rect, field: launcher::MetaField) -> Rect {
    let dialog = meta_rect(rect);
    let art = meta_art_rect(rect);
    let label = 10 * font::GLYPH_W;
    let x = art.x + art.w + 12 + label;
    let at = launcher::MetaField::ALL
        .iter()
        .position(|&f| f == field)
        .unwrap_or(0);
    Rect {
        x,
        y: art.y + at * 24,
        w: (dialog.x + dialog.w).saturating_sub(x + 14),
        h: 18,
    }
}

#[cfg(feature = "game-library")]
pub(in crate::video::ui) fn meta_art_rect(rect: Rect) -> Rect {
    let dialog = meta_rect(rect);
    Rect {
        x: dialog.x + 14,
        y: dialog.y + TITLE_H + 14,
        w: META_ART.0,
        h: META_ART.1,
    }
}

/// Save, Clear and Cancel, in that order.
#[cfg(feature = "game-library")]
pub(in crate::video::ui) fn meta_button_rects(rect: Rect) -> [Rect; 3] {
    let dialog = meta_rect(rect);
    let (w, h, gap) = (66, 20, 8);
    let y = dialog.y + dialog.h - h - 12;
    std::array::from_fn(|i| Rect {
        x: dialog.x + dialog.w - 14 - (3 - i) * (w + gap) + gap,
        y,
        w,
        h,
    })
}

/// The sign-in dialog: a small box in the middle of the panel.
#[cfg(feature = "game-library")]
pub(in crate::video::ui) fn login_rect(rect: Rect) -> Rect {
    // Wide enough that the title clears its own close gadget.
    let (w, h) = (380, 128 + TITLE_H);
    Rect {
        x: rect.x + rect.w.saturating_sub(w) / 2,
        y: rect.y + rect.h.saturating_sub(h) / 2,
        w,
        h,
    }
}

#[cfg(feature = "game-library")]
pub(in crate::video::ui) fn login_button_rects(rect: Rect) -> (Rect, Rect) {
    let dialog = login_rect(rect);
    let (w, h) = (66, 20);
    let y = dialog.y + dialog.h - h - 12;
    (
        Rect {
            x: dialog.x + dialog.w - 2 * w - 12 - 8,
            y,
            w,
            h,
        },
        Rect {
            x: dialog.x + dialog.w - w - 12,
            y,
            w,
            h,
        },
    )
}

/// Its two value boxes, and its two buttons.
#[cfg(feature = "game-library")]
pub(in crate::video::ui) fn login_field_rect(rect: Rect, field: launcher::LoginField) -> Rect {
    let dialog = login_rect(rect);
    let label = 10 * font::GLYPH_W;
    Rect {
        x: dialog.x + 12 + label,
        y: dialog.y + TITLE_H + 20 + usize::from(field == launcher::LoginField::Pass) * 26,
        w: dialog.w.saturating_sub(24 + label),
        h: 18,
    }
}
