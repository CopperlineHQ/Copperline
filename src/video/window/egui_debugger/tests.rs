// SPDX-License-Identifier: GPL-3.0-or-later

use super::super::tests::test_app;
use super::*;

fn input(size: [f32; 2], events: Vec<egui::Event>) -> egui::RawInput {
    egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(size[0], size[1]),
        )),
        events,
        focused: true,
        ..Default::default()
    }
}

fn key(key: egui::Key, modifiers: egui::Modifiers) -> egui::Event {
    egui::Event::Key {
        key,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers,
    }
}

#[test]
fn all_tabs_and_bitmap_inspection_leave_the_machine_byte_identical() {
    let mut app = test_app();
    let before = app.emu.machine_state_bytes().unwrap();
    let context = egui::Context::default();
    configure_style(&context);
    let mut layout = Layout::default();
    let mut panel = ui::DebuggerPanel::new();
    for tab in ui::DEBUG_TABS.into_iter().chain([ui::DebugTab::Memory]) {
        panel.mem_view_bits = tab == ui::DebugTab::Memory && panel.tab == ui::DebugTab::Waveform;
        panel.tab = tab;
        for size in [[600.0, 600.0], [1100.0, 760.0], [1600.0, 1000.0]] {
            let view = app.build_debugger_view_with_clipping(&panel, false);
            let (output, actions) = run_frame(
                &context,
                &mut layout,
                input(size, vec![]),
                &mut panel,
                &view,
            );
            assert!(
                actions.is_empty(),
                "inspection generated an action on {tab:?}"
            );
            assert!(!context
                .tessellate(output.shapes, output.pixels_per_point)
                .is_empty());
        }
    }
    assert_eq!(before, app.emu.machine_state_bytes().unwrap());
}

#[test]
fn audio_rows_and_mute_targets_stay_fixed_as_status_lines_change() {
    let mut app = test_app();
    let before = app.emu.machine_state_bytes().unwrap();
    let mut panel = ui::DebuggerPanel::new();
    panel.tab = ui::DebugTab::Audio;
    let mut view = app.build_debugger_view_with_clipping(&panel, false);
    for (kind, label) in [
        (ui::AudioExtraKind::Synth, "MIDI"),
        (ui::AudioExtraKind::Toccata, "Toccata"),
        (ui::AudioExtraKind::Mhi, "MHI"),
    ] {
        view.audio.as_mut().unwrap().extras.push(ui::AudioExtraRow {
            kind,
            row: ui::AudioRowView {
                text: vec![ui::DbgLine::plain(label), ui::DbgLine::plain("idle")],
                muted: false,
                scope: vec![0; 128],
            },
        });
    }
    for width in [600.0, 1100.0, 1600.0] {
        let context = egui::Context::default();
        configure_style(&context);
        let mut layout = Layout::default();
        let size = [width, 1200.0];
        let mut baseline = None;
        let mut mute_positions = Vec::new();
        for pending in [false, true, false, true] {
            let audio = view.audio.as_mut().unwrap();
            audio.header = if pending {
                "DMACON 820F  DMAEN on  AUDEN 1 1 1 1  ADKCON 00FF  USE0V1 USE1V2 USE2V3 USE3VN USE0P1 USE1P2 USE2P3 USE3PN"
            } else {
                "DMACON 0000  DMAEN off  AUDEN . . . .  ADKCON 0000"
            }.into();
            for row in &mut audio.channels {
                row.text.truncate(3);
                if pending {
                    row.text.push(ui::DbgLine::plain(
                        "  pending: intreq2 dma-req dma-req-latched",
                    ));
                }
                row.scope = if pending {
                    vec![-120, 100, -20, 60]
                } else {
                    vec![0; 128]
                };
            }
            audio.extras[0].row.text[0] = ui::DbgLine::hilit(if pending {
                "CD-DA playing track 12 position 100000/200000"
            } else {
                "CD-DA idle"
            });
            // Let scrolling/tessellation settle, then compare actual paint
            // geometry rather than a duplicate of the row-size calculation.
            for pass in 0..3 {
                let (output, actions) = run_frame(
                    &context,
                    &mut layout,
                    input(size, vec![]),
                    &mut panel,
                    &view,
                );
                assert!(actions.is_empty());
                let mut scopes = Vec::new();
                mute_positions.clear();
                for shape in output.shapes {
                    match shape.shape {
                        egui::Shape::Rect(rect) if rect.fill == Color32::from_gray(25) => {
                            scopes.push(rect.rect);
                        }
                        egui::Shape::Text(text) if text.galley.job.text == "Mute" => {
                            mute_positions.push(text.pos + text.galley.size() * 0.5);
                        }
                        _ => {}
                    }
                }
                if pass == 2 {
                    assert_eq!(scopes.len(), 8);
                    assert_eq!(mute_positions.len(), 8);
                    for (scope, mute) in scopes.iter().zip(&mute_positions) {
                        assert!(scope.right() <= width, "scope escaped the viewport");
                        assert!(scope.top() <= mute.y && mute.y < scope.bottom());
                    }
                    let geometry = (scopes, mute_positions.clone());
                    if let Some(baseline) = &baseline {
                        assert_eq!(
                            &geometry, baseline,
                            "status moved audio rows at width {width}"
                        );
                    } else {
                        baseline = Some(geometry);
                    }
                }
            }
        }
        for (index, pos) in mute_positions.into_iter().enumerate() {
            let mut clicked = Vec::new();
            for pressed in [true, false] {
                let (_, actions) = run_frame(
                    &context,
                    &mut layout,
                    input(
                        size,
                        vec![
                            egui::Event::PointerMoved(pos),
                            egui::Event::PointerButton {
                                pos,
                                button: egui::PointerButton::Primary,
                                pressed,
                                modifiers: egui::Modifiers::NONE,
                            },
                        ],
                    ),
                    &mut panel,
                    &view,
                );
                clicked.extend(actions);
            }
            assert_eq!(clicked, [Action::Control(UiControl::DebugAudioMute(index))]);
        }
    }
    assert_eq!(before, app.emu.machine_state_bytes().unwrap());
}

#[test]
fn text_editing_and_clipboard_shortcuts_do_not_step_the_machine() {
    let app = test_app();
    let context = egui::Context::default();
    let mut layout = Layout::default();
    let mut panel = ui::DebuggerPanel::new();
    let view = app.build_debugger_view_with_clipping(&panel, false);
    let _ = run_frame(
        &context,
        &mut layout,
        input([1100.0, 760.0], vec![]),
        &mut panel,
        &view,
    );
    context.memory_mut(|m| m.request_focus(egui::Id::new("debugger_entry")));
    let _ = run_frame(
        &context,
        &mut layout,
        input([1100.0, 760.0], vec![]),
        &mut panel,
        &view,
    );
    let (_, actions) = run_frame(
        &context,
        &mut layout,
        input(
            [1100.0, 760.0],
            vec![
                key(egui::Key::S, egui::Modifiers::NONE),
                egui::Event::Text("s".into()),
                egui::Event::Paste("é路".into()),
            ],
        ),
        &mut panel,
        &view,
    );
    assert!(actions.is_empty());
    assert_eq!(panel.entry, "sé路");
    assert!(
        panel.find_pattern().is_none(),
        "non-ASCII input must be rejected without slicing inside UTF-8"
    );
    context.memory_mut(|m| m.surrender_focus(egui::Id::new("debugger_entry")));
    let mut clipboard = input(
        [1100.0, 760.0],
        vec![key(egui::Key::C, egui::Modifiers::COMMAND)],
    );
    clipboard.modifiers = egui::Modifiers::COMMAND;
    let (_, actions) = run_frame(&context, &mut layout, clipboard, &mut panel, &view);
    assert!(actions.is_empty(), "copy must not Copper-step");
}

#[test]
fn step_shortcut_dispatches_once_through_the_existing_debugger() {
    let mut app = test_app();
    app.open_debugger();
    let context = egui::Context::default();
    let mut layout = Layout::default();
    let mut panel = app.debugger_panel.clone().unwrap();
    let view = app.build_debugger_view_with_clipping(&panel, false);
    let before = app.emu.retired_instructions();
    let (_, actions) = run_frame(
        &context,
        &mut layout,
        input(
            [1100.0, 760.0],
            vec![key(egui::Key::S, egui::Modifiers::NONE)],
        ),
        &mut panel,
        &view,
    );
    assert_eq!(actions, [Action::Control(UiControl::DebugStep)]);
    for action in actions {
        app.apply_egui_debugger_action(action);
    }
    assert_eq!(app.emu.retired_instructions(), before + 1);
}

