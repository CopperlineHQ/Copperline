// SPDX-License-Identifier: GPL-3.0-or-later

//! Guest memory as the frontend sees it: the address map handed to
//! `RETRO_ENVIRONMENT_SET_MEMORY_MAPS` and the trainer-style cheat pokes
//! applied between frames.

use crate::abi::{MemoryDescriptor, MEMDESC_BIGENDIAN, MEMDESC_CONST, MEMDESC_SYSTEM_RAM};
use anyhow::{bail, ensure, Context, Result};
use copperline::bus::Bus;
use copperline::memory::{ACCEL_RAM_BASE, CHIP_RAM_BASE, ROM_BASE, SLOW_RAM_BASE};
use std::ffi::CStr;

const CHIP: &CStr = c"chip";
const SLOW: &CStr = c"slow";
const FAST: &CStr = c"fast";
const ROM: &CStr = c"rom";

/// A cheap stand-in for the descriptor list, hashed from everything
/// [`descriptors`] reads: the banks' host addresses and sizes, and the
/// autoconfigured Zorro windows. Building the descriptors allocates, and
/// the two hottest entry points (a frame, a rollback restore) would
/// otherwise pay that just to discover the map has not moved.
pub fn fingerprint(bus: &Bus) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    let mem = &bus.mem;
    for bank in [
        &mem.chip_ram,
        &mem.slow_ram,
        &mem.mb_ram,
        &mem.accel_ram,
        &mem.rom,
        &mem.extended_rom,
    ] {
        (bank.as_ptr() as usize, bank.len()).hash(&mut hasher);
    }
    (mem.mb_ram_base(), mem.extended_rom_base).hash(&mut hasher);
    for window in mem.zorro.fast_ram_windows() {
        window.hash(&mut hasher);
        let ram = mem.zorro.board_ram(window.2);
        (ram.as_ptr() as usize, ram.len()).hash(&mut hasher);
    }
    hasher.finish()
}

/// Every RAM bank and ROM window the machine decodes, in ascending address
/// order. A bank is split into naturally aligned power-of-two blocks so
/// each descriptor's `select` mask identifies exactly its own window: a
/// libretro descriptor covers one aligned block, and an 8 MiB Zorro II bank
/// at $200000 is three of them ($200000, $400000 and $800000). Zorro
/// windows appear once the guest has autoconfigured the boards, so the map
/// changes after boot and after a reset.
pub fn descriptors(bus: &mut Bus) -> Vec<MemoryDescriptor> {
    let ram = MEMDESC_BIGENDIAN;
    let rom = MEMDESC_BIGENDIAN | MEMDESC_CONST;
    let mem = &mut bus.mem;
    let mb_base = mem.mb_ram_base();
    let extended_base = mem.extended_rom_base;
    let mut banks: Vec<(u64, *mut u8, usize, &CStr, u64)> = vec![
        (
            CHIP_RAM_BASE,
            mem.chip_ram.as_mut_ptr(),
            mem.chip_ram.len(),
            CHIP,
            ram | MEMDESC_SYSTEM_RAM,
        ),
        (
            SLOW_RAM_BASE,
            mem.slow_ram.as_mut_ptr(),
            mem.slow_ram.len(),
            SLOW,
            ram,
        ),
        (
            mb_base,
            mem.mb_ram.as_mut_ptr(),
            mem.mb_ram.len(),
            FAST,
            ram,
        ),
        (
            ACCEL_RAM_BASE,
            mem.accel_ram.as_mut_ptr(),
            mem.accel_ram.len(),
            FAST,
            ram,
        ),
        (ROM_BASE, mem.rom.as_mut_ptr(), mem.rom.len(), ROM, rom),
        (
            extended_base,
            mem.extended_rom.as_mut_ptr(),
            mem.extended_rom.len(),
            ROM,
            rom,
        ),
    ];
    let windows: Vec<_> = mem.zorro.fast_ram_windows().collect();
    for (base, len, board) in windows {
        let bank = mem.zorro.board_ram_mut(board);
        let len = (len as usize).min(bank.len());
        banks.push((u64::from(base), bank.as_mut_ptr(), len, FAST, ram));
    }
    let mut out = Vec::new();
    for (base, ptr, total, space, flags) in banks {
        let mut offset = 0usize;
        while offset < total {
            let addr = base as usize + offset;
            // The largest aligned power-of-two block that fits at `addr`.
            let align = if addr == 0 {
                usize::MAX
            } else {
                addr & addr.wrapping_neg()
            };
            let remaining = total - offset;
            let len = (1usize << (usize::BITS - 1 - remaining.leading_zeros())).min(align);
            out.push(MemoryDescriptor {
                flags,
                ptr: ptr.wrapping_add(offset).cast(),
                offset: 0,
                start: addr,
                select: 0,
                disconnect: 0,
                len,
                addrspace: space.as_ptr(),
            });
            offset += len;
        }
    }
    out.sort_by_key(|descriptor| descriptor.start);
    // `select` is the bits distinguishing this block from every other
    // address in the map: the address width the map spans, less the
    // block's own offset bits.
    let top = out
        .iter()
        .map(|descriptor| descriptor.start + descriptor.len - 1)
        .max()
        .unwrap_or(0);
    let width = usize::MAX >> top.leading_zeros();
    for descriptor in &mut out {
        descriptor.select = width & !(descriptor.len - 1);
    }
    out
}

