// SPDX-License-Identifier: GPL-3.0-or-later

//! The debugger commands of the UAE debug adapter (uae-dap), which the
//! Amiga Assembly VS Code extension runs for FS-UAE and WinUAE, so the
//! same Debug Console and watch habits work against Copperline:
//!
//! - `m address[,size[,wordSize[,rowSize]]][,mode]`: memory dump
//! - `M address=bytes`: memory set
//! - `d address[,size]`: CPU disassembly
//! - `c address[,size]`: Copper disassembly
//! - `h [topic]`: help
//!
//! Also here: the `.dbgasm` document names the extension's *Disassemble
//! memory* and *Disassemble copper memory* commands open, and the number
//! formats of its *View as* variable menu. This module parses and
//! formats; the session evaluates the arguments and reads the machine.

use serde_json::Value;

/// The largest memory dump or disassembly a command serves, in bytes.
pub const MAX_BYTES: u32 = 64 * 1024;
/// The most instructions a `.dbgasm` document lists.
pub const MAX_DOCUMENT_INSTRUCTIONS: usize = 4096;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DumpMode {
    /// Hexadecimal words and the ASCII column (`ab`, the default).
    Both,
    /// Hexadecimal words only (`b`).
    Bytes,
    /// The ASCII column only (`a`).
    Ascii,
    /// CPU disassembly of the range instead (`d`).
    Disassemble,
    /// Copper disassembly of the range instead (`c`).
    Copper,
}

/// A parsed command. Addresses and sizes stay expression text for the
/// session to evaluate in the selected frame.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Command {
    Dump {
        address: String,
        size: Option<String>,
        word: Option<String>,
        row: Option<String>,
        mode: DumpMode,
    },
    Set {
        address: String,
        bytes: Vec<u8>,
    },
    Disassemble {
        address: String,
        size: Option<String>,
    },
    Copper {
        address: String,
        size: Option<String>,
    },
    Help(Option<String>),
}

/// `text` as a command, or `None` when it is an ordinary expression.
///
/// A command is its letter, whitespace, then arguments, as uae-dap
/// writes them. A letter followed by an operator stays an expression,
/// so a variable named `d` or `m` can still be watched (`d + 1`).
pub fn parse(text: &str) -> Option<Result<Command, String>> {
    let text = text.trim();
    if matches!(text, "h" | "H" | "?") {
        return Some(Ok(Command::Help(None)));
    }
    let mut chars = text.chars();
    let letter = chars.next()?;
    if !matches!(letter, 'm' | 'M' | 'd' | 'c' | 'h' | 'H' | '?') {
        return None;
    }
    let rest = chars.as_str();
    if !rest.starts_with(char::is_whitespace) {
        return None;
    }
    let rest = rest.trim();
    if rest.starts_with([
        '+', '-', '*', '/', '&', '|', '^', '<', '>', '=', ')', ']', '}',
    ]) {
        return None;
    }
    Some(match letter {
        'm' => parse_dump(rest),
        'M' => parse_set(rest),
        'd' => {
            parse_ranged(rest, "d").map(|(address, size)| Command::Disassemble { address, size })
        }
        'c' => parse_ranged(rest, "c").map(|(address, size)| Command::Copper { address, size }),
        _ => Ok(Command::Help(Some(rest.to_string()))),
    })
}

fn mode_of(arg: &str) -> Option<DumpMode> {
    Some(match arg.to_ascii_lowercase().as_str() {
        "ab" | "ba" => DumpMode::Both,
        "a" => DumpMode::Ascii,
        "b" => DumpMode::Bytes,
        "d" => DumpMode::Disassemble,
        "c" => DumpMode::Copper,
        _ => return None,
    })
}

