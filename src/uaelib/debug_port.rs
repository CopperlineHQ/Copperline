// SPDX-License-Identifier: GPL-3.0-or-later

//! WinUAE's write-only printf ports: queue arguments at $BFFF00, then
//! write the format-string pointer to $BFFF04. Integer conversions default
//! to Amiga words; `l` selects a longword. No host printf or guest I/O reads.

use super::{guest_byte, guest_bytes, guest_cstring, DEBUG_TEXT_MAX};
use crate::memory::Memory;

pub(crate) const ARGUMENT: u32 = 0x00BF_FF00;
pub(crate) const FORMAT: u32 = ARGUMENT + 4;
const ARGUMENT_MAX: usize = 32;

pub(crate) fn decodes(addr: u32) -> bool {
    matches!(addr, ARGUMENT | FORMAT) || addr == ARGUMENT + 2 || addr == FORMAT + 2
}

#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub(super) struct DebugPort {
    arguments: Vec<u32>,
    /// A provisional word argument is replaced when its low half arrives.
    /// The register is kept too, so unrelated halves cannot ring the port.
    pending_word: Option<(u32, u16)>,
}

impl DebugPort {
    pub(super) fn write(
        &mut self,
        addr: u32,
        size: usize,
        mut value: u32,
        mem: &Memory,
        address_mask: u32,
    ) -> Option<String> {
        let mut register = addr;
        match size {
            1 if addr == ARGUMENT || addr == FORMAT => {
                value &= 0xff;
                self.pending_word = None;
            }
            2 if addr == ARGUMENT || addr == FORMAT => {
                value &= 0xffff;
                self.pending_word = Some((addr, value as u16));
            }
            2 if addr == ARGUMENT + 2 || addr == FORMAT + 2 => {
                let (high_register, high) = self.pending_word.take()?;
                if high_register + 2 != addr {
                    return None;
                }
                register = high_register;
                value = (u32::from(high) << 16) | (value & 0xffff);
                if register == ARGUMENT {
                    self.arguments.pop();
                }
            }
            4 if addr == ARGUMENT || addr == FORMAT => self.pending_word = None,
            _ => return None,
        }
        if register == ARGUMENT {
            if self.arguments.len() < ARGUMENT_MAX {
                self.arguments.push(value);
            } else {
                // A dropped high word must not replace the previous argument
                // when its low half arrives.
                self.pending_word = None;
            }
            return None;
        }
        if register != FORMAT || (size != 4 && addr != FORMAT + 2) {
            return None;
        }
        self.pending_word = None;
        // A trigger consumes the queue even when the format is unreadable.
        let arguments = std::mem::take(&mut self.arguments);
        let format = guest_cstring(mem, value & address_mask, DEBUG_TEXT_MAX, address_mask)?;
        Some(format_message(&format, &arguments, mem, address_mask))
    }
}

#[derive(Default)]
struct Spec {
    left: bool,
    plus: bool,
    space: bool,
    alternate: bool,
    zero: bool,
    width: usize,
    precision: Option<usize>,
    long: bool,
    conversion: u8,
}

impl Spec {
    fn parse(format: &[u8], at: &mut usize) -> Option<Self> {
        let mut spec = Self::default();
        loop {
            match *format.get(*at)? {
                b'-' => spec.left = true,
                b'+' => spec.plus = true,
                b' ' => spec.space = true,
                b'#' => spec.alternate = true,
                b'0' => spec.zero = true,
                _ => break,
            }
            *at += 1;
        }
        spec.width = number(format, at);
        if format.get(*at) == Some(&b'.') {
            *at += 1;
            spec.precision = Some(number(format, at));
        }
        if format.get(*at) == Some(&b'l') {
            spec.long = true;
            *at += 1;
        }
        spec.conversion = *format.get(*at)?;
        *at += 1;
        Some(spec)
    }