#[test]
fn address_submission_register_edits_and_cpu_memory_paging_reuse_machine_actions() {
    let mut app = test_app();
    app.open_debugger();
    app.debugger_panel.as_mut().unwrap().entry = "D0 12345678".into();
    app.apply_egui_debugger_action(Action::Control(UiControl::DebugPoke));
    assert_eq!(app.emu.machine.d(0), 0x12345678);
    app.debugger_panel.as_mut().unwrap().entry = "F80020".into();
    app.apply_egui_debugger_action(Action::SubmitEntry);
    assert_eq!(
        app.debugger_panel.as_ref().unwrap().disasm_addr,
        Some(0xF80020)
    );
    let pinned = app.build_debugger_view_with_clipping(app.debugger_panel.as_ref().unwrap(), false);
    let machine = app.emu.machine_state_bytes().unwrap();
    app.debug_snapshot_dirty.set(false);
    app.apply_egui_debugger_action(Action::FollowPc);
    let followed =
        app.build_debugger_view_with_clipping(app.debugger_panel.as_ref().unwrap(), false);
    assert_ne!(
        pinned.cpu.unwrap().disassembly[0].text,
        followed.cpu.unwrap().disassembly[0].text,
    );
    assert!(
        app.debug_snapshot_dirty.get(),
        "paused views must refresh immediately"
    );
    assert_eq!(app.emu.machine_state_bytes().unwrap(), machine);
    app.debugger_panel.as_mut().unwrap().mem_view_bits = true;
    app.debugger_panel.as_mut().unwrap().mem_addr = 0;
    app.apply_egui_debugger_action(Action::MemoryScroll(16));
    let panel = app.debugger_panel.as_ref().unwrap();
    assert_eq!(
        panel.mem_addr, 256,
        "CPU memory always pages hex bytes, even after visiting Bits"
    );
    assert_eq!(panel.tab, ui::DebugTab::Cpu);
    assert!(panel.mem_view_bits);
}

/// Click the widget with `id`: pointer move, press, and release, each as
/// its own frame, as a real click arrives. Returns the release frame's
/// actions.
fn click_widget(
    context: &egui::Context,
    layout: &mut Layout,
    panel: &mut ui::DebuggerPanel,
    view: &ui::DebuggerView,
    size: [f32; 2],
    id: egui::Id,
) -> Vec<Action> {
    let pos = context
        .read_response(id)
        .unwrap_or_else(|| panic!("{id:?} is not laid out"))
        .rect
        .center();
    click_at(context, layout, panel, view, size, pos)
}

fn click_at(
    context: &egui::Context,
    layout: &mut Layout,
    panel: &mut ui::DebuggerPanel,
    view: &ui::DebuggerView,
    size: [f32; 2],
    pos: egui::Pos2,
) -> Vec<Action> {
    let mut last = Vec::new();
    for events in [
        vec![egui::Event::PointerMoved(pos)],
        vec![egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: egui::Modifiers::NONE,
        }],
        vec![egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: egui::Modifiers::NONE,
        }],
    ] {
        let (_, actions) = run_frame(context, layout, input(size, events), panel, view);
        last = actions;
    }
    last
}

fn text(text: &str) -> egui::Event {
    egui::Event::Text(text.into())
}

#[test]
fn memory_tab_edits_bytes_in_place_through_the_bus() {
    let mut app = test_app();
    app.open_debugger();
    // Drop the boot overlay so low chip RAM is CPU-visible.
    app.emu.bus_mut().mem.overlay = false;
    {
        let panel = app.debugger_panel.as_mut().unwrap();
        panel.tab = ui::DebugTab::Memory;
        panel.mem_addr = 0x60000;
    }
    let size = [1100.0, 760.0];
    let context = egui::Context::default();
    configure_style(&context);
    let mut layout = Layout::default();
    let mut panel = app.debugger_panel.clone().unwrap();
    let view = app.build_debugger_view_with_clipping(&panel, false);
    let memory = view.memory.as_ref().expect("hex mode carries the page");
    assert_eq!(memory.base, 0x60000);
    assert_eq!(memory.bytes.len(), 256);
    assert!(memory.writable.iter().all(|w| *w), "chip RAM is editable");
    let original = memory.bytes[3];
    let retired = app.emu.retired_instructions();
    let _ = run_frame(
        &context,
        &mut layout,
        input(size, vec![]),
        &mut panel,
        &view,
    );

    // Hex column: click, then type two digits. C and F are also the
    // Copper-step and Frame shortcuts; while a byte is selected they are
    // data, and nothing steps the machine.
    let actions = click_widget(
        &context,
        &mut layout,
        &mut panel,
        &view,
        size,
        memory_cell_id(0x60003, ui::MemColumn::Hex),
    );
    assert!(actions.is_empty());
    assert_eq!(
        panel.mem_cursor,
        Some(ui::MemCursor::new(0x60003, ui::MemColumn::Hex))
    );
    let (_, actions) = run_frame(
        &context,
        &mut layout,
        input(
            size,
            vec![key(egui::Key::C, egui::Modifiers::NONE), text("c")],
        ),
        &mut panel,
        &view,
    );
    assert!(actions.is_empty(), "{actions:?}");
    assert_eq!(
        panel.mem_pending_value(0x60003),
        Some(0xC0 | (original & 0x0F))
    );
    assert!(!panel.mem_cursor.unwrap().high_nibble);
    let (_, actions) = run_frame(
        &context,
        &mut layout,
        input(
            size,
            vec![key(egui::Key::F, egui::Modifiers::NONE), text("F")],
        ),
        &mut panel,
        &view,
    );
    assert!(actions.is_empty(), "{actions:?}");
    assert_eq!(panel.mem_pending, vec![(0x60003, 0xCF)]);
    assert_eq!(
        panel.mem_cursor,
        Some(ui::MemCursor::new(0x60004, ui::MemColumn::Hex)),
        "a completed byte advances the cursor"
    );
    assert_eq!(
        app.emu.machine.debug_read_memory(0x60003, 1),
        vec![original],
        "staged edits do not touch memory"
    );
    // Enter commits through the App, and the bus reads the byte back.
    let (_, actions) = run_frame(
        &context,
        &mut layout,
        input(size, vec![key(egui::Key::Enter, egui::Modifiers::NONE)]),
        &mut panel,
        &view,
    );
    assert_eq!(actions, vec![Action::MemoryCommit(vec![(0x60003, 0xCF)])]);
    assert!(panel.mem_cursor.is_none());
    assert!(panel.mem_pending.is_empty());
    for action in actions {
        app.apply_egui_debugger_action(action);
    }
    assert_eq!(app.emu.bus().peek_word_any(0x60002) & 0xFF, 0xCF);
    assert_eq!(app.emu.machine.debug_read_memory(0x60003, 1), vec![0xCF]);
    assert_eq!(
        app.emu.retired_instructions(),
        retired,
        "no timeline side effects"
    );
    assert!(app.paused);
    let status = app.debugger_panel.as_ref().unwrap().mem_status.clone();
    assert_eq!(status.as_deref(), Some("Wrote 1 byte at $060003"));

    // ASCII column: one typed character replaces the byte, and clicking
    // anywhere outside the dump commits, as leaving a field does.
    let view = app.build_debugger_view_with_clipping(&panel, false);
    let _ = run_frame(
        &context,
        &mut layout,
        input(size, vec![]),
        &mut panel,
        &view,
    );
    let actions = click_widget(
        &context,
        &mut layout,
        &mut panel,
        &view,
        size,
        memory_cell_id(0x60005, ui::MemColumn::Ascii),
    );
    assert!(actions.is_empty());
    let (_, actions) = run_frame(
        &context,
        &mut layout,
        input(
            size,
            vec![key(egui::Key::Z, egui::Modifiers::SHIFT), text("Z")],
        ),
        &mut panel,
        &view,
    );
    assert!(actions.is_empty(), "{actions:?}");
    assert_eq!(panel.mem_pending, vec![(0x60005, b'Z')]);
    assert_eq!(
        panel.mem_cursor,
        Some(ui::MemCursor::new(0x60006, ui::MemColumn::Ascii))
    );
    let first_cell = context
        .read_response(memory_cell_id(0x60000, ui::MemColumn::Hex))
        .unwrap()
        .rect;
    let address_label = first_cell.left_center() - egui::vec2(40.0, 0.0);
    let actions = click_at(
        &context,
        &mut layout,
        &mut panel,
        &view,
        size,
        address_label,
    );
    assert_eq!(actions, vec![Action::MemoryCommit(vec![(0x60005, b'Z')])]);
    assert!(panel.mem_cursor.is_none());
    for action in actions {
        app.apply_egui_debugger_action(action);
    }
    assert_eq!(app.emu.machine.debug_read_memory(0x60005, 1), vec![b'Z']);

    // Esc drops the staged edit without closing the workspace.
    let view = app.build_debugger_view_with_clipping(&panel, false);
    let _ = run_frame(
        &context,
        &mut layout,
        input(size, vec![]),
        &mut panel,
        &view,
    );
    click_widget(
        &context,
        &mut layout,
        &mut panel,
        &view,
        size,
        memory_cell_id(0x60008, ui::MemColumn::Hex),
    );
    let (_, actions) = run_frame(
        &context,
        &mut layout,
        input(size, vec![text("1")]),
        &mut panel,
        &view,
    );
    assert!(actions.is_empty());
    assert_eq!(panel.mem_pending.len(), 1);
    let (_, actions) = run_frame(
        &context,
        &mut layout,
        input(size, vec![key(egui::Key::Escape, egui::Modifiers::NONE)]),
        &mut panel,
        &view,
    );
    assert!(actions.is_empty(), "Esc while editing must not leave Debug");
    assert!(panel.mem_cursor.is_none());
    assert!(panel.mem_pending.is_empty());
    let before = app.emu.machine_state_bytes().unwrap();
    let _ = run_frame(
        &context,
        &mut layout,
        input(size, vec![]),
        &mut panel,
        &view,
    );
    assert_eq!(app.emu.machine_state_bytes().unwrap(), before);
}

