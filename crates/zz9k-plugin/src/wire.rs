// SPDX-License-Identifier: GPL-3.0-or-later

//! ZZ9000 SDK v2 wire-protocol constants and big-endian codecs.
//!
//! Every value here mirrors the SDK's `include/zz9k/abi.h` (and the register
//! offsets in `host/src/zz9k_host.c`) at the commit pinned in
//! docs/internals/zz9k.md. The whole protocol is big-endian byte arrays on
//! the wire -- the 68k writes 16-bit stores, this plugin is little-endian
//! wasm32, so nothing here may ever cast a struct: all access goes through
//! the explicit be16/be32 helpers below.

// The full protocol constant set is kept even where the board itself has no
// reader for a value yet (test harnesses and the contract page do).
#![allow(dead_code)]

// -- Register offsets (board-relative; 16-bit registers) -------------------

/// Legacy CONFIG register: reads report interrupt status, a write with
/// [`CONFIG_ACK_SDK`] set acknowledges the SDK interrupt.
pub const REG_CONFIG: u32 = 0x0004;
/// Latched config-key query group: write a key id to CONFIG_KEY, then read
/// CONFIG_KEY back for the value and CONFIG_PRESENT for "key known".
pub const REG_CONFIG_KEY: u32 = 0x00E8;
pub const REG_CONFIG_PRESENT: u32 = 0x00EA;
/// SDK bootstrap registers.
pub const REG_SDK_MAGIC: u32 = 0x0100;
pub const REG_SDK_VERSION: u32 = 0x0102;
pub const REG_SDK_MAILBOX_HI: u32 = 0x0104;
pub const REG_SDK_MAILBOX_LO: u32 = 0x0106;
pub const REG_SDK_DOORBELL: u32 = 0x0108;
pub const REG_SDK_STATUS: u32 = 0x010A;
pub const REG_SDK_IRQ_CTRL: u32 = 0x010C;

/// The Z3 write aperture: board offsets 0x1000..0x1FFF alias 0x0000..0x0FFF
/// (the transport rings the doorbell through it on Zorro III).
pub const Z3_REGISTER_WINDOW_OFFSET: u32 = 0x1000;

pub const SDK_MAGIC_VALUE: u16 = 0x5A39;
/// ABI 2.3: the ZZ9000.CFG config-key register group exists (2.3 behavior);
/// the transport only ever checks major == 2.
pub const SDK_VERSION_VALUE: u16 = 0x0203;

/// Interrupt status/ack bits in REG_CONFIG.
pub const INTERRUPT_SDK: u16 = 0x0008;
pub const CONFIG_ACK_SDK: u16 = 0x0080;

/// REG_SDK_IRQ_CTRL write values.
pub const SDK_IRQ_ACK: u16 = 0x0001;
pub const SDK_IRQ_ENABLE: u16 = 0x0002;
pub const SDK_IRQ_DISABLE: u16 = 0x0004;

/// Config-key ids for the CONFIG_KEY group. Key 5 selects the interrupt
/// line: value nonzero = INT2 (PORTS), zero/absent = INT6 (EXTER).
pub const CFG_KEY_INT2: u16 = 5;

// -- Address map -----------------------------------------------------------

/// ARM address the general shared heap starts at; maps to board offset
/// [`AMIGA_MEMORY_OFFSET`].
pub const ARM_MEMORY_START: u32 = 0x0020_0000;
/// Board offset the Amiga-visible RAM window starts at.
pub const AMIGA_MEMORY_OFFSET: u32 = 0x0001_0000;
/// Legacy mapped-IO window: board offsets 0xA000..0xFFFF map ARM
/// 0x3FE40000..0x3FE46000; the bootstrap mailbox descriptor lives inside it.
pub const MAPPED_IO_ARM_START: u32 = 0x3FE4_0000;
pub const MAPPED_IO_BOARD_OFFSET: u32 = 0x0000_A000;
/// Board offset of the 128-byte mailbox descriptor (= ARM 0x3FE43000).
pub const MAILBOX_BOARD_OFFSET: u32 = 0x0000_D000;
pub const MAILBOX_ARM_ADDRESS: u32 =
    MAPPED_IO_ARM_START + (MAILBOX_BOARD_OFFSET - MAPPED_IO_BOARD_OFFSET);