    fn integer(&self, value: u32) -> String {
        let value = if self.long { value } else { value & 0xffff };
        let signed = matches!(self.conversion, b'd' | b'i');
        let integer = if self.long {
            i64::from(value as i32)
        } else {
            i64::from(value as i16)
        };
        let mut digits = match self.conversion {
            b'x' => format!("{value:x}"),
            b'X' => format!("{value:X}"),
            b'o' => format!("{value:o}"),
            _ if signed => integer.unsigned_abs().to_string(),
            _ => value.to_string(),
        };
        if self.precision == Some(0) && value == 0 {
            digits.clear();
        }
        if let Some(precision) = self.precision {
            digits = format!(
                "{}{}",
                "0".repeat(precision.saturating_sub(digits.len())),
                digits
            );
        }
        let sign = if signed && integer < 0 {
            "-"
        } else if signed && self.plus {
            "+"
        } else if signed && self.space {
            " "
        } else {
            ""
        };
        let prefix = match self.conversion {
            b'x' if self.alternate && value != 0 => "0x",
            b'X' if self.alternate && value != 0 => "0X",
            b'o' if self.alternate && !digits.starts_with('0') => "0",
            _ => "",
        };
        let padding = self
            .width
            .saturating_sub(sign.len() + prefix.len() + digits.len());
        if self.left {
            format!("{sign}{prefix}{digits}{}", " ".repeat(padding))
        } else if self.zero && self.precision.is_none() {
            format!("{sign}{prefix}{}{digits}", "0".repeat(padding))
        } else {
            format!("{}{sign}{prefix}{digits}", " ".repeat(padding))
        }
    }
}

fn number(format: &[u8], at: &mut usize) -> usize {
    let mut n = 0usize;
    while let Some(b'0'..=b'9') = format.get(*at) {
        n = (n * 10 + usize::from(format[*at] - b'0')).min(DEBUG_TEXT_MAX);
        *at += 1;
    }
    n
}

fn append(out: &mut Vec<u8>, bytes: &[u8]) {
    out.extend_from_slice(&bytes[..bytes.len().min(DEBUG_TEXT_MAX.saturating_sub(out.len()))]);
}

fn append_text(out: &mut Vec<u8>, bytes: &[u8], spec: &Spec) {
    let bytes = &bytes[..spec.precision.unwrap_or(bytes.len()).min(bytes.len())];
    let padding = vec![b' '; spec.width.saturating_sub(bytes.len())];
    if !spec.left {
        append(out, &padding);
    }
    append(out, bytes);
    if spec.left {
        append(out, &padding);
    }
}