#[test]
fn memory_edit_cursor_keys_follow_the_page_and_refuse_rom() {
    let mut app = test_app();
    app.open_debugger();
    app.emu.bus_mut().mem.overlay = false;
    {
        let panel = app.debugger_panel.as_mut().unwrap();
        panel.tab = ui::DebugTab::Memory;
        panel.mem_addr = 0x60000;
    }
    let size = [1100.0, 760.0];
    let context = egui::Context::default();
    configure_style(&context);
    let mut layout = Layout::default();
    let mut panel = app.debugger_panel.clone().unwrap();
    let view = app.build_debugger_view_with_clipping(&panel, false);
    let _ = run_frame(
        &context,
        &mut layout,
        input(size, vec![]),
        &mut panel,
        &view,
    );

    // Up from the first row: the cursor moves to the row above and the
    // page scrolls to keep it visible.
    click_widget(
        &context,
        &mut layout,
        &mut panel,
        &view,
        size,
        memory_cell_id(0x60000, ui::MemColumn::Hex),
    );
    let (_, actions) = run_frame(
        &context,
        &mut layout,
        input(size, vec![key(egui::Key::ArrowUp, egui::Modifiers::NONE)]),
        &mut panel,
        &view,
    );
    assert_eq!(actions, vec![Action::MemoryScroll(-1)]);
    assert_eq!(panel.mem_cursor.unwrap().addr, 0x5FFF0);
    // Within the page the arrows only move the cursor.
    panel.mem_cursor = Some(ui::MemCursor::new(0x60010, ui::MemColumn::Hex));
    let (_, actions) = run_frame(
        &context,
        &mut layout,
        input(
            size,
            vec![
                key(egui::Key::ArrowRight, egui::Modifiers::NONE),
                key(egui::Key::ArrowDown, egui::Modifiers::NONE),
            ],
        ),
        &mut panel,
        &view,
    );
    assert!(actions.is_empty());
    assert_eq!(panel.mem_cursor.unwrap().addr, 0x60021);
    // Right past the last byte scrolls one row; Page Down a whole page.
    panel.mem_cursor = Some(ui::MemCursor::new(0x600FF, ui::MemColumn::Ascii));
    let (_, actions) = run_frame(
        &context,
        &mut layout,
        input(
            size,
            vec![key(egui::Key::ArrowRight, egui::Modifiers::NONE)],
        ),
        &mut panel,
        &view,
    );
    assert_eq!(actions, vec![Action::MemoryScroll(1)]);
    assert_eq!(panel.mem_cursor.unwrap().addr, 0x60100);
    panel.mem_cursor = Some(ui::MemCursor::new(0x60040, ui::MemColumn::Hex));
    let (_, actions) = run_frame(
        &context,
        &mut layout,
        input(size, vec![key(egui::Key::PageDown, egui::Modifiers::NONE)]),
        &mut panel,
        &view,
    );
    assert_eq!(actions, vec![Action::MemoryScroll(16)]);
    assert_eq!(panel.mem_cursor.unwrap().addr, 0x60140);
    // The page scroll goes through the same App action as the buttons.
    for action in actions {
        app.apply_egui_debugger_action(action);
    }
    assert_eq!(app.debugger_panel.as_ref().unwrap().mem_addr, 0x60100);

    // ROM is shown but refused: a click there selects nothing and says why.
    panel.mem_edit_cancel();
    panel.mem_addr = crate::memory::ROM_BASE as u32;
    let view = app.build_debugger_view_with_clipping(&panel, false);
    let memory = view.memory.as_ref().unwrap();
    assert!(memory.writable.iter().all(|w| !*w));
    let _ = run_frame(
        &context,
        &mut layout,
        input(size, vec![]),
        &mut panel,
        &view,
    );
    let rom_cell = memory_cell_id(crate::memory::ROM_BASE as u32 + 4, ui::MemColumn::Hex);
    let actions = click_widget(&context, &mut layout, &mut panel, &view, size, rom_cell);
    assert!(actions.is_empty());
    assert!(panel.mem_cursor.is_none());
    assert_eq!(
        panel.mem_status.as_deref(),
        Some("$F80004 is read-only (ROM or a device window); not editable")
    );
    // With no cursor the ordinary shortcuts are back.
    let (_, actions) = run_frame(
        &context,
        &mut layout,
        input(size, vec![key(egui::Key::PageDown, egui::Modifiers::NONE)]),
        &mut panel,
        &view,
    );
    assert_eq!(actions, vec![Action::MemoryScroll(16)]);
}

#[test]
fn memory_tab_commit_matches_the_control_protocol_write() {
    use crate::control::exec::{exec_core, CoreOp};
    use crate::control::session::SessionCtx;

    // The same edits through the tab and through mem.write leave two
    // fresh machines byte-identical, watch baselines included.
    let mut gui = test_app();
    gui.open_debugger();
    gui.emu.bus_mut().mem.overlay = false;
    gui.emu.machine.ui_toggle_watch(0x60010);
    let mut ccp = test_app();
    ccp.open_debugger();
    ccp.emu.bus_mut().mem.overlay = false;
    ccp.emu.machine.ui_toggle_watch(0x60010);
    assert_eq!(
        gui.emu.machine_state_bytes().unwrap(),
        ccp.emu.machine_state_bytes().unwrap()
    );

    gui.apply_egui_debugger_action(Action::MemoryCommit(vec![
        (0x60010, 0x42),
        (0x60011, 0x43),
        (crate::memory::ROM_BASE as u32, 0xFF),
    ]));
    let mut ctx = SessionCtx::new();
    let written = exec_core(
        &mut ccp.emu,
        &mut ctx,
        &CoreOp::MemWrite {
            addr: 0x60010,
            data: vec![0x42, 0x43],
        },
    )
    .unwrap();
    assert_eq!(written["written"], 2);
    let refused = exec_core(
        &mut ccp.emu,
        &mut ctx,
        &CoreOp::MemWrite {
            addr: crate::memory::ROM_BASE as u32,
            data: vec![0xFF],
        },
    )
    .unwrap();
    assert_eq!(refused["written"], 0);

    assert_eq!(gui.emu.bus().peek_word_any(0x60010), 0x4243);
    let watch = &gui.emu.machine.ui_breaks().watches[0];
    assert_eq!(
        watch.last, 0x4243,
        "the poke itself must not trip the watch"
    );
    assert_eq!(
        gui.emu.machine_state_bytes().unwrap(),
        ccp.emu.machine_state_bytes().unwrap()
    );
    assert_eq!(
        gui.debugger_panel.as_ref().unwrap().mem_status.as_deref(),
        Some("Wrote 2 of 3 bytes; $F80000 is not writable RAM")
    );
}

