//! Host <-> guest clipboard sharing: the clipboard unit of the Copperline
//! services board (`[clipboard] share`, `--clipboard`).
//!
//! Modelled on WinUAE's clipboard bridge. Text the guest copies (an IFF
//! `FTXT` clip in `clipboard.device` unit 0) lands on the host clipboard,
//! and text on the host clipboard is written into the guest's
//! `clipboard.device` as an `FTXT` clip, ready to paste. The guest side is
//! a small resident bridge in the services ROM (`clipboard_main` in
//! `guest/services/handler.c`): the handler process of the `HOSTCLIP:`
//! DOS device, which exists only so DOS starts the process at mount time.
//! It opens `clipboard.device`, registers a `CBD_CHANGEHOOK` hook (polls
//! the clip ID on Kickstart 1.3, whose V34 device has no hooks) and an
//! `INTB_PORTS` server, and talks to this module through the register bank
//! and the two 4K transfer windows laid out in
//! `guest/services/copperline_board.h`.
//!
//! Host -> guest: [`ClipboardService::stage_host_text`] converts the text
//! to Latin-1 with LF line ends, bumps the host generation, and raises the
//! board's INT2 line. The guest's server acknowledges the interrupt
//! (`CLIP_CTRL_IRQACK`) and signals the bridge process, which pulls the
//! text through the H2G window with `CLIP_CTRL_FETCH` chunk by chunk and
//! writes it into `clipboard.device`, then reports the generation it
//! wrote in `CLIP_REG_GUESTGEN`. New host text arriving mid-transfer is
//! held back until the next `FETCH` at offset 0, so a transfer is never
//! torn, and the guest loops while the host generation is ahead of what
//! it has written.
//!
//! Guest -> host: the change hook fires for every clip that is not the
//! bridge's own write (it remembers its own clip ID). The bridge reads
//! the raw IFF stream into the G2H window in chunks (`CLIP_CTRL_PUSH`)
//! and `COMMIT`s it; this module parses the `FORM FTXT` and concatenates
//! its `CHRS` chunks into the committed text, which the window loop hands
//! to the host clipboard (and the control protocol's `clipboard.get`
//! reports). Anything that is not `FTXT` is dropped.
//!
//! Echo suppression is two-sided: the guest ignores hook messages for its
//! own clip ID, and the host records the hash of every text it puts on or
//! takes from the host clipboard so its poll does not restage what the
//! guest just sent.
//!
//! Determinism: the host clipboard is live host state, so it only ever
//! reaches the machine when a windowed session (or a control-protocol
//! client) stages it. The service is fitted only when configured -- off by
//! default headless -- and a headless run with it fitted never reads the
//! host clipboard, so its timeline stays reproducible; see
//! `docs/internals/architecture.md`.

// Board-window layout of the clipboard unit. Keep in sync with
// `guest/services/copperline_board.h`; the tests in `filesys.rs` lock it.

/// The clipboard unit's register bank: its first 0x40 bytes are the same
/// packet-pump registers a mount bank has (`REG_DOSPKT`..`REG_ARG`), the
/// rest the bridge registers below.
pub const CLIP_REGS_OFFSET: u32 = 0x3A00;
pub const CLIP_BANK_SIZE: u32 = 0x100;
/// Write: a `CLIP_CTRL_*` verb, acted on within the write.
pub const CLIP_REG_CTRL: u32 = 0x40;
/// Write: byte offset for FETCH (into the staged host text) and PUSH (of
/// the chunk within the guest's IFF stream; 0 starts a new stream).
pub const CLIP_REG_OFFSET: u32 = 0x50;
/// FETCH: read, bytes the host placed in the H2G window (0 = end of text).
/// PUSH: write, bytes the guest placed in the G2H window.
pub const CLIP_REG_LEN: u32 = 0x60;
/// Read: total length of the staged host text (valid after FETCH offset 0).
pub const CLIP_REG_TOTAL: u32 = 0x70;
/// Read: `CLIP_ST_*` bits.
pub const CLIP_REG_STATUS: u32 = 0x80;
/// Read: generation of the newest host text; a bump is what the doorbell
/// interrupt announces.
pub const CLIP_REG_HOSTGEN: u32 = 0x90;
/// Write: the generation the guest has finished writing into
/// `clipboard.device`.
pub const CLIP_REG_GUESTGEN: u32 = 0xA0;
/// Read: generation of the text FETCH offset 0 staged for transfer.
pub const CLIP_REG_STAGEDGEN: u32 = 0xB0;
/// Host -> guest text window.
pub const CLIP_H2G_OFFSET: u32 = 0x4000;
/// Guest -> host IFF window.
pub const CLIP_G2H_OFFSET: u32 = 0x5000;
pub const CLIP_CHUNK_SIZE: u32 = 0x1000;