fn parse_dump(rest: &str) -> Result<Command, String> {
    const SYNTAX: &str = "expected m address[,size=16[,wordSize=4[,rowSize=4]]][,ab|a|b|d|c]";
    let mut args = split_args(rest).map_err(|e| format!("{e}; {SYNTAX}"))?;
    let mut mode = DumpMode::Both;
    // A trailing mode letter, as in uae-dap: never the address itself.
    if args.len() > 1 {
        if let Some(m) = args.last().and_then(|a| mode_of(a)) {
            mode = m;
            args.pop();
        }
    }
    if args.is_empty() || args.len() > 4 {
        return Err(SYNTAX.into());
    }
    let mut args = args.into_iter();
    Ok(Command::Dump {
        address: args.next().expect("checked"),
        size: args.next(),
        word: args.next(),
        row: args.next(),
        mode,
    })
}

fn parse_set(rest: &str) -> Result<Command, String> {
    const SYNTAX: &str = "expected M address=bytes, bytes in hexadecimal (M $5c50=0ff534)";
    let (address, bytes) = rest.split_once('=').ok_or(SYNTAX)?;
    let address = address.trim();
    if address.is_empty() {
        return Err(SYNTAX.into());
    }
    let bytes = parse_hex_bytes(bytes).map_err(|e| format!("{e}; {SYNTAX}"))?;
    Ok(Command::Set {
        address: address.to_string(),
        bytes,
    })
}

fn parse_ranged(rest: &str, letter: &str) -> Result<(String, Option<String>), String> {
    let syntax = format!("expected {letter} address[,size=16]");
    let args = split_args(rest).map_err(|e| format!("{e}; {syntax}"))?;
    if args.is_empty() || args.len() > 2 {
        return Err(syntax);
    }
    let mut args = args.into_iter();
    Ok((args.next().expect("checked"), args.next()))
}

/// Split command arguments at the commas outside brackets, so an
/// argument can hold `@(a0,2)`.
pub fn split_args(text: &str) -> Result<Vec<String>, String> {
    let mut out = Vec::new();
    let mut depth = 0i32;
    let mut start = 0;
    for (i, c) in text.char_indices() {
        match c {
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => depth -= 1,
            ',' if depth == 0 => {
                out.push(text[start..i].trim().to_string());
                start = i + 1;
            }
            _ => {}
        }
    }
    out.push(text[start..].trim().to_string());
    if out.iter().any(String::is_empty) {
        return Err("empty argument".into());
    }
    Ok(out)
}

/// Unprefixed hexadecimal digits (`0ff534`) as bytes; a `$` or `0x`
/// prefix and spaces between bytes are accepted too.
pub fn parse_hex_bytes(text: &str) -> Result<Vec<u8>, String> {
    let text = text.trim();
    let text = text
        .strip_prefix('$')
        .or_else(|| text.strip_prefix("0x"))
        .or_else(|| text.strip_prefix("0X"))
        .unwrap_or(text);
    let digits: Vec<u8> = text
        .chars()
        .filter(|c| !c.is_whitespace())
        .map(|c| {
            c.to_digit(16)
                .map(|d| d as u8)
                .ok_or_else(|| format!("'{c}' is not a hexadecimal digit"))
        })
        .collect::<Result<_, _>>()?;
    if digits.is_empty() {
        return Err("no bytes to write".into());
    }
    if !digits.len().is_multiple_of(2) {
        return Err("bytes need an even number of hexadecimal digits".into());
    }
    if digits.len() / 2 > MAX_BYTES as usize {
        return Err(format!("at most {MAX_BYTES} bytes can be written at once"));
    }
    Ok(digits.chunks(2).map(|p| (p[0] << 4) | p[1]).collect())
}

/// A dump size or count argument, checked against `1..=max`.
pub fn checked_size(value: i64, what: &str, max: u32) -> Result<u32, String> {
    u32::try_from(value)
        .ok()
        .filter(|v| (1..=max).contains(v))
        .ok_or_else(|| format!("{what} must be 1 to {max}, not {value}"))
}

