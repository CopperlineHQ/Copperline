// SPDX-License-Identifier: GPL-3.0-or-later

//! lcov `.info` export of guest coverage, and the `--run PROG --coverage
//! FILE` run that collects it from `LoadSeg()` to the program's exit.
//!
//! The renderer maps executed runtime addresses through the program's debug
//! information (`crate::debuginfo`: vasm `LINE` hunks, amiga-gcc DWARF, an
//! ELF sibling) to `SF`/`FN`/`FNDA`/`DA` records, one record block per
//! source file, exactly the subset of the format `genhtml`, VS Code's
//! Coverage Gutters and the `lcov-parse` family read. Instructions that map
//! to no source line are not dropped silently: the file starts with `#`
//! comment lines (ignored by every lcov reader) that account for every
//! retired instruction, and the same summary is returned to the caller.
//!
//! Line counts follow gcov's meaning as closely as instruction counters
//! allow: a line's count is, over each contiguous run of line-table rows for
//! that line, the highest hit count of any instruction in the run, summed
//! across the runs. A loop header split into an initialisation run and a
//! test run therefore counts each pass once. A function's count is the hit
//! count of its entry instruction, or of its most executed instruction when
//! control only ever entered mid-body.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use crate::debuginfo::{hunk::HunkKind, DebugInfo, HunkAddr};

use super::coverage::{CoverageCollector, CoverageData};

/// The accounting behind one rendered `.info` file.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LcovSummary {
    pub files: usize,
    pub lines_found: u64,
    pub lines_hit: u64,
    pub functions_found: u64,
    pub functions_hit: u64,
    /// Instruction executions on addresses with a source line.
    pub mapped_hits: u64,
    /// Executions inside the program's hunks with no line information,
    /// and the distinct addresses they occurred at.
    pub unmapped_hits: u64,
    pub unmapped_addresses: usize,
    /// Executions outside the program: Kickstart, libraries, other tasks.
    pub outside_hits: u64,
    pub total_hits: u64,
}

impl LcovSummary {
    /// The human summary, one line per fact, as the file's leading comments
    /// carry it and as the converter prints it.
    pub fn lines(&self) -> Vec<String> {
        let mut out = vec![format!(
            "{} instruction(s) retired: {} on {} source line(s) in {} file(s)",
            self.total_hits, self.mapped_hits, self.lines_found, self.files
        )];
        out.push(format!(
            "{} instruction(s) at {} program address(es) without line information",
            self.unmapped_hits, self.unmapped_addresses
        ));
        out.push(format!(
            "{} instruction(s) outside the program (Kickstart, libraries, other tasks)",
            self.outside_hits
        ));
        out.push(format!(
            "lines {}/{} hit, functions {}/{} hit",
            self.lines_hit, self.lines_found, self.functions_hit, self.functions_found
        ));
        out
    }
}

/// A function to report: name, unrelocated entry, byte size, and the
/// DWARF file/line when known.
type FunctionEntry = (String, HunkAddr, u32, Option<u32>, Option<u32>);

#[derive(Debug, Clone, Default)]
struct FileRecord {
    /// Function name -> (line, count).
    functions: BTreeMap<String, (u32, u64)>,
    /// Line -> count.
    lines: BTreeMap<u32, u64>,
}