/// One write of a cheat code: `size` bytes of `value`, big-endian, at
/// `addr`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Poke {
    pub addr: u32,
    pub value: u32,
    pub size: usize,
}

/// A parsed cheat: the pokes it applies each frame while enabled.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cheat {
    pub enabled: bool,
    pub pokes: Vec<Poke>,
}

/// Parse `AAAAAA:VV`, `AAAAAA:VVVV` or `AAAAAA:VVVVVVVV` (byte, word, long
/// pokes), several joined with `+`. Addresses are hexadecimal up to 32
/// bits; the value's digit count picks the width. Whitespace around parts
/// is ignored and letters are case-insensitive.
pub fn parse(code: &str) -> Result<Vec<Poke>> {
    let mut pokes = Vec::new();
    for part in code.split('+') {
        let part = part.trim();
        let (addr, value) = part
            .split_once(':')
            .with_context(|| format!("cheat part {part:?} is not ADDRESS:VALUE"))?;
        let (addr, value) = (addr.trim(), value.trim());
        ensure!(
            !addr.is_empty() && addr.len() <= 8 && addr.bytes().all(|b| b.is_ascii_hexdigit()),
            "cheat address {addr:?} is not a hexadecimal address of up to 8 digits"
        );
        let size = match value.len() {
            2 => 1,
            4 => 2,
            8 => 4,
            _ => bail!("cheat value {value:?} must be 2, 4 or 8 hexadecimal digits"),
        };
        ensure!(
            value.bytes().all(|b| b.is_ascii_hexdigit()),
            "cheat value {value:?} is not hexadecimal"
        );
        pokes.push(Poke {
            addr: u32::from_str_radix(addr, 16)?,
            value: u32::from_str_radix(value, 16)?,
            size,
        });
    }
    ensure!(!pokes.is_empty(), "empty cheat code");
    Ok(pokes)
}

/// Apply one poke straight into the RAM bank decoding `addr`, like a
/// trainer writing through the CPU bus without the bus. Writes outside any
/// fitted RAM (custom registers, ROM, unconfigured boards) do nothing.
pub fn poke(bus: &mut Bus, poke: Poke) {
    let Some(bytes) = ram_at(bus, poke.addr, poke.size) else {
        return;
    };
    let value = poke.value.to_be_bytes();
    bytes.copy_from_slice(&value[4 - poke.size..]);
}

fn ram_at(bus: &mut Bus, addr: u32, size: usize) -> Option<&mut [u8]> {
    let mb_base = bus.mem.mb_ram_base();
    let copperline::memory::Memory {
        chip_ram,
        slow_ram,
        mb_ram,
        accel_ram,
        zorro,
        ..
    } = &mut bus.mem;
    let banks = [
        (CHIP_RAM_BASE, chip_ram),
        (SLOW_RAM_BASE, slow_ram),
        (mb_base, mb_ram),
        (ACCEL_RAM_BASE, accel_ram),
    ];
    for (base, bank) in banks {
        let offset = u64::from(addr).wrapping_sub(base);
        if offset + size as u64 <= bank.len() as u64 {
            let offset = offset as usize;
            return Some(&mut bank[offset..offset + size]);
        }
    }
    let (board, offset) = zorro.region_at(addr, size)?;
    Some(&mut zorro.board_ram_mut(board)[offset..offset + size])
}

/// The address space name of a descriptor, for tests.
#[cfg(test)]
pub fn addrspace(descriptor: &MemoryDescriptor) -> &'static str {
    // The descriptors only ever carry the static names defined above.
    unsafe { CStr::from_ptr(descriptor.addrspace) }
        .to_str()
        .unwrap_or("?")
}