/// Memory rows as uae-dap formats them: `word`-byte groups in
/// hexadecimal, `row` groups per line, then (for `Both`) ` | ` and the
/// bytes as ASCII, with `.` for anything unprintable. Each row is its
/// address and text.
pub fn dump_rows(
    address: u32,
    bytes: &[u8],
    word: usize,
    row: usize,
    mode: DumpMode,
) -> Vec<(u32, String)> {
    let word = word.max(1);
    let row_bytes = word * row.max(1);
    let full_hex_width = row.max(1) * (word * 2) + row.max(1) - 1;
    bytes
        .chunks(row_bytes)
        .enumerate()
        .map(|(i, line)| {
            let hex: Vec<String> = line
                .chunks(word)
                .map(|w| w.iter().map(|b| format!("{b:02X}")).collect())
                .collect();
            let hex = hex.join(" ");
            let ascii: String = line
                .iter()
                .map(|&b| {
                    if (0x20..0x7F).contains(&b) {
                        b as char
                    } else {
                        '.'
                    }
                })
                .collect();
            let text = match mode {
                DumpMode::Bytes => hex,
                DumpMode::Ascii => ascii,
                _ => format!("{hex:<full_hex_width$} | {ascii}"),
            };
            (address.wrapping_add((i * row_bytes) as u32), text)
        })
        .collect()
}

/// The dump a hover (or a Watch entry) on a label shows, and in Watch
/// an address register too: uae-dap's `memoryFormats` launch argument.
/// The defaults are the extension's settings, `m ${symbol},24,2,4` for
/// hovers and `m ${symbol},104,2,4` for Watch.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MemoryFormat {
    /// Bytes to dump; 0 shows no dump.
    pub length: u32,
    pub word: usize,
    pub row: usize,
    pub mode: DumpMode,
}

impl MemoryFormat {
    pub const HOVER: Self = Self {
        length: 24,
        word: 2,
        row: 4,
        mode: DumpMode::Both,
    };
    pub const WATCH: Self = Self {
        length: 104,
        word: 2,
        row: 4,
        mode: DumpMode::Both,
    };

    /// `{"length", "wordLength", "rowLength", "mode"}`, each optional;
    /// out-of-range fields keep the default's.
    pub fn from_value(v: &Value, default: Self) -> Self {
        let field = |key: &str, max: u64| v[key].as_u64().filter(|n| *n <= max);
        Self {
            length: field("length", u64::from(MAX_BYTES)).map_or(default.length, |n| n as u32),
            word: field("wordLength", 16)
                .filter(|n| *n > 0)
                .map_or(default.word, |n| n as usize),
            row: field("rowLength", 64)
                .filter(|n| *n > 0)
                .map_or(default.row, |n| n as usize),
            mode: v["mode"]
                .as_str()
                .and_then(mode_of)
                .filter(|m| matches!(m, DumpMode::Both | DumpMode::Bytes | DumpMode::Ascii))
                .unwrap_or(default.mode),
        }
    }
}

/// The command summary `h` prints.
pub const HELP: &str = "\
Commands (the UAE debug adapter's, usable in the Debug Console and Watch):
  m address[,size=16[,wordSize=4[,rowSize=4]]][,ab]  Memory dump; ab: bytes and ASCII
                                                     (default), b: bytes, a: ASCII,
                                                     d: CPU or c: Copper disassembly
  M address=bytes                                    Memory set (bytes in hexadecimal)
  d address[,size=16]                                Disassemble size bytes of CPU code
  c address[,size=16]                                Disassemble size bytes of a Copper list
  h command|expressions                              Detailed help
  !method {json}                                     Raw control-protocol request
Arguments are expressions; 'h expressions' lists the syntax.
";