pub const CLIP_CTRL_ENABLE: u32 = 1;
pub const CLIP_CTRL_DISABLE: u32 = 2;
pub const CLIP_CTRL_IRQACK: u32 = 3;
pub const CLIP_CTRL_FETCH: u32 = 4;
pub const CLIP_CTRL_PUSH: u32 = 5;
pub const CLIP_CTRL_COMMIT: u32 = 6;

/// Host text is waiting (the INT2 line is asserted).
pub const CLIP_ST_IRQ: u32 = 0x01;
/// The host side is sharing its clipboard.
pub const CLIP_ST_PRESENT: u32 = 0x02;

/// Mount-table entry kinds (the entry's last byte).
pub const MOUNT_KIND_FILESYS: u8 = 0;
pub const MOUNT_KIND_CLIPBOARD: u8 = 1;

/// The DOS device whose handler process runs the guest bridge.
pub const DEVICE_NAME: &str = "HOSTCLIP";

/// Longest text carried in either direction. Text beyond it is truncated
/// (host -> guest) or dropped (guest -> host): the guest streams the clip
/// through a 4K window and `clipboard.device` keeps clips in RAM or in
/// `CLIPS:`, so a runaway clip must not eat the machine.
pub const MAX_TEXT_BYTES: usize = 1 << 20;

/// A host string as the guest sees it: Latin-1, LF line ends. Characters
/// outside Latin-1 become `?` (the guest could neither display nor round-
/// trip them), CRLF and bare CR fold to the Amiga's LF.
pub fn host_to_guest(text: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if out.len() >= MAX_TEXT_BYTES {
            break;
        }
        match c {
            '\r' => {
                if chars.peek() == Some(&'\n') {
                    chars.next();
                }
                out.push(b'\n');
            }
            c if u32::from(c) <= 0xFF => out.push(c as u8),
            _ => out.push(b'?'),
        }
    }
    out
}

/// Guest clip text as a host string: every Latin-1 byte is a code point,
/// and LF stays LF (the window layer applies the platform's line ends).
pub fn guest_to_host(bytes: &[u8]) -> String {
    bytes.iter().map(|&b| b as char).collect()
}

/// The text of an IFF `FORM FTXT` clip: its `CHRS` chunks concatenated, or
/// `None` if the stream is not an FTXT form (an `ILBM` picture, say) or
/// carries no `CHRS` at all.
pub fn ftxt_text(iff: &[u8]) -> Option<Vec<u8>> {
    if iff.len() < 12 || &iff[0..4] != b"FORM" || &iff[8..12] != b"FTXT" {
        return None;
    }
    let form_len = u32::from_be_bytes(iff[4..8].try_into().unwrap()) as usize;
    let end = form_len.saturating_add(8).min(iff.len());
    let mut pos = 12;
    let mut text = Vec::new();
    let mut found = false;
    while pos + 8 <= end {
        let id = &iff[pos..pos + 4];
        let len = u32::from_be_bytes(iff[pos + 4..pos + 8].try_into().unwrap()) as usize;
        let body = pos + 8;
        let body_end = body.saturating_add(len).min(end);
        if id == b"CHRS" {
            text.extend_from_slice(&iff[body..body_end]);
            found = true;
        }
        // Chunks are word-aligned; a truncated last chunk ends the walk.
        pos = match body.checked_add(len + (len & 1)) {
            Some(next) if body_end == body + len => next,
            _ => break,
        };
    }
    found.then_some(text)
}

