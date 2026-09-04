// SPDX-License-Identifier: GPL-3.0-or-later

//! Static executable-size reports in the same `.cpuprofile` container used by
//! the instruction profiler. The profile's time unit is bytes: opening it in
//! the VS Code Amiga profile viewer therefore gives a flame-chart breakdown of
//! hunk, section and function footprint without a second visualization format.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use object::{Object as _, ObjectSection as _};
use serde_json::{json, Value};

#[derive(Debug, Clone)]
struct SizedSymbol {
    name: String,
    offset: u32,
    size: u32,
    source: String,
    line: i64,
}

#[derive(Debug, Clone)]
struct HunkBreakdown {
    index: usize,
    kind: &'static str,
    size: u32,
    section: String,
    symbols: Vec<SizedSymbol>,
}

fn section_names(elf: Option<&[u8]>, hunk_count: usize) -> Result<Vec<String>, String> {
    let Some(elf) = elf else {
        return Ok(Vec::new());
    };
    let file = object::File::parse(elf).map_err(|error| format!("not an ELF: {error}"))?;
    if file.is_little_endian() {
        return Err("little-endian ELF: not a 68k program".into());
    }
    let mut names = Vec::new();
    for section in file.sections() {
        let allocated = match section.flags() {
            object::SectionFlags::Elf { sh_flags, .. } => {
                (sh_flags & object::elf::SHF_ALLOC).0 != 0
            }
            _ => false,
        };
        if allocated && section.size() != 0 {
            names.push(section.name().unwrap_or("<unnamed>").to_string());
        }
    }
    if names.len() != hunk_count {
        return Err(format!(
            "ELF has {} allocatable non-empty sections but the executable has {hunk_count} hunks",
            names.len()
        ));
    }
    Ok(names)
}

fn symbol_breakdown(
    debug: &crate::debuginfo::DebugInfo,
    hunk: usize,
    hunk_size: u32,
) -> Vec<SizedSymbol> {
    let mut candidates: Vec<(u32, Option<u32>, String, String, i64)> = debug
        .functions
        .iter()
        .filter(|function| function.at.hunk as usize == hunk && function.at.offset < hunk_size)
        .map(|function| {
            let (source, line) = function
                .file
                .and_then(|file| debug.files.get(file as usize))
                .map_or_else(
                    || (String::new(), -1),
                    |file| {
                        (
                            file.path.clone(),
                            function
                                .line
                                .map_or(-1, |line| i64::from(line.saturating_sub(1))),
                        )
                    },
                );
            (
                function.at.offset,
                Some(function.size),
                function.name.clone(),
                source,
                line,
            )
        })
        .collect();
    if candidates.is_empty() {
        candidates.extend(
            debug
                .symbols
                .iter()
                .filter(|symbol| symbol.at.hunk as usize == hunk && symbol.at.offset < hunk_size)
                .map(|symbol| {
                    (
                        symbol.at.offset,
                        symbol.size,
                        symbol.name.clone(),
                        String::new(),
                        -1,
                    )
                }),
        );
    }
    candidates.sort_by_key(|candidate| candidate.0);
    candidates.dedup_by(|left, right| left.0 == right.0);

    let mut result = Vec::new();
    for (index, (offset, declared, name, source, line)) in candidates.iter().enumerate() {
        let next = candidates
            .iter()
            .skip(index + 1)
            .find_map(|candidate| (candidate.0 > *offset).then_some(candidate.0))
            .unwrap_or(hunk_size);
        let available = next.min(hunk_size).saturating_sub(*offset);
        let size = declared.unwrap_or(available).min(available);
        if size != 0 {
            result.push(SizedSymbol {
                name: name.clone(),
                offset: *offset,
                size,
                source: source.clone(),
                line: *line,
            });
        }
    }
    result
}

fn breakdown(debug: &crate::debuginfo::DebugInfo, sections: &[String]) -> Vec<HunkBreakdown> {
    debug
        .hunks
        .iter()
        .enumerate()
        .map(|(index, hunk)| HunkBreakdown {
            index,
            kind: hunk.kind.name(),
            size: hunk.size,
            section: sections
                .get(index)
                .cloned()
                .unwrap_or_else(|| hunk.kind.name().to_string()),
            symbols: symbol_breakdown(debug, index, hunk.size),
        })
        .collect()
}

fn call_frame(name: &str, source: &str, line: i64) -> Value {
    json!({
        "functionName": name,
        "scriptId": source,
        "url": source,
        "lineNumber": line,
        "columnNumber": -1,
    })
}

