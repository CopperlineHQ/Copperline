// SPDX-License-Identifier: GPL-3.0-or-later

//! A minimal SD memory card, emulated in SPI mode.
//!
//! Backs the SF2000 accelerator's Zorro SD card controller
//! (`crate::sf2000sd`) with a real block image through
//! [`crate::harddrive::HardDriveImage`] -- the same raw 512-byte-sector
//! backend `ata.rs`/`a2091.rs` already use, so an SD card image is handled
//! exactly like a `[lide]`/`[copperhf]` hardfile (RDB images, bare-partition
//! hardfiles with a synthesized RDB, gzip-compressed images).
//!
//! Implements the SD-over-SPI command set the driver actually issues, verified
//! against Mike Stirling's `sd.c` (`k1208-drivers`, also used by `spisd2` for
//! the SF2000): CMD0/CMD8/CMD55+ACMD41/CMD58 (the init handshake), CMD9/CMD10
//! (CSD/CID), CMD13 (status), CMD16 (accepted no-op -- block length is always
//! 512), CMD17/CMD24 (single-block read/write), CMD18/CMD25/CMD12 (multi-block
//! read/write/stop -- the path any trackdisk-style request wider than one
//! sector takes, which is the common case, not an edge case: `device.c`
//! passes `io_Length >> 9` straight through as the sector count), ACMD23 (its
//! result is never checked by the driver, so it falls through to the
//! catch-all "unknown command" response harmlessly), CMD59 (accepted no-op --
//! CRC checking is never enforced, including on CMD0/CMD8's
//! normally-mandatory fixed CRC bytes: a permissive card is friendlier for
//! driver bring-up than a strict one).
//!
//! Presented throughout as a block-addressed (SDHC-style) card: CMD8 and
//! ACMD41's HCS bit and CMD58's OCR CCS bit all say so, so a real driver
//! always addresses it by block number, matching `HardDriveImage`'s own
//! `u64` LBA unit directly -- no byte-address/block-address branch needed.
//!
//! ## The `clock_byte` model
//!
//! Every SPI byte-time is bidirectional on the real bus: MOSI and MISO both
//! carry a bit on each SCLK edge. `sdcard.v`'s TX/RX/BOTH shifter "mode" is a
//! local FPGA buffering convenience, not a bus-level distinction -- in TX
//! mode the shifter still receives a byte from the card each byte-time, it
//! just discards it instead of pushing it to the RX FIFO; in RX mode it
//! still drives real clock edges, it just always sends `0xFF` filler.
//! [`SdCard::clock_byte`] models the one true primitive: given the byte the
//! host is driving onto MOSI this byte-time, advance the card's
//! command/response state machine and return the byte it drives back onto
//! MISO. `crate::sf2000sd::Sf2000Sd` calls it once per byte-time regardless
//! of which RTL "mode" is active, discarding the reply on a TX-only byte
//! exactly as the real shifter does.
//!
//! Response timing (Ncr, the gap between a command and its reply, and the
//! similar gap before a data token) is collapsed to zero: a real card may
//! take a few filler bytes to answer, and every real driver's init/poll loop
//! already tolerates that, so answering immediately is still spec-legal and
//! keeps the model simple. A short fixed run of busy (`0x00`) bytes after a
//! block write stands in for real flash program time, so a driver's
//! busy-poll path is still exercised.

use crate::harddrive::HardDriveImage;
use std::collections::VecDeque;

const R1_ILLEGAL_COMMAND: u8 = 0x04;
const R1_PARAM_ERROR: u8 = 0x40;

/// SD data error token sent in place of the `0xFE` start-of-block token:
/// bit0 (generic error) + bit3 (out of range), matching the SD Physical
/// Layer spec's data-error-token format for a read that runs off the end
/// of the card.
const DATA_ERROR_OUT_OF_RANGE: u8 = 0x09;

/// OCR reported by CMD58: power-up complete (bit31), card capacity status
/// set (bit30, SDHC/SDXC block addressing), a conventional 2.7-3.6V window.
const OCR: u32 = 0xC0FF_8000;

/// Busy (`0x00`) clocks a driver's post-write poll loop sees before the card
/// goes ready, standing in for real flash program time.
const WRITE_BUSY_CLOCKS: usize = 8;

/// Arbitrary, not a registered manufacturer ID: CID has no correctness
/// requirement beyond being present and readable. MID/OID/PNM/PRV/PSN/MDT
/// (see the SD Physical Layer spec) followed by an unchecked CRC+stop byte.
const CID: [u8; 16] = [
    0x82, b'C', b'L', b'C', b'P', b'L', b'S', b'D', 0x10, 0x00, 0x00, 0x00, 0x01, 0x00, 0x01, 0x01,
];