// -- Mailbox ---------------------------------------------------------------

pub const ABI_MAGIC: u32 = 0x5A5A_394B; // "ZZ9K"
pub const ABI_MAJOR: u16 = 2;
pub const ABI_MINOR: u16 = 3;
pub const MAILBOX_DESCRIPTOR_SIZE: u32 = 128;
pub const MAILBOX_ENTRY_SIZE: u32 = 64;
/// Ring geometry: request and completion rings directly after the
/// descriptor, 32 entries each (cryptobench keeps 16 in flight).
pub const REQUEST_RING_OFFSET: u32 = 0x080; // relative to the descriptor
pub const COMPLETION_RING_OFFSET: u32 = 0x880;
pub const RING_ENTRIES: u32 = 32;

/// Descriptor field offsets (relative to the descriptor base).
pub const DESC_MAGIC: u32 = 0;
pub const DESC_ABI_MAJOR: u32 = 4;
pub const DESC_ABI_MINOR: u32 = 6;
pub const DESC_SIZE: u32 = 8;
pub const DESC_REQ_RING_OFFSET: u32 = 12;
pub const DESC_REQ_RING_ENTRIES: u32 = 16;
pub const DESC_REQ_HEAD: u32 = 20;
pub const DESC_REQ_TAIL: u32 = 24;
pub const DESC_COMPL_RING_OFFSET: u32 = 28;
pub const DESC_COMPL_RING_ENTRIES: u32 = 32;
pub const DESC_COMPL_HEAD: u32 = 36;
pub const DESC_COMPL_TAIL: u32 = 40;
pub const DESC_CAPABILITY_BITS: u32 = 44;

/// Wire-entry field offsets (within a 64-byte entry).
pub const ENTRY_REQUEST_ID: u32 = 0;
pub const ENTRY_OPCODE: u32 = 4;
pub const ENTRY_STATUS: u32 = 6;
pub const ENTRY_FLAGS: u32 = 8;
pub const ENTRY_PAYLOAD_LEN: u32 = 10;
pub const ENTRY_USER_COOKIE: u32 = 12;
pub const ENTRY_PAYLOAD: u32 = 16;
pub const ENTRY_PAYLOAD_MAX: u32 = 48;

pub const ENTRY_FLAG_INLINE_PAYLOAD: u16 = 1 << 0;

// -- Status codes ----------------------------------------------------------

pub const STATUS_OK: u16 = 0;
pub const STATUS_QUEUED: u16 = 1;
pub const STATUS_BUSY: u16 = 2;
pub const STATUS_UNSUPPORTED: u16 = 3;
pub const STATUS_BAD_REQUEST: u16 = 4;
pub const STATUS_BAD_HANDLE: u16 = 5;
pub const STATUS_NO_MEMORY: u16 = 6;
pub const STATUS_IO_ERROR: u16 = 9;
pub const STATUS_NOT_FOUND: u16 = 10;

// -- Capability bits (mailbox descriptor + QUERY_CAPS) ---------------------

pub const CAP_MAILBOX: u32 = 1 << 0;
pub const CAP_IRQ_COMPLETION: u32 = 1 << 1;
pub const CAP_SHARED_ALLOC: u32 = 1 << 2;
pub const CAP_CRYPTO: u32 = 1 << 8;
pub const CAP_MEMORY_OPS: u32 = 1 << 10;
pub const CAP_DIAGNOSTICS: u32 = 1 << 11;
pub const CAP_DOORBELL: u32 = 1 << 12;
pub const CAP_POLLING_COMPLETION: u32 = 1 << 13;
pub const CAP_SERVICE_DISCOVERY: u32 = 1 << 14;
// Deliberately NOT advertised: CAP_HOST_WINDOW_HEAP (1 << 20) and
// CAP_APERTURE_LAYOUT (1 << 24). Their absence selects the transport's
// "historical fixed 4 MB" Zorro II path and skips the aperture-layout
// acknowledgement dance entirely (zz9k_host.c's alloc/attach paths).