/// The `FORM FTXT { CHRS }` stream the guest bridge writes for `text` --
/// the same bytes byte for byte, so the tests can stand in for the guest.
pub fn ftxt_build(text: &[u8]) -> Vec<u8> {
    let pad = (text.len() & 1) as u32;
    let mut out = Vec::with_capacity(20 + text.len() + 1);
    out.extend_from_slice(b"FORM");
    out.extend_from_slice(&(4 + 8 + text.len() as u32 + pad).to_be_bytes());
    out.extend_from_slice(b"FTXT");
    out.extend_from_slice(b"CHRS");
    out.extend_from_slice(&(text.len() as u32).to_be_bytes());
    out.extend_from_slice(text);
    if pad == 1 {
        out.push(0);
    }
    out
}

fn hash_text(text: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    text.hash(&mut h);
    h.finish()
}

/// The host side of the clipboard unit. Lives inside the
/// [`crate::filesys::FilesysBoard`], which routes the unit's register
/// writes here and holds the window image the registers and transfer
/// windows are latched into.
#[derive(Default, serde::Serialize, serde::Deserialize)]
pub struct ClipboardService {
    /// Host sharing is on (`[clipboard] share`, the menu toggle). Off, the
    /// guest bridge still runs but sees `CLIP_ST_PRESENT` clear, no host
    /// text is ever staged, and guest clips are discarded.
    enabled: bool,
    /// The guest bridge is up (`CLIP_CTRL_ENABLE`): its INT2 server is
    /// installed, so the line may be asserted.
    guest_enabled: bool,
    /// The doorbell: held until the guest's server acknowledges it.
    irq_pending: bool,
    /// Generation of the newest host text (0 = none yet).
    host_gen: u32,
    /// Host text waiting to be promoted at the guest's next FETCH offset 0.
    pending: Option<(u32, Vec<u8>)>,
    /// The text a transfer is reading, and its generation.
    staged: (u32, Vec<u8>),
    /// Newest generation the guest reports written into `clipboard.device`.
    guest_gen: u32,
    /// The guest's IFF stream, assembled PUSH by PUSH until COMMIT.
    guest_blob: Vec<u8>,
    /// A PUSH overran [`MAX_TEXT_BYTES`] or arrived out of order: the
    /// stream is void until the next PUSH at offset 0.
    guest_blob_void: bool,
    /// Text the guest committed that the host has not yet taken. Transient:
    /// the host clipboard is outside the save state.
    #[serde(skip)]
    committed: Option<String>,
    /// The newest text the guest committed, kept for `clipboard.get`.
    #[serde(skip)]
    last_guest_text: Option<String>,
    /// Hash of the host clipboard text last seen or set by this side, so
    /// the host poll only stages a change.
    #[serde(skip)]
    last_host_hash: Option<u64>,
}

impl ClipboardService {
    /// Host sharing on or off; `image` is the board window so the guest's
    /// status register follows.
    pub fn set_enabled(&mut self, on: bool, image: &mut [u8]) {
        self.enabled = on;
        if !on {
            self.committed = None;
        }
        self.sync_regs(image);
    }

    pub fn enabled(&self) -> bool {
        self.enabled
    }

    /// Whether the guest bridge has come up (opened `clipboard.device` and
    /// installed its interrupt server).
    pub fn guest_ready(&self) -> bool {
        self.guest_enabled
    }

    /// The generation the guest last reported written into the clip.
    pub fn guest_gen(&self) -> u32 {
        self.guest_gen
    }

    /// The generation of the newest staged host text.
    pub fn host_gen(&self) -> u32 {
        self.host_gen
    }

    /// INT2 line state: the doorbell, once the guest can answer it.
    pub fn int2_line(&self) -> bool {
        self.guest_enabled && self.irq_pending
    }

    /// Record `text` as the host clipboard's current content; true if it
    /// differs from what this side last saw or set there.
    pub fn host_text_changed(&mut self, text: &str) -> bool {
        let h = hash_text(text);
        if self.last_host_hash == Some(h) {
            return false;
        }
        self.last_host_hash = Some(h);
        true
    }

    /// Stage host `text` for the guest: converted, given the next
    /// generation, and announced through the doorbell. Empty text (a
    /// cleared host clipboard) stages nothing. `image` is the board window.
    /// Returns the generation staged, or `None`.
    pub fn stage_host_text(&mut self, text: &str, image: &mut [u8]) -> Option<u32> {
        if !self.enabled {
            return None;
        }
        let bytes = host_to_guest(text);
        if bytes.is_empty() {
            return None;
        }
        self.host_gen = self.host_gen.wrapping_add(1).max(1);
        self.pending = Some((self.host_gen, bytes));
        self.irq_pending = true;
        self.sync_regs(image);
        Some(self.host_gen)
    }