/// What happens once the current [`Activity::Replying`] queue drains.
#[derive(Default)]
enum AfterReply {
    #[default]
    Idle,
    /// CMD24/CMD25 accepted: awaiting the next block's start token (or, for
    /// CMD25, the stop token). See [`Activity::AwaitWriteToken`].
    AwaitWriteToken { lba: u64, multi: bool },
    /// CMD18 accepted (or the last block of a CMD18 stream just finished
    /// draining): the card is ready to produce block `next_lba`, or to be
    /// interrupted by CMD12. See [`Activity::StreamingRead`].
    StreamingRead { next_lba: u64 },
}

/// The card's current byte-stream role. `clock_byte` both feeds and drains
/// this each call, since drive/response are simultaneous each byte-time.
#[derive(Default)]
enum Activity {
    /// Ready for the next command frame's first byte (or the write data
    /// token once `after_reply` handed us here after CMD24/CMD25's R1).
    #[default]
    Idle,
    /// Draining a fully-built reply (R1/R2/R3/R7, or a data-block response:
    /// R1 + `0xFE` + data + 2 dummy CRC bytes) one byte per `clock_byte`.
    Replying(VecDeque<u8>),
    /// CMD24/CMD25 accepted; waiting for the data start token, discarding
    /// anything else -- a driver may send filler bytes first. Single-block
    /// (CMD24, `multi: false`) only recognizes `0xFE`; multi-block (CMD25)
    /// recognizes `0xFC` (another block follows) or `0xFD` (STOP_TRAN: no
    /// data phase, just a one-byte reply the driver discards).
    AwaitWriteToken { lba: u64, multi: bool },
    /// Accumulating a write block's 512 data bytes + 2 CRC bytes (the CRC is
    /// received but never checked).
    ReceivingWrite { lba: u64, multi: bool, buf: Vec<u8> },
    /// Mid CMD18 (READ_MULTIPLE_BLOCK): about to produce block `next_lba`
    /// unless the next byte is actually the start of a CMD12
    /// (STOP_TRANSMISSION) frame -- the driver only ever sends CMD12
    /// strictly between blocks, once it has fully drained the last one it
    /// asked for, never mid-block, so checking at exactly this point is
    /// sufficient (see [`SdCard::clock_byte`]).
    StreamingRead { next_lba: u64 },
}

/// An SD memory card, emulated in SPI mode, backed by a raw block image.
#[derive(serde::Serialize, serde::Deserialize)]
pub struct SdCard {
    image: HardDriveImage,
    /// GO_IDLE_STATE (CMD0) sets this; SD_SEND_OP_COND (ACMD41) clears it.
    idle: bool,
    /// CMD55 (APP_CMD) seen: the next command frame is interpreted as an
    /// application-specific command.
    app_cmd: bool,
    cmd_buf: [u8; 6],
    cmd_len: u8,
    // Transient protocol state (mid-command-frame, mid-reply, mid-write) is
    // not worth round-tripping through a save state: the guest driver always
    // restarts a command from CS deassertion, and no real command sequence
    // spans a save/load boundary. Skipped fields reset to idle on load,
    // matching a freshly reselected card.
    #[serde(skip, default)]
    activity: Activity,
    #[serde(skip, default)]
    after_reply: AfterReply,
}

impl SdCard {
    pub fn new(image: HardDriveImage) -> Self {
        Self {
            image,
            idle: true,
            app_cmd: false,
            cmd_buf: [0; 6],
            cmd_len: 0,
            activity: Activity::Idle,
            after_reply: AfterReply::Idle,
        }
    }

    /// Reset the protocol state machine to power-on (idle, no in-flight
    /// command/reply/write). The backing image's contents are untouched.
    pub fn reset(&mut self) {
        self.idle = true;
        self.app_cmd = false;
        self.cmd_len = 0;
        self.activity = Activity::Idle;
        self.after_reply = AfterReply::Idle;
    }