/// Render an lcov `.info` file. `hits` are `(runtime address, count)`
/// pairs; `debug` must already be relocated. `outside_hits` counts
/// executions the collector saw beyond its ranges, `total_hits` everything
/// it retired. `notes` are extra leading comment lines.
pub fn render(
    debug: &DebugInfo,
    test_name: &str,
    hits: &[(u32, u32)],
    outside_hits: u64,
    total_hits: u64,
    source_map: &[(String, String)],
    notes: &[String],
) -> (String, LcovSummary) {
    let mut summary = LcovSummary {
        outside_hits,
        total_hits,
        ..Default::default()
    };
    // Runs of consecutive rows sharing (hunk, file, line): run id per row.
    let mut run_of_row = Vec::with_capacity(debug.rows.len());
    let mut run_line: Vec<(u32, u32)> = Vec::new(); // (file, line) per run
    for (index, row) in debug.rows.iter().enumerate() {
        let continues = index > 0 && {
            let prev = debug.rows[index - 1];
            !prev.end_sequence
                && !row.end_sequence
                && prev.at.hunk == row.at.hunk
                && prev.file == row.file
                && prev.line == row.line
        };
        if !continues && !row.end_sequence {
            run_line.push((row.file, row.line));
        }
        run_of_row.push(run_line.len().saturating_sub(1));
    }
    let mut run_max = vec![0u32; run_line.len()];
    let mut unmapped: BTreeMap<u32, u64> = BTreeMap::new();
    let mut hit_map: BTreeMap<u32, u32> = BTreeMap::new();
    for &(addr, count) in hits {
        let slot = hit_map.entry(addr & !1).or_default();
        *slot = slot.saturating_add(count);
    }
    for (&addr, &count) in &hit_map {
        if debug.locate(addr).is_none() {
            summary.outside_hits = summary.outside_hits.saturating_add(u64::from(count));
            continue;
        }
        match debug.line_for(addr) {
            Some(hit) => {
                let run = run_of_row[hit.row];
                run_max[run] = run_max[run].max(count);
                summary.mapped_hits = summary.mapped_hits.saturating_add(u64::from(count));
            }
            None => {
                *unmapped.entry(addr).or_default() += u64::from(count);
            }
        }
    }
    summary.unmapped_addresses = unmapped.len();
    summary.unmapped_hits = unmapped.values().sum();

    let mut files: BTreeMap<u32, FileRecord> = BTreeMap::new();
    for (run, &(file, line)) in run_line.iter().enumerate() {
        let record = files.entry(file).or_default();
        let count = record.lines.entry(line).or_default();
        *count = count.saturating_add(u64::from(run_max[run]));
    }

    // Functions: DWARF subprograms, else the code hunks' symbols.
    let entry_count = |at: HunkAddr, size: u32| -> u64 {
        let Some(start) = debug.runtime(at) else {
            return 0;
        };
        if let Some(&count) = hit_map.get(&start) {
            if count != 0 {
                return u64::from(count);
            }
        }
        let end = start.wrapping_add(size.max(2));
        hit_map
            .range(start..end)
            .map(|(_, count)| u64::from(*count))
            .max()
            .unwrap_or(0)
    };
    let mut functions: Vec<FunctionEntry> = debug
        .functions
        .iter()
        .map(|f| (f.name.clone(), f.at, f.size, f.file, f.line))
        .collect();
    if functions.is_empty() {
        for (index, symbol) in debug.symbols.iter().enumerate() {
            let hunk = debug.hunks.get(symbol.at.hunk as usize);
            if hunk.is_none_or(|h| h.kind != HunkKind::Code) {
                continue;
            }
            let hunk_size = hunk.map_or(0, |h| h.size);
            let end = debug
                .symbols
                .get(index + 1..)
                .into_iter()
                .flatten()
                .find(|next| next.at.hunk == symbol.at.hunk && next.at.offset > symbol.at.offset)
                .map_or(hunk_size, |next| next.at.offset);
            let size = symbol
                .size
                .unwrap_or_else(|| end.saturating_sub(symbol.at.offset));
            functions.push((symbol.name.clone(), symbol.at, size, None, None));
        }
    }
    for (name, at, size, file, line) in functions {
        // By hunk address, so a program that never loaded (no relocation)
        // still lists every function at zero.
        let located = line_at(debug, at);
        let file = file.or(located.map(|(file, _)| file));
        let line = line.or(located.map(|(_, line)| line));
        let (Some(file), Some(line)) = (file, line) else {
            continue;
        };
        let count = entry_count(at, size);
        let record = files.entry(file).or_default();
        let slot = record.functions.entry(name).or_insert((line, 0));
        slot.1 = slot.1.saturating_add(count);
    }

    for record in files.values() {
        summary.lines_found += record.lines.len() as u64;
        summary.lines_hit += record.lines.values().filter(|c| **c != 0).count() as u64;
        summary.functions_found += record.functions.len() as u64;
        summary.functions_hit += record.functions.values().filter(|(_, c)| *c != 0).count() as u64;
    }
    summary.files = files.len();

    let mut out = String::new();
    let _ = writeln!(out, "# Copperline guest coverage: {test_name}");
    for note in notes {
        let _ = writeln!(out, "# {note}");
    }
    for line in summary.lines() {
        let _ = writeln!(out, "# {line}");
    }
    let _ = writeln!(out, "TN:{}", sanitize(test_name));
    for (file, record) in &files {
        let path = debug
            .files
            .get(*file as usize)
            .map(|f| f.path.clone())
            .unwrap_or_default();
        let _ = writeln!(out, "SF:{}", apply_source_map(path, source_map));
        let mut functions: Vec<(&String, &(u32, u64))> = record.functions.iter().collect();
        functions.sort_by_key(|(name, (line, _))| (*line, (*name).clone()));
        for (name, (line, _)) in &functions {
            let _ = writeln!(out, "FN:{line},{}", sanitize(name));
        }
        for (name, (_, count)) in &functions {
            let _ = writeln!(out, "FNDA:{count},{}", sanitize(name));
        }
        let _ = writeln!(out, "FNF:{}", functions.len());
        let _ = writeln!(
            out,
            "FNH:{}",
            functions.iter().filter(|(_, (_, c))| *c != 0).count()
        );
        for (line, count) in &record.lines {
            let _ = writeln!(out, "DA:{line},{count}");
        }
        let _ = writeln!(out, "LF:{}", record.lines.len());
        let _ = writeln!(
            out,
            "LH:{}",
            record.lines.values().filter(|c| **c != 0).count()
        );
        let _ = writeln!(out, "end_of_record");
    }
    (out, summary)
}

