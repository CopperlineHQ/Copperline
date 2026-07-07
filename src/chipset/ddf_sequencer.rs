//! Agnus bitplane DDF sequencer: the per-colour-clock start/stop flop model.
//!
//! The bitplane fetch window is NOT a simple [DDFSTRT, DDFSTOP] value range.
//! Agnus runs a small synchronous state machine on comparator EDGES: DDFSTRT
//! and DDFSTOP matches set/clear flip-flops, the hardwired window ($18/$D8)
//! gates and force-stops runs, a stop request drains through one final fetch
//! unit (which applies the modulos), and the whole state carries across line
//! boundaries. Missed comparators (values rewritten too late, or values that
//! never match) therefore produce fetch runs that a value-range model cannot
//! express: runs to the hardware stop, runs that wrap through horizontal
//! blanking into the next line, and lines with no run at all.
//!
//! The flop semantics are transcribed from vAmiga 4.4's Sequencer (OCS and
//! ECS variants, hardware-verified by the vAmigaTS Agnus/DDF suite). The
//! aggregate behaviour is pinned to real hardware by the
//! Agnus/DDF/DDF/oldhwstop1-4 A500 photos: the colour-swatch band below the
//! experiment rows encodes every preceding row's fetched word count through
//! the bitplane pointer progression, and the photos match this model's
//! output.
//!
//! This module is deliberately free-standing (no Bus/Agnus state): callers
//! feed a signal list for one line and the carried [`DdfState`], and receive
//! the per-cck fetch events.

/// One horizontal line's colour-clock count is supplied by the caller (PAL
/// 227; programmable modes differ). The hardwired fetch window is fixed.
pub const DDF_HARD_START_CCK: u16 = 0x18;
pub const DDF_HARD_STOP_CCK: u16 = 0xD8;

/// Signal bits, mirroring the hardware comparator strobes. Multiple signals
/// can coincide on one colour clock (e.g. DDFSTRT == DDFSTOP), which the
/// flop logic decodes as a distinct case.
pub mod sig {
    pub const SHW: u32 = 1 << 0;
    pub const RHW: u32 = 1 << 1;
    pub const BPHSTART: u32 = 1 << 2;
    pub const BPHSTOP: u32 = 1 << 3;
    pub const BMAPEN_CLR: u32 = 1 << 4;
    pub const BMAPEN_SET: u32 = 1 << 5;
    pub const VFLOP_SET: u32 = 1 << 6;
    pub const VFLOP_CLR: u32 = 1 << 7;
    pub const CON: u32 = 1 << 8;
    pub const DONE: u32 = 1 << 9;
}

/// A signal strobe at a colour clock. `bplcon0` carries the new control
/// value for `sig::CON` strobes (BPLCON0 writes reaching Agnus).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DdfSignal {
    pub cck: u16,
    pub bits: u32,
    pub bplcon0: u16,
}

/// The sequencer flip-flops. Carried across line boundaries; a line's walk
/// starts from the previous line's final state.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct DdfState {
    /// Vertical DIW flip-flop (bitplane DMA enabled vertically).
    pub bpv: bool,
    /// DMACON master+bitplane DMA enable.
    pub bmapen: bool,
    /// Past the hardwired start ($18). OCS: cleared when a fetch unit
    /// completes; ECS: cleared at end of line.
    pub shw: bool,
    /// Past the hardwired stop ($D8).
    pub rhw: bool,
    /// DDFSTRT comparator flip-flop.
    pub bphstart: bool,
    /// DDFSTOP comparator flip-flop.
    pub bphstop: bool,
    /// Bitplane fetch running.
    pub bprun: bool,
    /// The final fetch unit (modulos apply) is in progress.
    pub last_fu: bool,
    /// A stop was requested; honoured at the next fetch-unit boundary.
    pub stopreq: bool,
    /// Fetch-unit position counter (2 colour clocks per step, 4 steps).
    pub cnt: u8,
    /// The BPLCON0 value the sequencer currently sees.
    pub bplcon0: u16,
}

/// One bitplane fetch slot produced by the walk.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DdfFetch {
    pub cck: u16,
    /// 0-based plane index.
    pub plane: u8,
    /// Fetch-unit ordinal within the line's run(s): which 8-cck unit this
    /// slot belongs to, counting units the sequencer actually ran. Word
    /// addressing is unit-based on the hardware, so a plane enabled
    /// mid-line keeps fetching at its unit's word position.
    pub unit_ord: u16,
    /// Unit offset of the slot (0..7), for sub-unit word placement in
    /// hires/SHRES units.
    pub counter: u8,
    /// This is the plane's fetch in the final unit: the plane's modulo is
    /// added after the word (BPLxMOD).
    pub apply_modulo: bool,
}