    /// Text the guest committed since the last call, converted for the
    /// host; the caller puts it on the host clipboard.
    pub fn take_guest_text(&mut self) -> Option<String> {
        self.committed.take()
    }

    /// The newest text the guest committed, if any.
    pub fn guest_text(&self) -> Option<&str> {
        self.last_guest_text.as_deref()
    }

    /// Per-boot reset (expansion init, machine reset): the guest bridge is
    /// gone, so is everything in flight. Host sharing stays configured, and
    /// the host hash is forgotten so the next poll restages the host text
    /// for the rebooted guest.
    pub fn reset(&mut self, image: &mut [u8]) {
        let enabled = self.enabled;
        let last_guest_text = self.last_guest_text.take();
        *self = Self {
            enabled,
            last_guest_text,
            ..Self::default()
        };
        self.sync_regs(image);
    }

    /// `CLIP_REG_GUESTGEN` written: the guest has that generation in
    /// `clipboard.device`.
    pub fn note_guest_gen(&mut self, gen: u32) {
        self.guest_gen = gen;
    }

    /// `CLIP_REG_CTRL` written: perform `verb` against the register bank
    /// and transfer windows in `image`.
    pub fn control(&mut self, verb: u32, image: &mut [u8]) {
        match verb {
            CLIP_CTRL_ENABLE => {
                self.guest_enabled = true;
                log::info!("clipboard: guest bridge up");
            }
            CLIP_CTRL_DISABLE => {
                self.guest_enabled = false;
                self.irq_pending = false;
            }
            CLIP_CTRL_IRQACK => self.irq_pending = false,
            CLIP_CTRL_FETCH => self.fetch(image),
            CLIP_CTRL_PUSH => self.push(image),
            CLIP_CTRL_COMMIT => self.commit(),
            other => log::warn!("clipboard: unknown control verb {other}"),
        }
        self.sync_regs(image);
    }

    fn fetch(&mut self, image: &mut [u8]) {
        let offset = reg(image, CLIP_REG_OFFSET) as usize;
        if offset == 0 {
            if let Some(next) = self.pending.take() {
                self.staged = next;
            }
        }
        let (gen, text) = &self.staged;
        let chunk = text.get(offset..).unwrap_or(&[]);
        let n = chunk.len().min(CLIP_CHUNK_SIZE as usize);
        let at = CLIP_H2G_OFFSET as usize;
        image[at..at + n].copy_from_slice(&chunk[..n]);
        latch(image, CLIP_REG_LEN, n as u32);
        latch(image, CLIP_REG_TOTAL, text.len() as u32);
        latch(image, CLIP_REG_STAGEDGEN, *gen);
    }

    fn push(&mut self, image: &mut [u8]) {
        let offset = reg(image, CLIP_REG_OFFSET) as usize;
        let len = (reg(image, CLIP_REG_LEN) as usize).min(CLIP_CHUNK_SIZE as usize);
        if offset == 0 {
            self.guest_blob.clear();
            self.guest_blob_void = false;
        }
        if self.guest_blob_void {
            return;
        }
        if offset != self.guest_blob.len() || self.guest_blob.len() + len > MAX_TEXT_BYTES {
            log::warn!("clipboard: guest clip stream dropped (out of order or too long)");
            self.guest_blob.clear();
            self.guest_blob_void = true;
            return;
        }
        let at = CLIP_G2H_OFFSET as usize;
        self.guest_blob.extend_from_slice(&image[at..at + len]);
    }

    fn commit(&mut self) {
        let blob = std::mem::take(&mut self.guest_blob);
        if self.guest_blob_void {
            self.guest_blob_void = false;
            return;
        }
        match ftxt_text(&blob) {
            Some(text) if !text.is_empty() => {
                let text = guest_to_host(&text);
                log::debug!("clipboard: guest clip of {} byte(s)", text.len());
                if self.enabled {
                    self.committed = Some(text.clone());
                }
                self.last_guest_text = Some(text);
            }
            Some(_) => {}
            None => log::debug!("clipboard: guest clip is not FTXT ({} bytes)", blob.len()),
        }
    }