/// The line-table row covering an unrelocated address: `(file, line)`.
fn line_at(debug: &DebugInfo, at: HunkAddr) -> Option<(u32, u32)> {
    let idx = debug.rows.partition_point(|r| r.at <= at);
    let row = debug.rows.get(idx.checked_sub(1)?)?;
    (row.at.hunk == at.hunk && !row.end_sequence).then_some((row.file, row.line))
}

/// Names and test labels are one field of a comma-separated record.
fn sanitize(name: &str) -> String {
    name.chars()
        .map(|c| if c == ',' || c.is_control() { '_' } else { c })
        .collect()
}

pub fn apply_source_map(mut path: String, source_map: &[(String, String)]) -> String {
    for (from, to) in source_map {
        if let Some(rest) = path.strip_prefix(from) {
            path = format!("{to}{rest}");
            break;
        }
    }
    path
}

/// Turn a `coverage.bin` histogram (or the collector's snapshot) into the
/// `(address, count)` list `render` takes.
pub fn hits_of(data: &CoverageData) -> Vec<(u32, u32)> {
    data.hits().collect()
}

// -------------------------------------------------------------------------
// The `--run PROG --coverage FILE` run

/// Frames between periodic rewrites of the `.info` file while collecting,
/// so a run that never reaches the program's exit still leaves a current
/// file behind (five PAL seconds).
const FLUSH_INTERVAL_FRAMES: u64 = 250;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    /// The program has not been seen loaded yet; `deadline` is emulated
    /// seconds, set on the first poll.
    Waiting,
    Collecting,
    Done,
}

/// What one poll observed, for the owning loop's logging and exit rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CoveragePoll {
    Idle,
    /// The program was just loaded and counting began.
    Loaded,
    /// The program exited (or ran to completion before its load was
    /// observed): the final file is written.
    Written,
    /// The program was not loaded within the launch timeout: an empty
    /// file is written and the run is over.
    TimedOut,
}

