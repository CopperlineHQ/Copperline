// SPDX-License-Identifier: GPL-3.0-or-later

//! Convert precise profile sidecars to Chrome/Bartman CPU profiles.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde::Serialize;
use serde_json::{json, Value};

use super::samples::{IRQ_MARKER, REGISTER_COUNT};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReportFormat {
    Chrome,
    Bartman,
}

impl ReportFormat {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "chrome" => Some(Self::Chrome),
            "bartman" => Some(Self::Bartman),
            _ => None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ReportOptions {
    pub input_dir: PathBuf,
    pub program: PathBuf,
    pub elf: Option<PathBuf>,
    pub out: PathBuf,
    pub format: ReportFormat,
    pub per_frame: bool,
    /// Prefix substitutions applied to source URLs after debug lookup.
    pub source_map: Vec<(String, String)>,
}

#[derive(Debug, Clone)]
struct RawSample {
    pcs: Vec<u32>,
    total_cck: u32,
    instruction_cck: u32,
    bus_wait_cck: u32,
    irq_level: Option<u8>,
    irq_vector: Option<u16>,
    registers: Option<[u32; REGISTER_COUNT]>,
}

#[derive(Debug)]
struct FrameSamples {
    frame: u64,
    samples: Vec<RawSample>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct Location {
    function: String,
    url: String,
    line: i64,
    column: i64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct CallFrame {
    function_name: String,
    script_id: String,
    url: String,
    line_number: i64,
    column_number: i64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct Node {
    id: u32,
    call_frame: CallFrame,
    hit_count: u64,
    children: Vec<u32>,
}

struct ProfileBuilder<'a> {
    debug: &'a crate::debuginfo::DebugInfo,
    base: Option<u32>,
    source_map: &'a [(String, String)],
    format: ReportFormat,
    nodes: Vec<Node>,
    node_map: HashMap<(u32, Location), u32>,
    samples: Vec<u32>,
    time_deltas: Vec<u32>,
    pc_trace: Vec<u32>,
    register_trace: Vec<u32>,
    end_time: u64,
}

impl<'a> ProfileBuilder<'a> {
    fn new(
        debug: &'a crate::debuginfo::DebugInfo,
        base: Option<u32>,
        source_map: &'a [(String, String)],
        format: ReportFormat,
    ) -> Self {
        Self {
            debug,
            base,
            source_map,
            format,
            nodes: vec![Node {
                id: 1,
                call_frame: CallFrame {
                    function_name: "(root)".into(),
                    script_id: "0".into(),
                    url: String::new(),
                    line_number: -1,
                    column_number: -1,
                },
                hit_count: 0,
                children: Vec::new(),
            }],
            node_map: HashMap::new(),
            samples: Vec::new(),
            time_deltas: Vec::new(),
            pc_trace: Vec::new(),
            register_trace: Vec::new(),
            end_time: 0,
        }
    }

    fn add_frame(&mut self, frame: &FrameSamples) {
        let mut last_stack = Vec::new();
        for sample in &frame.samples {
            let stack = self.locations(sample, &last_stack);
            if sample.instruction_cck != 0 {
                self.add_weight(&stack, sample.instruction_cck);
            }
            if sample.bus_wait_cck != 0 {
                let mut waiting = stack.clone();
                waiting.push(Location {
                    function: "[Bus wait]".into(),
                    url: String::new(),
                    line: 0,
                    column: 0,
                });
                self.add_weight(&waiting, sample.bus_wait_cck);
            }
            if sample.instruction_cck == 0 && sample.bus_wait_cck == 0 {
                self.add_weight(&stack, sample.total_cck.max(1));
            }
            if self.format == ReportFormat::Bartman {
                let pc = sample.pcs.first().copied().unwrap_or(u32::MAX);
                self.pc_trace.extend_from_slice(&[pc, sample.total_cck]);
                if let Some(registers) = &sample.registers {
                    self.register_trace.extend_from_slice(registers);
                }
            }
            last_stack = stack;
        }
    }

    fn locations(&self, sample: &RawSample, last_stack: &[Location]) -> Vec<Location> {
        if sample.pcs.first() == Some(&IRQ_MARKER) {
            return vec![Location {
                function: match (sample.irq_level, sample.irq_vector) {
                    (Some(level), Some(vector)) => format!("[IRQ] level {level}, vector {vector}"),
                    _ => "[IRQ]".into(),
                },
                url: String::new(),
                line: 0,
                column: 0,
            }];
        }
        if sample
            .pcs
            .first()
            .is_some_and(|pc| (0x00f8_0000..0x0100_0000).contains(pc))
        {
            let mut locations: Vec<_> = last_stack
                .iter()
                .filter(|location| !location.url.is_empty())
                .cloned()
                .collect();
            locations.extend(self.locations_for_pc(sample.pcs[0]));
            return locations;
        }
        if sample.pcs.is_empty() {
            let mut locations = last_stack.to_vec();
            if locations
                .last()
                .is_none_or(|location| location.function != "[External]")
            {
                locations.push(Location {
                    function: "[External]".into(),
                    url: String::new(),
                    line: 0,
                    column: 0,
                });
            }
            return locations;
        }
        let mut locations = Vec::with_capacity(sample.pcs.len());
        for (index, &stored_pc) in sample.pcs.iter().enumerate().rev() {
            let mut pc = self.runtime_pc(stored_pc);
            if index != 0 && !(0x00f8_0000..0x0100_0000).contains(&pc) {
                pc = pc.wrapping_sub(2);
            }
            locations.extend(self.locations_for_pc(pc));
        }
        locations
    }

    fn runtime_pc(&self, stored_pc: u32) -> u32 {
        if (0x00f8_0000..0x0100_0000).contains(&stored_pc) || self.debug.locate(stored_pc).is_some()
        {
            return stored_pc;
        }
        let hunk0_size = self.debug.hunks.first().map_or(0, |hunk| hunk.size);
        if stored_pc < hunk0_size {
            self.base
                .map_or(stored_pc, |base| base.wrapping_add(stored_pc))
        } else {
            stored_pc
        }
    }

    fn locations_for_pc(&self, pc: u32) -> Vec<Location> {
        if (0x00f8_0000..0x0100_0000).contains(&pc) {
            return vec![Location {
                function: "[Kickstart]".into(),
                url: String::new(),
                line: 0,
                column: 0,
            }];
        }
        let function = self
            .debug
            .function_at(pc)
            .map(|function| function.name.clone())
            .or_else(|| {
                self.debug.symbol_at(pc).map(|(symbol, distance)| {
                    if distance == 0 {
                        symbol.name.clone()
                    } else {
                        format!("{}+{distance:#x}", symbol.name)
                    }
                })
            })
            .unwrap_or_else(|| format!("${pc:08X}"));
        let current_location = self.debug.line_for(pc).map_or_else(
            || (String::new(), 0, 0),
            |hit| {
                let path = self
                    .debug
                    .files
                    .get(hit.file as usize)
                    .map(|file| file.path.clone())
                    .unwrap_or_default();
                (
                    apply_source_map(path, self.source_map),
                    i64::from(hit.line.saturating_sub(1)),
                    i64::from(hit.column.saturating_sub(1)),
                )
            },
        );
        let inlines = self.debug.inline_functions_at(pc);
        let call_site = |inline: &crate::debuginfo::InlineFunction| {
            let url = inline
                .file
                .and_then(|file| self.debug.files.get(file as usize))
                .map(|file| apply_source_map(file.path.clone(), self.source_map))
                .unwrap_or_default();
            (
                url,
                inline
                    .line
                    .map_or(0, |line| i64::from(line.saturating_sub(1))),
                0,
            )
        };
        // DW_AT_call_file/line belong to the caller. Give each parent the
        // next inline's call site; only the innermost inline owns the current
        // line-table location.
        let (url, line, column) = inlines
            .first()
            .map_or_else(|| current_location.clone(), |inline| call_site(inline));
        let mut locations = vec![Location {
            function,
            url,
            line,
            column,
        }];
        for (index, inline) in inlines.iter().enumerate() {
            let (url, line, column) = inlines
                .get(index + 1)
                .map_or_else(|| current_location.clone(), |child| call_site(child));
            locations.push(Location {
                function: format!("{} (inlined)", inline.name),
                url,
                line,
                column,
            });
        }
        locations
    }

    fn add_weight(&mut self, stack: &[Location], cck: u32) {
        let mut parent = 1u32;
        for location in stack {
            let key = (parent, location.clone());
            let id = if let Some(id) = self.node_map.get(&key) {
                *id
            } else {
                let id = self.nodes.len() as u32 + 1;
                let node = Node {
                    id,
                    call_frame: CallFrame {
                        function_name: location.function.clone(),
                        script_id: location.url.clone(),
                        url: location.url.clone(),
                        line_number: location.line,
                        column_number: location.column,
                    },
                    hit_count: 0,
                    children: Vec::new(),
                };
                self.nodes[parent as usize - 1].children.push(id);
                self.nodes.push(node);
                self.node_map.insert(key, id);
                id
            };
            parent = id;
        }
        if self.samples.last() == Some(&parent) {
            if let Some(delta) = self.time_deltas.last_mut() {
                *delta = delta.saturating_add(cck);
            }
            self.end_time = self.end_time.saturating_add(u64::from(cck));
            return;
        }
        self.nodes[parent as usize - 1].hit_count += 1;
        self.samples.push(parent);
        self.time_deltas.push(cck);
        self.end_time = self.end_time.saturating_add(u64::from(cck));
    }

    fn finish(self, frames: &[u64]) -> Value {
        let mut profile = json!({
            "nodes": self.nodes,
            "startTime": 0,
            "endTime": self.end_time,
            "samples": self.samples,
            "timeDeltas": self.time_deltas,
            "$copperline": {
                "version": 1,
                "clockUnit": "cck",
                "clockHz": crate::config::COLOR_CLOCK_HZ,
                "frames": frames,
                "contentionNode": "[Bus wait]",
            },
        });
        if self.format == ReportFormat::Bartman {
            profile["$amiga"] = json!({
                "chipsetFlags": 0,
                "customRegs": [],
                "agaColors": [],
                "dmaRecords": [],
                "gfxResources": [],
                "idleCycles": 0,
                "uniqueCallFrames": [],
                "callFrames": [],
                "pcTrace": self.pc_trace,
                "registerTrace": self.register_trace,
            });
        }
        profile
    }
}

fn apply_source_map(mut path: String, source_map: &[(String, String)]) -> String {
    for (from, to) in source_map {
        if let Some(rest) = path.strip_prefix(from) {
            path = format!("{to}{rest}");
            break;
        }
    }
    path
}

pub fn generate(options: &ReportOptions) -> Result<Vec<PathBuf>, String> {
    let summary: Value = serde_json::from_slice(
        &fs::read(options.input_dir.join("profile.json")).map_err(|e| e.to_string())?,
    )
    .map_err(|e| format!("profile.json: {e}"))?;
    let registers = summary["options"]["registers"].as_bool().unwrap_or(false);
    let unwind_base = summary["sampling"]["unwind_base"]
        .as_u64()
        .and_then(|base| u32::try_from(base).ok());
    let mut relocation_bases: Vec<u32> = summary["sampling"]["relocation_bases"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|base| base.as_u64().and_then(|base| u32::try_from(base).ok()))
        .collect();
    if relocation_bases.is_empty() {
        relocation_bases.extend(unwind_base);
    }
    let base = relocation_bases.first().copied();
    let program =
        fs::read(&options.program).map_err(|e| format!("{}: {e}", options.program.display()))?;
    let elf = options
        .elf
        .as_ref()
        .map(|path| fs::read(path).map_err(|e| format!("{}: {e}", path.display())))
        .transpose()?;
    let mut debug = crate::debuginfo::DebugInfo::load(&program, elf.as_deref())?;
    if !relocation_bases.is_empty() {
        debug.relocate(relocation_bases);
    }
    let frames = load_frames(&options.input_dir, registers)?;
    if frames.is_empty() {
        return Err("profile contains no precise sample frames".into());
    }

    if options.per_frame {
        let mut outputs = Vec::with_capacity(frames.len());
        let mut occurrences = HashMap::<u64, u64>::new();
        for frame in &frames {
            let mut builder =
                ProfileBuilder::new(&debug, base, &options.source_map, options.format);
            builder.add_frame(frame);
            let value = builder.finish(&[frame.frame]);
            let occurrence = occurrences.entry(frame.frame).or_default();
            *occurrence += 1;
            let path = frame_output_path(&options.out, frame.frame, *occurrence);
            write_json(&path, &value)?;
            outputs.push(path);
        }
        Ok(outputs)
    } else {
        let mut builder = ProfileBuilder::new(&debug, base, &options.source_map, options.format);
        for frame in &frames {
            builder.add_frame(frame);
        }
        let frame_numbers: Vec<u64> = frames.iter().map(|frame| frame.frame).collect();
        let value = builder.finish(&frame_numbers);
        write_json(&options.out, &value)?;
        Ok(vec![options.out.clone()])
    }
}

fn write_json(path: &Path, value: &Value) -> Result<(), String> {
    crate::paths::ensure_parent(path).map_err(|e| e.to_string())?;
    fs::write(path, serde_json::to_vec(value).map_err(|e| e.to_string())?)
        .map_err(|e| format!("{}: {e}", path.display()))
}

fn frame_output_path(out: &Path, frame: u64, occurrence: u64) -> PathBuf {
    let parent = out.parent().unwrap_or_else(|| Path::new(""));
    let stem = out
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("profile");
    let extension = out
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("cpuprofile");
    if occurrence == 1 {
        parent.join(format!("{stem}-{frame:06}.{extension}"))
    } else {
        parent.join(format!("{stem}-{frame:06}-{occurrence:03}.{extension}"))
    }
}

fn load_frames(dir: &Path, registers: bool) -> Result<Vec<FrameSamples>, String> {
    let stream = fs::read_to_string(dir.join("profile.jsonl")).map_err(|e| e.to_string())?;
    let mut frames = Vec::new();
    for (line_no, line) in stream.lines().enumerate() {
        let record: Value = serde_json::from_str(line)
            .map_err(|e| format!("profile.jsonl line {}: {e}", line_no + 1))?;
        let (Some(frame), Some(samples_file), Some(metadata_file)) = (
            record["frame"].as_u64(),
            record["samples"].as_str(),
            record["samples_meta"].as_str(),
        ) else {
            continue;
        };
        let samples = parse_frame(
            &fs::read(dir.join(samples_file)).map_err(|e| e.to_string())?,
            &fs::read(dir.join(metadata_file)).map_err(|e| e.to_string())?,
            registers,
        )?;
        frames.push(FrameSamples { frame, samples });
    }
    Ok(frames)
}

fn parse_frame(stream: &[u8], metadata: &[u8], registers: bool) -> Result<Vec<RawSample>, String> {
    if metadata.len() < 12 || &metadata[..4] != b"CLSM" {
        return Err("bad profile sample metadata header".into());
    }
    let word = |at: usize| -> u32 {
        u32::from_le_bytes(
            metadata[at..at + 4]
                .try_into()
                .expect("checked metadata word"),
        )
    };
    if word(4) != 1 {
        return Err(format!("unsupported profile metadata version {}", word(4)));
    }
    let count = word(8) as usize;
    if metadata.len() != 12 + count * 20 {
        return Err("profile sample metadata length does not match its count".into());
    }
    if !stream.len().is_multiple_of(4) {
        return Err("profile sample stream is not u32-aligned".into());
    }
    let words: Vec<u32> = stream
        .chunks_exact(4)
        .map(|bytes| u32::from_le_bytes(bytes.try_into().expect("four bytes")))
        .collect();
    let mut cursor = 0usize;
    let mut samples = Vec::with_capacity(count);
    for index in 0..count {
        let mut pcs = Vec::new();
        let total_cck = loop {
            let value = *words
                .get(cursor)
                .ok_or("profile sample stream ended before a cycle marker")?;
            cursor += 1;
            if value >= 0xffff_0000 {
                break u32::MAX - value;
            }
            pcs.push(value);
        };
        let sample_registers = if registers {
            let end = cursor + REGISTER_COUNT;
            let values = words
                .get(cursor..end)
                .ok_or("profile sample stream ended inside registers")?;
            cursor = end;
            Some(values.try_into().expect("register count checked"))
        } else {
            None
        };
        let at = 12 + index * 20;
        let irq_level = word(at + 12);
        let irq_vector = word(at + 16);
        samples.push(RawSample {
            pcs,
            total_cck,
            instruction_cck: word(at + 4),
            bus_wait_cck: word(at + 8),
            irq_level: (irq_level != u32::MAX).then_some(irq_level as u8),
            irq_vector: (irq_vector != u32::MAX).then_some(irq_vector as u16),
            registers: sample_registers,
        });
    }
    if cursor != words.len() {
        return Err("profile sample stream has trailing words".into());
    }
    Ok(samples)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn absolute_pcs_in_later_hunks_are_not_rebased_through_hunk_zero() {
        let mut debug = crate::debuginfo::DebugInfo::default();
        debug.hunks = vec![
            crate::debuginfo::HunkMeta {
                kind: crate::debuginfo::hunk::HunkKind::Code,
                size: 0x20,
            },
            crate::debuginfo::HunkMeta {
                kind: crate::debuginfo::hunk::HunkKind::Code,
                size: 0x20,
            },
        ];
        debug.relocate(vec![0x1000, 0x3000]);
        let builder = ProfileBuilder::new(&debug, Some(0x1000), &[], ReportFormat::Chrome);
        assert_eq!(builder.runtime_pc(4), 0x1004);
        assert_eq!(builder.runtime_pc(0x3004), 0x3004);
    }

    #[test]
    fn inline_call_sites_are_assigned_to_their_parent_frames() {
        use crate::debuginfo::{Function, HunkAddr, HunkMeta, InlineFunction, LineRow, SourceFile};
        let mut debug = crate::debuginfo::DebugInfo::default();
        debug.hunks = vec![HunkMeta {
            kind: crate::debuginfo::hunk::HunkKind::Code,
            size: 0x100,
        }];
        debug.files = vec![
            SourceFile {
                path: "parent.c".into(),
            },
            SourceFile {
                path: "outer.c".into(),
            },
            SourceFile {
                path: "leaf.c".into(),
            },
        ];
        debug.rows = vec![LineRow {
            at: HunkAddr::new(0, 0x20),
            file: 2,
            line: 300,
            column: 7,
            is_stmt: true,
            end_sequence: false,
        }];
        debug.functions = vec![Function {
            name: "parent".into(),
            at: HunkAddr::new(0, 0),
            size: 0x100,
            frame_base: crate::debuginfo::Location::Unsupported,
            params: Vec::new(),
            locals: Vec::new(),
            file: Some(0),
            line: Some(1),
        }];
        debug.inline_functions = vec![
            InlineFunction {
                name: "outer".into(),
                ranges: vec![(HunkAddr::new(0, 0x20), 2)],
                file: Some(0),
                line: Some(10),
                depth: 0,
            },
            InlineFunction {
                name: "leaf".into(),
                ranges: vec![(HunkAddr::new(0, 0x20), 2)],
                file: Some(1),
                line: Some(20),
                depth: 1,
            },
        ];
        debug.relocate(vec![0x1000]);
        let builder = ProfileBuilder::new(&debug, Some(0x1000), &[], ReportFormat::Chrome);
        let locations = builder.locations_for_pc(0x1020);
        assert_eq!(
            locations[0],
            Location {
                function: "parent".into(),
                url: "parent.c".into(),
                line: 9,
                column: 0
            }
        );
        assert_eq!(
            locations[1],
            Location {
                function: "outer (inlined)".into(),
                url: "outer.c".into(),
                line: 19,
                column: 0
            }
        );
        assert_eq!(
            locations[2],
            Location {
                function: "leaf (inlined)".into(),
                url: "leaf.c".into(),
                line: 299,
                column: 6
            }
        );
    }

    #[test]
    fn repeated_per_frame_outputs_are_not_overwritten() {
        let out = Path::new("profile.cpuprofile");
        assert_eq!(
            frame_output_path(out, 42, 1),
            PathBuf::from("profile-000042.cpuprofile")
        );
        assert_eq!(
            frame_output_path(out, 42, 2),
            PathBuf::from("profile-000042-002.cpuprofile")
        );
    }

    #[test]
    fn parses_bartman_stream_and_contention_metadata() {
        let mut stream = Vec::new();
        for value in [0x20u32, u32::MAX - 12] {
            stream.extend_from_slice(&value.to_le_bytes());
        }
        let mut metadata = b"CLSM".to_vec();
        for value in [1u32, 1, 12, 8, 4, u32::MAX, u32::MAX] {
            metadata.extend_from_slice(&value.to_le_bytes());
        }
        let samples = parse_frame(&stream, &metadata, false).unwrap();
        assert_eq!(samples[0].pcs, vec![0x20]);
        assert_eq!(samples[0].instruction_cck, 8);
        assert_eq!(samples[0].bus_wait_cck, 4);
    }
}
