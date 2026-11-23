// SPDX-License-Identifier: GPL-3.0-or-later

//! Resumable 53C710 SCRIPTS bus cycles. The instruction, table descriptor,
//! and data may each live on a different bus. Sample once per grant.

use super::*;
use crate::buster::Transfer;
use crate::zorro_device::DeviceHost;

#[derive(Default, serde::Serialize, serde::Deserialize)]
pub(super) enum Script {
    #[default]
    Idle,
    Fetch,
    Operand(u32),
    Select(u32),
    TableCount(u32),
    TableAddress {
        at: u32,
        count: u32,
    },
    Block {
        addr: u32,
        left: u32,
        message: Vec<u8>,
    },
    MoveDestination {
        src: u32,
        count: u32,
    },
    MoveRead {
        src: u32,
        dst: u32,
        left: u32,
    },
    MoveWrite {
        src: u32,
        dst: u32,
        left: u32,
        data: [u8; 4],
        len: u8,
    },
}

impl A4091 {
    pub(super) fn start_scripts(&mut self, host: &mut DeviceHost) {
        self.script = Script::Fetch;
        if self.arbitrated {
            return;
        }
        // A machine without a slot arbiter uses the same transfers within
        // the register access. Bound runaway instruction loops there.
        let mut instructions = 0;
        while self.request_address().is_some() {
            if matches!(self.script, Script::Fetch) {
                if instructions == 4096 {
                    self.regs[REG_DSTAT] |= DSTAT_IID;
                    self.script = Script::Idle;
                    break;
                }
                instructions += 1;
            }
            self.transfer_cycle(host);
        }
    }

    pub(crate) fn request_address(&self) -> Option<u32> {
        Some(match &self.script {
            Script::Idle => return None,
            Script::Fetch | Script::MoveDestination { .. } => self.reg32(REG_DSP),
            Script::Operand(_) => self.reg32(REG_DSP).wrapping_add(4),
            Script::TableCount(addr) | Script::Select(addr) | Script::Block { addr, .. } => *addr,
            Script::TableAddress { at, .. } => *at,
            Script::MoveRead { src, .. } => *src,
            Script::MoveWrite { dst, .. } => *dst,
        })
    }

    pub(crate) fn transfer_cycle(&mut self, host: &mut DeviceHost) -> usize {
        let mut written = 0;
        self.transfer_with(|chip, addr, data| {
            let own = host.own_window_offset(addr).is_some();
            match data {
                Transfer::Read(bytes) if own => {
                    for (i, b) in bytes.iter_mut().enumerate() {
                        *b = chip.dma_byte(host, addr.wrapping_add(i as u32));
                    }
                }
                Transfer::Write(bytes) if own => {
                    for (i, &b) in bytes.iter().enumerate() {
                        chip.dma_write_byte(host, addr.wrapping_add(i as u32), b);
                    }
                }
                Transfer::Read(bytes) => host.dma_read(addr, bytes),
                Transfer::Write(bytes) => {
                    host.dma_write(addr, bytes);
                    written = bytes.len();
                }
            }
        });
        written
    }