/// Everything this board implements.
pub const CAPABILITY_BITS: u32 = CAP_MAILBOX
    | CAP_IRQ_COMPLETION
    | CAP_SHARED_ALLOC
    | CAP_CRYPTO
    | CAP_MEMORY_OPS
    | CAP_DIAGNOSTICS
    | CAP_DOORBELL
    | CAP_POLLING_COMPLETION
    | CAP_SERVICE_DISCOVERY;

// -- Services and opcodes --------------------------------------------------

pub const SERVICE_CORE: u16 = 0x0000;
pub const SERVICE_MEMORY: u16 = 0x0100;
pub const SERVICE_CRYPTO: u16 = 0x0800;
pub const SERVICE_DIAG: u16 = 0x0900;

pub const OP_NOP: u16 = SERVICE_CORE;
pub const OP_QUERY_CAPS: u16 = SERVICE_CORE + 0x01;
pub const OP_PING: u16 = SERVICE_CORE + 0x02;
pub const OP_CANCEL: u16 = SERVICE_CORE + 0x03;
pub const OP_QUERY_SERVICE: u16 = SERVICE_CORE + 0x04;
pub const OP_QUERY_APERTURE_LAYOUT: u16 = SERVICE_CORE + 0x05;

pub const OP_ALLOC_SHARED: u16 = SERVICE_MEMORY;
pub const OP_FREE_SHARED: u16 = SERVICE_MEMORY + 0x01;
pub const OP_MEM_FILL: u16 = SERVICE_MEMORY + 0x02;
pub const OP_MEM_COPY: u16 = SERVICE_MEMORY + 0x03;

pub const OP_CRYPTO_HASH: u16 = SERVICE_CRYPTO;
pub const OP_CRYPTO_STREAM: u16 = SERVICE_CRYPTO + 0x01;
pub const OP_CRYPTO_AEAD: u16 = SERVICE_CRYPTO + 0x02;
pub const OP_CRYPTO_KX: u16 = SERVICE_CRYPTO + 0x03;
pub const OP_CRYPTO_VERIFY: u16 = SERVICE_CRYPTO + 0x04;

pub const OP_DIAG_READ: u16 = SERVICE_DIAG;

// -- Service flags (QUERY_SERVICE reply) -----------------------------------

pub const SERVICE_FLAG_FIRMWARE: u32 = 1 << 0;
pub const SERVICE_FLAG_CRYPTO_X25519: u32 = 1 << 16;
pub const SERVICE_FLAG_CRYPTO_P256: u32 = 1 << 17;
pub const SERVICE_FLAG_CRYPTO_ECDSA_P256: u32 = 1 << 18;
pub const SERVICE_FLAG_CRYPTO_RSA_2048: u32 = 1 << 19;
pub const SERVICE_FLAG_CRYPTO_AES_GCM: u32 = 1 << 20;
pub const SERVICE_FLAG_CRYPTO_P256_KEYGEN: u32 = 1 << 21;

pub const CRYPTO_SERVICE_FLAGS: u32 = SERVICE_FLAG_FIRMWARE
    | SERVICE_FLAG_CRYPTO_X25519
    | SERVICE_FLAG_CRYPTO_P256
    | SERVICE_FLAG_CRYPTO_ECDSA_P256
    | SERVICE_FLAG_CRYPTO_RSA_2048
    | SERVICE_FLAG_CRYPTO_AES_GCM
    | SERVICE_FLAG_CRYPTO_P256_KEYGEN;

// -- Memory service --------------------------------------------------------