fn plane_count(bplcon0: u16, aga: bool) -> u8 {
    crate::chipset::agnus::bitplane_dma_planes(bplcon0, aga) as u8
}

/// Per-unit fetch layout: `slots[counter]` = Some(plane) when a DMA slot for
/// that plane sits at unit offset `counter` (0..7 colour clocks; hires
/// fetches two words per unit per plane; SHRES four).
fn unit_slot(bplcon0: u16, aga: bool, counter: u8) -> Option<u8> {
    let planes = plane_count(bplcon0, aga);
    let has = |p: u8| -> Option<u8> { (planes >= p).then_some(p - 1) };
    if crate::chipset::agnus::bitplane_shres(bplcon0) {
        match counter & 1 {
            0 => has(2),
            _ => has(1),
        }
    } else if crate::chipset::agnus::bitplane_hires(bplcon0) {
        match counter & 3 {
            0 => has(4),
            1 => has(2),
            2 => has(3),
            _ => has(1),
        }
    } else {
        match counter {
            1 => has(4),
            2 => has(6),
            3 => has(2),
            5 => has(3),
            6 => has(5),
            7 => has(1),
            _ => None,
        }
    }
}

/// Whether a fetch at unit offset `counter` in the final unit is the plane's
/// last of that unit (modulo applies). Lores planes fetch once per unit
/// (always last); hires planes fetch twice (second half is last); SHRES four
/// times (last quarter).
fn modulo_slot(bplcon0: u16, counter: u8) -> bool {
    if crate::chipset::agnus::bitplane_shres(bplcon0) {
        counter >= 6
    } else if crate::chipset::agnus::bitplane_hires(bplcon0) {
        counter >= 4
    } else {
        true
    }
}

/// Emulate the flop updates for one signal strobe. Transcribed from
/// vAmiga's `Sequencer::processSignal` (OCS and ECS variants).
fn process_signal(ecs: bool, signal: &DdfSignal, state: &mut DdfState) {
    let bits = signal.bits;

    if bits & sig::CON != 0 {
        state.bplcon0 = signal.bplcon0;
    }

    if ecs {
        process_signal_ecs(bits, state);
    } else {
        process_signal_ocs(bits, state);
    }
}

fn process_signal_ocs(bits: u32, state: &mut DdfState) {
    match bits & (sig::BMAPEN_CLR | sig::BMAPEN_SET) {
        x if x == sig::BMAPEN_CLR => {
            state.bmapen = false;
            state.bprun = false;
            state.cnt = 0;
        }
        x if x == sig::BMAPEN_SET => {
            state.bmapen = true;
        }
        _ => {}
    }
    match bits & (sig::VFLOP_SET | sig::VFLOP_CLR) {
        x if x == sig::VFLOP_SET => {
            state.bpv = true;
        }
        x if x == sig::VFLOP_CLR => {
            state.bpv = false;
            state.bprun = false;
            state.cnt = 0;
        }
        _ => {}
    }
    match bits & (sig::SHW | sig::RHW) {
        x if x == sig::SHW => {
            state.shw = true;
        }
        x if x == sig::RHW => {
            state.rhw |= state.bprun;
            state.stopreq |= state.bprun;
        }
        _ => {}
    }
    match bits & (sig::BPHSTART | sig::BPHSTOP) {
        x if x == sig::BPHSTART | sig::BPHSTOP => {
            if state.bprun {
                state.bphstart &= !state.bprun;
                state.bphstop |= state.bprun;
                state.stopreq |= state.bprun;
            } else {
                state.bphstart = state.bphstart || state.shw;
                state.bprun = (state.bprun || state.shw) && state.bpv && state.bmapen;
            }
        }
        x if x == sig::BPHSTART => {
            state.bphstart |= state.shw && state.bmapen;
            state.bprun = (state.bprun || state.shw) && state.bpv && state.bmapen;
        }
        x if x == sig::BPHSTOP => {
            state.bphstart &= !state.bprun;
            state.bphstop |= state.bprun;
            state.stopreq |= state.bprun;
        }
        _ => {}
    }
    if bits & sig::DONE != 0 {
        state.rhw = false;
        state.stopreq = false;
    }
}