    pub(crate) fn transfer_with(&mut self, mut access: impl FnMut(&mut Self, u32, Transfer)) {
        let read_long =
            |chip: &mut Self, addr, access: &mut dyn FnMut(&mut Self, u32, Transfer)| {
                let mut data = [0; 4];
                access(chip, addr, Transfer::Read(&mut data));
                u32::from_be_bytes(data)
            };
        let dsp = self.reg32(REG_DSP);
        self.script = match std::mem::take(&mut self.script) {
            Script::Idle => Script::Idle,
            Script::Fetch => Script::Operand(read_long(self, dsp, &mut access)),
            Script::Operand(opcode) => {
                let mut insn = [0; 8];
                insn[..4].copy_from_slice(&opcode.to_be_bytes());
                access(self, dsp.wrapping_add(4), Transfer::Read(&mut insn[4..]));
                self.decode_instruction(insn);
                return;
            }
            Script::Select(at) => {
                let entry = read_long(self, at, &mut access);
                self.scsi_select(entry);
                if self.conn.is_some() {
                    Script::Fetch
                } else {
                    Script::Idle
                }
            }
            Script::TableCount(at) => Script::TableAddress {
                at: at.wrapping_add(4),
                count: read_long(self, at, &mut access) & DBC_MASK,
            },
            Script::TableAddress { at, count } => {
                let addr = read_long(self, at, &mut access);
                let Some(conn) = self.conn.as_ref() else {
                    self.script = Script::Fetch;
                    return;
                };
                let phase = conn.phase;
                if let (Some(due), Some(now)) = (
                    self.block_move_due(phase, count as usize),
                    self.target_now(),
                ) {
                    if due > now {
                        self.set_reg32(REG_DSP, dsp.wrapping_sub(8));
                        self.stall_until = Some(due);
                        return;
                    }
                }
                let left = match phase {
                    Phase::DataIn | Phase::Status | Phase::MsgIn => {
                        count.min(conn.out.len() as u32)
                    }
                    Phase::DataOut => {
                        count.min(conn.data_out_needed.saturating_sub(conn.data_out.len()) as u32)
                    }
                    _ => count,
                };
                if left == 0 {
                    Script::Fetch
                } else {
                    Script::Block {
                        addr,
                        left,
                        message: Vec::new(),
                    }
                }
            }
            Script::Block {
                addr,
                left,
                mut message,
            } => {
                let Some(phase) = self.conn.as_ref().map(|c| c.phase) else {
                    self.script = Script::Fetch;
                    return;
                };
                if left == 0 {
                    self.dma_bus_error();
                    return;
                }
                let n = left.min(4 - (addr & 3)) as usize;
                let mut data = [0; 4];
                let beat = &mut data[..n];
                match phase {
                    Phase::DataIn | Phase::Status | Phase::MsgIn => {
                        let c = self.conn.as_mut().unwrap();
                        for byte in &mut *beat {
                            *byte = c.out.pop_front().unwrap_or(0);
                        }
                        if phase == Phase::DataIn {
                            if let Some(t) = &mut c.timing {
                                t.moved += n as u64;
                            }
                        }
                        let drained = c.out.is_empty();
                        access(self, addr, Transfer::Write(beat));
                        if drained {
                            self.advance_in_phase();
                        }
                    }
                    Phase::DataOut => {
                        access(self, addr, Transfer::Read(beat));
                        self.accept_data_out(beat);
                    }
                    Phase::MsgOut | Phase::Command => {
                        access(self, addr, Transfer::Read(beat));
                        message.extend_from_slice(beat);
                        if left == n as u32 {
                            if phase == Phase::Command {
                                self.scsi_command(&message);
                            } else {
                                self.scsi_msg_out(&message);
                            }
                        }
                    }
                }
                self.regs[REG_SFBR] = beat[n - 1];
                if left == n as u32 {
                    Script::Fetch
                } else {
                    Script::Block {
                        addr: addr.wrapping_add(n as u32),
                        left: left - n as u32,
                        message,
                    }
                }
            }
            Script::MoveDestination { src, count } => {
                let dst = read_long(self, dsp, &mut access);
                self.set_reg32(REG_DSP, dsp.wrapping_add(4));
                if count == 0 {
                    Script::Fetch
                } else {
                    Script::MoveRead {
                        src,
                        dst,
                        left: count,
                    }
                }
            }
            Script::MoveRead { src, dst, left } => {
                let len = left.min(4 - (src & 3)).min(4 - (dst & 3)) as u8;
                let mut data = [0; 4];
                access(self, src, Transfer::Read(&mut data[..len as usize]));
                Script::MoveWrite {
                    src,
                    dst,
                    left,
                    data,
                    len,
                }
            }
            Script::MoveWrite {
                src,
                dst,
                left,
                data,
                len,
            } => {
                if !(1..=4).contains(&len) || u32::from(len) > left {
                    self.dma_bus_error();
                    return;
                }
                access(self, dst, Transfer::Write(&data[..len as usize]));
                let used = u32::from(len);
                let left = left - used;
                self.regs[REG_DBC..REG_DBC + 3].copy_from_slice(&left.to_be_bytes()[1..]);
                if left == 0 {
                    Script::Fetch
                } else {
                    Script::MoveRead {
                        src: src.wrapping_add(used),
                        dst: dst.wrapping_add(used),
                        left,
                    }
                }
            }
        }
    }
}