    /// One SPI byte-time: `tx` is the byte the host drives onto MOSI; the
    /// return value is the byte the card drives back onto MISO. See the
    /// module documentation's "The `clock_byte` model" section.
    pub fn clock_byte(&mut self, tx: u8) -> u8 {
        let activity = std::mem::replace(&mut self.activity, Activity::Idle);
        let (next, out) = match activity {
            Activity::Replying(mut q) => {
                let b = q.pop_front().unwrap_or(0xFF);
                let next = if q.is_empty() {
                    match std::mem::replace(&mut self.after_reply, AfterReply::Idle) {
                        AfterReply::Idle => Activity::Idle,
                        AfterReply::AwaitWriteToken { lba, multi } => {
                            Activity::AwaitWriteToken { lba, multi }
                        }
                        AfterReply::StreamingRead { next_lba } => {
                            Activity::StreamingRead { next_lba }
                        }
                    }
                } else {
                    Activity::Replying(q)
                };
                (next, b)
            }
            Activity::AwaitWriteToken { lba, multi } => {
                let next = if multi && tx == 0xFD {
                    // STOP_TRAN: no data phase, just a one-byte reply the
                    // driver reads and discards (`sd_write_block`'s
                    // `token == 0xfd` branch).
                    Activity::Replying(VecDeque::from([0xFFu8]))
                } else if (multi && tx == 0xFC) || (!multi && tx == 0xFE) {
                    Activity::ReceivingWrite {
                        lba,
                        multi,
                        buf: Vec::with_capacity(514),
                    }
                } else {
                    Activity::AwaitWriteToken { lba, multi }
                };
                (next, 0xFF)
            }
            Activity::ReceivingWrite {
                lba,
                multi,
                mut buf,
            } => {
                buf.push(tx);
                let next = if buf.len() == 514 {
                    // The trailing 2 bytes are the (unchecked) CRC.
                    let reply = self.finish_write(lba, &buf[..512]);
                    if multi {
                        self.after_reply = AfterReply::AwaitWriteToken {
                            lba: lba + 1,
                            multi: true,
                        };
                    }
                    Activity::Replying(reply)
                } else {
                    Activity::ReceivingWrite { lba, multi, buf }
                };
                (next, 0xFF)
            }
            Activity::StreamingRead { next_lba } => {
                if tx & 0xC0 == 0x40 {
                    // The start of a new command frame -- in practice always
                    // CMD12 (STOP_TRANSMISSION), the only command the driver
                    // sends here. Hand off to the ordinary command-frame
                    // accumulator below; this byte is its first.
                    self.cmd_buf[0] = tx;
                    self.cmd_len = 1;
                    (Activity::Idle, 0xFF)
                } else if next_lba >= self.image.total_sectors() {
                    // A real card refuses to stream past its own capacity.
                    // The initial LBA is range-checked at CMD18 dispatch,
                    // but nothing bounded `next_lba` as the stream kept
                    // advancing -- a driver bug that miscomputes the block
                    // count (or simply asks for more than the card holds)
                    // would otherwise read an unbounded stream of filler
                    // bytes into whatever buffer it supplied, forever,
                    // since nothing here would ever say no. Send a data
                    // error token instead of the `0xFE` start token and end
                    // the stream: the driver's own `sd_read_block` treats
                    // any non-`0xFE`/`0xFF` token as a failure and aborts
                    // the transfer, so this surfaces as a clean read error
                    // instead of a runaway buffer overrun.
                    (Activity::Idle, DATA_ERROR_OUT_OF_RANGE)
                } else {
                    let mut block = self.data_block_reply(next_lba);
                    let b = block.pop_front().unwrap_or(0xFF);
                    self.after_reply = AfterReply::StreamingRead {
                        next_lba: next_lba + 1,
                    };
                    (Activity::Replying(block), b)
                }
            }
            Activity::Idle => {
                // A command frame always starts `01xxxxxx`; anything else
                // seen with no frame in progress is idle/sync filler (real
                // drivers commonly clock dummy 0xFF bytes before a command),
                // not a framing error.
                if self.cmd_len == 0 && tx & 0xC0 != 0x40 {
                    (Activity::Idle, 0xFF)
                } else {
                    self.cmd_buf[self.cmd_len as usize] = tx;
                    self.cmd_len += 1;
                    if self.cmd_len == 6 {
                        self.cmd_len = 0;
                        let frame = self.cmd_buf;
                        (Activity::Replying(self.dispatch(frame)), 0xFF)
                    } else {
                        (Activity::Idle, 0xFF)
                    }
                }
            }
        };
        self.activity = next;
        out
    }

    fn r1(&self) -> u8 {
        u8::from(self.idle)
    }