#[test]
fn dragging_workspace_and_cpu_dividers_preserves_the_new_pane_sizes() {
    let app = test_app();
    let context = egui::Context::default();
    configure_style(&context);
    let mut layout = Layout {
        workspace: true,
        ..Default::default()
    };
    let mut panel = ui::DebuggerPanel::new();
    let view = app.build_debugger_view_with_clipping(&panel, false);
    for _ in 0..3 {
        let _ = run_frame(
            &context,
            &mut layout,
            input([1600.0, 900.0], vec![]),
            &mut panel,
            &view,
        );
    }
    for (name, offset) in [
        ("debug_display", egui::vec2(90.0, 0.0)),
        ("debugger_registers", egui::vec2(90.0, 0.0)),
        ("debugger_cpu_memory", egui::vec2(0.0, -80.0)),
    ] {
        let id = egui::Id::new(name).with("__resize");
        let start = context.read_response(id).unwrap().rect.center();
        let end = start + offset;
        for events in [
            vec![egui::Event::PointerMoved(start)],
            vec![egui::Event::PointerButton {
                pos: start,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: egui::Modifiers::NONE,
            }],
            vec![egui::Event::PointerMoved(end)],
            vec![egui::Event::PointerButton {
                pos: end,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: egui::Modifiers::NONE,
            }],
            vec![],
        ] {
            let (_, actions) = run_frame(
                &context,
                &mut layout,
                input([1600.0, 900.0], events),
                &mut panel,
                &view,
            );
            assert!(actions.is_empty());
        }
        let actual = context.read_response(id).unwrap().rect.center();
        let axis = if offset.x != 0.0 { 0 } else { 1 };
        assert!(
            (actual[axis] - end[axis]).abs() < 2.0,
            "{name}: {start:?} -> {actual:?}, expected {end:?}"
        );
    }
}

struct Offscreen {
    renderer: egui_wgpu::Renderer,
    target: wgpu::Texture,
    screen: egui_wgpu::ScreenDescriptor,
    prepared: Option<PreparedFrame>,
}

fn analyzer_app() -> App {
    use super::super::tests::uaelib_insights::{fit_uaelib, register, resource_bytes};
    let mut app = test_app();
    fit_uaelib(&mut app);
    app.open_frame_analyzer();
    app.frame_analyzer_set_tab(ui::AnalyzerTab::Memory);
    {
        let bus = app.emu.bus_mut();
        for (i, byte) in bus.mem.chip_ram[0x20000..0x20100].iter_mut().enumerate() {
            *byte = if (i / 4 + i / 64) % 2 == 0 {
                0xAA
            } else {
                0x55
            };
        }
        bus.mem.chip_ram[0x30000..0x30008]
            .copy_from_slice(&[0, 0, 0x0F, 0x80, 0x04, 0xAF, 0x0F, 0xFF]);
        bus.mem.chip_ram[0x40000..0x40008]
            .copy_from_slice(&[0x01, 0x80, 0x0F, 0, 0xFF, 0xFF, 0xFF, 0xFE]);
        bus.custom_write(0x096, 2, 0x8240);
        bus.custom_write(0x040, 2, 0x01F0);
        bus.custom_write(0x044, 2, 0xFFFF);
        bus.custom_write(0x046, 2, 0xFFFF);
        bus.custom_write(0x074, 2, 0xBEEF);
        bus.custom_write(0x054, 4, 0x0006_0000);
        bus.custom_write(0x058, 2, 0x0404); // 16 rows, 4 words
    }
    register(
        &mut app,
        0x5000,
        &resource_bytes(0x30000, 8, "palette", 1, 0, [4, 0, 0]),
    );
    register(
        &mut app,
        0x5100,
        &resource_bytes(0x20000, 256, "bitmap", 0, 0, [32, 32, 2]),
    );
    register(
        &mut app,
        0x5200,
        &resource_bytes(0x40000, 8, "copper", 2, 0, [0, 0, 0]),
    );
    for i in 0..20 {
        register(
            &mut app,
            0x5300,
            &resource_bytes(
                0x31000 + i * 16,
                8,
                &format!("palette {i}"),
                1,
                0,
                [4, 0, 0],
            ),
        );
    }
    app.frame_analyzer_step_frame();
    for (index, toucher) in [
        crate::heatmap::Toucher::CpuRead,
        crate::heatmap::Toucher::CpuWrite,
        crate::heatmap::Toucher::Blitter,
        crate::heatmap::Toucher::Copper,
        crate::heatmap::Toucher::Bitplane,
    ]
    .into_iter()
    .enumerate()
    {
        app.emu
            .bus_mut()
            .note_heat(index as u32 * 0x10000, 0x8000, toucher);
    }
    app.frame_analyzer_set_tab(ui::AnalyzerTab::Beam);
    app
}

#[test]
fn both_inspectors_share_one_window_and_restore_the_same_run_state() {
    for analyzer_first in [false, true] {
        for initially_paused in [false, true] {
            let mut app = test_app();
            app.paused = initially_paused;
            let first = if analyzer_first {
                ToolPanelKind::FrameAnalyzer
            } else {
                ToolPanelKind::Debugger
            };
            let second = if analyzer_first {
                ToolPanelKind::Debugger
            } else {
                ToolPanelKind::FrameAnalyzer
            };
            app.apply_egui_debugger_action(Action::SelectTool(first));
            assert!(app.paused);
            assert!(app.debug_layout_active);
            app.apply_egui_debugger_action(Action::SelectTool(second));
            assert_eq!(app.egui_selected_tool, second);
            assert_eq!(app.topmost_tool_panel(), Some(second));
            assert!(app.paused);
            app.close_tool_panel(first);
            assert!(app.paused, "closing one inspector leaves the other paused");
            assert!(app.debug_layout_active);
            close_all_inspectors(&mut app);
            assert_eq!(app.paused, initially_paused);
            assert!(!app.debug_layout_active);
        }
    }
}

#[test]
fn console_shares_the_workspace_in_every_open_and_close_order() {
    for first in ToolPanelKind::ALL {
        for second in ToolPanelKind::ALL {
            if first == second {
                continue;
            }
            let third = ToolPanelKind::ALL
                .into_iter()
                .find(|k| *k != first && *k != second)
                .unwrap();
            for initially_paused in [false, true] {
                for close_first in ToolPanelKind::ALL {
                    let mut app = test_app();
                    app.paused = initially_paused;
                    for kind in [first, second, third] {
                        app.apply_egui_debugger_action(Action::SelectTool(kind));
                        assert!(app.paused);
                        assert_eq!(app.topmost_tool_panel(), Some(kind));
                    }
                    assert!(app.debug_layout_active);
                    app.close_tool_panel(close_first);
                    assert!(app.paused);
                    assert!(app.tool_panel_is_open(app.egui_selected_tool));
                    close_all_inspectors(&mut app);
                    assert_eq!(app.paused, initially_paused);
                    assert!(!app.egui_workspace_open());
                }
            }
        }
    }
    let mut app = analyzer_app();
    app.open_console();
    app.apply_egui_debugger_action(Action::ConsoleSubmit("RUN".into()));
    app.open_debugger();
    assert!(!app.paused);
    app.close_tool_panel(ToolPanelKind::Console);
    assert!(!app.paused);
    app.open_console();
    app.apply_egui_debugger_action(Action::ConsoleSubmit("PAUSE\nCLOSE\nRUN".into()));
    assert!(
        app.console_panel.is_none(),
        "CLOSE ends the submitted batch"
    );
    close_all_inspectors(&mut app);
    assert!(
        app.paused,
        "the last explicit pause survives closing every inspector"
    );
}

#[test]
fn analyzer_navigation_pins_addresses_without_changing_the_capture_or_machine() {
    let mut app = analyzer_app();
    app.open_console();
    app.console_panel.as_mut().unwrap().input = "status".into();
    let capture = app.emu.bus().frame_bus_trace().unwrap().frame;
    let selection = app.frame_analyzer_panel.as_ref().unwrap().selected_hpos;
    let before = app.emu.machine_state_bytes().unwrap();
    for (tab, address) in [
        (ui::DebugTab::Cpu, 0x121),
        (ui::DebugTab::Memory, 0x135),
        (ui::DebugTab::Copper, 0x200),
    ] {
        app.apply_egui_debugger_action(Action::Navigate(tab, address));
        assert_eq!(app.egui_selected_tool, ToolPanelKind::Debugger);
        let panel = app.debugger_panel.as_ref().unwrap();
        assert_eq!(panel.tab, tab);
        match tab {
            ui::DebugTab::Cpu => assert_eq!(panel.disasm_addr, Some(0x120)),
            ui::DebugTab::Memory => assert_eq!(panel.mem_addr, 0x130),
            ui::DebugTab::Copper => {
                assert_eq!(panel.copper_addr, Some(0x200));
                let view = app.build_debugger_view_with_clipping(panel, false);
                assert!(view.lines.iter().any(|line| line.text.contains("000200")));
            }
            _ => unreachable!(),
        }
        app.open_frame_analyzer();
        assert_eq!(
            app.frame_analyzer_panel.as_ref().unwrap().selected_hpos,
            selection
        );
        assert_eq!(app.emu.bus().frame_bus_trace().unwrap().frame, capture);
        assert_eq!(before, app.emu.machine_state_bytes().unwrap());
    }
    assert_eq!(app.console_panel.as_ref().unwrap().input, "status");
}