/// Detailed help for `h topic`.
pub fn help(topic: Option<&str>) -> String {
    let Some(topic) = topic.map(str::trim).filter(|t| !t.is_empty()) else {
        return HELP.to_string();
    };
    match topic {
        "m" => "\
m address[,size=16[,wordSize=4[,rowSize=4]]][,mode]
  Dump size bytes from address in groups of wordSize bytes, rowSize groups
  per line. mode: ab (bytes and ASCII, the default), b (bytes), a (ASCII),
  d (disassemble the range as CPU code), c (as Copper instructions).
  In Watch the dump updates at every stop; expand it for the rows.
  m $5c50,10            10 bytes from $5c50
  m CList,CListE-CList  the bytes between two labels
  m a0,64,2,8,b         64 bytes from A0 as words, eight per line
"
        .to_string(),
        "M" => "\
M address=bytes
  Write bytes, given as hexadecimal digits, from address.
  M $5c50=0ff534        three bytes at $5c50
  M MyLabel=4e71        a NOP at MyLabel
"
        .to_string(),
        "d" => "\
d address[,size=16]
  Disassemble the CPU instructions in size bytes from address.
  d pc,32               32 bytes from the program counter
"
        .to_string(),
        "c" => "\
c address[,size=16]
  Disassemble the Copper instructions (four bytes each) in size bytes
  from address.
  c CList,4*10          ten Copper instructions from CList
"
        .to_string(),
        "expressions" | "e" => "\
Expressions:
  registers d0-d7, a0-a7, sp, fp, pc, sr; symbols and variables by name
  numbers: 1234, $DFF000, 0xDFF000, %1010, 0b1010
  operators: + - * / & | ^ << >> ~, unary -, parentheses
  memory: [addr] (long), [addr].w, [addr].b, *addr
          @(addr[,size=4]) unsigned, @s(addr[,size=4]) signed (size 1, 2 or 4)
          #{addr} (the long at addr), ${expr} (the expression's value)
  value.w / value.b keep the low word or byte.
"
        .to_string(),
        other => format!("No help for '{other}'.\n{HELP}"),
    }
}

/// The document a `disassembledFileContents` request names. The
/// extension builds the names as uae-dap does:
/// `FRAME__ADDRESS__COUNT.dbgasm` for CPU code (ADDRESS evaluated in
/// stack frame FRAME), `copper_ADDRESS__COUNT.dbgasm` for a Copper list
/// (`1` and `2` are COP1LC and COP2LC), and `seg_N.dbgasm` for a whole
/// program hunk.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Document {
    Cpu {
        frame: usize,
        address: String,
        count: usize,
    },
    Copper {
        address: String,
        count: usize,
    },
    Segment(usize),
}

pub fn parse_document(path: &str) -> Result<Document, String> {
    let name = path.rsplit(['/', '\\']).next().unwrap_or(path);
    let stem = name
        .strip_suffix(".dbgasm")
        .ok_or_else(|| format!("{path:?} is not a disassembly document (.dbgasm)"))?;
    let bad = || format!("unrecognised disassembly document name {name:?}");
    let count = |text: &str| -> Result<usize, String> {
        let n: usize = text.parse().map_err(|_| bad())?;
        if n == 0 {
            return Err(bad());
        }
        Ok(n.min(MAX_DOCUMENT_INSTRUCTIONS))
    };
    if let Some(segment) = stem.strip_prefix("seg_") {
        return segment.parse().map(Document::Segment).map_err(|_| bad());
    }
    // The address is everything between the first and last separator,
    // so a symbol with underscores in it still parses.
    if let Some(rest) = stem.strip_prefix("copper_") {
        let (address, n) = rest.rsplit_once("__").ok_or_else(bad)?;
        if address.is_empty() {
            return Err(bad());
        }
        return Ok(Document::Copper {
            address: address.to_string(),
            count: count(n)?,
        });
    }
    let (frame, rest) = stem.split_once("__").ok_or_else(bad)?;
    let (address, n) = rest.rsplit_once("__").ok_or_else(bad)?;
    if address.is_empty() {
        return Err(bad());
    }
    Ok(Document::Cpu {
        frame: frame.parse().map_err(|_| bad())?,
        address: address.to_string(),
        count: count(n)?,
    })
}

/// A *View as* format from the extension's `modifyVariableFormat`
/// request (uae-dap's `NumberFormat`: 0 binary, 1 decimal, 2 hex).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NumberFormat {
    Binary,
    Decimal,
    Hexadecimal,
}