pub const INVALID_HANDLE: u32 = 0xFFFF_FFFF;
pub const ALLOC_HOST_WINDOW: u32 = 1 << 0;
pub const ALLOC_CARD_ONLY: u32 = 1 << 1;
pub const MAX_SHARED_BUFFERS: u32 = 64;

// -- Crypto service --------------------------------------------------------

pub const HASH_SHA1: u32 = 1;
pub const HASH_SHA256: u32 = 2;
pub const HASH_SHA384: u32 = 3;
pub const HASH_SHA512: u32 = 4;
pub const HASH_BLAKE2S: u32 = 5;
pub const HASH_POLY1305: u32 = 6;
pub const HASH_FLAG_HMAC: u32 = 1 << 0;

pub const STREAM_CHACHA20: u32 = 1;

pub const AEAD_CHACHA20_POLY1305: u32 = 1;
pub const AEAD_AES128_GCM: u32 = 2;
pub const AEAD_AES256_GCM: u32 = 3;
pub const AEAD_FLAG_DECRYPT: u32 = 1 << 0;
pub const AEAD_ALG_SHIFT: u32 = 8;
pub const AEAD_ALG_MASK: u32 = 0xFF << 8;
pub const AEAD_TAG_BYTES: u32 = 16;

pub const KX_X25519: u32 = 1;
pub const KX_P256: u32 = 2;
pub const KX_FLAG_KEYGEN: u32 = 1;

pub const VERIFY_ECDSA_P256_SHA256: u32 = 1;
pub const VERIFY_RSA_PKCS1_2048_SHA256: u32 = 2;

pub const P256_POINT_BYTES: u32 = 65;

// -- Codecs ----------------------------------------------------------------

pub fn get_be16(buf: &[u8], off: u32) -> u16 {
    let off = off as usize;
    u16::from_be_bytes([buf[off], buf[off + 1]])
}

pub fn get_be32(buf: &[u8], off: u32) -> u32 {
    let off = off as usize;
    u32::from_be_bytes([buf[off], buf[off + 1], buf[off + 2], buf[off + 3]])
}

pub fn put_be16(buf: &mut [u8], off: u32, value: u16) {
    buf[off as usize..off as usize + 2].copy_from_slice(&value.to_be_bytes());
}

pub fn put_be32(buf: &mut [u8], off: u32, value: u32) {
    buf[off as usize..off as usize + 4].copy_from_slice(&value.to_be_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capability_bits_match_the_contract() {
        // The OR of the nine advertised bits, with HOST_WINDOW_HEAP (1<<20)
        // and APERTURE_LAYOUT (1<<24) clear.
        assert_eq!(CAPABILITY_BITS, 0x7D07);
        assert_eq!(CAPABILITY_BITS & (1 << 20), 0);
        assert_eq!(CAPABILITY_BITS & (1 << 24), 0);
    }

    #[test]
    fn crypto_service_flags_match_the_contract() {
        assert_eq!(CRYPTO_SERVICE_FLAGS, 0x003F_0001);
    }

    #[test]
    fn descriptor_layout_is_the_abi_struct() {
        // Field offsets of ZZ9KMailboxDescriptor (abi.h).
        assert_eq!(DESC_REQ_HEAD, 20);
        assert_eq!(DESC_REQ_TAIL, 24);
        assert_eq!(DESC_COMPL_HEAD, 36);
        assert_eq!(DESC_COMPL_TAIL, 40);
        assert_eq!(DESC_CAPABILITY_BITS, 44);
        assert_eq!(MAILBOX_ARM_ADDRESS, 0x3FE4_3000);
    }

    #[test]
    fn be_codecs_round_trip() {
        let mut buf = [0u8; 8];
        put_be32(&mut buf, 2, 0xDEAD_BEEF);
        assert_eq!(get_be32(&buf, 2), 0xDEAD_BEEF);
        assert_eq!(buf[2], 0xDE);
        put_be16(&mut buf, 0, 0x5A39);
        assert_eq!(buf[0], 0x5A);
        assert_eq!(get_be16(&buf, 0), 0x5A39);
    }
}