    /// Latch the read-only registers the guest polls.
    fn sync_regs(&self, image: &mut [u8]) {
        let mut status = 0;
        if self.irq_pending {
            status |= CLIP_ST_IRQ;
        }
        if self.enabled {
            status |= CLIP_ST_PRESENT;
        }
        latch(image, CLIP_REG_STATUS, status);
        latch(image, CLIP_REG_HOSTGEN, self.host_gen);
    }
}

fn reg(image: &[u8], off: u32) -> u32 {
    let at = (CLIP_REGS_OFFSET + off) as usize;
    u32::from_be_bytes(image[at..at + 4].try_into().unwrap())
}

fn latch(image: &mut [u8], off: u32, value: u32) {
    let at = (CLIP_REGS_OFFSET + off) as usize;
    image[at..at + 4].copy_from_slice(&value.to_be_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_text_becomes_latin1_with_lf_line_ends() {
        assert_eq!(host_to_guest("a\r\nb\rc\nd"), b"a\nb\nc\nd");
        assert_eq!(host_to_guest("caf\u{e9} \u{20ac}"), b"caf\xe9 ?");
        assert_eq!(guest_to_host(b"caf\xe9\n"), "caf\u{e9}\n");
    }

    #[test]
    fn host_text_is_capped() {
        let long = "x".repeat(MAX_TEXT_BYTES + 10);
        assert_eq!(host_to_guest(&long).len(), MAX_TEXT_BYTES);
    }

    #[test]
    fn ftxt_round_trips_and_pads_odd_lengths() {
        let iff = ftxt_build(b"odd");
        assert_eq!(iff.len(), 20 + 4);
        assert_eq!(&iff[..4], b"FORM");
        assert_eq!(u32::from_be_bytes(iff[4..8].try_into().unwrap()), 4 + 8 + 4);
        assert_eq!(ftxt_text(&iff).unwrap(), b"odd");
        let iff = ftxt_build(b"even");
        assert_eq!(iff.len(), 20 + 4);
        assert_eq!(ftxt_text(&iff).unwrap(), b"even");
    }

    #[test]
    fn ftxt_concatenates_chrs_and_skips_other_chunks() {
        let mut iff = Vec::new();
        iff.extend_from_slice(b"FORM");
        iff.extend_from_slice(&0u32.to_be_bytes()); // patched below
        iff.extend_from_slice(b"FTXT");
        iff.extend_from_slice(b"FONS");
        iff.extend_from_slice(&3u32.to_be_bytes());
        iff.extend_from_slice(b"abc\0"); // 3 bytes + pad
        iff.extend_from_slice(b"CHRS");
        iff.extend_from_slice(&5u32.to_be_bytes());
        iff.extend_from_slice(b"Hello\0");
        iff.extend_from_slice(b"CHRS");
        iff.extend_from_slice(&6u32.to_be_bytes());
        iff.extend_from_slice(b" world");
        let len = (iff.len() - 8) as u32;
        iff[4..8].copy_from_slice(&len.to_be_bytes());
        assert_eq!(ftxt_text(&iff).unwrap(), b"Hello world");
    }

    #[test]
    fn non_ftxt_and_truncated_streams_are_rejected() {
        assert!(ftxt_text(b"FORM\0\0\0\x04ILBM").is_none());
        assert!(ftxt_text(b"FORM\0\0\0\x04FTXT").is_none());
        assert!(ftxt_text(b"").is_none());
        // A CHRS whose declared length runs past the stream yields what
        // is there rather than panicking.
        let mut iff = ftxt_build(b"Hello");
        iff.truncate(22);
        assert_eq!(ftxt_text(&iff).unwrap(), b"He");
    }

    fn image() -> Vec<u8> {
        vec![0u8; 0x1_0000]
    }

    #[test]
    fn host_text_is_fetched_in_chunks_after_the_guest_enables() {
        let mut img = image();
        let mut svc = ClipboardService::default();
        // Nothing is staged while sharing is off.
        assert_eq!(svc.stage_host_text("hi", &mut img), None);
        svc.set_enabled(true, &mut img);
        assert_eq!(reg(&img, CLIP_REG_STATUS), CLIP_ST_PRESENT);

        let text = "A".repeat(5000) + "\r\nB";
        assert_eq!(svc.stage_host_text(&text, &mut img), Some(1));
        // Staged but the guest bridge is not up: no interrupt yet.
        assert!(!svc.int2_line());
        assert_eq!(reg(&img, CLIP_REG_STATUS), CLIP_ST_PRESENT | CLIP_ST_IRQ);
        svc.control(CLIP_CTRL_ENABLE, &mut img);
        assert!(svc.int2_line());
        svc.control(CLIP_CTRL_IRQACK, &mut img);
        assert!(!svc.int2_line());
        assert_eq!(reg(&img, CLIP_REG_HOSTGEN), 1);

        // FETCH at offset 0 promotes the text; a full window first.
        latch(&mut img, CLIP_REG_OFFSET, 0);
        svc.control(CLIP_CTRL_FETCH, &mut img);
        assert_eq!(reg(&img, CLIP_REG_LEN), CLIP_CHUNK_SIZE);
        assert_eq!(reg(&img, CLIP_REG_TOTAL), 5002);
        assert_eq!(reg(&img, CLIP_REG_STAGEDGEN), 1);
        let h2g = CLIP_H2G_OFFSET as usize;
        assert!(img[h2g..h2g + 4096].iter().all(|&b| b == b'A'));

        // The tail, with the CRLF folded to LF.
        latch(&mut img, CLIP_REG_OFFSET, 4096);
        svc.control(CLIP_CTRL_FETCH, &mut img);
        assert_eq!(reg(&img, CLIP_REG_LEN), 5002 - 4096);
        assert_eq!(&img[h2g + 904..h2g + 906], b"\nB");

        // Past the end: nothing.
        latch(&mut img, CLIP_REG_OFFSET, 5002);
        svc.control(CLIP_CTRL_FETCH, &mut img);
        assert_eq!(reg(&img, CLIP_REG_LEN), 0);

        latch(&mut img, CLIP_REG_GUESTGEN, 1);
        svc.note_guest_gen(1);
        assert_eq!(svc.guest_gen(), 1);
    }

    #[test]
    fn newer_host_text_waits_for_the_next_transfer() {
        let mut img = image();
        let mut svc = ClipboardService::default();
        svc.set_enabled(true, &mut img);
        svc.control(CLIP_CTRL_ENABLE, &mut img);
        svc.stage_host_text("first", &mut img);
        latch(&mut img, CLIP_REG_OFFSET, 0);
        svc.control(CLIP_CTRL_FETCH, &mut img);
        assert_eq!(reg(&img, CLIP_REG_STAGEDGEN), 1);
        // A second text lands mid-transfer: generation 2 is announced but
        // the staged text and its generation stay fixed until offset 0.
        assert_eq!(svc.stage_host_text("second", &mut img), Some(2));
        assert_eq!(reg(&img, CLIP_REG_HOSTGEN), 2);
        latch(&mut img, CLIP_REG_OFFSET, 3);
        svc.control(CLIP_CTRL_FETCH, &mut img);
        assert_eq!(reg(&img, CLIP_REG_STAGEDGEN), 1);
        assert_eq!(reg(&img, CLIP_REG_LEN), 2);
        let h2g = CLIP_H2G_OFFSET as usize;
        assert_eq!(&img[h2g..h2g + 2], b"st");
        latch(&mut img, CLIP_REG_OFFSET, 0);
        svc.control(CLIP_CTRL_FETCH, &mut img);
        assert_eq!(reg(&img, CLIP_REG_STAGEDGEN), 2);
        assert_eq!(reg(&img, CLIP_REG_TOTAL), 6);
        assert_eq!(&img[h2g..h2g + 6], b"second");
    }

    #[test]
    fn guest_clip_is_pushed_in_chunks_and_parsed_on_commit() {
        let mut img = image();
        let mut svc = ClipboardService::default();
        svc.set_enabled(true, &mut img);
        let text: Vec<u8> = (0..6000).map(|i| b'a' + (i % 26) as u8).collect();
        let iff = ftxt_build(&text);
        let g2h = CLIP_G2H_OFFSET as usize;
        let mut off = 0;
        for chunk in iff.chunks(CLIP_CHUNK_SIZE as usize) {
            img[g2h..g2h + chunk.len()].copy_from_slice(chunk);
            latch(&mut img, CLIP_REG_OFFSET, off);
            latch(&mut img, CLIP_REG_LEN, chunk.len() as u32);
            svc.control(CLIP_CTRL_PUSH, &mut img);
            off += chunk.len() as u32;
        }
        assert!(svc.take_guest_text().is_none(), "not before COMMIT");
        svc.control(CLIP_CTRL_COMMIT, &mut img);
        let got = svc.take_guest_text().unwrap();
        assert_eq!(got.as_bytes(), &text[..]);
        assert_eq!(svc.guest_text().map(str::len), Some(6000));
        assert!(svc.take_guest_text().is_none(), "taken once");
    }

    #[test]
    fn out_of_order_or_non_ftxt_pushes_are_dropped() {
        let mut img = image();
        let mut svc = ClipboardService::default();
        svc.set_enabled(true, &mut img);
        let g2h = CLIP_G2H_OFFSET as usize;
        // An ILBM is not text.
        let ilbm = b"FORM\0\0\0\x04ILBM";
        img[g2h..g2h + ilbm.len()].copy_from_slice(ilbm);
        latch(&mut img, CLIP_REG_OFFSET, 0);
        latch(&mut img, CLIP_REG_LEN, ilbm.len() as u32);
        svc.control(CLIP_CTRL_PUSH, &mut img);
        svc.control(CLIP_CTRL_COMMIT, &mut img);
        assert!(svc.take_guest_text().is_none());
        // A chunk that skips ahead voids the stream until offset 0.
        let iff = ftxt_build(b"hello");
        img[g2h..g2h + iff.len()].copy_from_slice(&iff);
        latch(&mut img, CLIP_REG_OFFSET, 8);
        latch(&mut img, CLIP_REG_LEN, iff.len() as u32);
        svc.control(CLIP_CTRL_PUSH, &mut img);
        svc.control(CLIP_CTRL_COMMIT, &mut img);
        assert!(svc.take_guest_text().is_none());
        latch(&mut img, CLIP_REG_OFFSET, 0);
        svc.control(CLIP_CTRL_PUSH, &mut img);
        svc.control(CLIP_CTRL_COMMIT, &mut img);
        assert_eq!(svc.take_guest_text().as_deref(), Some("hello"));
    }

    #[test]
    fn host_hash_suppresses_echo_and_reset_forgets_it() {
        let mut img = image();
        let mut svc = ClipboardService::default();
        assert!(svc.host_text_changed("x"));
        assert!(!svc.host_text_changed("x"));
        assert!(svc.host_text_changed("y"));
        svc.set_enabled(true, &mut img);
        svc.stage_host_text("y", &mut img);
        svc.control(CLIP_CTRL_ENABLE, &mut img);
        svc.reset(&mut img);
        assert!(svc.enabled());
        assert!(!svc.guest_ready());
        assert!(!svc.int2_line());
        assert_eq!(reg(&img, CLIP_REG_STATUS), CLIP_ST_PRESENT);
        assert_eq!(reg(&img, CLIP_REG_HOSTGEN), 0);
        assert!(
            svc.host_text_changed("y"),
            "restaged for the rebooted guest"
        );
    }

    #[test]
    fn disabled_sharing_hides_the_service_and_discards_guest_clips() {
        let mut img = image();
        let mut svc = ClipboardService::default();
        svc.control(CLIP_CTRL_ENABLE, &mut img);
        assert_eq!(reg(&img, CLIP_REG_STATUS), 0);
        let iff = ftxt_build(b"secret");
        let g2h = CLIP_G2H_OFFSET as usize;
        img[g2h..g2h + iff.len()].copy_from_slice(&iff);
        latch(&mut img, CLIP_REG_OFFSET, 0);
        latch(&mut img, CLIP_REG_LEN, iff.len() as u32);
        svc.control(CLIP_CTRL_PUSH, &mut img);
        svc.control(CLIP_CTRL_COMMIT, &mut img);
        assert!(svc.take_guest_text().is_none());
        assert_eq!(
            svc.guest_text(),
            Some("secret"),
            "still reported to clipboard.get"
        );
    }
}