/// The state machine behind `--run PROG --coverage FILE`: watch the guest's
/// `LoadSeg()` results for the program, relocate its debug information by
/// the segments the loader reports, arm the collector over its code hunks,
/// and write the lcov file when the boot script's completion marker appears
/// or the owner ends the run.
pub struct CoverageRun {
    target: String,
    done_marker: PathBuf,
    out: PathBuf,
    debug: DebugInfo,
    source_map: Vec<(String, String)>,
    phase: Phase,
    deadline_secs: Option<f64>,
    last_flush_frame: u64,
    notes: Vec<String>,
}

impl CoverageRun {
    /// Read the program's debug information now, so a missing or
    /// unreadable executable fails before the machine boots. `elf` is an
    /// explicit DWARF sibling; without one, `PROG.elf` beside the program
    /// is used when present.
    pub fn prepare(
        program: &Path,
        elf: Option<&Path>,
        target: String,
        done_marker: PathBuf,
        out: PathBuf,
        source_map: Vec<(String, String)>,
    ) -> Result<Self, String> {
        let bytes = std::fs::read(program).map_err(|e| format!("{}: {e}", program.display()))?;
        let sibling = elf.map(Path::to_path_buf).or_else(|| {
            let mut path = program.as_os_str().to_owned();
            path.push(".elf");
            let path = PathBuf::from(path);
            path.is_file().then_some(path)
        });
        let elf_bytes = sibling
            .as_ref()
            .map(|path| std::fs::read(path).map_err(|e| format!("{}: {e}", path.display())))
            .transpose()?;
        let debug = DebugInfo::load(&bytes, elf_bytes.as_deref())?;
        let mut notes = Vec::new();
        if debug.rows.is_empty() {
            notes.push(format!(
                "{} carries no line information (build with -g or vasm -linedebug); \
                 only instruction totals can be reported",
                program.display()
            ));
        }
        Ok(Self {
            target,
            done_marker,
            out,
            debug,
            source_map,
            phase: Phase::Waiting,
            deadline_secs: None,
            last_flush_frame: 0,
            notes,
        })
    }

    pub fn out(&self) -> &Path {
        &self.out
    }

    pub fn written(&self) -> bool {
        self.phase == Phase::Done
    }