fn process_signal_ecs(bits: u32, state: &mut DdfState) {
    match bits & (sig::VFLOP_SET | sig::VFLOP_CLR) {
        x if x == sig::VFLOP_SET => {
            state.bpv = true;
        }
        x if x == sig::VFLOP_CLR => {
            state.bpv = false;
            state.bprun = false;
            state.cnt = 0;
        }
        _ => {}
    }
    match bits & (sig::SHW | sig::RHW) {
        x if x == sig::SHW => {
            state.shw = true;
            state.bprun |= state.bphstart && bits & sig::BPHSTOP == 0;
        }
        x if x == sig::RHW => {
            state.rhw = true;
            state.stopreq |= state.bprun;
        }
        _ => {}
    }
    match bits & (sig::BPHSTART | sig::BPHSTOP | sig::SHW | sig::RHW) {
        x if x == sig::BPHSTART | sig::BPHSTOP | sig::SHW => {
            state.bphstart = true;
            state.bprun = (state.bprun || state.shw) && state.bpv && state.bmapen;
        }
        x if x == sig::BPHSTART | sig::BPHSTOP | sig::RHW => {
            state.bphstop |= state.bprun;
            state.stopreq |= state.bprun;
            state.bphstart = true;
        }
        x if x == sig::BPHSTART | sig::BPHSTOP => {
            state.bphstop |= state.bprun;
            state.stopreq |= state.bprun;
            // vAmiga: "likely fix for test case arosddf2 and arosddf4".
            state.bphstart = state.bpv;
            state.bprun = (state.bprun || state.shw) && state.bpv && state.bmapen;
        }
        x if x == sig::BPHSTART
            || x == sig::BPHSTART | sig::SHW
            || x == sig::BPHSTART | sig::RHW =>
        {
            state.bphstart = true;
            state.bprun = (state.bprun || state.shw) && state.bpv && state.bmapen;
        }
        x if x == sig::BPHSTOP || x == sig::BPHSTOP | sig::SHW || x == sig::BPHSTOP | sig::RHW => {
            state.bphstart = false;
            state.bphstop |= state.bprun;
            state.stopreq |= state.bprun;
        }
        _ => {}
    }
    match bits & (sig::BMAPEN_CLR | sig::BMAPEN_SET) {
        x if x == sig::BMAPEN_CLR => {
            state.bmapen = false;
            state.bprun = false;
            state.cnt = 0;
        }
        x if x == sig::BMAPEN_SET => {
            state.bmapen = true;
            state.bprun = (state.bprun || state.shw) && state.bpv && state.bphstart;
        }
        _ => {}
    }
    if bits & sig::DONE != 0 {
        state.rhw = false;
        state.shw = false;
        state.bphstop = false;
    }
}

/// Emulate the fetch logic for colour clocks `[start, stop)`, appending
/// produced DMA slots. Transcribed from vAmiga's
/// `Sequencer::computeBplEvents`.
fn walk_span(
    aga: bool,
    ecs: bool,
    start: u16,
    stop: u16,
    state: &mut DdfState,
    unit_ord: &mut Option<u16>,
    fetches: &mut Vec<DdfFetch>,
) {
    for j in start..stop {
        let counter = (state.cnt << 1) | (j & 1) as u8;

        if counter == 0 {
            if state.last_fu {
                state.bprun = false;
                state.last_fu = false;
                state.bphstop = false;
                if !ecs {
                    state.shw = false;
                }
            }
            if state.stopreq {
                state.stopreq = false;
                state.last_fu = true;
            }
            if state.bprun {
                *unit_ord = Some(match *unit_ord {
                    Some(ord) => ord.saturating_add(1),
                    None => 0,
                });
            }
        }

        if state.bprun {
            if let Some(plane) = unit_slot(state.bplcon0, aga, counter) {
                fetches.push(DdfFetch {
                    cck: j,
                    plane,
                    unit_ord: unit_ord.unwrap_or(0),
                    counter,
                    apply_modulo: state.last_fu && modulo_slot(state.bplcon0, counter),
                });
            }
            if j & 1 == 1 {
                state.cnt = (state.cnt + 1) & 3;
            }
        } else {
            state.cnt = 0;
        }
    }
}