#[test]
fn console_submission_survives_a_failed_presentation_without_repeating() {
    let mut app = test_app();
    app.open_console();
    let context = egui::Context::default();
    let mut layout = Layout::default();
    let mut panel = app.console_panel.clone().unwrap();
    panel.input = "step".into();
    let _ = run_content_frame(
        &context,
        &mut layout,
        input([1100.0, 760.0], vec![]),
        Content::Console(&mut panel, "Paused"),
    );
    let (_, actions) = run_content_frame(
        &context,
        &mut layout,
        input(
            [1100.0, 760.0],
            vec![key(egui::Key::Enter, egui::Modifiers::NONE)],
        ),
        Content::Console(&mut panel, "Paused"),
    );
    app.console_panel = Some(panel);
    let before = app.emu.retired_instructions();
    app.dispatch_egui_frame(actions, Err(pixels::Error::Validation));
    assert_eq!(app.emu.retired_instructions(), before + 1);
    assert_eq!(app.console_panel.as_ref().unwrap().history, ["step"]);
    let mut panel = app.console_panel.clone().unwrap();
    let (_, actions) = run_content_frame(
        &context,
        &mut layout,
        input([1100.0, 760.0], vec![]),
        Content::Console(&mut panel, "Paused"),
    );
    assert!(actions.is_empty());
    app.console_panel = Some(panel);
    app.dispatch_egui_frame(actions, Ok(()));
    assert_eq!(app.emu.retired_instructions(), before + 1);
}

#[test]
fn console_batches_preserve_case_and_ignore_blank_lines() {
    let mut app = test_app();
    app.open_console();
    app.apply_egui_debugger_action(Action::ConsoleSubmit("b $c01000".into()));
    assert!(app.emu.machine.ui_breaks().is_breakpoint(0x00C0_1000));
    assert!(app.console_panel.as_ref().unwrap().input.is_empty());
    app.apply_egui_debugger_action(Action::ConsoleSubmit(
        "btrap 100 40\n\nsetreg d2 77\nm 0".into(),
    ));
    assert_eq!(app.emu.bus().ui_beam_traps().len(), 1);
    assert_eq!(app.emu.machine.d(2), 0x77);
    assert_eq!(
        app.console_panel.as_ref().unwrap().history,
        ["b $c01000", "btrap 100 40", "setreg d2 77", "m 0"]
    );
    assert!(app.console_panel.as_ref().unwrap().input.is_empty());
}

#[test]
fn console_paste_history_and_execution_are_separate_from_layout() {
    let mut app = test_app();
    app.open_console();
    let context = egui::Context::default();
    let mut layout = Layout::default();
    let mut panel = app.console_panel.clone().unwrap();
    let before = app.emu.machine_state_bytes().unwrap();
    for size in [[600.0, 480.0], [1100.0, 760.0]] {
        let (_, actions) = run_content_frame(
            &context,
            &mut layout,
            input(size, vec![]),
            Content::Console(&mut panel, "Paused"),
        );
        assert!(actions.is_empty());
    }
    let (_, actions) = run_content_frame(
        &context,
        &mut layout,
        input(
            [1100.0, 760.0],
            vec![egui::Event::Paste("status\nstep".into())],
        ),
        Content::Console(&mut panel, "Paused"),
    );
    assert!(actions.is_empty());
    assert_eq!(panel.input, "status\nstep");
    assert_eq!(before, app.emu.machine_state_bytes().unwrap());
    let (_, actions) = run_content_frame(
        &context,
        &mut layout,
        input(
            [1100.0, 760.0],
            vec![key(egui::Key::Enter, egui::Modifiers::SHIFT)],
        ),
        Content::Console(&mut panel, "Paused"),
    );
    assert!(actions.is_empty(), "Shift+Enter belongs to the editor");
    assert_eq!(panel.input, "status\nstep\n");
    let mut released = key(egui::Key::Enter, egui::Modifiers::SHIFT);
    if let egui::Event::Key { pressed, .. } = &mut released {
        *pressed = false;
    }
    let _ = run_content_frame(
        &context,
        &mut layout,
        input([1100.0, 760.0], vec![released]),
        Content::Console(&mut panel, "Paused"),
    );
    // Exercise egui's repeated sizing pass as well as ordinary command input.
    context.options_mut(|options| options.max_passes = 2.try_into().unwrap());
    let (_, actions) = run_content_frame(
        &context,
        &mut layout,
        input(
            [1100.0, 760.0],
            vec![key(egui::Key::Enter, egui::Modifiers::NONE)],
        ),
        Content::Console(&mut panel, "Paused"),
    );
    assert_eq!(actions, [Action::ConsoleSubmit("status\nstep\n".into())]);
    assert!(panel.input.is_empty());
    assert_eq!(
        before,
        app.emu.machine_state_bytes().unwrap(),
        "layout cannot execute commands"
    );
    app.console_panel = Some(panel);
    let retired = app.emu.retired_instructions();
    for action in actions {
        app.apply_egui_debugger_action(action);
    }
    assert_eq!(app.emu.retired_instructions(), retired + 1);
    let mut panel = app.console_panel.clone().unwrap();
    assert_eq!(panel.history, ["status", "step"]);
    for (key_name, expected) in [
        (egui::Key::ArrowUp, "step"),
        (egui::Key::ArrowUp, "status"),
        (egui::Key::ArrowDown, "step"),
    ] {
        let (_, actions) = run_content_frame(
            &context,
            &mut layout,
            input([1100.0, 760.0], vec![key(key_name, egui::Modifiers::NONE)]),
            Content::Console(&mut panel, "Paused"),
        );
        assert!(actions.is_empty());
        assert_eq!(panel.input, expected);
    }
}

#[test]
fn saved_workspace_and_cpu_pane_sizes_restore_in_a_fresh_egui_context() {
    let app = test_app();
    let mut panel = ui::DebuggerPanel::new();
    let view = app.build_debugger_view_with_clipping(&panel, false);
    let mut layout = Layout::default();
    layout.preferences.register_width = 310.0;
    layout.preferences.memory_height = 245.0;
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("layout.toml");
    layout.preferences.save(&path).unwrap();
    let mut restored = Layout {
        workspace: true,
        preferences: preferences::Preferences::load(&path),
        ..Default::default()
    };
    let context = egui::Context::default();
    for _ in 0..3 {
        let _ = run_frame(
            &context,
            &mut restored,
            input([1600.0, 900.0], vec![]),
            &mut panel,
            &view,
        );
    }
    assert!((restored.preferences.register_width - 310.0).abs() < 1.0);
    assert!((restored.preferences.memory_height - 245.0).abs() < 1.0);
    for (id, expected, axis) in [
        ("debug_display", 560.0, 0),
        ("debugger_registers", 310.0, 0),
        ("debugger_cpu_memory", 245.0, 1),
    ] {
        let size = egui::containers::panel::PanelState::load(&context, egui::Id::new(id))
            .unwrap()
            .size();
        assert!((size[axis] - expected).abs() < 1.0, "{id}: {size:?}");
    }
}

#[test]
fn clicking_analyzer_addresses_dispatches_the_matching_destination() {
    let app = analyzer_app();
    let mut panel = app.frame_analyzer_panel.clone().unwrap();
    panel.show_cpu_wait = true;
    let mut view = app.build_frame_analyzer_view(&panel);
    view.trace.as_mut().unwrap().top_stalled_pcs = vec![(0x120, 17, None)];
    let context = egui::Context::default();
    let mut layout = Layout::default();
    let mut point = None;
    for _ in 0..3 {
        let (output, _) = run_content_frame(
            &context,
            &mut layout,
            input([1100.0, 1000.0], vec![]),
            Content::Analyzer(&mut panel, &view),
        );
        for shape in output.shapes {
            if let egui::Shape::Text(text) = shape.shape {
                if text.galley.job.text == "$00000120  17 cck" {
                    point = Some(text.pos + text.galley.size() * 0.5);
                }
            }
        }
    }
    let point = point.expect("stalled PC link is rendered");
    let mut actions = Vec::new();
    for events in [
        vec![egui::Event::PointerMoved(point)],
        vec![egui::Event::PointerButton {
            pos: point,
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: egui::Modifiers::NONE,
        }],
        vec![egui::Event::PointerButton {
            pos: point,
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: egui::Modifiers::NONE,
        }],
    ] {
        actions.extend(
            run_content_frame(
                &context,
                &mut layout,
                input([1100.0, 1000.0], events),
                Content::Analyzer(&mut panel, &view),
            )
            .1,
        );
    }
    assert_eq!(actions, [Action::Navigate(ui::DebugTab::Cpu, 0x120)]);
}

