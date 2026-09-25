// SPDX-License-Identifier: GPL-3.0-or-later

//! Timestamped CD drive command trace.
//!
//! A host-side observer of a CD drive's command channel, fed by the drive
//! model (`crate::akiko` for the CD32's Chinon). Every command the host
//! sends becomes one record stamped in emulated colour clocks at each step
//! of its life:
//!
//! - `issued`: the host handed the command's first byte to the drive link
//!   (Akiko's TX DMA fetched it from the command ring, or the CPU wrote it
//!   to the PIO port);
//! - `accepted`: the drive parsed the complete packet and started its
//!   command turnaround;
//! - `executed`: the drive acted on it;
//! - `responded`: the reply packet reached the host (RX DMA or PIO);
//! - `first_sector` / `last_sector`: the first and latest unit a read,
//!   play or TOC dump delivered (a data sector into a PBX slot, a CD-DA
//!   sector into the mixer, a TOC packet once the host has read it);
//! - `completed`: the command's life ended, with its `outcome`.
//!
//! Differences between these stamps are the drive latencies a ROM or
//! driver comparison needs: command turnaround, locate time to the first
//! sector, delivery gaps, and how long a read stayed open. The stamps are
//! emulated time, so they are identical between runs and unaffected by
//! warp or host speed.
//!
//! Recording is always on: a drive exchanges a few packets per second, so
//! the cost is negligible, and the history is there when a debugger view
//! opens. The trace never feeds back into emulation and is not part of a
//! save state (the drive model carries it across state loads as a host
//! resource, like the control protocol's event queues).

use std::collections::VecDeque;

/// Commands kept for listing (the debugger's CD tab, `cd.trace`).
pub const CD_TRACE_RECORDS: usize = 256;

/// Phase events buffered for streaming observers (`event.cd`).
pub const CD_TRACE_EVENT_CAPACITY: usize = 1024;

/// Pending first-byte stamps kept for bytes still queued in the drive's
/// receive buffer. Bounded by the drive model's own buffer size; this is
/// only a backstop.
const MAX_FIFO_STAMPS: usize = 8192;

/// What a command asked the drive to do, decoded from its packet.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CdCommandKind {
    Noop,
    Stop,
    Pause,
    Unpause,
    /// Read data sectors (the multi command with its data bit set).
    Read,
    /// Play CD-DA (the multi command aimed at program area audio).
    Play,
    /// Dump the lead-in TOC (the multi command aimed at the lead-in).
    Toc,
    Led,
    /// Report the Q-channel position.
    Subq,
    /// Firmware identification (the host's INFO request).
    Info,
    /// A defined-length opcode the drive acknowledges without acting.
    Other(u8),
    /// A packet that failed its checksum or named no known opcode.
    Invalid,
}

impl CdCommandKind {
    pub fn name(self) -> &'static str {
        match self {
            Self::Noop => "noop",
            Self::Stop => "stop",
            Self::Pause => "pause",
            Self::Unpause => "unpause",
            Self::Read => "read",
            Self::Play => "play",
            Self::Toc => "toc",
            Self::Led => "led",
            Self::Subq => "subq",
            Self::Info => "info",
            Self::Other(_) => "other",
            Self::Invalid => "invalid",
        }
    }
}

/// How a command's life ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CdOutcome {
    /// A one-shot command finished normally.
    Ok,
    /// The drive answered "no disc".
    NoDisc,
    ChecksumError,
    BadCommand,
    /// The drive refused the request (a play aimed at a data track).
    Refused,
    /// A read, play or TOC dump ran to its end.
    End,
    /// A STOP (or, for a TOC dump, a PAUSE) ended it.
    Stopped,
    /// A later seek/play/read command replaced it.
    Superseded,
    /// The disc was removed while it ran.
    Ejected,
    /// A machine reset ended it.
    Reset,
    /// A state load (or a reverse-debugging restore) replaced the timeline
    /// it was running on.
    Abandoned,
    /// The drive reported an error while it ran (an unreadable TOC).
    Error,
}