    /// Why run-ahead cannot be used while this run is armed.
    pub fn runahead_block_reason(&self) -> Option<&'static str> {
        (self.phase != Phase::Done).then_some("coverage collection armed")
    }

    /// One committed frame. `machine` owns the collector while counting.
    pub fn poll(
        &mut self,
        machine: &mut crate::cpu::M68kMachine,
        frame: u64,
    ) -> Result<CoveragePoll, String> {
        match self.phase {
            Phase::Done => Ok(CoveragePoll::Idle),
            Phase::Waiting => {
                let now = machine.bus().emulated_seconds();
                let first_poll = self.deadline_secs.is_none();
                let deadline = *self
                    .deadline_secs
                    .get_or_insert(now + crate::runprog::WARP_LAUNCH_TIMEOUT_SECS);
                if first_poll {
                    // The machine catches the load before the program's
                    // first instruction; a frame-granular poll would miss
                    // most of a short program.
                    let code_hunks = self
                        .debug
                        .hunks
                        .iter()
                        .map(|hunk| hunk.kind == HunkKind::Code)
                        .collect();
                    if !machine.arm_coverage_on_load(self.target.clone(), code_hunks) {
                        return Err(
                            "coverage is already being collected by a profile capture".into()
                        );
                    }
                }
                if let Some(segments) = machine.take_coverage_loaded() {
                    self.loaded(machine, &segments)?;
                    self.last_flush_frame = frame;
                    return Ok(CoveragePoll::Loaded);
                }
                let target = &self.target;
                if self.done_marker.exists() {
                    // The program loaded, ran and exited between two polls:
                    // nothing was counted, and nothing more will run.
                    self.notes.push(format!(
                        "{target} ran to completion before its load was observed; \
                         no instructions were counted"
                    ));
                    machine.cancel_coverage_arm();
                    self.write(&CoverageData::default())?;
                    self.phase = Phase::Done;
                    return Ok(CoveragePoll::Written);
                }
                if now >= deadline {
                    self.notes.push(format!(
                        "{target} was not loaded within {:.0} emulated seconds",
                        crate::runprog::WARP_LAUNCH_TIMEOUT_SECS
                    ));
                    machine.cancel_coverage_arm();
                    self.write(&CoverageData::default())?;
                    self.phase = Phase::Done;
                    return Ok(CoveragePoll::TimedOut);
                }
                Ok(CoveragePoll::Idle)
            }
            Phase::Collecting => {
                if self.done_marker.exists() {
                    self.notes
                        .push(format!("{} exited; final counts", self.target));
                    let data = machine
                        .stop_coverage()
                        .map(CoverageCollector::into_data)
                        .unwrap_or_default();
                    self.write(&data)?;
                    self.phase = Phase::Done;
                    return Ok(CoveragePoll::Written);
                }
                if frame.saturating_sub(self.last_flush_frame) >= FLUSH_INTERVAL_FRAMES {
                    self.last_flush_frame = frame;
                    if let Some(data) = machine.coverage_snapshot() {
                        let mut interim = self.notes.clone();
                        interim.push(format!(
                            "{} still running at frame {frame}; interim counts",
                            self.target
                        ));
                        self.write_with_notes(&data, &interim)?;
                    }
                }
                Ok(CoveragePoll::Idle)
            }
        }
    }

    /// The run is over (window closed, last capture fired, server shut
    /// down): write the final file if the program is still running.
    /// Idempotent.
    pub fn finish(&mut self, machine: &mut crate::cpu::M68kMachine) -> Result<bool, String> {
        match self.phase {
            Phase::Done => Ok(false),
            Phase::Waiting => {
                machine.cancel_coverage_arm();
                self.notes.push(format!(
                    "the run ended before {} was loaded; no instructions were counted",
                    self.target
                ));
                self.phase = Phase::Done;
                self.write(&CoverageData::default())?;
                Ok(true)
            }
            Phase::Collecting => {
                self.notes.push(format!(
                    "the run ended while {} was still running; counts up to that point",
                    self.target
                ));
                self.phase = Phase::Done;
                let data = machine
                    .stop_coverage()
                    .map(CoverageCollector::into_data)
                    .unwrap_or_default();
                self.write(&data)?;
                Ok(true)
            }
        }
    }

    /// The program is in memory at `segments`: relocate the debug
    /// information and make sure the counters run (the machine's arm
    /// normally started them already).
    fn loaded(
        &mut self,
        machine: &mut crate::cpu::M68kMachine,
        segments: &[(u32, u32)],
    ) -> Result<(), String> {
        if segments.len() != self.debug.hunks.len() {
            self.notes.push(format!(
                "the guest reports {} segment(s) but the file has {} hunk(s); \
                 source mapping may be off",
                segments.len(),
                self.debug.hunks.len()
            ));
        }
        let bases: Vec<u32> = segments.iter().map(|(start, _)| *start).collect();
        self.debug.relocate(bases.clone());
        let mut ranges: Vec<(u32, u32)> = bases
            .iter()
            .zip(&self.debug.hunks)
            .filter(|(_, hunk)| hunk.kind == HunkKind::Code)
            .map(|(&base, hunk)| (base, hunk.size))
            .collect();
        if ranges.is_empty() {
            ranges = segments.to_vec();
        }
        if !machine.coverage_active() && !machine.start_coverage(&ranges) {
            return Err("coverage is already being collected by a profile capture".into());
        }
        self.phase = Phase::Collecting;
        Ok(())
    }

    fn write(&self, data: &CoverageData) -> Result<(), String> {
        self.write_with_notes(data, &self.notes)
    }

    fn write_with_notes(&self, data: &CoverageData, notes: &[String]) -> Result<(), String> {
        let (text, _) = render(
            &self.debug,
            &self.target,
            &hits_of(data),
            data.outside_hits,
            data.total_hits,
            &self.source_map,
            notes,
        );
        crate::paths::ensure_parent(&self.out).map_err(|e| e.to_string())?;
        std::fs::write(&self.out, text).map_err(|e| format!("{}: {e}", self.out.display()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::debuginfo::{
        hunk::HunkKind, Function, HunkMeta, LineRow, Location, SourceFile, Symbol,
    };

    fn row(hunk: u32, offset: u32, file: u32, line: u32) -> LineRow {
        LineRow {
            at: HunkAddr::new(hunk, offset),
            file,
            line,
            column: 0,
            is_stmt: true,
            end_sequence: false,
        }
    }

    fn function(name: &str, offset: u32, size: u32, file: u32, line: u32) -> Function {
        Function {
            name: name.into(),
            at: HunkAddr::new(0, offset),
            size,
            frame_base: Location::Unsupported,
            params: Vec::new(),
            locals: Vec::new(),
            file: Some(file),
            line: Some(line),
        }
    }

    /// Two source files in one code hunk plus a data hunk: `main.c` holds
    /// `entry` (lines 10-12, with a loop whose header is split into two
    /// row runs) and `never` (line 20); `util.c` holds `helper` (line 5).
    fn synthetic() -> DebugInfo {
        let mut debug = DebugInfo::default();
        debug.hunks = vec![
            HunkMeta {
                kind: HunkKind::Code,
                size: 0x40,
            },
            HunkMeta {
                kind: HunkKind::Data,
                size: 0x10,
            },
        ];
        debug.files = vec![
            SourceFile {
                path: "/build/src/main.c".into(),
            },
            SourceFile {
                path: "/build/src/util.c".into(),
            },
        ];
        debug.rows = vec![
            row(0, 0x00, 0, 10),
            row(0, 0x04, 0, 11), // loop init
            row(0, 0x08, 0, 12), // loop body
            row(0, 0x0c, 0, 11), // loop test (second run of line 11)
            row(0, 0x10, 0, 13),
            row(0, 0x20, 1, 5),
            row(0, 0x30, 0, 20),
            LineRow {
                end_sequence: true,
                ..row(0, 0x38, 0, 20)
            },
        ];
        debug.functions = vec![
            function("entry", 0x00, 0x20, 0, 10),
            function("helper", 0x20, 0x10, 1, 5),
            function("never", 0x30, 0x08, 0, 20),
        ];
        debug.relocate(vec![0x1000, 0x2000]);
        debug
    }

    #[test]
    fn renders_lines_functions_and_the_unmapped_summary() {
        let debug = synthetic();
        let hits = vec![
            (0x1000, 1),
            (0x1002, 1),
            (0x1004, 1), // loop init once
            (0x1008, 3), // body three times
            (0x100c, 4), // test four times
            (0x1010, 1),
            (0x1020, 3), // helper entry
            (0x1022, 3),
            (0x103a, 2), // code past the sequence end: no line
            (0x2004, 1), // the data hunk: no line
            (0xf8_1000, 7),
        ];
        let map = vec![("/build/src".to_string(), "src".to_string())];
        let (text, summary) = render(&debug, "prog", &hits, 100, 200, &map, &[]);
        assert!(
            text.starts_with("# Copperline guest coverage: prog\n"),
            "{text}"
        );
        assert!(text.contains("TN:prog\n"), "{text}");
        let main = text
            .split("SF:src/main.c\n")
            .nth(1)
            .and_then(|rest| rest.split("end_of_record").next())
            .expect("main.c record");
        assert!(main.contains("FN:10,entry\n"), "{main}");
        assert!(main.contains("FN:20,never\n"), "{main}");
        assert!(main.contains("FNDA:1,entry\n"), "{main}");
        assert!(main.contains("FNDA:0,never\n"), "{main}");
        assert!(main.contains("FNF:2\nFNH:1\n"), "{main}");
        assert!(main.contains("DA:10,1\n"), "{main}");
        // Sum of the two runs' maxima: init once plus four tests.
        assert!(main.contains("DA:11,5\n"), "{main}");
        assert!(main.contains("DA:12,3\n"), "{main}");
        assert!(main.contains("DA:13,1\n"), "{main}");
        assert!(main.contains("DA:20,0\n"), "{main}");
        assert!(main.contains("LF:5\nLH:4\n"), "{main}");
        let util = text
            .split("SF:src/util.c\n")
            .nth(1)
            .and_then(|rest| rest.split("end_of_record").next())
            .expect("util.c record");
        assert!(util.contains("FNDA:3,helper\n"), "{util}");
        assert!(util.contains("DA:5,3\n"), "{util}");
        assert!(util.contains("LF:1\nLH:1\n"), "{util}");
        assert_eq!(summary.files, 2);
        assert_eq!((summary.lines_found, summary.lines_hit), (6, 5));
        assert_eq!((summary.functions_found, summary.functions_hit), (3, 2));
        assert_eq!(summary.mapped_hits, 1 + 1 + 1 + 3 + 4 + 1 + 3 + 3);
        assert_eq!(summary.unmapped_hits, 3);
        assert_eq!(summary.unmapped_addresses, 2);
        // The ROM hit lands on top of the collector's own outside count.
        assert_eq!(summary.outside_hits, 107);
        assert_eq!(summary.total_hits, 200);
        assert!(
            text.contains("# 3 instruction(s) at 2 program address(es) without line information\n"),
            "{text}"
        );
    }

    #[test]
    fn function_entered_mid_body_and_symbol_fallback() {
        let mut debug = synthetic();
        // A function whose entry never ran but whose body did counts its
        // most executed instruction.
        let (_, summary) = render(&debug, "t", &[(0x1024, 2)], 0, 2, &[], &[]);
        assert_eq!(summary.functions_hit, 1);
        let (text, _) = render(&debug, "t", &[(0x1024, 2)], 0, 2, &[], &[]);
        assert!(text.contains("FNDA:2,helper\n"), "{text}");

        // Without DWARF functions, code-hunk symbols stand in; data
        // symbols and symbols without a line are skipped.
        debug.functions.clear();
        debug.symbols = vec![
            Symbol {
                name: "start".into(),
                at: HunkAddr::new(0, 0),
                size: None,
            },
            Symbol {
                name: "twice".into(),
                at: HunkAddr::new(0, 0x20),
                size: None,
            },
            Symbol {
                name: "value".into(),
                at: HunkAddr::new(1, 0),
                size: None,
            },
        ];
        let (text, summary) = render(&debug, "t", &[(0x1000, 1), (0x1020, 2)], 0, 3, &[], &[]);
        assert!(text.contains("FN:10,start\nFNDA:1,start\n"), "{text}");
        assert!(text.contains("FN:5,twice\nFNDA:2,twice\n"), "{text}");
        assert!(!text.contains("value"), "{text}");
        assert_eq!(summary.functions_found, 2);
    }

    #[test]
    fn names_with_commas_are_kept_to_one_field() {
        assert_eq!(sanitize("f<a,b>"), "f<a_b>");
        assert_eq!(
            apply_source_map("/build/x.c".into(), &[("/build".into(), "/src".into())]),
            "/src/x.c"
        );
    }

    /// The committed `guest/dap-test/hello` probe (amiga-gcc 6.5 DWARF):
    /// synthetic hits at the three functions' entries name them all.
    #[test]
    fn hello_fixture_functions_appear_with_their_entry_counts() {
        let hello = include_bytes!("../../guest/dap-test/hello");
        let mut debug = DebugInfo::load(hello, None).expect("hello parses");
        debug.relocate(vec![0x2_0000, 0x3_0000, 0x4_0000]);
        let entry = |name: &str| {
            debug
                .functions
                .iter()
                .find(|f| f.name == name)
                .and_then(|f| debug.runtime(f.at))
                .expect(name)
        };
        let hits = vec![(entry("add"), 4), (entry("scale"), 3), (entry("entry"), 1)];
        let (text, summary) = render(&debug, "hello", &hits, 50, 58, &[], &[]);
        assert!(text.contains("FNDA:4,add\n"), "{text}");
        assert!(text.contains("FNDA:3,scale\n"), "{text}");
        assert!(text.contains("FNDA:1,entry\n"), "{text}");
        assert!(text.contains("hello.c\n"), "{text}");
        assert_eq!(summary.functions_found, 3);
        assert_eq!(summary.functions_hit, 3);
        assert_eq!(summary.mapped_hits, 8);
        assert!(summary.lines_found > 20, "{summary:?}");
        assert_eq!(summary.lines_hit, 3);
    }

    #[cfg(feature = "control")]
    #[test]
    fn run_state_machine_writes_on_exit_timeout_and_finish() {
        let dir = std::env::temp_dir().join(format!(
            "copperline-covrun-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let program = dir.join("hello");
        std::fs::write(&program, include_bytes!("../../guest/dap-test/hello")).unwrap();
        let out = dir.join("out/cov.info");
        let marker = dir.join("done");
        let mut run = CoverageRun::prepare(
            &program,
            None,
            "hello".into(),
            marker.clone(),
            out.clone(),
            Vec::new(),
        )
        .unwrap();
        assert!(!run.written());
        assert_eq!(
            run.runahead_block_reason(),
            Some("coverage collection armed")
        );
        let mut emu = crate::control::test_emulator();
        // Nothing loaded, no marker: idle.
        assert_eq!(run.poll(&mut emu.machine, 1).unwrap(), CoveragePoll::Idle);
        assert!(!out.exists());
        // The marker without an observed load: written empty.
        std::fs::write(&marker, b"done\n").unwrap();
        assert_eq!(
            run.poll(&mut emu.machine, 2).unwrap(),
            CoveragePoll::Written
        );
        let text = std::fs::read_to_string(&out).unwrap();
        assert!(text.contains("before its load was observed"), "{text}");
        assert!(text.contains("FNDA:0,entry\n"), "{text}");
        assert!(run.written());
        assert_eq!(run.runahead_block_reason(), None);
        assert!(!run.finish(&mut emu.machine).unwrap());

        // Armed directly (as a LoadSeg observation would), then finished
        // by the run's end: the collector's counts land in the file.
        let _ = std::fs::remove_file(&marker);
        let mut run = CoverageRun::prepare(
            &program,
            None,
            "hello".into(),
            marker,
            out.clone(),
            Vec::new(),
        )
        .unwrap();
        let sizes: Vec<u32> = run.debug.hunks.iter().map(|h| h.size).collect();
        let segments: Vec<(u32, u32)> = sizes
            .iter()
            .enumerate()
            .map(|(i, size)| (0x1_0000 * (i as u32 + 1), *size))
            .collect();
        run.loaded(&mut emu.machine, &segments).unwrap();
        assert!(emu.machine.coverage_active());
        let add = run
            .debug
            .functions
            .iter()
            .find(|f| f.name == "add")
            .and_then(|f| run.debug.runtime(f.at))
            .unwrap();
        emu.machine.coverage_hit_for_test(add);
        emu.machine.coverage_hit_for_test(add);
        assert!(run.finish(&mut emu.machine).unwrap());
        assert!(!emu.machine.coverage_active());
        let text = std::fs::read_to_string(&out).unwrap();
        assert!(text.contains("FNDA:2,add\n"), "{text}");
        assert!(
            text.contains("still running; counts up to that point"),
            "{text}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