#[test]
#[ignore = "writes GPU-rendered Console preview for visual review"]
fn render_console_preview() {
    let gpu =
        super::super::crt_shader::test_gpu("egui_console_preview").expect("hardware GPU required");
    let mut app = analyzer_app();
    app.open_console();
    app.apply_egui_debugger_action(Action::ConsoleSubmit("STATUS\nREGS\nCPUWAIT".into()));
    let mut panel = app.console_panel.clone().unwrap();
    panel.input = "dis 100 8".into();
    let context = egui::Context::default();
    configure_style(&context);
    context.set_pixels_per_point(2.0);
    let mut layout = Layout::default();
    let mut offscreen = Offscreen::new(gpu.device(), 2200, 1520, 2.0);
    for _ in 0..3 {
        let (output, actions) = run_content_frame(
            &context,
            &mut layout,
            input([1100.0, 760.0], vec![]),
            Content::Console(&mut panel, "Paused"),
        );
        assert!(actions.is_empty());
        offscreen.draw(&context, output, gpu.device(), gpu.queue());
    }
    offscreen.save(
        gpu.device(),
        gpu.queue(),
        std::path::Path::new("target/egui-debugger/Console.png"),
    );
}

#[test]
fn switching_inspectors_preserves_capture_selection_and_explicit_run_pause() {
    let mut app = analyzer_app();
    app.frame_analyzer_panel.as_mut().unwrap().selected_hpos = 90;
    let trace = app.emu.bus().frame_bus_trace().unwrap().frame;
    let retired = app.emu.retired_instructions();
    app.frame_analyzer_toggle_run();
    assert!(!app.paused);
    app.open_debugger();
    assert!(
        !app.paused,
        "switching tools must not pause a running capture"
    );
    app.open_frame_analyzer();
    assert_eq!(app.frame_analyzer_panel.as_ref().unwrap().selected_hpos, 90);
    assert_eq!(app.emu.bus().frame_bus_trace().unwrap().frame, trace);
    assert_eq!(app.emu.retired_instructions(), retired);
    app.debugger_toggle_run();
    assert!(app.paused);
    close_all_inspectors(&mut app);
    assert!(
        app.paused,
        "an explicit Pause survives closing the shared window"
    );
}

#[test]
fn analyzer_tabs_and_resource_previews_leave_machine_state_unchanged() {
    let mut app = analyzer_app();
    let context = egui::Context::default();
    configure_style(&context);
    let mut layout = Layout::default();
    for (tab, resource) in ui::ANALYZER_TABS.into_iter().map(|tab| (tab, 1)).chain([
        (ui::AnalyzerTab::Resources, 0),
        (ui::AnalyzerTab::Resources, 2),
    ]) {
        app.frame_analyzer_set_tab(tab);
        app.frame_analyzer_select_resource(resource);
        let mut panel = app.frame_analyzer_panel.clone().unwrap();
        let before = app.emu.machine_state_bytes().unwrap();
        let view = app.build_frame_analyzer_view(&panel);
        if tab == ui::AnalyzerTab::Blits {
            assert!(view.blits.is_some());
        }
        if tab == ui::AnalyzerTab::Resources {
            assert!(view.resources.as_ref().unwrap().detail.is_some());
        }
        for size in [[600.0, 480.0], [1100.0, 760.0], [1600.0, 1000.0]] {
            let (output, actions) = run_content_frame(
                &context,
                &mut layout,
                input(size, vec![]),
                Content::Analyzer(&mut panel, &view),
            );
            assert!(actions.is_empty());
            assert!(!context
                .tessellate(output.shapes, output.pixels_per_point)
                .is_empty());
        }
        assert_eq!(before, app.emu.machine_state_bytes().unwrap(), "{tab:?}");
    }
}

#[test]
fn analyzer_shortcuts_capture_once_and_pickers_track_resized_images() {
    let mut app = analyzer_app();
    let context = egui::Context::default();
    configure_style(&context);
    let mut layout = Layout::default();
    let mut panel = app.frame_analyzer_panel.clone().unwrap();
    let view = app.build_frame_analyzer_view(&panel);
    let (_, actions) = run_content_frame(
        &context,
        &mut layout,
        input(
            [1100.0, 760.0],
            vec![key(egui::Key::F, egui::Modifiers::NONE)],
        ),
        Content::Analyzer(&mut panel, &view),
    );
    assert_eq!(actions, [Action::AnalyzerKey(KeyCode::KeyF)]);
    let frame = app.emu.bus().emulated_frames();
    app.apply_egui_debugger_action(actions.into_iter().next().unwrap());
    assert_eq!(app.emu.bus().emulated_frames(), frame + 1);
    for size in [[600.0, 480.0], [1100.0, 760.0]] {
        for _ in 0..3 {
            let _ = run_content_frame(
                &context,
                &mut layout,
                input(size, vec![]),
                Content::Analyzer(&mut panel, &view),
            );
        }
        assert_eq!(context.viewport_rect().width(), size[0]);
        let rect = context
            .read_response(egui::Id::new("analyzer_beam_pick"))
            .unwrap()
            .rect;
        let point = rect.min + rect.size() * egui::vec2(0.75, 0.25);
        let _ = run_content_frame(
            &context,
            &mut layout,
            input(size, vec![egui::Event::PointerMoved(point)]),
            Content::Analyzer(&mut panel, &view),
        );
        let mut clicks = Vec::new();
        for pressed in [true, false] {
            let (_, actions) = run_content_frame(
                &context,
                &mut layout,
                input(
                    size,
                    vec![
                        egui::Event::PointerMoved(point),
                        egui::Event::PointerButton {
                            pos: point,
                            button: egui::PointerButton::Primary,
                            pressed,
                            modifiers: egui::Modifiers::NONE,
                        },
                    ],
                ),
                Content::Analyzer(&mut panel, &view),
            );
            clicks.extend(actions);
        }
        assert_eq!(
            clicks,
            [Action::Analyzer(UiControl::AnalyzerPick {
                x: 768,
                y: 256,
                scanline: false
            })],
            "window {size:?}, raster {rect:?}, pointer {point:?}"
        );
        for action in clicks {
            app.apply_egui_debugger_action(action);
        }
        let trace = view.trace.as_ref().unwrap();
        assert_eq!(
            app.frame_analyzer_panel.as_ref().unwrap().selected_hpos as usize,
            768 * trace.cols / 1024
        );
        assert_eq!(
            app.frame_analyzer_panel.as_ref().unwrap().selected_vpos as usize,
            256 * trace.rows / 1024
        );
    }
    app.frame_analyzer_set_tab(ui::AnalyzerTab::Resources);
    app.apply_egui_debugger_action(Action::ResourceScroll(
        ui::ANALYZER_RESOURCE_ROWS_MAX as isize,
    ));
    app.apply_egui_debugger_action(Action::Analyzer(UiControl::AnalyzerResourceRow(0)));
    let panel = app.frame_analyzer_panel.as_ref().unwrap();
    assert!(panel.resource_scroll > 0);
    assert_eq!(
        panel.resource_selected,
        Some(app.emu.uaelib_resources()[panel.resource_scroll].address)
    );
}

impl Offscreen {
    fn new(device: &wgpu::Device, width: u32, height: u32, scale: f32) -> Self {
        let renderer = egui_wgpu::Renderer::new(
            device,
            wgpu::TextureFormat::Rgba8UnormSrgb,
            egui_wgpu::RendererOptions {
                dithering: false,
                ..Default::default()
            },
        );
        let target = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("egui_debugger_preview"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        Self {
            renderer,
            target,
            prepared: None,
            screen: egui_wgpu::ScreenDescriptor {
                size_in_pixels: [width, height],
                pixels_per_point: scale,
            },
        }
    }

    fn draw(
        &mut self,
        context: &egui::Context,
        output: egui::FullOutput,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
    ) {
        PreparedFrame::replace(
            &mut self.prepared,
            &mut self.renderer,
            device,
            queue,
            context,
            output,
            egui_wgpu::ScreenDescriptor {
                size_in_pixels: self.screen.size_in_pixels,
                pixels_per_point: self.screen.pixels_per_point,
            },
        );
        self.redraw(device, queue);
    }

    fn redraw(&mut self, device: &wgpu::Device, queue: &wgpu::Queue) {
        let frame = self.prepared.as_ref().unwrap();
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        paint(
            &mut self.renderer,
            device,
            queue,
            &mut encoder,
            &self.target.create_view(&Default::default()),
            &frame.jobs,
            &frame.screen,
            true,
        );
        queue.submit([encoder.finish()]);
    }

    fn save(&self, device: &wgpu::Device, queue: &wgpu::Queue, path: &std::path::Path) {
        let [width, height] = self.screen.size_in_pixels;
        crate::screenshot::save(path, &self.pixels(device, queue), width, height).unwrap();
    }

    fn pixels(&self, device: &wgpu::Device, queue: &wgpu::Queue) -> Vec<u32> {
        let [width, height] = self.screen.size_in_pixels;
        let padded = (width * 4).div_ceil(256) * 256;
        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: u64::from(padded * height),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        encoder.copy_texture_to_buffer(
            self.target.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(padded),
                    rows_per_image: Some(height),
                },
            },
            self.target.size(),
        );
        queue.submit([encoder.finish()]);
        let (tx, rx) = std::sync::mpsc::channel();
        buffer
            .slice(..)
            .map_async(wgpu::MapMode::Read, move |result| {
                tx.send(result).unwrap();
            });
        device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
        rx.recv().unwrap().unwrap();
        let bytes = buffer.slice(..).get_mapped_range();
        bytes
            .chunks_exact(padded as usize)
            .flat_map(|row| {
                row[..width as usize * 4]
                    .chunks_exact(4)
                    .map(|p| u32::from_le_bytes(p.try_into().unwrap()))
            })
            .collect()
    }
}

