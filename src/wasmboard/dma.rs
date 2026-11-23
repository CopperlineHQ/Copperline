// SPDX-License-Identifier: GPL-3.0-or-later

use super::{BoardCall, DmaHost, WasmBoard, MAX_PENDING_DMA_BYTES, MAX_PENDING_DMA_ENTRIES};
use crate::zorro::ZorroVersion;
use crate::zorro_device::DeviceHost;
use anyhow::{ensure, Result};
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;

#[derive(Default, Clone, Serialize, Deserialize)]
pub(super) struct Writes {
    queue: VecDeque<(u32, Vec<u8>)>,
    offset: usize,
}

impl Writes {
    pub(super) fn validate(&self) -> Result<usize> {
        ensure!(
            self.queue.len() <= MAX_PENDING_DMA_ENTRIES,
            "oversized DMA queue"
        );
        ensure!(
            self.queue
                .front()
                .map_or(self.offset == 0, |(_, b)| self.offset < b.len()),
            "invalid DMA cursor"
        );
        let mut bytes = 0usize;
        for (_, buf) in &self.queue {
            ensure!(
                !buf.is_empty() && buf.len() <= MAX_PENDING_DMA_BYTES - bytes,
                "invalid DMA buffer"
            );
            bytes += buf.len();
        }
        Ok(bytes)
    }
}

struct Read {
    addr: u32,
    bytes: Vec<u8>,
    offset: usize,
}

#[derive(Default)]
pub(crate) struct DmaPort {
    pub(crate) version: Option<ZorroVersion>,
    pub(super) writes: Writes,
    pub(super) queued_bytes: usize,
    read: Option<Read>,
}

impl DmaPort {
    pub(super) fn reset(&mut self) {
        *self = Self {
            version: self.version,
            ..Self::default()
        };
    }

    pub(crate) fn writing(&self) -> bool {
        !self.writes.queue.is_empty()
    }

    pub(crate) fn z3(&self) -> bool {
        self.version == Some(ZorroVersion::III)
    }

    pub(super) fn submit(&mut self, writes: Vec<(u32, Vec<u8>)>, bytes: usize) -> Result<()> {
        ensure!(
            writes.len() <= MAX_PENDING_DMA_ENTRIES - self.writes.queue.len()
                && bytes <= MAX_PENDING_DMA_BYTES - self.queued_bytes,
            "WASM board DMA queue full"
        );
        self.queued_bytes += bytes;
        self.writes
            .queue
            .extend(writes.into_iter().filter(|(_, data)| !data.is_empty()));
        Ok(())
    }

    pub(super) fn start_read(&mut self, addr: u32, bytes: Vec<u8>) {
        self.read = Some(Read {
            addr,
            bytes,
            offset: 0,
        });
    }

    pub(super) fn reading(&self) -> bool {
        self.read.as_ref().is_some_and(|r| r.offset < r.bytes.len())
    }

    pub(super) fn finish_read(&mut self) -> Vec<u8> {
        self.read.take().expect("active synchronous read").bytes
    }

    pub(crate) fn request_address(&self) -> Option<u32> {
        let addr = if let Some((base, _)) = self.writes.queue.front() {
            base.wrapping_add(self.writes.offset as u32)
        } else {
            let r = self.read.as_ref()?;
            if r.offset >= r.bytes.len() {
                return None;
            }
            r.addr.wrapping_add(r.offset as u32)
        };
        Some(if self.z3() { addr } else { addr & 0x00ff_ffff })
    }

    /// Deliver one aligned portion of a buffer after Buster and Agnus grant
    /// it. The DMA capability covers RAM. Holes and unsupported slaves have
    /// the same open-bus read / discarded-write convention as DeviceHost.
    pub(crate) fn transfer(&mut self, addr: u32, selected: bool, host: &mut DeviceHost) -> usize {
        let width = self.version.expect("wired DMA port").width();
        let lanes = width - (addr as usize & (width - 1));
        if let Some((_, bytes)) = self.writes.queue.front() {
            let offset = self.writes.offset;
            let n = lanes.min(bytes.len() - offset);
            let wrote = if selected {
                if let Some(dst) = host.dma_ram_mut(addr, n) {
                    dst.copy_from_slice(&bytes[offset..offset + n]);
                    n
                } else {
                    0
                }
            } else {
                0
            };
            self.writes.offset += n;
            if self.writes.offset == bytes.len() {
                self.queued_bytes -= bytes.len();
                self.writes.queue.pop_front();
                self.writes.offset = 0;
            }
            wrote
        } else {
            let r = self.read.as_mut().expect("active DMA request");
            let n = lanes.min(r.bytes.len() - r.offset);
            if selected {
                host.dma_read(addr, &mut r.bytes[r.offset..r.offset + n]);
            }
            r.offset += n;
            0
        }
    }
}

impl crate::bus::Bus {
    fn wasm_board_mut(&mut self, index: usize) -> &mut WasmBoard {
        let crate::zorro_device::BoardDevice::Wasm(board) = &mut self.devices[index] else {
            unreachable!("WASM DMA master slot")
        };
        board
    }

    pub(super) fn wasm_port(&mut self, index: usize) -> &mut DmaPort {
        &mut self.wasm_board_mut(index).dma
    }

    /// Run a DMA-capable plugin with exclusive access to the machine. While
    /// its synchronous read import waits, its port stays in the slot and
    /// the other bus masters and chipset keep running.
    pub(crate) fn wasm_board_call(&mut self, index: usize, mut call: BoardCall) -> Option<u32> {
        let locked = self.buster.cpu_locked;
        let crate::zorro_device::BoardDevice::Wasm(board) = &mut self.devices[index] else {
            return None;
        };
        board.dma.version?;
        if let BoardCall::Tick(cck) = call {
            if locked {
                board.defer_tick(cck);
                return Some(0);
            }
            call = BoardCall::Tick(cck.saturating_add(std::mem::take(&mut board.deferred_ticks)));
        }
        let mut rt = board.rt.take().expect("top-level plugin call");
        let result = rt.get_mut().invoke(call, DmaHost::Machine(self, index));
        self.wasm_board_mut(index).rt = Some(rt);
        self.sample_buster_requests();
        Some(result)
    }

    pub(super) fn wasm_dma_read(
        &mut self,
        index: usize,
        addr: u32,
        bytes: &mut Vec<u8>,
        fuel: &mut u64,
    ) -> Result<()> {
        bytes.fill(0xff);
        if bytes.is_empty() || self.buster.cpu_locked {
            return Ok(());
        }
        if self.wasm_port(index).z3() && !self.buster.accepts_z3_request() {
            return Ok(());
        }
        // The register strobe that invoked the plugin has been billed. Only
        // a processor RMC cycle holds the bus across the plugin's response.
        let driving = std::mem::take(&mut self.buster.cpu_driving);
        self.wasm_port(index)
            .start_read(addr, std::mem::take(bytes));
        self.sample_buster_requests();
        let mut exhausted = false;
        while self.wasm_port(index).reading() {
            if *fuel == 0 {
                exhausted = true;
                break;
            }
            if self.wasm_port(index).z3() && !self.buster.accepts_z3_request() {
                break;
            }
            *fuel -= 1;
            self.clock_buster_wait();
        }
        *bytes = self.wasm_port(index).finish_read();
        self.buster.cpu_driving = driving;
        self.sample_buster_requests();
        anyhow::ensure!(!exhausted, "WASM DMA wait exhausted plugin fuel");
        Ok(())
    }
}