fn profile_value(hunks: &[HunkBreakdown], file_bytes: u64) -> Value {
    let mut nodes = vec![json!({
        "id": 1,
        "callFrame": call_frame("(root)", "", -1),
        "hitCount": 0,
        "children": [],
    })];
    let mut next_id = 2u32;
    let mut samples = Vec::new();
    let mut deltas = Vec::new();
    let mut hunk_rows = Vec::new();

    for hunk in hunks {
        let hunk_id = next_id;
        next_id += 1;
        let section_id = next_id;
        next_id += 1;
        let mut section_children = Vec::new();
        let mut ranges: BTreeMap<u32, u32> = BTreeMap::new();
        let mut function_rows = Vec::new();
        for symbol in &hunk.symbols {
            let id = next_id;
            next_id += 1;
            section_children.push(id);
            nodes.push(json!({
                "id": id,
                "callFrame": call_frame(&symbol.name, &symbol.source, symbol.line),
                "hitCount": 1,
                "children": [],
            }));
            samples.push(id);
            deltas.push(symbol.size);
            ranges.insert(symbol.offset, symbol.offset.saturating_add(symbol.size));
            function_rows.push(json!({
                "name": symbol.name,
                "offset": symbol.offset,
                "bytes": symbol.size,
            }));
        }
        let attributed: u32 = ranges
            .into_iter()
            .fold((0u32, 0u32), |(end, total), (start, range_end)| {
                let added = range_end.saturating_sub(start.max(end));
                (end.max(range_end), total.saturating_add(added))
            })
            .1;
        let unattributed = hunk.size.saturating_sub(attributed);
        if unattributed != 0 {
            let id = next_id;
            next_id += 1;
            section_children.push(id);
            nodes.push(json!({
                "id": id,
                "callFrame": call_frame("[unattributed]", "", -1),
                "hitCount": 1,
                "children": [],
            }));
            samples.push(id);
            deltas.push(unattributed);
        }
        nodes.push(json!({
            "id": section_id,
            "callFrame": call_frame(&hunk.section, "", -1),
            "hitCount": 0,
            "children": section_children,
        }));
        nodes.push(json!({
            "id": hunk_id,
            "callFrame": call_frame(&format!("Hunk {} ({})", hunk.index, hunk.kind), "", -1),
            "hitCount": 0,
            "children": [section_id],
        }));
        hunk_rows.push(json!({
            "index": hunk.index,
            "kind": hunk.kind,
            "section": hunk.section,
            "bytes": hunk.size,
            "functions": function_rows,
            "unattributedBytes": unattributed,
        }));
        nodes[0]["children"]
            .as_array_mut()
            .expect("root children are an array")
            .push(json!(hunk_id));
    }
    nodes.sort_by_key(|node| node["id"].as_u64().unwrap_or(0));
    let total: u64 = hunks.iter().map(|hunk| u64::from(hunk.size)).sum();
    json!({
        "nodes": nodes,
        "startTime": 0,
        "endTime": total,
        "samples": samples,
        "timeDeltas": deltas,
        "$copperline": {
            "version": 1,
            "metric": "bytes",
            "unit": "bytes",
            "fileBytes": file_bytes,
            "totalBytes": total,
            "hunks": hunk_rows,
        },
        "$amiga": {
            "chipsetFlags": 0,
            "customRegs": [],
            "agaColors": [],
            "dmaRecords": [],
            "gfxResources": [],
            "idleCycles": 0,
            "uniqueCallFrames": [],
            "callFrames": [],
            "pcTrace": [],
            "registerTrace": [],
        },
    })
}

/// Generate a byte-weighted `.cpuprofile` for an Amiga hunk executable.
pub fn generate(program: &Path, elf: Option<&Path>, out: &Path) -> Result<PathBuf, String> {
    let program_bytes =
        fs::read(program).map_err(|error| format!("{}: {error}", program.display()))?;
    let elf_bytes = elf
        .map(|path| fs::read(path).map_err(|error| format!("{}: {error}", path.display())))
        .transpose()?;
    let debug = crate::debuginfo::DebugInfo::load(&program_bytes, elf_bytes.as_deref())?;
    let sections = section_names(elf_bytes.as_deref(), debug.hunks.len())?;
    let value = profile_value(&breakdown(&debug, &sections), program_bytes.len() as u64);
    crate::paths::ensure_parent(out).map_err(|error| error.to_string())?;
    fs::write(
        out,
        serde_json::to_vec(&value).map_err(|error| error.to_string())?,
    )
    .map_err(|error| format!("{}: {error}", out.display()))?;
    Ok(out.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn size_profile_weights_sum_to_the_hunk_allocation() {
        let program = include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/guest/dap-test/hello"));
        let debug = crate::debuginfo::DebugInfo::load(program, None).expect("test executable");
        let value = profile_value(&breakdown(&debug, &[]), program.len() as u64);
        let weights: u64 = value["timeDeltas"]
            .as_array()
            .expect("weights")
            .iter()
            .map(|weight| weight.as_u64().expect("integer weight"))
            .sum();
        assert_eq!(
            weights,
            value["$copperline"]["totalBytes"].as_u64().unwrap()
        );
        assert_eq!(value["$copperline"]["metric"], "bytes");
        assert!(value["$copperline"]["hunks"].as_array().unwrap().len() >= 2);
    }
}