impl NumberFormat {
    pub fn from_code(code: u64) -> Option<Self> {
        Some(match code {
            0 => Self::Binary,
            1 => Self::Decimal,
            2 => Self::Hexadecimal,
            _ => return None,
        })
    }

    /// `raw`, `bits` wide, in this format. Decimal shows the signed
    /// reading too when the top bit is set.
    pub fn render(self, raw: u64, bits: u32) -> String {
        let bits = bits.clamp(1, 64);
        let raw = if bits == 64 {
            raw
        } else {
            raw & ((1u64 << bits) - 1)
        };
        match self {
            Self::Binary => format!("0b{raw:0width$b}", width = bits as usize),
            Self::Hexadecimal => format!("0x{raw:0width$X}", width = bits.div_ceil(4) as usize),
            Self::Decimal => {
                let shift = 64 - bits;
                let signed = ((raw << shift) as i64) >> shift;
                if signed < 0 {
                    format!("{raw} ({signed})")
                } else {
                    raw.to_string()
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cmd(text: &str) -> Command {
        parse(text)
            .unwrap_or_else(|| panic!("{text:?} is not a command"))
            .unwrap_or_else(|e| panic!("{text:?}: {e}"))
    }

    #[test]
    fn memory_dumps_parse_with_their_optional_fields() {
        assert_eq!(
            cmd("m $5c50,10"),
            Command::Dump {
                address: "$5c50".into(),
                size: Some("10".into()),
                word: None,
                row: None,
                mode: DumpMode::Both,
            }
        );
        assert_eq!(
            cmd("m a0, DATA_SIZE, 2, 4, a"),
            Command::Dump {
                address: "a0".into(),
                size: Some("DATA_SIZE".into()),
                word: Some("2".into()),
                row: Some("4".into()),
                mode: DumpMode::Ascii,
            }
        );
        // An argument may hold a comma inside brackets.
        assert_eq!(
            cmd("m @(a0,2),4,d"),
            Command::Dump {
                address: "@(a0,2)".into(),
                size: Some("4".into()),
                word: None,
                row: None,
                mode: DumpMode::Disassemble,
            }
        );
        // A lone argument is the address even when it spells a mode.
        assert!(
            matches!(cmd("m a"), Command::Dump { address, mode: DumpMode::Both, .. } if address == "a")
        );
        assert!(parse("m 1,2,3,4,5,ab").unwrap().is_err());
        assert!(parse("m 1,,2").unwrap().is_err());
    }

    #[test]
    fn other_commands_parse() {
        assert_eq!(
            cmd("M $5c50=0ff534"),
            Command::Set {
                address: "$5c50".into(),
                bytes: vec![0x0F, 0xF5, 0x34],
            }
        );
        assert_eq!(
            cmd("d pc,10"),
            Command::Disassemble {
                address: "pc".into(),
                size: Some("10".into()),
            }
        );
        assert_eq!(
            cmd("c CList"),
            Command::Copper {
                address: "CList".into(),
                size: None,
            }
        );
        assert_eq!(cmd("h"), Command::Help(None));
        assert_eq!(cmd("?"), Command::Help(None));
        assert_eq!(cmd("h m"), Command::Help(Some("m".into())));
        assert!(parse("M $100").unwrap().is_err());
        assert!(parse("M $100=abc").unwrap().is_err());
        assert!(parse("d 1,2,3").unwrap().is_err());
    }

    #[test]
    fn expressions_are_not_taken_for_commands() {
        for text in [
            "d0", "d", "c", "m", "mask", "d + 1", "m * 2", "c - 4", "D 1", "M",
        ] {
            assert!(parse(text).is_none(), "{text:?}");
        }
    }

    #[test]
    fn hex_bytes_accept_prefixes_and_spaces() {
        assert_eq!(
            parse_hex_bytes("$4E71 4E75"),
            Ok(vec![0x4E, 0x71, 0x4E, 0x75])
        );
        assert_eq!(parse_hex_bytes("0xff"), Ok(vec![0xFF]));
        assert!(parse_hex_bytes("").is_err());
        assert!(parse_hex_bytes("fg").is_err());
    }

    #[test]
    fn dump_rows_match_the_uae_dap_layout() {
        let bytes = b"\x01\xfc\x00\x00\x01\x00\x02\x00\x01\x80HI";
        let rows = dump_rows(0x1FC, bytes, 2, 4, DumpMode::Both);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0], (0x1FC, "01FC 0000 0100 0200 | ........".into()));
        // The last row is padded to the hexadecimal column's width.
        assert_eq!(rows[1], (0x204, format!("{:<19} | ..HI", "0180 4849")));
        let rows = dump_rows(0x10, bytes, 4, 4, DumpMode::Bytes);
        assert_eq!(rows, vec![(0x10, "01FC0000 01000200 01804849".into())]);
        let rows = dump_rows(0x10, b"ABC", 1, 2, DumpMode::Ascii);
        assert_eq!(rows, vec![(0x10, "AB".into()), (0x12, "C".into())]);
    }

    #[test]
    fn documents_parse_from_their_names() {
        assert_eq!(
            parse_document("0__$5c50__1000.dbgasm"),
            Ok(Document::Cpu {
                frame: 0,
                address: "$5c50".into(),
                count: 1000,
            })
        );
        assert_eq!(
            parse_document("/x/2___my_label__16.dbgasm"),
            Ok(Document::Cpu {
                frame: 2,
                address: "_my_label".into(),
                count: 16,
            })
        );
        assert_eq!(
            parse_document("copper_1__3000.dbgasm"),
            Ok(Document::Copper {
                address: "1".into(),
                count: 3000,
            })
        );
        assert_eq!(parse_document("seg_1.dbgasm"), Ok(Document::Segment(1)));
        assert_eq!(
            parse_document("0__pc__999999.dbgasm"),
            Ok(Document::Cpu {
                frame: 0,
                address: "pc".into(),
                count: MAX_DOCUMENT_INSTRUCTIONS,
            })
        );
        assert!(parse_document("main.s").is_err());
        assert!(parse_document("copper___3000.dbgasm").is_err());
        assert!(parse_document("x__pc__1.dbgasm").is_err());
        assert!(parse_document("0__pc__0.dbgasm").is_err());
    }

    #[test]
    fn memory_formats_read_the_uae_dap_launch_argument() {
        let v = serde_json::json!({"length": 32, "wordLength": 4, "mode": "b"});
        assert_eq!(
            MemoryFormat::from_value(&v, MemoryFormat::HOVER),
            MemoryFormat {
                length: 32,
                word: 4,
                row: 4,
                mode: DumpMode::Bytes,
            }
        );
        assert_eq!(
            MemoryFormat::from_value(&Value::Null, MemoryFormat::WATCH),
            MemoryFormat::WATCH
        );
        let v = serde_json::json!({"length": 0, "wordLength": 0, "mode": "d"});
        let f = MemoryFormat::from_value(&v, MemoryFormat::HOVER);
        assert_eq!((f.length, f.word, f.mode), (0, 2, DumpMode::Both));
    }

    #[test]
    fn number_formats_render_at_the_value_width() {
        assert_eq!(NumberFormat::from_code(0), Some(NumberFormat::Binary));
        assert_eq!(NumberFormat::from_code(3), None);
        assert_eq!(NumberFormat::Binary.render(5, 8), "0b00000101");
        assert_eq!(NumberFormat::Hexadecimal.render(0x1F, 16), "0x001F");
        assert_eq!(NumberFormat::Decimal.render(42, 32), "42");
        assert_eq!(
            NumberFormat::Decimal.render(0xFFFF_FFFF, 32),
            "4294967295 (-1)"
        );
        assert_eq!(NumberFormat::Decimal.render(0x1FF, 8), "255 (-1)");
    }
}