/// Build the default signal list for a line with static register values.
/// Mid-line register writes append extra signals via `extra` (already
/// positioned at the colour clock where the write reaches the sequencer);
/// same-cck signals are merged like the hardware strobes.
pub fn line_signals(
    ddfstrt: u16,
    ddfstop: u16,
    line_ccks: u16,
    extra: &[DdfSignal],
) -> Vec<DdfSignal> {
    line_signals_with_hard_stop(ddfstrt, ddfstop, DDF_HARD_STOP_CCK, line_ccks, extra)
}

/// [`line_signals`] with a caller-supplied hardware-stop position
/// (BEAMCON0.HARDDIS relaxes the hardwired stop).
pub fn line_signals_with_hard_stop(
    ddfstrt: u16,
    ddfstop: u16,
    hard_stop_cck: u16,
    line_ccks: u16,
    extra: &[DdfSignal],
) -> Vec<DdfSignal> {
    let mut signals: Vec<DdfSignal> = Vec::with_capacity(5 + extra.len());
    let mut push = |cck: u16, bits: u32, bplcon0: u16| {
        if let Some(existing) = signals.iter_mut().find(|s| s.cck == cck) {
            existing.bits |= bits;
            if bits & sig::CON != 0 {
                existing.bplcon0 = bplcon0;
            }
            return;
        }
        signals.push(DdfSignal { cck, bits, bplcon0 });
    };
    push(DDF_HARD_START_CCK, sig::SHW, 0);
    if ddfstrt < line_ccks {
        push(ddfstrt, sig::BPHSTART, 0);
    }
    if ddfstop < line_ccks {
        push(ddfstop, sig::BPHSTOP, 0);
    }
    push(hard_stop_cck, sig::RHW, 0);
    for s in extra {
        push(s.cck, s.bits, s.bplcon0);
    }
    push(line_ccks, sig::DONE, 0);
    signals.sort_by_key(|s| s.cck);
    signals
}

/// Walk one full line. `state` carries across lines: pass the previous
/// line's final state (with `bpv`/`bmapen`/`bplcon0` refreshed by the caller
/// for line-granular changes) and receive this line's final state in place.
pub fn walk_line(
    aga: bool,
    ecs: bool,
    signals: &[DdfSignal],
    state: &mut DdfState,
) -> Vec<DdfFetch> {
    let mut fetches = Vec::new();
    let mut cycle = 0u16;
    let mut unit_ord: Option<u16> = None;
    for signal in signals {
        walk_span(
            aga,
            ecs,
            cycle,
            signal.cck,
            state,
            &mut unit_ord,
            &mut fetches,
        );
        process_signal(ecs, signal, state);
        if signal.bits & sig::DONE != 0 {
            break;
        }
        cycle = signal.cck;
    }
    fetches
}

#[cfg(test)]
mod tests {
    use super::*;

    const LINE: u16 = 227;

    fn lores4() -> u16 {
        0x4200 // 4 planes, lores, COLOR on
    }

    fn ready_state(bplcon0: u16) -> DdfState {
        DdfState {
            bpv: true,
            bmapen: true,
            bplcon0,
            ..DdfState::default()
        }
    }

    fn words_for_plane(fetches: &[DdfFetch], plane: u8) -> usize {
        fetches.iter().filter(|f| f.plane == plane).count()
    }

    fn walk_static(ecs: bool, ddfstrt: u16, ddfstop: u16, state: &mut DdfState) -> Vec<DdfFetch> {
        let signals = line_signals(ddfstrt, ddfstop, LINE, &[]);
        walk_line(false, ecs, &signals, state)
    }

    #[test]
    fn standard_lores_window_fetches_twenty_words_after_stop_drain() {
        // $38/$D0: the stop request lands at $D0 (a unit boundary), the
        // final unit drains with modulos: 20 words per plane, plane 1's
        // last fetch at $D7.
        for ecs in [false, true] {
            let mut state = ready_state(lores4());
            let fetches = walk_static(ecs, 0x38, 0xD0, &mut state);
            assert_eq!(words_for_plane(&fetches, 0), 20, "ecs={ecs}");
            assert_eq!(
                fetches.first().map(|f| f.cck),
                Some(0x39),
                "first slot (plane 4) one cck into the unit; ecs={ecs}"
            );
            assert_eq!(fetches.last().map(|f| (f.cck, f.plane)), Some((0xD7, 0)));
            assert!(!state.bprun, "run stops before the line ends; ecs={ecs}");
            let mods: Vec<_> = fetches.iter().filter(|f| f.apply_modulo).collect();
            assert_eq!(mods.len(), 4, "each plane takes its modulo once");
            assert!(mods.iter().all(|f| f.cck >= 0xD0));
        }
    }