    fn dispatch(&mut self, frame: [u8; 6]) -> VecDeque<u8> {
        let is_acmd = std::mem::take(&mut self.app_cmd);
        let cmd = frame[0] & 0x3F;
        let arg = u32::from_be_bytes([frame[1], frame[2], frame[3], frame[4]]);

        if is_acmd {
            return match cmd {
                // SD_SEND_OP_COND: report ready immediately -- a real card's
                // init latency (Ncr-scale here too) is collapsed to zero.
                41 => {
                    self.idle = false;
                    VecDeque::from([self.r1()])
                }
                _ => VecDeque::from([self.r1() | R1_ILLEGAL_COMMAND]),
            };
        }

        match cmd {
            0 => {
                // GO_IDLE_STATE
                self.idle = true;
                VecDeque::from([0x01])
            }
            8 => {
                // SEND_IF_COND -> R7: R1 + the echoed voltage window/check
                // pattern (the low 12 bits of the argument).
                let mut q = VecDeque::from([self.r1()]);
                q.extend((arg & 0x0FFF).to_be_bytes());
                q
            }
            9 => {
                // SEND_CSD -> R1 + data block (token + 16-byte CSD + CRC).
                let mut q = VecDeque::from([self.r1()]);
                q.push_back(0xFE);
                q.extend(self.csd());
                q.extend([0x00, 0x00]);
                q
            }
            10 => {
                // SEND_CID -> R1 + data block (token + 16-byte CID + CRC).
                let mut q = VecDeque::from([self.r1()]);
                q.push_back(0xFE);
                q.extend(CID);
                q.extend([0x00, 0x00]);
                q
            }
            12 => {
                // STOP_TRANSMISSION: ends a CMD18 read stream. The driver
                // (`sd_send_cmd`'s `cmd == CMD12` branch) reads and discards
                // one stuff byte ahead of the normal R1 poll, so the reply
                // needs at least two bytes; the stuff byte's value is
                // irrelevant.
                VecDeque::from([0xFFu8, self.r1()])
            }
            13 => VecDeque::from([self.r1(), 0x00]), // SEND_STATUS -> R2
            16 => VecDeque::from([self.r1()]),       // SET_BLOCKLEN: always 512, accepted
            17 => {
                // READ_SINGLE_BLOCK
                let lba = u64::from(arg);
                if lba >= self.image.total_sectors() {
                    VecDeque::from([self.r1() | R1_PARAM_ERROR])
                } else {
                    let mut q = VecDeque::from([self.r1()]);
                    q.extend(self.data_block_reply(lba));
                    q
                }
            }
            18 => {
                // READ_MULTIPLE_BLOCK: accept now, then stream consecutive
                // data blocks (see `Activity::StreamingRead`) until the host
                // sends CMD12.
                let lba = u64::from(arg);
                if lba >= self.image.total_sectors() {
                    VecDeque::from([self.r1() | R1_PARAM_ERROR])
                } else {
                    self.after_reply = AfterReply::StreamingRead { next_lba: lba };
                    VecDeque::from([self.r1()])
                }
            }
            24 => {
                // WRITE_BLOCK: accept the command now, then wait for the
                // host to clock the data token + 512 bytes + CRC.
                let lba = u64::from(arg);
                if lba >= self.image.total_sectors() {
                    VecDeque::from([self.r1() | R1_PARAM_ERROR])
                } else {
                    self.after_reply = AfterReply::AwaitWriteToken { lba, multi: false };
                    VecDeque::from([self.r1()])
                }
            }
            25 => {
                // WRITE_MULTIPLE_BLOCK: accept now, then repeatedly accept
                // `0xFC`-prefixed blocks until the host sends the `0xFD`
                // STOP_TRAN token (see `Activity::AwaitWriteToken`).
                let lba = u64::from(arg);
                if lba >= self.image.total_sectors() {
                    VecDeque::from([self.r1() | R1_PARAM_ERROR])
                } else {
                    self.after_reply = AfterReply::AwaitWriteToken { lba, multi: true };
                    VecDeque::from([self.r1()])
                }
            }
            55 => {
                // APP_CMD
                self.app_cmd = true;
                VecDeque::from([self.r1()])
            }
            58 => {
                // READ_OCR -> R3: R1 + 4-byte OCR.
                let mut q = VecDeque::from([self.r1()]);
                q.extend(OCR.to_be_bytes());
                q
            }
            59 => VecDeque::from([self.r1()]), // CRC_ON_OFF: accepted, never enforced either way
            _ => VecDeque::from([self.r1() | R1_ILLEGAL_COMMAND]),
        }
    }

    /// One data block reply -- `0xFE` + 512 bytes + 2 dummy CRC bytes, with
    /// no R1 in front -- shared by CMD17 (which prefixes it with a fresh R1)
    /// and CMD18's block stream (which sends R1 only once, for the command
    /// itself, not per block). `lba` is assumed already range-checked by the
    /// caller; a read failure past that point (which should not happen
    /// against a valid image) falls back to `0xFF` filler rather than
    /// breaking the byte stream's framing.
    fn data_block_reply(&mut self, lba: u64) -> VecDeque<u8> {
        let mut data = [0xFFu8; 512];
        let _ = self.image.read_sector(lba, &mut data);
        let mut q = VecDeque::from([0xFEu8]);
        q.extend(data);
        q.extend([0x00, 0x00]); // dummy CRC, never checked
        q
    }