impl CdOutcome {
    pub fn name(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::NoDisc => "no_disc",
            Self::ChecksumError => "checksum_error",
            Self::BadCommand => "bad_command",
            Self::Refused => "refused",
            Self::End => "end",
            Self::Stopped => "stopped",
            Self::Superseded => "superseded",
            Self::Ejected => "ejected",
            Self::Reset => "reset",
            Self::Abandoned => "abandoned",
            Self::Error => "error",
        }
    }
}

/// The step of a command's life an event reports.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CdPhase {
    Executed,
    FirstSector,
    Completed,
}

impl CdPhase {
    pub fn name(self) -> &'static str {
        match self {
            Self::Executed => "executed",
            Self::FirstSector => "first_sector",
            Self::Completed => "completed",
        }
    }
}

/// The streaming work a command can leave running after it executes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CdStream {
    Read,
    Play,
    Toc,
}

/// One command's life on the drive link. Times are emulated colour clocks
/// since power-on (the bus's `emulated_cck`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CdCommandRecord {
    /// Monotonic command number, unique within the session.
    pub seq: u64,
    /// The packet as the drive parsed it, checksum included.
    pub bytes: Vec<u8>,
    pub kind: CdCommandKind,
    /// First sector of a read or play, as a logical sector number.
    pub start_lsn: Option<i64>,
    /// Exclusive end sector of a read or play.
    pub end_lsn: Option<i64>,
    /// Requested speed factor (1 or 2) of a seek/play/read command.
    pub speed: Option<u8>,
    pub issued_cck: u64,
    pub accepted_cck: u64,
    pub executed_cck: Option<u64>,
    pub responded_cck: Option<u64>,
    pub first_sector_cck: Option<u64>,
    pub last_sector_cck: Option<u64>,
    pub completed_cck: Option<u64>,
    /// Units delivered: data sectors for a read, CD-DA sectors for a
    /// play, TOC packets for a TOC dump.
    pub sectors: u32,
    /// The drive's status byte in the reply (the byte after the echoed
    /// command), when the command had a reply.
    pub status: Option<u8>,
    pub outcome: Option<CdOutcome>,
}

impl CdCommandRecord {
    /// `cck`, raised to the command's own execution time: nothing that
    /// follows from a command can be stamped before it ran.
    fn floor(&self, cck: u64) -> u64 {
        cck.max(self.executed_cck.unwrap_or(self.accepted_cck))
    }

    /// The command's opcode nibble.
    pub fn opcode(&self) -> u8 {
        self.bytes.first().map_or(0, |b| b & 0x0F)
    }

    /// A compact human summary: the headline, then each later stamp as an
    /// offset from the issue time, e.g.
    /// `#12 read 1234..1300 x2 st $02: +1.02ms exec, +1.10ms reply,
    /// +254.10ms first, 66 units, +701.40ms end`.
    pub fn describe(&self) -> String {
        format!("{}: {}", self.headline(), self.timeline().join(", "))
    }

    /// What was asked: `#12 read 1234..1300 x2 st $02`.
    pub fn headline(&self) -> String {
        let mut text = format!("#{} {}", self.seq, self.kind.name());
        if let (Some(start), Some(end)) = (self.start_lsn, self.end_lsn) {
            text.push_str(&format!(" {start}..{end}"));
        }
        if let Some(speed) = self.speed {
            text.push_str(&format!(" x{speed}"));
        }
        if let Some(status) = self.status {
            text.push_str(&format!(" st ${status:02X}"));
        }
        text
    }