fn format_message(format: &[u8], arguments: &[u32], mem: &Memory, mask: u32) -> String {
    let mut out = Vec::new();
    let mut at = 0;
    let mut values = arguments.iter();
    while at < format.len() && out.len() < DEBUG_TEXT_MAX {
        if format[at] != b'%' {
            out.push(format[at]);
            at += 1;
            continue;
        }
        let start = at;
        at += 1;
        if format.get(at) == Some(&b'%') {
            out.push(b'%');
            at += 1;
            continue;
        }
        let Some(spec) = Spec::parse(format, &mut at) else {
            append(&mut out, &format[start..]);
            break;
        };
        if !matches!(
            spec.conversion,
            b'd' | b'i' | b'u' | b'x' | b'X' | b'o' | b'p' | b'c' | b's' | b'b'
        ) {
            append(&mut out, &format[start..at]);
            continue;
        }
        let Some(&value) = values.next() else {
            append(&mut out, b"<missing>");
            continue;
        };
        match spec.conversion {
            b'p' => append(&mut out, format!("${value:08x}").as_bytes()),
            b'c' => append_text(&mut out, &[value as u8], &spec),
            b's' | b'b' => {
                let address = value & mask;
                let text = if spec.conversion == b's' {
                    guest_cstring(mem, address, DEBUG_TEXT_MAX, mask)
                } else {
                    guest_byte(mem, address)
                        .and_then(|len| {
                            guest_bytes(mem, address.wrapping_add(1) & mask, usize::from(len), mask)
                        })
                        .map(|mut bytes| {
                            for b in &mut bytes {
                                if *b == 0 {
                                    *b = b'.';
                                }
                            }
                            bytes
                        })
                };
                append_text(&mut out, text.as_deref().unwrap_or(b"<invalid>"), &spec);
            }
            _ => append(&mut out, spec.integer(value).as_bytes()),
        }
    }
    let mut text = String::from_utf8_lossy(&out).into_owned();
    if text.len() > DEBUG_TEXT_MAX {
        let mut end = DEBUG_TEXT_MAX;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::zorro::ZorroChain;

    const MASK: u32 = 0x00FF_FFFF;

    fn memory() -> Memory {
        let mut mem = Memory::placeholder(512 * 1024, 0, ZorroChain::default());
        mem.chip_ram[0x1000..0x1006].copy_from_slice(b"hello\0");
        mem.chip_ram[0x1100..0x1105].copy_from_slice(b"\x04ab\0c");
        mem
    }

    #[test]
    fn debug_port_formats_amiga_words_longs_strings_and_padding() {
        let mem = memory();
        for (format, args, expected) in [
            (
                "%d %ld %u %lu",
                vec![0xffff, 0xffff_ffff, 0xffff_ffff, 0xffff_ffff],
                "-1 -1 65535 4294967295",
            ),
            (
                "%x %08lx %X %o %p",
                vec![0x1234_abcd, 0x1234_abcd, 0xabcd, 8, 0x1234],
                "abcd 1234abcd ABCD 10 $00001234",
            ),
            (
                "%c %s %b %%",
                vec![b'A' as u32, 0x1000, 0x1100],
                "A hello ab.c %",
            ),
            (
                "%+06ld %#08lx %-6.3s %.0u %#.0o",
                vec![(-42i32) as u32, 42, 0x1000, 0, 0],
                "-00042 0x00002a hel     0",
            ),
            (
                "%ld %ld",
                vec![0x1234_5678, 0x1234_5678],
                "305419896 305419896",
            ),
        ] {
            assert_eq!(
                format_message(format.as_bytes(), &args, &mem, MASK),
                expected,
                "{format}"
            );
        }
    }

    #[test]
    fn debug_port_bounds_bad_formats_missing_arguments_and_guest_pointers() {
        let mem = memory();
        assert_eq!(
            format_message(b"%n %f %ld %ld %", &[42], &mem, MASK),
            "%n %f 42 <missing> %"
        );
        assert_eq!(
            format_message(b"%s %b", &[0xbf_ff00, 0xbf_ff04], &mem, MASK),
            "<invalid> <invalid>"
        );
        assert_eq!(
            format_message(b"%99999999999999999999ld", &[42], &mem, MASK).len(),
            DEBUG_TEXT_MAX
        );
        assert_eq!(
            format_message(b"%.99999999999999999ld", &[42], &mem, MASK).len(),
            DEBUG_TEXT_MAX
        );
        assert_eq!(
            format_message(&vec![b'x'; DEBUG_TEXT_MAX * 2], &[], &mem, MASK).len(),
            DEBUG_TEXT_MAX
        );
        assert!(
            format_message(&vec![0xff; DEBUG_TEXT_MAX], &[], &mem, MASK).len() <= DEBUG_TEXT_MAX
        );
    }

    #[test]
    fn debug_port_masks_byte_and_word_arguments_and_waits_for_a_complete_format_pointer() {
        let mut mem = memory();
        mem.chip_ram[0x2000..0x200b].copy_from_slice(b"%c %d %ld\n\0");
        let mut port = DebugPort::default();
        port.write(ARGUMENT, 1, 0x1234_0041, &mem, MASK);
        port.write(ARGUMENT, 2, 0x1234_ffd6, &mem, MASK);
        port.write(ARGUMENT, 4, 0xffff_ffd6, &mem, MASK);
        assert!(port.write(FORMAT, 1, 0, &mem, MASK).is_none());
        assert!(port.write(FORMAT, 2, 0, &mem, MASK).is_none());
        assert_eq!(
            port.write(FORMAT + 2, 2, 0x2000, &mem, MASK).as_deref(),
            Some("A -42 -42\n")
        );
        assert!(port.write(FORMAT + 2, 2, 0x2000, &mem, MASK).is_none());
    }

    #[test]
    fn debug_port_split_longs_overflow_and_invalid_triggers_consume_no_stale_values() {
        let mut mem = memory();
        mem.chip_ram[0x2000..0x2008].copy_from_slice(b"%ld %ld\0");
        let mut port = DebugPort::default();
        for _ in 0..2 {
            port.write(ARGUMENT, 2, 0x1234, &mem, MASK);
            port.write(ARGUMENT + 2, 2, 0x5678, &mem, MASK);
        }
        port.write(FORMAT, 2, 0, &mem, MASK);
        let encoded = bincode::serialize(&port).unwrap();
        let mut port: DebugPort = bincode::deserialize(&encoded).unwrap();
        assert_eq!(
            port.write(FORMAT + 2, 2, 0x2000, &mem, MASK).as_deref(),
            Some("305419896 305419896")
        );
        for i in 0..ARGUMENT_MAX {
            port.write(ARGUMENT, 4, i as u32, &mem, MASK);
        }
        port.write(ARGUMENT, 2, 0xbeef, &mem, MASK);
        port.write(ARGUMENT + 2, 2, 0xabcd, &mem, MASK);
        assert_eq!(port.arguments, (0..ARGUMENT_MAX as u32).collect::<Vec<_>>());
        assert!(port.write(FORMAT, 4, FORMAT, &mem, MASK).is_none());
        assert!(port.arguments.is_empty());
        assert_eq!(
            port.write(FORMAT, 4, 0x2000, &mem, MASK).as_deref(),
            Some("<missing> <missing>")
        );
    }
}