    #[test]
    fn missed_stop_runs_to_the_hardware_stop() {
        // DDFSTOP that never matches ($FF is beyond the line): RHW at $D8
        // requests the stop, one further unit drains with modulos.
        for ecs in [false, true] {
            let mut state = ready_state(lores4());
            let fetches = walk_static(ecs, 0x38, 0xFF, &mut state);
            // Units at $38..$D0 = 20, then the $D8 unit drains as the final
            // unit (the RHW strobe lands exactly on its boundary): 21 words.
            assert_eq!(words_for_plane(&fetches, 0), 21, "ecs={ecs}");
            assert_eq!(fetches.last().map(|f| f.cck), Some(0xDF), "ecs={ecs}");
            assert!(!state.bprun, "ecs={ecs}");
        }
    }

    #[test]
    fn missed_start_produces_no_fetch_on_ocs() {
        let mut state = ready_state(lores4());
        let fetches = walk_static(false, 0xFF, 0xA0, &mut state);
        assert!(fetches.is_empty());
        assert!(!state.bprun);
    }

    #[test]
    fn ecs_latched_start_restarts_at_the_hard_window() {
        // ECS: BPHSTART is a latch surviving the line end. A start that
        // matched on an earlier line keeps starting runs at SHW ($18) even
        // when DDFSTRT never matches again.
        let mut state = ready_state(lores4());
        state.bphstart = true;
        let fetches = walk_static(true, 0xFF, 0xA0, &mut state);
        assert!(!fetches.is_empty());
        assert_eq!(
            fetches.first().map(|f| f.cck),
            Some(0x19),
            "run starts at the hard window start"
        );
        // The $A0 stop still matches and drains the run.
        assert!(fetches.last().map(|f| f.cck).unwrap() < 0xB0);
    }

    #[test]
    fn ocs_start_flop_does_not_restart_without_a_match() {
        // Same scenario on OCS: the latched BPHSTART flop alone does not
        // start a run; OCS needs the comparator edge.
        let mut state = ready_state(lores4());
        state.bphstart = true;
        let fetches = walk_static(false, 0xFF, 0xA0, &mut state);
        assert!(fetches.is_empty());
    }

    #[test]
    fn late_start_past_the_hard_stop_wraps_into_the_next_line_on_ocs() {
        // A DDFSTRT match after $D8 starts a run that the missed RHW can no
        // longer stop: fetching continues through the line end, wraps into
        // the next line (through horizontal blanking) and stops after the
        // next line's $D8 drain.
        let mut state = ready_state(lores4());
        let fetches = walk_static(false, 0xE0, 0xFF, &mut state);
        assert!(!fetches.is_empty());
        assert!(state.bprun, "run carries across the line boundary");

        let next = walk_static(false, 0xE0, 0xFF, &mut state);
        assert_eq!(
            next.first().map(|f| f.cck),
            Some(0x01),
            "the carried run fetches from the start of the next line"
        );
        // ($E0 matches again on this line while the run is already up, and
        // the next-line stop drain repeats; the run keeps cycling.)
        assert!(words_for_plane(&next, 0) > 20);
    }

    #[test]
    fn stop_without_running_fetch_is_ignored() {
        for ecs in [false, true] {
            let mut state = ready_state(lores4());
            let fetches = walk_static(ecs, 0xFF, 0xA0, &mut state);
            assert!(!state.bphstop, "stop flop only latches while running");
            let _ = fetches;
        }
    }

    #[test]
    fn dma_disabled_produces_no_fetches() {
        for ecs in [false, true] {
            let mut state = ready_state(lores4());
            state.bmapen = false;
            let fetches = walk_static(ecs, 0x38, 0xD0, &mut state);
            assert!(fetches.is_empty(), "ecs={ecs}");
        }
    }

    #[test]
    fn vertical_flop_off_produces_no_fetches() {
        for ecs in [false, true] {
            let mut state = ready_state(lores4());
            state.bpv = false;
            let fetches = walk_static(ecs, 0x38, 0xD0, &mut state);
            assert!(fetches.is_empty(), "ecs={ecs}");
        }
    }

    #[test]
    fn hires_window_fetches_two_words_per_unit() {
        // Standard hires $3C/$D4: 8-cck units carry two words per plane.
        let mut state = ready_state(0x8200 | 0x4000); // hires es, 4 planes
        let fetches = walk_static(true, 0x3C, 0xD4, &mut state);
        let plane0 = words_for_plane(&fetches, 0);
        assert_eq!(plane0 % 2, 0);
        assert_eq!(plane0, 40, "20 units, two words per unit");
    }