    /// Each step reached so far as an offset from the issue time, ending
    /// with the outcome, or `open` while the command is still running.
    pub fn timeline(&self) -> Vec<String> {
        let mut parts = Vec::new();
        let since = |cck: u64| format_ms(cck.saturating_sub(self.issued_cck));
        if self.accepted_cck > self.issued_cck {
            parts.push(format!("{} accept", since(self.accepted_cck)));
        }
        if let Some(cck) = self.executed_cck {
            parts.push(format!("{} exec", since(cck)));
        }
        if let Some(cck) = self.responded_cck {
            parts.push(format!("{} reply", since(cck)));
        }
        if let Some(cck) = self.first_sector_cck {
            parts.push(format!("{} first", since(cck)));
        }
        if self.sectors > 0 {
            parts.push(format!("{} units", self.sectors));
        }
        match (self.completed_cck, self.outcome) {
            (Some(cck), Some(outcome)) => parts.push(format!("{} {}", since(cck), outcome.name())),
            (Some(cck), None) => parts.push(format!("{} done", since(cck))),
            (None, _) => parts.push("open".to_string()),
        }
        parts
    }
}

/// Emulated seconds of `cck` colour clocks.
pub fn cck_seconds(cck: u64) -> f64 {
    cck as f64 / f64::from(crate::chipset::paula::PAULA_CLOCK_HZ)
}

/// `cck` colour clocks as "+N.NNms".
pub fn format_ms(cck: u64) -> String {
    format!("+{:.2}ms", cck_seconds(cck) * 1000.0)
}

/// One published step of a command's life, for streaming observers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CdTraceEvent {
    /// Monotonic event number (not the command's `seq`).
    pub sequence: u64,
    pub phase: CdPhase,
    /// The command as it stood when the phase was reached.
    pub record: CdCommandRecord,
}

/// A TOC dump packet on its way to the host (see `CdTrace::toc_packet`).
#[derive(Clone, Copy, Debug)]
struct TocPacket {
    seq: u64,
    unit: bool,
    ends: Option<CdOutcome>,
}

/// The trace: recent command records, the phase-event queue, and the
/// bookkeeping that ties drive-model moments to the command they belong
/// to.
pub struct CdTrace {
    /// Emulated time of the moment being processed, set by the drive model
    /// before each hook (see `set_now`).
    now: u64,
    records: VecDeque<CdCommandRecord>,
    events: VecDeque<CdTraceEvent>,
    next_seq: u64,
    next_event: u64,
    /// Issue stamps of bytes still queued in the drive's receive buffer,
    /// aligned with the buffer's tail (see `fifo_pop`).
    fifo_stamps: VecDeque<u64>,
    /// Issue stamp of the packet the drive is part-way through parsing.
    pending_issue: Option<u64>,
    /// The command in its turnaround (accepted, not executed yet).
    parsing: Option<u64>,
    /// The command whose reply packet is in flight to the host.
    reply: Option<u64>,
    /// The TOC dump packet in flight to the host: a unit counts, and a
    /// dump ends, when the packet reaches the host, not when the drive
    /// builds it (the host may leave the receive channel stalled).
    toc_packet: Option<TocPacket>,
    read: Option<u64>,
    play: Option<u64>,
    toc: Option<u64>,
    /// `COPPERLINE_DBG_CD`: log each phase at info level.
    log: bool,
}

impl Default for CdTrace {
    fn default() -> Self {
        Self {
            now: 0,
            records: VecDeque::new(),
            events: VecDeque::new(),
            next_seq: 1,
            next_event: 0,
            fifo_stamps: VecDeque::new(),
            pending_issue: None,
            parsing: None,
            reply: None,
            toc_packet: None,
            read: None,
            play: None,
            toc: None,
            log: crate::envcfg::flag("COPPERLINE_DBG_CD"),
        }
    }
}

impl CdTrace {
    /// Set the emulated time the following hooks happen at.
    pub fn set_now(&mut self, cck: u64) {
        self.now = cck;
    }

    pub fn now(&self) -> u64 {
        self.now
    }