    fn finish_write(&mut self, lba: u64, data: &[u8]) -> VecDeque<u8> {
        let mut q = VecDeque::new();
        match self.image.write_sector(lba, data) {
            Ok(()) => {
                q.push_back(0x05); // data response token: accepted
                for _ in 0..WRITE_BUSY_CLOCKS {
                    q.push_back(0x00); // busy (programming)
                }
            }
            Err(_) => q.push_back(0x0D), // data response token: write error
        }
        q
    }

    /// SD Physical Layer CSD version 2.0 (SDHC/SDXC), encoding the image's
    /// real capacity in `C_SIZE` -- the field drivers actually rely on.
    /// Surrounding fields (transfer speed, command classes, block length,
    /// erase/write-protect group sizing) are set to conventional real-card
    /// values rather than independently derived; none of it is checked
    /// against the CRC byte, which is never verified.
    fn csd(&self) -> [u8; 16] {
        let blocks_1k = self.image.total_sectors() / 1024;
        let c_size = (blocks_1k.saturating_sub(1)).min(0x3F_FFFF) as u32; // 22 bits
        [
            0x40,                          // CSD_STRUCTURE=01 (v2.0), reserved
            0x0E,                          // TAAC (fixed, unused for SDHC)
            0x00,                          // NSAC
            0x32,                          // TRAN_SPEED: 25 MHz
            0x5B,                          // CCC[11:4]
            0x59,                          // CCC[3:0] | READ_BL_LEN=9 (512 bytes)
            0x00,                          // READ_BL_PARTIAL/misalign/DSR_IMP/reserved
            ((c_size >> 16) & 0x3F) as u8, // reserved(2) | C_SIZE[21:16]
            (c_size >> 8) as u8,           // C_SIZE[15:8]
            c_size as u8,                  // C_SIZE[7:0]
            0x7F,                          // reserved | ERASE_BLK_EN=1 | SECTOR_SIZE[6:1]
            0x80,                          // SECTOR_SIZE[0] | WP_GRP_SIZE=0
            0x0A, // WP_GRP_ENABLE=0 | reserved | R2W_FACTOR=2 | WRITE_BL_LEN[3:2]
            0x40, // WRITE_BL_LEN[1:0] | WRITE_BL_PARTIAL=0 | reserved
            0x00, // reserved | FILE_FORMAT_GRP/COPY/PERM_WP/TMP_WP/FILE_FORMAT
            0x01, // CRC7 (unused, unchecked) | stop bit
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diskimage::FileSystem;

    fn temp_image(sectors: u64) -> std::path::PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "copperline-sdcard-test-{}-{}.img",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::write(&path, vec![0u8; (sectors * 512) as usize]).unwrap();
        path
    }

    fn card(sectors: u64) -> (SdCard, std::path::PathBuf) {
        let path = temp_image(sectors);
        let image =
            HardDriveImage::open(&path, "DH0", "sf2000sd", None, 0, FileSystem::FFS).unwrap();
        (SdCard::new(image), path)
    }

    /// Send a 6-byte command frame, clocking dummy 0xFF for the response
    /// bytes it produces. Returns the reply bytes seen after the frame,
    /// stopping once `want` bytes have been collected.
    fn send_cmd(card: &mut SdCard, cmd: u8, arg: u32, crc: u8, want: usize) -> Vec<u8> {
        let frame = {
            let mut f = [0u8; 6];
            f[0] = 0x40 | (cmd & 0x3F);
            f[1..5].copy_from_slice(&arg.to_be_bytes());
            f[5] = crc;
            f
        };
        for b in frame {
            card.clock_byte(b);
        }
        (0..want).map(|_| card.clock_byte(0xFF)).collect()
    }

    #[test]
    fn init_handshake_clears_idle_and_reports_sdhc() {
        let (mut card, path) = card(1 << 16); // 32 MiB-ish image, well over the 1024-block CSD unit
        assert_eq!(send_cmd(&mut card, 0, 0, 0x95, 1), [0x01]); // CMD0: idle
        assert_eq!(
            send_cmd(&mut card, 8, 0x1AA, 0x87, 5),
            [0x01, 0x00, 0x00, 0x01, 0xAA]
        ); // CMD8 echoes the pattern, still idle
        assert_eq!(send_cmd(&mut card, 55, 0, 0, 1), [0x01]); // APP_CMD
        assert_eq!(send_cmd(&mut card, 41, 1 << 30, 0, 1), [0x00]); // ACMD41 with HCS: ready
        let ocr = send_cmd(&mut card, 58, 0, 0, 5); // CMD58: R1 + OCR
        assert_eq!(ocr[0], 0x00);
        assert_eq!(ocr[1] & 0x40, 0x40, "CCS bit set: block-addressed card");
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn single_block_write_then_read_round_trips() {
        let (mut card, path) = card(2048);
        send_cmd(&mut card, 0, 0, 0x95, 1); // CMD0
        send_cmd(&mut card, 55, 0, 0, 1); // APP_CMD
        send_cmd(&mut card, 41, 1 << 30, 0, 1); // ACMD41: clear idle

        // WRITE_BLOCK to LBA 5: R1, then clock the data token + 512 bytes +
        // 2 CRC bytes, then drain the data-response token and busy run.
        let r1 = send_cmd(&mut card, 24, 5, 0, 1);
        assert_eq!(r1, [0x00]);
        card.clock_byte(0xFE); // start token
        let mut pattern = [0u8; 512];
        for (i, b) in pattern.iter_mut().enumerate() {
            *b = (i % 251) as u8;
        }
        for &b in &pattern {
            card.clock_byte(b);
        }
        card.clock_byte(0x00); // CRC hi (unchecked)
        card.clock_byte(0x00); // CRC lo (unchecked)
        let resp = card.clock_byte(0xFF);
        assert_eq!(resp & 0x1F, 0x05, "data accepted token");
        let mut saw_ready = false;
        for _ in 0..WRITE_BUSY_CLOCKS + 4 {
            if card.clock_byte(0xFF) == 0xFF {
                saw_ready = true;
                break;
            }
        }
        assert!(saw_ready, "card must leave the busy state");

        // READ_SINGLE_BLOCK from the same LBA reads the pattern back.
        let head = send_cmd(&mut card, 17, 5, 0, 2); // R1 + start token
        assert_eq!(head, [0x00, 0xFE]);
        let read_back: Vec<u8> = (0..512).map(|_| card.clock_byte(0xFF)).collect();
        assert_eq!(read_back, pattern);
        card.clock_byte(0xFF); // CRC hi -- drained for hygiene, matching sd_read_block
        card.clock_byte(0xFF); // CRC lo
        std::fs::remove_file(&path).ok();
    }

    /// Mirrors `sd.c`'s `sd_read()` for `count > 1` exactly: CMD18, then
    /// `sd_read_block` called back to back with no intervening command
    /// bytes for each requested sector, then CMD12 to stop -- the path any
    /// trackdisk-style request wider than one sector takes.
    #[test]
    fn multi_block_read_streams_consecutive_blocks_until_stop_transmission() {
        let (mut card, path) = card(2048);
        send_cmd(&mut card, 0, 0, 0x95, 1);
        send_cmd(&mut card, 55, 0, 0, 1);
        send_cmd(&mut card, 41, 1 << 30, 0, 1);

        // Seed three consecutive sectors with distinct patterns.
        let mut patterns = [[0u8; 512]; 3];
        for (i, pattern) in patterns.iter_mut().enumerate() {
            for (j, b) in pattern.iter_mut().enumerate() {
                *b = ((i * 37 + j) % 251) as u8;
            }
            let r1 = send_cmd(&mut card, 24, 10 + i as u32, 0, 1);
            assert_eq!(r1, [0x00]);
            card.clock_byte(0xFE);
            for &b in pattern.iter() {
                card.clock_byte(b);
            }
            card.clock_byte(0x00);
            card.clock_byte(0x00);
            assert_eq!(card.clock_byte(0xFF) & 0x1F, 0x05);
            while card.clock_byte(0xFF) != 0xFF {}
        }

        let r1 = send_cmd(&mut card, 18, 10, 0, 1);
        assert_eq!(r1, [0x00]);
        for pattern in &patterns {
            assert_eq!(card.clock_byte(0xFF), 0xFE, "start token");
            let block: Vec<u8> = (0..512).map(|_| card.clock_byte(0xFF)).collect();
            assert_eq!(&block, pattern);
            card.clock_byte(0xFF); // CRC hi
            card.clock_byte(0xFF); // CRC lo
        }

        // CMD12 (STOP_TRANSMISSION): an ordinary 6-byte frame; the real
        // driver reads and discards one stuff byte ahead of the normal R1
        // poll (`sd_send_cmd`'s `cmd == CMD12` branch).
        let stop = {
            let mut f = [0u8; 6];
            f[0] = 0x40 | 12;
            f[5] = 0x01;
            f
        };
        for b in stop {
            card.clock_byte(b);
        }
        card.clock_byte(0xFF); // stuff byte, discarded by the real driver
        assert_eq!(card.clock_byte(0xFF), 0x00, "R1 after STOP_TRANSMISSION");
        std::fs::remove_file(&path).ok();
    }

    /// A CMD18 stream that walks off the end of the image must stop itself:
    /// nothing bounds `next_lba` as it advances (only the initial LBA is
    /// range-checked at dispatch), so without this the card would stream an
    /// unbounded run of filler blocks into whatever buffer the driver
    /// supplied, forever, if the driver ever asks for more blocks than the
    /// card holds. Confirmed this is exactly what the real driver's
    /// `sd_read_block` needs: it treats any non-`0xFE`/`0xFF` token as a
    /// failure and aborts, rather than trusting an unbounded stream.
    #[test]
    fn multi_block_read_stops_with_an_error_token_past_the_last_sector() {
        let (mut card, path) = card(4); // 4 sectors: LBAs 0..=3
        send_cmd(&mut card, 0, 0, 0x95, 1);
        send_cmd(&mut card, 55, 0, 0, 1);
        send_cmd(&mut card, 41, 1 << 30, 0, 1);

        let r1 = send_cmd(&mut card, 18, 2, 0, 1); // start at LBA 2
        assert_eq!(r1, [0x00]);

        // LBA 2: a normal block.
        assert_eq!(card.clock_byte(0xFF), 0xFE, "start token for LBA 2");
        for _ in 0..512 {
            card.clock_byte(0xFF);
        }
        card.clock_byte(0xFF); // CRC hi
        card.clock_byte(0xFF); // CRC lo

        // LBA 3: still in range, still a normal block.
        assert_eq!(card.clock_byte(0xFF), 0xFE, "start token for LBA 3");
        for _ in 0..512 {
            card.clock_byte(0xFF);
        }
        card.clock_byte(0xFF);
        card.clock_byte(0xFF);

        // LBA 4: past the last sector (image has only 4, LBAs 0..=3) --
        // an error token, not 0xFE, and the stream ends (no more blocks
        // follow no matter how many more bytes are clocked).
        assert_eq!(
            card.clock_byte(0xFF),
            0x09,
            "data error token (out of range) past the last sector"
        );
        for _ in 0..16 {
            assert_eq!(
                card.clock_byte(0xFF),
                0xFF,
                "idle, not more streamed blocks, after the error token"
            );
        }

        // The card is back in the idle command state: a fresh command
        // frame is accepted normally.
        let r1 = send_cmd(&mut card, 13, 0, 0, 2); // SEND_STATUS -> R2
        assert_eq!(r1, [0x00, 0x00]);

        std::fs::remove_file(&path).ok();
    }

    /// Mirrors `sd.c`'s `sd_write()` for `count > 1`: an (ignored) ACMD23,
    /// CMD25, `0xFC`-prefixed blocks back to back, then the `0xFD`
    /// STOP_TRAN token with no data phase.
    #[test]
    fn multi_block_write_accepts_consecutive_blocks_then_stop_tran() {
        let (mut card, path) = card(2048);
        send_cmd(&mut card, 0, 0, 0x95, 1);
        send_cmd(&mut card, 55, 0, 0, 1);
        send_cmd(&mut card, 41, 1 << 30, 0, 1);

        // ACMD23 (SET_WR_BLK_ERASE_COUNT): the driver never checks its
        // result, so any response -- including "illegal command", since it
        // is not specially implemented -- is fine.
        send_cmd(&mut card, 55, 0, 0, 1);
        send_cmd(&mut card, 23, 2, 0, 1);

        let r1 = send_cmd(&mut card, 25, 20, 0, 1);
        assert_eq!(r1, [0x00]);

        let mut patterns = [[0u8; 512]; 2];
        for (i, pattern) in patterns.iter_mut().enumerate() {
            for (j, b) in pattern.iter_mut().enumerate() {
                *b = ((i * 61 + j) % 251) as u8;
            }
            card.clock_byte(0xFC); // multi-block start token
            for &b in pattern.iter() {
                card.clock_byte(b);
            }
            card.clock_byte(0x00);
            card.clock_byte(0x00);
            assert_eq!(card.clock_byte(0xFF) & 0x1F, 0x05, "data accepted token");
            while card.clock_byte(0xFF) != 0xFF {}
        }

        card.clock_byte(0xFD); // STOP_TRAN: no data phase
        card.clock_byte(0xFF); // the one byte sd_write_block reads and discards

        for (i, pattern) in patterns.iter().enumerate() {
            let head = send_cmd(&mut card, 17, 20 + i as u32, 0, 2);
            assert_eq!(head, [0x00, 0xFE]);
            let read_back: Vec<u8> = (0..512).map(|_| card.clock_byte(0xFF)).collect();
            assert_eq!(&read_back, pattern);
            card.clock_byte(0xFF); // CRC hi -- must be drained (sd_read_block always
            card.clock_byte(0xFF); // CRC lo    reads it) before the next command frame
        }
        std::fs::remove_file(&path).ok();
    }

    /// A filesystem doing bulk I/O (PFS3 in particular issues large
    /// `HD_SCSICMD` READ_10/WRITE_10 transfers, up to 65535 sectors in one
    /// call per `spisd2`'s `scsidirect.c`) exercises far more blocks per
    /// CMD18/CMD25 stream than the small 2-3 block tests above -- large
    /// enough to catch anything that only breaks at scale (an LBA-tracking
    /// off-by-one that only compounds after many blocks, a queue/backpressure
    /// assumption that only breaks past some byte count, etc.). 128 blocks
    /// (64K) is a representative bulk transfer size; `sd.c`'s driver-level
    /// per-block granularity is mirrored exactly (a separate 1-byte token
    /// poll, then one bulk 512-byte transfer, then a separate 2-byte CRC
    /// poll -- three distinct SHIFT_CTRL bursts per block, matching
    /// `sd_read_block`/`sd_write_block`).
    #[test]
    fn large_multi_block_transfer_round_trips_at_realistic_bulk_scale() {
        const BLOCKS: u64 = 128;
        let (mut card, path) = card(4096);
        send_cmd(&mut card, 0, 0, 0x95, 1);
        send_cmd(&mut card, 55, 0, 0, 1);
        send_cmd(&mut card, 41, 1 << 30, 0, 1);

        let pattern_byte =
            |block: u64, i: usize| -> u8 { ((block * 131 + i as u64 * 17) % 251) as u8 };

        // Write BLOCKS sectors via CMD25, one sd_write_block-style sequence
        // per block: 0xFC token, 512 bytes, 2 CRC bytes, then poll for the
        // data-response token and busy-clear before the next block.
        let r1 = send_cmd(&mut card, 25, 100, 0, 1);
        assert_eq!(r1, [0x00]);
        for block in 0..BLOCKS {
            card.clock_byte(0xFC);
            for i in 0..512 {
                card.clock_byte(pattern_byte(block, i));
            }
            card.clock_byte(0x00); // CRC hi
            card.clock_byte(0x00); // CRC lo
            assert_eq!(
                card.clock_byte(0xFF) & 0x1F,
                0x05,
                "block {block}: data accepted"
            );
            let mut spins = 0;
            while card.clock_byte(0xFF) != 0xFF {
                spins += 1;
                assert!(spins < 64, "block {block}: card never left the busy state");
            }
        }
        card.clock_byte(0xFD); // STOP_TRAN
        card.clock_byte(0xFF); // discarded reply byte

        // Read all BLOCKS back via CMD18, one sd_read_block-style sequence
        // per block: poll for the 0xFE token, read 512 bytes, read 2 CRC
        // bytes -- then CMD12 to stop.
        let r1 = send_cmd(&mut card, 18, 100, 0, 1);
        assert_eq!(r1, [0x00]);
        for block in 0..BLOCKS {
            let mut spins = 0;
            loop {
                let token = card.clock_byte(0xFF);
                if token == 0xFE {
                    break;
                }
                assert_eq!(token, 0xFF, "block {block}: garbage before the start token");
                spins += 1;
                assert!(spins < 64, "block {block}: start token never arrived");
            }
            for i in 0..512 {
                assert_eq!(
                    card.clock_byte(0xFF),
                    pattern_byte(block, i),
                    "block {block} byte {i} mismatch"
                );
            }
            card.clock_byte(0xFF); // CRC hi
            card.clock_byte(0xFF); // CRC lo
        }
        let stop = {
            let mut f = [0u8; 6];
            f[0] = 0x40 | 12;
            f[5] = 0x01;
            f
        };
        for b in stop {
            card.clock_byte(b);
        }
        card.clock_byte(0xFF); // stuff byte
        assert_eq!(card.clock_byte(0xFF), 0x00, "R1 after STOP_TRANSMISSION");
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn out_of_range_lba_reports_parameter_error_and_no_data_phase() {
        let (mut card, path) = card(16);
        send_cmd(&mut card, 0, 0, 0x95, 1);
        send_cmd(&mut card, 55, 0, 0, 1);
        send_cmd(&mut card, 41, 1 << 30, 0, 1);

        let r1 = send_cmd(&mut card, 17, 1_000_000, 0, 1);
        assert_eq!(r1, [R1_PARAM_ERROR]);
        // No data token follows: the bus stays idle (0xFF) rather than
        // producing a spurious data block.
        assert_eq!(card.clock_byte(0xFF), 0xFF);
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn leading_filler_bytes_before_a_command_are_ignored() {
        let (mut card, path) = card(16);
        for _ in 0..10 {
            assert_eq!(card.clock_byte(0xFF), 0xFF);
        }
        assert_eq!(send_cmd(&mut card, 0, 0, 0x95, 1), [0x01]);
        std::fs::remove_file(&path).ok();
    }
}