    #[test]
    fn equal_start_and_stop_stops_a_running_fetch_from_a_prior_line() {
        // DDFSTRT == DDFSTOP: the combined strobe requests a stop when a
        // run is up, and starts one otherwise (OCS).
        let mut state = ready_state(lores4());
        let signals = line_signals(0x60, 0x60, LINE, &[]);
        let fetches = walk_line(false, false, &signals, &mut state);
        // Starts at $60 (no run was up), then RHW stops it.
        assert!(!fetches.is_empty());
        assert_eq!(fetches.first().map(|f| f.cck), Some(0x61));
    }

    #[test]
    fn ocs_start_below_hard_window_runs_on_alternating_lines_from_its_raw_grid() {
        // DDFSTRT below the hardwired start ($10 < $18): the comparator
        // fires while SHW is still down, so a fresh line starts no run. SHW
        // set at $18 survives the line end on OCS (only a completed fetch
        // run clears it), so the NEXT line's $10 match arms a run anchored
        // at the raw $10 grid; the missed DDFSTOP leaves it to the RHW
        // drain ($D8 unit). The run then alternates: line with run clears
        // SHW, line without re-arms it. Hardware-verified by the vAmigaTS
        // Agnus/DDF oldhwstop3/4 A500 photos (via the vAmiga sequencer).
        let mut state = ready_state(lores4());
        // Preceding line with a standard window: its completed run leaves
        // SHW cleared.
        let _ = walk_static(false, 0x60, 0xA0, &mut state);
        assert!(!state.shw);

        let first = walk_static(false, 0x10, 0x10, &mut state);
        assert!(first.is_empty(), "fresh line: SHW still down at $10");
        assert!(state.shw, "SHW armed at $18 survives the line end");

        let second = walk_static(false, 0x10, 0x10, &mut state);
        assert_eq!(
            second.first().map(|f| f.cck),
            Some(0x11),
            "run anchors at the raw $10 unit, not the hard start"
        );
        assert_eq!(second.last().map(|f| f.cck), Some(0xDF), "RHW drain");
        assert_eq!(words_for_plane(&second, 0), 26);
        assert!(!state.shw, "the completed run clears SHW again");

        let third = walk_static(false, 0x10, 0x10, &mut state);
        assert!(third.is_empty(), "alternating lines stay empty");

        // A reachable DDFSTOP still ends the armed run at its position.
        let stopped = walk_static(false, 0x10, 0xA0, &mut state);
        assert_eq!(stopped.first().map(|f| f.cck), Some(0x11));
        assert_eq!(stopped.last().map(|f| f.cck), Some(0xA7));
        assert_eq!(words_for_plane(&stopped, 0), 19);
    }

    #[test]
    fn ecs_start_below_hard_window_latches_and_runs_every_line_from_shw() {
        // Same registers on ECS: BPHSTART is a latch, so the $10 match arms
        // it every line and the run starts at the hard window ($18) on
        // every line (SHW is cleared at each line end on ECS).
        let mut state = ready_state(lores4());
        let _ = walk_static(true, 0x60, 0xA0, &mut state);

        for line in 0..3 {
            let fetches = walk_static(true, 0x10, 0xA0, &mut state);
            assert_eq!(
                fetches.first().map(|f| f.cck),
                Some(0x19),
                "line {line}: run starts at the hard window start"
            );
            assert_eq!(fetches.last().map(|f| f.cck), Some(0xA7), "line {line}");
        }
    }

    #[test]
    fn mid_line_stop_rewrite_before_match_moves_the_stop() {
        // A DDFSTOP rewrite landing before the old value matches replaces
        // the stop position: the walk uses the merged signal list.
        let mut state = ready_state(lores4());
        let extra = [DdfSignal {
            cck: 0x80,
            bits: sig::BPHSTOP,
            bplcon0: 0,
        }];
        // Old stop $D0 removed by the caller; new stop $80 as an extra.
        let signals = line_signals(0x38, 0xFF, LINE, &extra);
        let fetches = walk_line(false, false, &signals, &mut state);
        let last = fetches.last().unwrap().cck;
        assert!(
            last < 0x90,
            "run drains at the rewritten stop, got {last:#x}"
        );
    }
}