    /// Recent commands, oldest first.
    pub fn records(&self) -> impl DoubleEndedIterator<Item = &CdCommandRecord> + '_ {
        self.records.iter()
    }

    /// The number the next accepted command will get.
    pub fn next_seq(&self) -> u64 {
        self.next_seq
    }

    /// The next event sequence number (a fresh observer's cursor).
    pub fn event_cursor(&self) -> u64 {
        self.next_event
    }

    /// Events from `cursor` on, the new cursor, and how many events the
    /// bounded queue dropped before the observer could read them.
    pub fn events_since(&self, cursor: u64) -> (Vec<CdTraceEvent>, u64, u64) {
        let oldest = self
            .events
            .front()
            .map_or(self.next_event, |event| event.sequence);
        let dropped = oldest.saturating_sub(cursor);
        let start = cursor.max(oldest);
        let events = self
            .events
            .iter()
            .filter(|event| event.sequence >= start)
            .cloned()
            .collect();
        (events, self.next_event, dropped)
    }

    /// A byte entered the drive's receive buffer, now `fifo_len` long.
    pub fn fifo_push(&mut self, fifo_len: usize) {
        self.fifo_stamps.push_back(self.now);
        while self.fifo_stamps.len() > fifo_len.min(MAX_FIFO_STAMPS) {
            self.fifo_stamps.pop_front();
        }
    }

    /// The drive is about to take the front byte of its receive buffer,
    /// which holds `fifo_len` bytes: that byte's issue stamp, if known.
    ///
    /// Stamps cover the buffer's newest bytes. After a state load the
    /// buffer can still hold bytes queued before the trace existed; those
    /// have no stamp and are simply skipped.
    pub fn fifo_pop(&mut self, fifo_len: usize) -> Option<u64> {
        while self.fifo_stamps.len() > fifo_len {
            self.fifo_stamps.pop_front();
        }
        if fifo_len > 0 && self.fifo_stamps.len() == fifo_len {
            self.fifo_stamps.pop_front()
        } else {
            None
        }
    }

    /// The drive started parsing a new packet with a byte issued at
    /// `stamp` (None: unknown, use now).
    pub fn packet_started(&mut self, stamp: Option<u64>) {
        self.pending_issue = Some(stamp.unwrap_or(self.now));
    }

    /// The drive parsed a complete packet and started its turnaround.
    pub fn command_accepted(
        &mut self,
        bytes: &[u8],
        kind: CdCommandKind,
        range: Option<(i64, i64)>,
        speed: Option<u8>,
    ) {
        let seq = self.next_seq;
        self.next_seq += 1;
        let issued = self.pending_issue.take().unwrap_or(self.now).min(self.now);
        if self.records.len() == CD_TRACE_RECORDS {
            // Evict the oldest command that is no longer running: a read or
            // play can stay open while hundreds of SUBQ polls and LED
            // packets pass, and it must still collect its units and its end.
            let open = self.open_seqs();
            let index = self
                .records
                .iter()
                .position(|record| !open.contains(&Some(record.seq)))
                .unwrap_or(0);
            self.records.remove(index);
        }
        self.records.push_back(CdCommandRecord {
            seq,
            bytes: bytes.to_vec(),
            kind,
            start_lsn: range.map(|(start, _)| start),
            end_lsn: range.map(|(_, end)| end),
            speed,
            issued_cck: issued,
            accepted_cck: self.now,
            executed_cck: None,
            responded_cck: None,
            first_sector_cck: None,
            last_sector_cck: None,
            completed_cck: None,
            sectors: 0,
            status: None,
            outcome: None,
        });
        self.parsing = Some(seq);
    }

    /// Set the outcome of the command being executed ahead of its
    /// completion (a refused play, a missing disc).
    pub fn set_outcome(&mut self, outcome: CdOutcome) {
        if let Some(record) = self.parsing.and_then(|seq| self.record_mut(seq)) {
            record.outcome = Some(outcome);
        }
    }

    /// The command being executed leaves `stream` running: it completes
    /// when that stream ends instead of when its reply arrives. Any stream
    /// of the same kind still open is superseded by it.
    pub fn open_stream(&mut self, stream: CdStream) {
        let Some(seq) = self.parsing else {
            return;
        };
        self.close_stream(stream, CdOutcome::Superseded);
        *self.stream_slot(stream) = Some(seq);
    }

    /// The drive executed the command in its turnaround. `status` is the
    /// reply's status byte and `replies` whether a reply packet was queued
    /// for the host.
    pub fn command_executed(&mut self, status: Option<u8>, replies: bool) {
        let Some(seq) = self.parsing.take() else {
            return;
        };
        let now = self.now;
        let Some(record) = self.record_mut(seq) else {
            return;
        };
        record.executed_cck = Some(now);
        record.status = status;
        self.emit(seq, CdPhase::Executed);
        if replies {
            self.reply = Some(seq);
        } else if !self.is_streaming(seq) {
            self.complete(seq, CdOutcome::Ok);
        }
    }

    /// The drive queued a TOC dump packet for the host: an entry (`unit`)
    /// or the error answer, possibly the one that ends the dump (`ends`).
    pub fn toc_packet_queued(&mut self, unit: bool, ends: Option<CdOutcome>) {
        self.toc_packet = self.toc.map(|seq| TocPacket { seq, unit, ends });
    }

    /// The packet in flight reached the host (RX DMA or the PIO port): a
    /// command's reply, or a TOC dump packet.
    pub fn packet_delivered(&mut self) {
        if let Some(packet) = self.toc_packet.take() {
            if self.toc == Some(packet.seq) {
                if packet.unit {
                    self.stream_unit(CdStream::Toc);
                }
                if let Some(outcome) = packet.ends {
                    self.close_stream(CdStream::Toc, outcome);
                }
            }
        }
        let Some(seq) = self.reply.take() else {
            return;
        };
        let now = self.now;
        if let Some(record) = self.record_mut(seq) {
            record.responded_cck = Some(now);
        }
        if !self.is_streaming(seq) {
            self.complete(seq, CdOutcome::Ok);
        }
    }

    /// `stream` delivered one more unit (sector or TOC packet).
    pub fn stream_unit(&mut self, stream: CdStream) {
        let Some(seq) = *self.stream_slot(stream) else {
            return;
        };
        let now = self.now;
        let Some(record) = self.record_mut(seq) else {
            return;
        };
        // A unit stamped inside the same batch as its command's execution
        // can fall before it (the frame counter expired first); it cannot
        // precede the command that started the stream.
        let now = record.floor(now);
        record.sectors = record.sectors.saturating_add(1);
        record.last_sector_cck = Some(now);
        if record.first_sector_cck.is_none() {
            record.first_sector_cck = Some(now);
            self.emit(seq, CdPhase::FirstSector);
        }
    }

    /// `stream` ended; its command completes with `outcome`.
    pub fn close_stream(&mut self, stream: CdStream, outcome: CdOutcome) {
        if let Some(seq) = self.stream_slot(stream).take() {
            self.complete(seq, outcome);
        }
    }

    /// End every open stream with `outcome` (disc removed, machine reset).
    pub fn close_all(&mut self, outcome: CdOutcome) {
        for stream in [CdStream::Read, CdStream::Play, CdStream::Toc] {
            self.close_stream(stream, outcome);
        }
    }

    /// The drive model restarted without the traced exchange (a machine
    /// reset): open streams end with `Reset`, and the half-parsed packet,
    /// turnaround and reply in flight are forgotten.
    pub fn reset(&mut self) {
        self.close_all(CdOutcome::Reset);
        for seq in [self.parsing.take(), self.reply.take()]
            .into_iter()
            .flatten()
        {
            self.complete(seq, CdOutcome::Reset);
        }
        self.toc_packet = None;
        self.pending_issue = None;
        self.fifo_stamps.clear();
    }

    /// The trace moved onto a drive model restored from a save state (or
    /// a reverse-debugging restore): keep the history and the sequence
    /// numbers, so observers' cursors stay valid, and end every command
    /// still in flight as `Abandoned` at the last time of the timeline it
    /// ran on. A command the restored drive was already running is not in
    /// the trace; tracing resumes with the next packet it parses.
    pub fn forget_in_flight(&mut self) {
        for seq in self.open_seqs().into_iter().flatten() {
            self.complete(seq, CdOutcome::Abandoned);
        }
        self.pending_issue = None;
        self.parsing = None;
        self.reply = None;
        self.toc_packet = None;
        self.read = None;
        self.play = None;
        self.toc = None;
        self.fifo_stamps.clear();
    }

    /// Every command a link still points at (with duplicates: a read is
    /// both streaming and awaiting its reply).
    fn open_seqs(&self) -> [Option<u64>; 5] {
        [self.parsing, self.reply, self.read, self.play, self.toc]
    }

    fn is_streaming(&self, seq: u64) -> bool {
        [self.read, self.play, self.toc].contains(&Some(seq))
    }

    fn stream_slot(&mut self, stream: CdStream) -> &mut Option<u64> {
        match stream {
            CdStream::Read => &mut self.read,
            CdStream::Play => &mut self.play,
            CdStream::Toc => &mut self.toc,
        }
    }

    fn complete(&mut self, seq: u64, outcome: CdOutcome) {
        let now = self.now;
        let Some(record) = self.record_mut(seq) else {
            return;
        };
        if record.completed_cck.is_some() {
            return;
        }
        let now = record.floor(now);
        record.completed_cck = Some(now);
        // An outcome decided at execution (refused, no disc) stands.
        record.outcome.get_or_insert(outcome);
        self.emit(seq, CdPhase::Completed);
    }

    fn record_mut(&mut self, seq: u64) -> Option<&mut CdCommandRecord> {
        // Open commands are recent: search from the newest.
        self.records
            .iter_mut()
            .rev()
            .find(|record| record.seq == seq)
    }

    fn emit(&mut self, seq: u64, phase: CdPhase) {
        let Some(record) = self.records.iter().rev().find(|r| r.seq == seq).cloned() else {
            return;
        };
        if self.log {
            log::info!(
                "DBG CD {} {} t={:.6} cck={}",
                phase.name(),
                record.describe(),
                cck_seconds(self.now),
                self.now
            );
        }
        if self.events.len() == CD_TRACE_EVENT_CAPACITY {
            self.events.pop_front();
        }
        let sequence = self.next_event;
        self.next_event += 1;
        self.events.push_back(CdTraceEvent {
            sequence,
            phase,
            record,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn accept(trace: &mut CdTrace, at: u64, kind: CdCommandKind) -> u64 {
        trace.set_now(at);
        trace.packet_started(None);
        trace.command_accepted(&[0x04], kind, None, None);
        trace.records().last().unwrap().seq
    }

    #[test]
    fn one_shot_command_completes_when_its_reply_arrives() {
        let mut trace = CdTrace::default();
        trace.set_now(100);
        trace.fifo_push(1);
        trace.set_now(150);
        let stamp = trace.fifo_pop(1);
        assert_eq!(stamp, Some(100));
        trace.packet_started(stamp);
        trace.set_now(160);
        trace.command_accepted(&[0x07, 0xF8], CdCommandKind::Info, None, None);
        trace.set_now(3_700);
        trace.command_executed(Some(0x01), true);
        trace.set_now(4_000);
        trace.packet_delivered();

        let record = trace.records().last().unwrap();
        assert_eq!(record.issued_cck, 100);
        assert_eq!(record.accepted_cck, 160);
        assert_eq!(record.executed_cck, Some(3_700));
        assert_eq!(record.responded_cck, Some(4_000));
        assert_eq!(record.completed_cck, Some(4_000));
        assert_eq!(record.outcome, Some(CdOutcome::Ok));
        let (events, cursor, dropped) = trace.events_since(0);
        let phases: Vec<CdPhase> = events.iter().map(|e| e.phase).collect();
        assert_eq!(phases, [CdPhase::Executed, CdPhase::Completed]);
        assert_eq!((cursor, dropped), (2, 0));
    }

    #[test]
    fn read_stays_open_until_its_stream_ends() {
        let mut trace = CdTrace::default();
        let seq = accept(&mut trace, 10, CdCommandKind::Read);
        trace.open_stream(CdStream::Read);
        trace.set_now(20);
        trace.command_executed(Some(0x02), true);
        trace.set_now(30);
        trace.packet_delivered();
        assert_eq!(trace.records().last().unwrap().completed_cck, None);
        trace.set_now(900);
        trace.stream_unit(CdStream::Read);
        trace.set_now(950);
        trace.stream_unit(CdStream::Read);
        trace.set_now(990);
        trace.close_stream(CdStream::Read, CdOutcome::End);

        let record = trace.records().find(|r| r.seq == seq).unwrap();
        assert_eq!(record.first_sector_cck, Some(900));
        assert_eq!(record.last_sector_cck, Some(950));
        assert_eq!(record.sectors, 2);
        assert_eq!(record.completed_cck, Some(990));
        assert_eq!(record.outcome, Some(CdOutcome::End));
        let phases: Vec<CdPhase> = trace.events_since(0).0.iter().map(|e| e.phase).collect();
        assert_eq!(
            phases,
            [CdPhase::Executed, CdPhase::FirstSector, CdPhase::Completed]
        );
    }

    #[test]
    fn a_new_read_supersedes_the_open_one() {
        let mut trace = CdTrace::default();
        let first = accept(&mut trace, 10, CdCommandKind::Read);
        trace.open_stream(CdStream::Read);
        trace.command_executed(Some(0x02), false);
        let second = accept(&mut trace, 50, CdCommandKind::Read);
        trace.open_stream(CdStream::Read);
        trace.command_executed(Some(0x02), false);

        let outcome = |seq| trace.records().find(|r| r.seq == seq).unwrap().outcome;
        assert_eq!(outcome(first), Some(CdOutcome::Superseded));
        assert_eq!(outcome(second), None);
    }

    #[test]
    fn an_outcome_set_at_execution_survives_completion() {
        let mut trace = CdTrace::default();
        accept(&mut trace, 10, CdCommandKind::Play);
        trace.set_outcome(CdOutcome::Refused);
        trace.command_executed(Some(0x42), true);
        trace.packet_delivered();
        assert_eq!(
            trace.records().last().unwrap().outcome,
            Some(CdOutcome::Refused)
        );
    }

    #[test]
    fn an_open_read_outlives_ring_eviction() {
        let mut trace = CdTrace::default();
        let read = accept(&mut trace, 10, CdCommandKind::Read);
        trace.open_stream(CdStream::Read);
        trace.command_executed(Some(0x02), false);
        for n in 0..(CD_TRACE_RECORDS as u64 + 20) {
            accept(&mut trace, 20 + n, CdCommandKind::Subq);
            trace.command_executed(Some(0), false);
        }
        assert_eq!(trace.records().count(), CD_TRACE_RECORDS);
        trace.set_now(5_000);
        trace.stream_unit(CdStream::Read);
        trace.close_stream(CdStream::Read, CdOutcome::End);
        let record = trace.records().find(|r| r.seq == read).expect("kept");
        assert_eq!((record.sectors, record.outcome), (1, Some(CdOutcome::End)));
        let (events, _, _) = trace.events_since(0);
        assert!(events
            .iter()
            .any(|e| e.record.seq == read && e.phase == CdPhase::Completed));
    }

    #[test]
    fn a_state_load_abandons_what_was_in_flight() {
        let mut trace = CdTrace::default();
        let read = accept(&mut trace, 10, CdCommandKind::Read);
        trace.open_stream(CdStream::Read);
        trace.command_executed(Some(0x02), true);
        let info = accept(&mut trace, 90, CdCommandKind::Info);
        trace.set_now(400);
        trace.forget_in_flight();
        for seq in [read, info] {
            let record = trace.records().find(|r| r.seq == seq).unwrap();
            assert_eq!(record.outcome, Some(CdOutcome::Abandoned));
            assert_eq!(record.completed_cck, Some(400));
        }
        // Nothing still points at them: later units go nowhere.
        trace.stream_unit(CdStream::Read);
        assert_eq!(trace.records().find(|r| r.seq == read).unwrap().sectors, 0);
    }

    #[test]
    fn a_unit_is_never_stamped_before_its_command_executed() {
        let mut trace = CdTrace::default();
        accept(&mut trace, 10, CdCommandKind::Play);
        trace.open_stream(CdStream::Play);
        trace.set_now(900);
        trace.command_executed(Some(0x42), false);
        // The audio frame counter expired earlier in the same batch.
        trace.set_now(600);
        trace.stream_unit(CdStream::Play);
        let record = trace.records().last().unwrap();
        assert_eq!(record.first_sector_cck, Some(900));
    }

    #[test]
    fn a_toc_packet_counts_when_it_reaches_the_host() {
        let mut trace = CdTrace::default();
        let seq = accept(&mut trace, 10, CdCommandKind::Toc);
        trace.open_stream(CdStream::Toc);
        trace.command_executed(Some(0), true);
        trace.set_now(20);
        trace.packet_delivered();
        // The last entry is built long before the host drains it.
        trace.set_now(1_000);
        trace.toc_packet_queued(true, Some(CdOutcome::End));
        let record = || trace.records().find(|r| r.seq == seq).unwrap().clone();
        assert_eq!((record().sectors, record().completed_cck), (0, None));
        trace.set_now(5_000);
        trace.packet_delivered();
        let done = trace.records().find(|r| r.seq == seq).unwrap();
        assert_eq!(done.sectors, 1);
        assert_eq!(done.first_sector_cck, Some(5_000));
        assert_eq!(
            (done.completed_cck, done.outcome),
            (Some(5_000), Some(CdOutcome::End))
        );
    }

    #[test]
    fn fifo_stamps_skip_bytes_queued_before_the_trace_existed() {
        let mut trace = CdTrace::default();
        // Two bytes were already queued (a restored machine), then one
        // arrives with a stamp.
        trace.set_now(500);
        trace.fifo_push(3);
        assert_eq!(trace.fifo_pop(3), None);
        assert_eq!(trace.fifo_pop(2), None);
        assert_eq!(trace.fifo_pop(1), Some(500));
    }

    #[test]
    fn the_event_queue_counts_what_it_dropped() {
        let mut trace = CdTrace::default();
        for n in 0..(CD_TRACE_EVENT_CAPACITY as u64 + 10) {
            accept(&mut trace, n, CdCommandKind::Noop);
            trace.command_executed(None, false);
        }
        let (events, cursor, dropped) = trace.events_since(0);
        assert_eq!(events.len(), CD_TRACE_EVENT_CAPACITY);
        assert_eq!(cursor, 2 * (CD_TRACE_EVENT_CAPACITY as u64 + 10));
        assert_eq!(dropped, cursor - CD_TRACE_EVENT_CAPACITY as u64);
        assert_eq!(trace.records().count(), CD_TRACE_RECORDS);
    }

    #[test]
    fn describe_reports_offsets_from_issue() {
        let record = CdCommandRecord {
            seq: 3,
            bytes: vec![0x14],
            kind: CdCommandKind::Read,
            start_lsn: Some(16),
            end_lsn: Some(32),
            speed: Some(2),
            issued_cck: 0,
            accepted_cck: 0,
            executed_cck: Some(3_547),
            responded_cck: None,
            first_sector_cck: None,
            last_sector_cck: None,
            completed_cck: None,
            sectors: 0,
            status: Some(2),
            outcome: None,
        };
        assert_eq!(
            record.describe(),
            "#3 read 16..32 x2 st $02: +1.00ms exec, open"
        );
    }
}