/// Texture retirement must preserve both the first draw and cached redraws.
#[test]
#[ignore = "requires a hardware GPU to verify cached texture lifetime"]
fn retired_textures_survive_cached_redraws_until_replacement() {
    let gpu =
        super::super::crt_shader::test_gpu("egui_cached_texture").expect("hardware GPU required");
    let context = egui::Context::default();
    let texture = context.load_texture(
        "retired_image",
        egui::ColorImage::filled([1, 1], Color32::RED),
        egui::TextureOptions::NEAREST,
    );
    let id = texture.id();
    let mut handle = Some(texture);
    let output = context.run_ui(input([128.0, 128.0], vec![]), |ui| {
        ui.painter().image(
            id,
            egui::Rect::from_min_size(egui::pos2(32.0, 32.0), egui::vec2(32.0, 32.0)),
            egui::Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0)),
            Color32::WHITE,
        );
        drop(handle.take());
    });
    assert!(output.textures_delta.free.contains(&id));
    let mut offscreen = Offscreen::new(gpu.device(), 128, 128, 1.0);
    offscreen.draw(&context, output, gpu.device(), gpu.queue());
    for _ in 0..3 {
        assert!(offscreen.renderer.texture(&id).is_some());
        offscreen.redraw(gpu.device(), gpu.queue());
        assert_eq!(
            offscreen.pixels(gpu.device(), gpu.queue())[48 * 128 + 48],
            0xff00_00ff
        );
    }
    let output = context.run_ui(input([128.0, 128.0], vec![]), |_| {});
    offscreen.draw(&context, output, gpu.device(), gpu.queue());
    assert!(offscreen.renderer.texture(&id).is_none());
    assert_ne!(
        offscreen.pixels(gpu.device(), gpu.queue())[48 * 128 + 48],
        0xff00_00ff
    );
}

/// Reproducible review artifacts, without opening a host window. Requires a
/// hardware GPU; use --ignored --nocapture and inspect target/egui-debugger/.
#[test]
#[ignore = "writes GPU-rendered debugger previews for visual review"]
fn render_debugger_previews() {
    let gpu =
        super::super::crt_shader::test_gpu("egui_debugger_preview").expect("hardware GPU required");
    let mut app = test_app();
    app.open_debugger();
    let context = egui::Context::default();
    configure_style(&context);
    let mut layout = Layout::default();
    let mut panel = ui::DebuggerPanel::new();
    let mut offscreen = Offscreen::new(gpu.device(), 2200, 1520, 2.0);
    context.set_pixels_per_point(2.0);
    for tab in ui::DEBUG_TABS {
        panel.tab = tab;
        let view = app.build_debugger_view_with_clipping(&panel, false);
        for _ in 0..3 {
            let (output, actions) = run_frame(
                &context,
                &mut layout,
                input([1100.0, 760.0], vec![]),
                &mut panel,
                &view,
            );
            assert!(actions.is_empty());
            offscreen.draw(&context, output, gpu.device(), gpu.queue());
        }
        let path = format!("target/egui-debugger/{tab:?}.png");
        offscreen.save(gpu.device(), gpu.queue(), std::path::Path::new(&path));
        eprintln!("{path}");
    }
}

#[test]
#[ignore = "writes GPU-rendered Frame Analyzer previews for visual review"]
fn render_analyzer_previews() {
    let gpu =
        super::super::crt_shader::test_gpu("egui_analyzer_preview").expect("hardware GPU required");
    let mut app = analyzer_app();
    let context = egui::Context::default();
    configure_style(&context);
    context.set_pixels_per_point(2.0);
    let mut layout = Layout::default();
    let mut offscreen = Offscreen::new(gpu.device(), 2200, 1520, 2.0);
    for tab in ui::ANALYZER_TABS {
        app.frame_analyzer_set_tab(tab);
        app.frame_analyzer_select_resource(1);
        let mut panel = app.frame_analyzer_panel.clone().unwrap();
        let view = app.build_frame_analyzer_view(&panel);
        for _ in 0..3 {
            let (output, actions) = run_content_frame(
                &context,
                &mut layout,
                input([1100.0, 760.0], vec![]),
                Content::Analyzer(&mut panel, &view),
            );
            assert!(actions.is_empty());
            offscreen.draw(&context, output, gpu.device(), gpu.queue());
        }
        let path = format!("target/egui-debugger/Analyzer{tab:?}.png");
        offscreen.save(gpu.device(), gpu.queue(), std::path::Path::new(&path));
        eprintln!("{path}");
    }
}

