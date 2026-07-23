//! Memory access trait.

/// Kind of bus-level fault during a memory access (distinct from 68000 address error).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BusFaultKind {
    /// Generic bus error (unmapped address, device error, etc).
    BusError,
}

/// A bus-level fault that occurred during a memory access.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BusFault {
    pub kind: BusFaultKind,
    pub address: u32,
}

pub trait AddressBus {
    fn read_byte(&mut self, address: u32) -> u8;
    fn read_word(&mut self, address: u32) -> u16;
    fn read_long(&mut self, address: u32) -> u32;
    fn write_byte(&mut self, address: u32, value: u8);
    fn write_word(&mut self, address: u32, value: u16);
    fn write_long(&mut self, address: u32, value: u32);

    /// Precise-timing callback (Part E.2): called immediately before each bus
    /// access with the number of CPU clocks of internal (non-bus) processing
    /// the core performed since its previous access. The access itself then
    /// takes the standard 4 CPU clocks of a 68000 bus cycle.
    ///
    /// Hosts that emulate surrounding hardware (DMA, video beam) advance it
    /// by `cpu_clocks` here so every access lands at the hardware-exact
    /// moment. The default is a no-op, so buses that only need functional
    /// emulation are unaffected.
    fn sync(&mut self, _cpu_clocks: u32) {}

    /// Fallible read variants used to surface bus/MMU faults to the CPU core.
    ///
    /// Default implementations delegate to the infallible variants to preserve backwards
    /// compatibility for existing buses.
    #[inline]
    fn try_read_byte(&mut self, address: u32) -> Result<u8, BusFault> {
        Ok(self.read_byte(address))
    }
    #[inline]
    fn try_read_word(&mut self, address: u32) -> Result<u16, BusFault> {
        Ok(self.read_word(address))
    }
    #[inline]
    fn try_read_long(&mut self, address: u32) -> Result<u32, BusFault> {
        Ok(self.read_long(address))
    }
    #[inline]
    fn try_write_byte(&mut self, address: u32, value: u8) -> Result<(), BusFault> {
        self.write_byte(address, value);
        Ok(())
    }
    #[inline]
    fn try_write_word(&mut self, address: u32, value: u16) -> Result<(), BusFault> {
        self.write_word(address, value);
        Ok(())
    }
    #[inline]
    fn try_write_long(&mut self, address: u32, value: u32) -> Result<(), BusFault> {
        self.write_long(address, value);
        Ok(())
    }

    fn read_immediate_word(&mut self, address: u32) -> u16 {
        self.read_word(address)
    }
    fn read_immediate_long(&mut self, address: u32) -> u32 {
        self.read_long(address)
    }
    /// Instruction-stream reads with bus-fault reporting, used by the
    /// non-prefetch (68010+) opcode/immediate path so hosts can tell
    /// fetches from data reads (e.g. to model a 32-bit fetch path).
    #[inline]
    fn try_read_immediate_word(&mut self, address: u32) -> Result<u16, BusFault> {
        Ok(self.read_immediate_word(address))
    }
    #[inline]
    fn try_read_immediate_long(&mut self, address: u32) -> Result<u32, BusFault> {
        Ok(self.read_immediate_long(address))
    }
    /// Whether the most recent instruction-stream read was served from the
    /// CPU's instruction cache. The 68060 timing model gates superscalar
    /// pairing and branch folding on a cached fetch stream; plain test
    /// buses default to true (pair freely).
    fn last_fetch_was_cached(&self) -> bool {
        true
    }

    /// Start tracking cache residency for one complete instruction. The
    /// MC68020 timing tables define their cache case for an instruction that
    /// is in the cache, including extension and immediate words rather than
    /// only the opcode word. Hosts with an instruction-cache model use this
    /// hook to reset their per-instruction hit accumulator.
    fn begin_instruction_fetches(&mut self) {}

    /// Whether every instruction-stream access since
    /// `begin_instruction_fetches` hit the instruction cache. Functional test
    /// buses without a cache model default to the most recent fetch result.
    fn instruction_fetches_were_cached(&self) -> bool {
        self.last_fetch_was_cached()
    }

    fn interrupt_acknowledge(&mut self, _level: u8) -> u32 {
        0xFFFF_FFFF
    }

    /// IPL poll-point marker. The 68000/68010 sample their IPL pins at ONE
    /// microcode-determined point per instruction, and the take-interrupt
    /// decision at the next instruction boundary consumes that sample. A
    /// timing-accurate host latches the IPL level at the start of every bus
    /// access and, by default, lets the instruction's LAST access provide
    /// the boundary sample. For instructions whose poll point is NOT the
    /// last access (e.g. read-modify-write instructions poll during the
    /// final prefetch that precedes the writeback), the core calls this
    /// right after the polling access: the host must keep that access's
    /// sample and ignore later accesses until the boundary decision
    /// consumes it. Functional-only buses can ignore it.
    fn ipl_hold_sample(&mut self) {}

    /// Release an `ipl_hold_sample` poll-point hold before the instruction
    /// boundary consumes it. Called on exception dispatch: the vector jump's
    /// handler-entry prefetch is a fresh poll point on real silicon (Moira
    /// jumpToVector polls during the final refill read), so a hold placed
    /// earlier in the faulted instruction must not survive into the handler.
    fn ipl_release_sample(&mut self) {}

    fn reset_devices(&mut self) {}
}