/// Isolated repaint measurement: compares the legacy CPU panel raster/upload
/// with egui layout, tessellation, and GPU submission. It excludes view-data
/// collection, swapchain/vsync waits, and emulation, and is not an FPS claim.
#[test]
#[ignore = "release-mode repaint benchmark; needs a hardware GPU"]
fn benchmark_debugger_repaint() {
    let gpu = super::super::crt_shader::test_gpu("egui_debugger_benchmark")
        .expect("hardware GPU required");
    let app = test_app();
    let context = egui::Context::default();
    configure_style(&context);
    context.set_pixels_per_point(2.0);
    let mut layout = Layout::default();
    let mut panel = ui::DebuggerPanel::new();
    let modern = app.build_debugger_view_with_clipping(&panel, false);
    let classic = ui::PanelViewData::Debugger(Box::new(app.build_debugger_view(&panel)));
    let classic_panel = ui::Panel::Debugger(panel.clone());
    let width = super::super::texture_width(2) as u32;
    let height = super::super::texture_height(2) as u32;
    let mut pixels = vec![0u8; width as usize * height as usize * 4];
    let classic_texture = gpu.device().create_texture(&wgpu::TextureDescriptor {
        label: None,
        size: wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8UnormSrgb,
        usage: wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    let mut offscreen = Offscreen::new(gpu.device(), 2200, 1520, 2.0);
    let mut classic_times = Vec::new();
    let mut egui_times = Vec::new();
    for round in 0..220 {
        for modern_first in [round % 2 == 0, round % 2 != 0] {
            gpu.device()
                .poll(wgpu::PollType::wait_indefinitely())
                .unwrap();
            let start = Instant::now();
            if modern_first {
                let (output, _) = run_frame(
                    &context,
                    &mut layout,
                    input([1100.0, 760.0], vec![]),
                    &mut panel,
                    &modern,
                );
                offscreen.draw(&context, output, gpu.device(), gpu.queue());
            } else {
                pixels.fill(0);
                ui::draw_panel_layer(&mut pixels, 2, &classic_panel, None, Some(&classic));
                gpu.queue().write_texture(
                    classic_texture.as_image_copy(),
                    &pixels,
                    wgpu::TexelCopyBufferLayout {
                        offset: 0,
                        bytes_per_row: Some(width * 4),
                        rows_per_image: Some(height),
                    },
                    classic_texture.size(),
                );
                gpu.queue().submit([]);
            }
            let elapsed = start.elapsed().as_secs_f64() * 1000.0;
            if round >= 20 {
                if modern_first {
                    egui_times.push(elapsed);
                } else {
                    classic_times.push(elapsed);
                }
            }
        }
    }
    classic_times.sort_by(f64::total_cmp);
    egui_times.sort_by(f64::total_cmp);
    eprintln!("CPU repaint host submission, 200 alternating pairs, 2x DPI: classic median {:.3} ms, p95 {:.3} ms; egui median {:.3} ms, p95 {:.3} ms",
        classic_times[100], classic_times[190], egui_times[100], egui_times[190]);
}

fn close_all_inspectors(app: &mut App) {
    for kind in ToolPanelKind::ALL {
        app.close_tool_panel(kind);
    }
}

#[test]
fn play_and_debug_preserve_panels_capture_and_explicit_run_state() {
    let mut app = analyzer_app();
    app.open_console();
    app.console_panel.as_mut().unwrap().input = "status".into();
    app.open_debugger();
    app.debugger_panel.as_mut().unwrap().tab = ui::DebugTab::Audio;
    app.frame_analyzer_panel.as_mut().unwrap().selected_hpos = 96;
    let capture = app.emu.bus().frame_bus_trace().unwrap().frame;
    for paused in [true, false] {
        app.paused = paused;
        let before = app.emu.machine_state_bytes().unwrap();
        for _ in 0..3 {
            app.apply_egui_debugger_action(Action::CloseWorkspace);
            assert!(!app.debug_layout_active);
            assert!(app.topmost_tool_panel().is_none());
            assert_eq!(app.paused, paused);
            app.toggle_debugger();
            assert!(app.debug_layout_active);
            assert_eq!(app.paused, paused);
            assert_eq!(
                app.debugger_panel.as_ref().unwrap().tab,
                ui::DebugTab::Audio
            );
            assert_eq!(app.console_panel.as_ref().unwrap().input, "status");
            assert_eq!(app.frame_analyzer_panel.as_ref().unwrap().selected_hpos, 96);
            assert_eq!(app.emu.bus().frame_bus_trace().unwrap().frame, capture);
            assert_eq!(app.emu.machine_state_bytes().unwrap(), before);
        }
    }
}

#[test]
fn debug_input_focus_blocks_guest_qualifiers_and_releases_held_input() {
    use winit::{
        event::{ElementState, RawKeyEvent},
        keyboard::PhysicalKey,
    };
    let mut app = test_app();
    app.main_window_focused = true;
    app.open_debugger();
    let shift = || RawKeyEvent {
        physical_key: PhysicalKey::Code(KeyCode::ShiftLeft),
        state: ElementState::Pressed,
    };
    let before = app.emu.machine_state_bytes().unwrap();
    app.handle_raw_device_key_event(shift());
    assert!(!app.amiga_rawkey_held(0x60));
    assert_eq!(app.emu.machine_state_bytes().unwrap(), before);
    app.capture_debug_guest_input();
    assert!(app.debug_guest_input);
    app.handle_raw_device_key_event(shift());
    assert!(app.amiga_rawkey_held(0x60));
    app.release_debug_guest_input();
    assert!(!app.debug_guest_input);
    assert!(!app.amiga_rawkey_held(0x60));
    assert!(app.raw_device_held_rawkeys.iter().all(|held| !held));
    app.handle_raw_device_key_event(shift());
    assert!(!app.amiga_rawkey_held(0x60));
}

#[test]
fn host_shortcuts_remain_available_without_stealing_text_edits() {
    use winit::keyboard::ModifiersState;
    let host = if cfg!(target_os = "macos") {
        ModifiersState::SUPER
    } else {
        ModifiersState::ALT
    };
    let routes = workspace::host_shortcut_reaches_main;
    for code in [
        KeyCode::KeyA,
        KeyCode::KeyB,
        KeyCode::KeyD,
        KeyCode::KeyE,
        KeyCode::KeyF,
        KeyCode::KeyJ,
        KeyCode::KeyK,
        KeyCode::KeyM,
        KeyCode::KeyP,
        KeyCode::KeyQ,
        KeyCode::KeyR,
        KeyCode::KeyS,
        KeyCode::KeyW,
        KeyCode::KeyZ,
        KeyCode::Digit0,
        KeyCode::Digit1,
        KeyCode::Digit2,
        KeyCode::Digit3,
        KeyCode::Digit4,
        KeyCode::Digit5,
        KeyCode::Digit6,
        KeyCode::Digit7,
        KeyCode::Digit8,
        KeyCode::Digit9,
    ] {
        for shift in [ModifiersState::empty(), ModifiersState::SHIFT] {
            assert!(routes(code, host | shift, false), "{code:?}");
            let text_edit =
                cfg!(target_os = "macos") && matches!(code, KeyCode::KeyA | KeyCode::KeyZ);
            assert_eq!(routes(code, host | shift, true), !text_edit, "{code:?}");
        }
        assert!(!routes(code, ModifiersState::empty(), false));
    }
    for code in [
        KeyCode::KeyL,
        KeyCode::Equal,
        KeyCode::Minus,
        KeyCode::Period,
        KeyCode::Comma,
    ] {
        assert!(routes(code, host | ModifiersState::SHIFT, false));
        assert!(!routes(code, host, false));
    }
    for code in [
        KeyCode::KeyC,
        KeyCode::KeyV,
        KeyCode::KeyX,
        KeyCode::KeyY,
        KeyCode::ArrowLeft,
    ] {
        for editing in [true, false] {
            assert!(!routes(code, host, editing), "{code:?}");
        }
    }
}

#[test]
fn debug_display_stays_clear_of_inspectors_and_clicks_only_transfer_input() {
    let mut app = test_app();
    let before = app.emu.machine_state_bytes().unwrap();
    for width in [900.0, 1440.0, 1920.0] {
        let context = egui::Context::default();
        configure_style(&context);
        let mut layout = Layout {
            workspace: true,
            ..Default::default()
        };
        let mut panel = ui::DebuggerPanel::new();
        let mut baseline = None;
        for tab in ui::DEBUG_TABS {
            panel.tab = tab;
            let mut view = app.build_debugger_view_with_clipping(&panel, false);
            view.status = if tab == ui::DebugTab::Audio {
                "running frame 999999 19999.98s | pos 123456789 rev 999 snaps, 512 MB".into()
            } else {
                "paused frame 0".into()
            };
            let mut output = None;
            for _ in 0..3 {
                let (frame, actions) = run_frame(
                    &context,
                    &mut layout,
                    input([width, 900.0], vec![]),
                    &mut panel,
                    &view,
                );
                assert!(actions.is_empty());
                output = Some(frame);
            }
            let rect = layout.display_rect.unwrap();
            assert!(rect.width() >= 200.0 && rect.height() >= 600.0);
            assert!(rect.right() <= width - 450.0);
            if let Some(first) = baseline {
                assert_eq!(rect, first);
            } else {
                baseline = Some(rect);
            }
            for shape in output.unwrap().shapes {
                if let egui::Shape::Rect(painted) = shape.shape {
                    if painted.fill != egui::Color32::TRANSPARENT {
                        assert!(
                            !painted
                                .rect
                                .intersect(shape.clip_rect)
                                .intersects(rect.shrink(2.0)),
                            "{tab:?}: UI background covers display: {:?}",
                            painted.rect
                        );
                    }
                }
            }
        }
        let pos = layout.display_rect.unwrap().center();
        let view = app.build_debugger_view_with_clipping(&panel, false);
        let mut clicked = Vec::new();
        for pressed in [true, false] {
            let (_, actions) = run_frame(
                &context,
                &mut layout,
                input(
                    [width, 900.0],
                    vec![
                        egui::Event::PointerMoved(pos),
                        egui::Event::PointerButton {
                            pos,
                            button: egui::PointerButton::Primary,
                            pressed,
                            modifiers: egui::Modifiers::NONE,
                        },
                    ],
                ),
                &mut panel,
                &view,
            );
            clicked.extend(actions);
        }
        assert_eq!(clicked, [Action::CaptureDisplay]);
    }
    assert_eq!(app.emu.machine_state_bytes().unwrap(), before);
}

#[test]
fn debug_viewport_uses_the_same_pixel_rect_for_picture_and_input() {
    use super::super::{debug_present_layout, DisplaySrc};
    use winit::dpi::PhysicalPosition;
    for scale in [1, 2, 3] {
        let viewport = (17 * scale, 73 * scale, 600 * scale, 500 * scale);
        for integer in [false, true] {
            let layout = debug_present_layout(
                viewport,
                integer,
                Some(DisplaySrc {
                    rect: (24, 18, 320, 200),
                    par: (1, 1),
                }),
            );
            let (x, y, w, h) = layout.display_dst;
            assert!(x >= viewport.0 && y >= viewport.1);
            assert!(x + w <= viewport.0 + viewport.2 && y + h <= viewport.1 + viewport.3);
            assert!(layout.chrome_dst.is_none());
            assert_eq!(
                layout.cursor_position(PhysicalPosition::new(x as f64, y as f64)),
                Some((24, 18))
            );
            assert_eq!(
                layout.cursor_position(PhysicalPosition::new(
                    (x + w - 1) as f64,
                    (y + h - 1) as f64
                )),
                Some((343, 217))
            );
            assert!(layout
                .cursor_position(PhysicalPosition::new((x + w) as f64, y as f64))
                .is_none());
            assert!(layout
                .cursor_position(PhysicalPosition::new((x - 1) as f64, y as f64))
                .is_none());
        }
    }
}
