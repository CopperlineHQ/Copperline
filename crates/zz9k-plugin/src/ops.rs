// SPDX-License-Identifier: GPL-3.0-or-later

//! Mailbox request dispatch: the CORE, MEMORY, CRYPTO, and DIAG services.
//!
//! `process` maps one 64-byte request wire entry to its completion entry
//! plus a deterministic completion latency in colour clocks. Everything is
//! bounds-checked against the shared-buffer heap; guest data can produce an
//! error status but never a panic (a wasm trap would fault the whole
//! board).

use crate::cryptoimpl;
use crate::wire::*;
use crate::Board;

/// A processed completion waiting for its deterministic deadline.
pub struct Reply {
    pub entry: [u8; 64],
    pub latency_cck: i64,
}

// Deterministic latency model (documented in docs/internals/zz9k.md):
// colour clocks at CCK_HZ = 3_546_895 per emulated second.
const LAT_ADMIN: i64 = 177; // 50 us: CORE/MEMORY/DIAG
const LAT_BULK_BASE: i64 = 709; // 200 us: hash/stream/AEAD setup
const LAT_X25519: i64 = 3_547; // 1 ms
const LAT_P256: i64 = 7_094; // 2 ms
const LAT_ECDSA: i64 = 7_094; // 2 ms
const LAT_RSA: i64 = 3_547; // 1 ms

/// Bulk throughput adder: ~50 MB/s => 3546895/50e6 ~= 0.0709 cck/byte.
fn bulk_cck(len: u32) -> i64 {
    LAT_BULK_BASE + (i64::from(len) * 71) / 1000
}

struct Request {
    request_id: u32,
    opcode: u16,
    // Parsed for completeness; the board acts on nothing in the request
    // flags today (NEEDS_IRQ is subsumed by the IRQ-enable register).
    #[allow(dead_code)]
    flags: u16,
    payload_len: u16,
    user_cookie: u32,
    payload: [u8; 48],
}

fn parse(entry: &[u8; 64]) -> Request {
    let mut payload = [0u8; 48];
    payload.copy_from_slice(&entry[ENTRY_PAYLOAD as usize..]);
    Request {
        request_id: get_be32(entry, ENTRY_REQUEST_ID),
        opcode: get_be16(entry, ENTRY_OPCODE),
        flags: get_be16(entry, ENTRY_FLAGS),
        payload_len: get_be16(entry, ENTRY_PAYLOAD_LEN),
        user_cookie: get_be32(entry, ENTRY_USER_COOKIE),
        payload,
    }
}

fn reply_entry(req: &Request, status: u16, payload: &[u8]) -> [u8; 64] {
    debug_assert!(payload.len() <= ENTRY_PAYLOAD_MAX as usize);
    let mut entry = [0u8; 64];
    put_be32(&mut entry, ENTRY_REQUEST_ID, req.request_id);
    put_be16(&mut entry, ENTRY_OPCODE, req.opcode);
    put_be16(&mut entry, ENTRY_STATUS, status);
    let flags = if payload.is_empty() {
        0
    } else {
        ENTRY_FLAG_INLINE_PAYLOAD
    };
    put_be16(&mut entry, ENTRY_FLAGS, flags);
    put_be16(&mut entry, ENTRY_PAYLOAD_LEN, payload.len() as u16);
    put_be32(&mut entry, ENTRY_USER_COOKIE, req.user_cookie);
    entry[ENTRY_PAYLOAD as usize..ENTRY_PAYLOAD as usize + payload.len()].copy_from_slice(payload);
    entry
}

/// A crypto result payload: {bytes_written, algorithm, flags} + zero pad to
/// the full 48 bytes (the reply decoder requires payload_len >= 48 and
/// bytes_written != 0 on an OK status).
fn crypto_result(bytes_written: u32, algorithm: u32, flags: u32) -> Vec<u8> {
    let mut payload = vec![0u8; 48];
    put_be32(&mut payload, 0, bytes_written);
    put_be32(&mut payload, 4, algorithm);
    put_be32(&mut payload, 8, flags);
    payload
}

impl Board {
    fn read_buf(&self, handle: u32, offset: u32, len: u32) -> Result<Vec<u8>, u16> {
        let (off, len) = self
            .heap
            .resolve(handle, offset, len)
            .ok_or(STATUS_BAD_HANDLE)?;
        Ok(self.win[off as usize..(off + len) as usize].to_vec())
    }

    fn write_buf(&mut self, handle: u32, offset: u32, data: &[u8]) -> Result<(), u16> {
        let (off, len) = self
            .heap
            .resolve(handle, offset, data.len() as u32)
            .ok_or(STATUS_BAD_HANDLE)?;
        self.win[off as usize..(off + len) as usize].copy_from_slice(data);
        Ok(())
    }
}

/// Process one request into its (deferred) completion.
pub fn process(board: &mut Board, entry: &[u8; 64]) -> Reply {
    let req = parse(entry);
    let (result, latency_cck) = dispatch(board, &req);
    let (status, payload) = match result {
        Ok(payload) => (STATUS_OK, payload),
        Err(status) => (status, Vec::new()),
    };
    if status == STATUS_OK {
        board.requests_completed = board.requests_completed.wrapping_add(1);
    } else {
        board.requests_failed = board.requests_failed.wrapping_add(1);
    }
    board.last_status = u32::from(status);
    Reply {
        entry: reply_entry(&req, status, &payload),
        latency_cck,
    }
}

type OpResult = Result<Vec<u8>, u16>;

fn dispatch(board: &mut Board, req: &Request) -> (OpResult, i64) {
    match req.opcode {
        OP_NOP => (Ok(Vec::new()), LAT_ADMIN),
        OP_PING => {
            let len = (req.payload_len as usize).min(48);
            (Ok(req.payload[..len].to_vec()), LAT_ADMIN)
        }
        OP_QUERY_CAPS => (Ok(query_caps()), LAT_ADMIN),
        OP_QUERY_SERVICE => (query_service(req), LAT_ADMIN),
        OP_CANCEL => (Err(STATUS_NOT_FOUND), LAT_ADMIN),
        OP_QUERY_APERTURE_LAYOUT => (Err(STATUS_UNSUPPORTED), LAT_ADMIN),
        OP_ALLOC_SHARED => (alloc_shared(board, req), LAT_ADMIN),
        OP_FREE_SHARED => (free_shared(board, req), LAT_ADMIN),
        OP_MEM_FILL => (mem_fill(board, req), LAT_ADMIN),
        OP_MEM_COPY => (mem_copy(board, req), LAT_ADMIN),
        OP_CRYPTO_HASH => crypto_hash(board, req),
        OP_CRYPTO_STREAM => crypto_stream(board, req),
        OP_CRYPTO_AEAD => crypto_aead(board, req),
        OP_CRYPTO_KX => crypto_kx(board, req),
        OP_CRYPTO_VERIFY => crypto_verify(board, req),
        OP_DIAG_READ => (Ok(diag_read(board)), LAT_ADMIN),
        // Unknown DIAG sub-ops are unsupported; everything else (surfaces,
        // gfx, audio, codec, video, modules, vendor) is not implemented by
        // this board.
        _ => (Err(STATUS_UNSUPPORTED), LAT_ADMIN),
    }
}

// Reported by QUERY_CAPS and QUERY_SERVICE; zz9k-info renders it as
// "major.minor" from the high/low 16-bit halves.
const FIRMWARE_VERSION: u32 = ((ABI_MAJOR as u32) << 16) | (ABI_MINOR as u32);

fn query_caps() -> Vec<u8> {
    let mut payload = vec![0u8; 40];
    put_be32(&mut payload, 0, ABI_MAGIC);
    put_be16(&mut payload, 4, ABI_MAJOR);
    put_be16(&mut payload, 6, ABI_MINOR);
    put_be32(&mut payload, 8, CAPABILITY_BITS);
    put_be32(&mut payload, 12, ENTRY_PAYLOAD_MAX);
    put_be32(&mut payload, 16, MAX_SHARED_BUFFERS);
    put_be32(&mut payload, 20, 0); // max_surfaces
    put_be32(&mut payload, 24, FIRMWARE_VERSION);
    put_be32(&mut payload, 28, RING_ENTRIES);
    put_be32(&mut payload, 32, RING_ENTRIES);
    put_be32(&mut payload, 36, 0); // host_window_heap_size (no aperture heap)
    payload
}

fn query_service(req: &Request) -> OpResult {
    if req.payload_len < 4 {
        return Err(STATUS_BAD_REQUEST);
    }
    let service_id = get_be32(&req.payload, 0);
    let (flags, opcode_count, name): (u32, u32, &[u8]) = match service_id {
        id if id == u32::from(SERVICE_CORE) => (SERVICE_FLAG_FIRMWARE, 6, b"core"),
        id if id == u32::from(SERVICE_MEMORY) => (SERVICE_FLAG_FIRMWARE, 4, b"memory"),
        id if id == u32::from(SERVICE_CRYPTO) => (CRYPTO_SERVICE_FLAGS, 5, b"crypto"),
        _ => return Err(STATUS_NOT_FOUND),
    };
    let mut payload = vec![0u8; 48];
    put_be32(&mut payload, 0, service_id);
    put_be32(&mut payload, 4, FIRMWARE_VERSION);
    put_be32(&mut payload, 8, CAPABILITY_BITS);
    put_be32(&mut payload, 12, flags);
    put_be32(&mut payload, 16, service_id); // opcode_base
    put_be32(&mut payload, 20, opcode_count);
    put_be32(&mut payload, 24, ENTRY_PAYLOAD_MAX);
    payload[28..28 + name.len()].copy_from_slice(name);
    Ok(payload)
}

fn alloc_shared(board: &mut Board, req: &Request) -> OpResult {
    if req.payload_len < 12 {
        return Err(STATUS_BAD_REQUEST);
    }
    let length = get_be32(&req.payload, 0);
    let alignment = get_be32(&req.payload, 4);
    let flags = get_be32(&req.payload, 8);
    // HOST_WINDOW and CARD_ONLY are placement hints; the whole heap lives
    // in the Amiga-visible window, so both are no-ops here.
    let (handle, off, rounded) = board
        .heap
        .alloc(length, alignment)
        .ok_or(STATUS_NO_MEMORY)?;
    let mut payload = vec![0u8; 16];
    put_be32(&mut payload, 0, handle);
    put_be32(
        &mut payload,
        4,
        ARM_MEMORY_START + (off - AMIGA_MEMORY_OFFSET),
    );
    put_be32(&mut payload, 8, rounded);
    put_be32(&mut payload, 12, flags);
    Ok(payload)
}

fn free_shared(board: &mut Board, req: &Request) -> OpResult {
    if req.payload_len < 4 {
        return Err(STATUS_BAD_REQUEST);
    }
    let handle = get_be32(&req.payload, 0);
    if board.heap.free(handle) {
        Ok(Vec::new())
    } else {
        Err(STATUS_BAD_HANDLE)
    }
}

fn mem_fill(board: &mut Board, req: &Request) -> OpResult {
    if req.payload_len < 13 {
        return Err(STATUS_BAD_REQUEST);
    }
    let handle = get_be32(&req.payload, 0);
    let offset = get_be32(&req.payload, 4);
    let length = get_be32(&req.payload, 8);
    let value = req.payload[12];
    let (off, len) = board
        .heap
        .resolve(handle, offset, length)
        .ok_or(STATUS_BAD_HANDLE)?;
    board.win[off as usize..(off + len) as usize].fill(value);
    Ok(Vec::new())
}

fn mem_copy(board: &mut Board, req: &Request) -> OpResult {
    if req.payload_len < 20 {
        return Err(STATUS_BAD_REQUEST);
    }
    let dst_handle = get_be32(&req.payload, 0);
    let dst_offset = get_be32(&req.payload, 4);
    let src_handle = get_be32(&req.payload, 8);
    let src_offset = get_be32(&req.payload, 12);
    let length = get_be32(&req.payload, 16);
    let data = board.read_buf(src_handle, src_offset, length)?;
    board.write_buf(dst_handle, dst_offset, &data)?;
    Ok(Vec::new())
}

fn crypto_hash(board: &mut Board, req: &Request) -> (OpResult, i64) {
    if req.payload_len < 40 {
        return (Err(STATUS_BAD_REQUEST), LAT_ADMIN);
    }
    let src_handle = get_be32(&req.payload, 0);
    let src_offset = get_be32(&req.payload, 4);
    let src_length = get_be32(&req.payload, 8);
    let dst_handle = get_be32(&req.payload, 12);
    let dst_offset = get_be32(&req.payload, 16);
    let key_handle = get_be32(&req.payload, 20);
    let key_offset = get_be32(&req.payload, 24);
    let key_length = get_be32(&req.payload, 28);
    let algorithm = get_be32(&req.payload, 32);
    let flags = get_be32(&req.payload, 36);
    let latency = bulk_cck(src_length);
    let hmac = flags & HASH_FLAG_HMAC != 0;
    let result = (|| {
        let Some(digest_len) = cryptoimpl::digest_len(algorithm) else {
            return Err(STATUS_UNSUPPORTED);
        };
        let src = board.read_buf(src_handle, src_offset, src_length)?;
        let key = if hmac || algorithm == HASH_POLY1305 {
            board.read_buf(key_handle, key_offset, key_length)?
        } else {
            Vec::new()
        };
        let digest = cryptoimpl::hash(algorithm, hmac, &src, &key)?;
        board.write_buf(dst_handle, dst_offset, &digest)?;
        debug_assert_eq!(digest.len() as u32, digest_len);
        Ok(crypto_result(digest_len, algorithm, 0))
    })();
    (result, latency)
}

fn crypto_stream(board: &mut Board, req: &Request) -> (OpResult, i64) {
    if req.payload_len < 48 {
        return (Err(STATUS_BAD_REQUEST), LAT_ADMIN);
    }
    let src_handle = get_be32(&req.payload, 0);
    let src_offset = get_be32(&req.payload, 4);
    let src_length = get_be32(&req.payload, 8);
    let dst_handle = get_be32(&req.payload, 12);
    let dst_offset = get_be32(&req.payload, 16);
    let key_handle = get_be32(&req.payload, 20);
    let key_offset = get_be32(&req.payload, 24);
    let nonce_handle = get_be32(&req.payload, 28);
    let nonce_offset = get_be32(&req.payload, 32);
    let counter = get_be32(&req.payload, 36);
    let algorithm = get_be32(&req.payload, 40);
    let latency = bulk_cck(src_length);
    let result = (|| {
        if algorithm != STREAM_CHACHA20 {
            return Err(STATUS_UNSUPPORTED);
        }
        let src = board.read_buf(src_handle, src_offset, src_length)?;
        let key = board.read_buf(key_handle, key_offset, 32)?;
        let nonce = board.read_buf(nonce_handle, nonce_offset, 12)?;
        let out = cryptoimpl::chacha20_stream(&key, &nonce, counter, &src)?;
        board.write_buf(dst_handle, dst_offset, &out)?;
        Ok(crypto_result(src_length, algorithm, 0))
    })();
    (result, latency)
}

fn crypto_aead(board: &mut Board, req: &Request) -> (OpResult, i64) {
    if req.payload_len < 48 {
        return (Err(STATUS_BAD_REQUEST), LAT_ADMIN);
    }
    let src_handle = get_be32(&req.payload, 0);
    let src_offset = get_be32(&req.payload, 4);
    let src_length = get_be32(&req.payload, 8);
    let dst_handle = get_be32(&req.payload, 12);
    let dst_offset = get_be32(&req.payload, 16);
    let aad_handle = get_be32(&req.payload, 20);
    let aad_offset = get_be32(&req.payload, 24);
    let aad_length = get_be32(&req.payload, 28);
    let key_handle = get_be32(&req.payload, 32);
    let key_offset = get_be32(&req.payload, 36);
    let nonce_handle = get_be32(&req.payload, 40);
    let flags = get_be32(&req.payload, 44);
    let latency = bulk_cck(src_length);
    let decrypt = flags & AEAD_FLAG_DECRYPT != 0;
    // The algorithm rides in flags bits 8-15; 0 is the legacy default,
    // ChaCha20-Poly1305, and the result reports the *resolved* id while
    // echoing only the DECRYPT bit (the zz9k-aead tool asserts both).
    let algorithm = match (flags & AEAD_ALG_MASK) >> AEAD_ALG_SHIFT {
        0 => AEAD_CHACHA20_POLY1305,
        alg => alg,
    };
    let result = (|| {
        let key_len = match algorithm {
            AEAD_AES128_GCM => 16,
            AEAD_CHACHA20_POLY1305 | AEAD_AES256_GCM => 32,
            _ => return Err(STATUS_UNSUPPORTED),
        };
        let key = board.read_buf(key_handle, key_offset, key_len)?;
        let nonce = board.read_buf(nonce_handle, 0, 12)?;
        let aad = if aad_length != 0 {
            board.read_buf(aad_handle, aad_offset, aad_length)?
        } else {
            Vec::new()
        };
        let (out, bytes_written) = if decrypt {
            // src holds ciphertext plus the 16-byte tag appended after it.
            let ct_tag = board.read_buf(src_handle, src_offset, src_length + AEAD_TAG_BYTES)?;
            (
                cryptoimpl::aead_decrypt(algorithm, &key, &nonce, &aad, &ct_tag)?,
                src_length,
            )
        } else {
            let pt = board.read_buf(src_handle, src_offset, src_length)?;
            (
                cryptoimpl::aead_encrypt(algorithm, &key, &nonce, &aad, &pt)?,
                src_length + AEAD_TAG_BYTES,
            )
        };
        board.write_buf(dst_handle, dst_offset, &out)?;
        Ok(crypto_result(
            bytes_written,
            algorithm,
            flags & AEAD_FLAG_DECRYPT,
        ))
    })();
    (result, latency)
}

fn crypto_kx(board: &mut Board, req: &Request) -> (OpResult, i64) {
    if req.payload_len < 32 {
        return (Err(STATUS_BAD_REQUEST), LAT_ADMIN);
    }
    let scalar_handle = get_be32(&req.payload, 0);
    let scalar_offset = get_be32(&req.payload, 4);
    let point_handle = get_be32(&req.payload, 8);
    let point_offset = get_be32(&req.payload, 12);
    let dst_handle = get_be32(&req.payload, 16);
    let dst_offset = get_be32(&req.payload, 20);
    let algorithm = get_be32(&req.payload, 24);
    let flags = get_be32(&req.payload, 28);
    let latency = if algorithm == KX_X25519 {
        LAT_X25519
    } else {
        LAT_P256
    };
    let keygen = algorithm == KX_P256 && flags == KX_FLAG_KEYGEN;
    let result = (|| {
        let scalar = board.read_buf(scalar_handle, scalar_offset, 32)?;
        let point = if keygen {
            // Keygen has no peer point; its descriptor carries an invalid
            // point handle by design.
            Vec::new()
        } else {
            let point_len = if algorithm == KX_X25519 {
                32
            } else {
                P256_POINT_BYTES
            };
            board.read_buf(point_handle, point_offset, point_len)?
        };
        let out = cryptoimpl::kx(algorithm, flags, &scalar, &point)?;
        board.write_buf(dst_handle, dst_offset, &out)?;
        Ok(crypto_result(
            out.len() as u32,
            algorithm,
            flags & KX_FLAG_KEYGEN,
        ))
    })();
    (result, latency)
}

fn crypto_verify(board: &mut Board, req: &Request) -> (OpResult, i64) {
    if req.payload_len < 40 {
        return (Err(STATUS_BAD_REQUEST), LAT_ADMIN);
    }
    let algorithm = get_be32(&req.payload, 0);
    let hash_handle = get_be32(&req.payload, 4);
    let hash_offset = get_be32(&req.payload, 8);
    let hash_length = get_be32(&req.payload, 12);
    let sig_handle = get_be32(&req.payload, 16);
    let sig_offset = get_be32(&req.payload, 20);
    let sig_length = get_be32(&req.payload, 24);
    let key_handle = get_be32(&req.payload, 28);
    let key_offset = get_be32(&req.payload, 32);
    let key_length = get_be32(&req.payload, 36);
    let latency = if algorithm == VERIFY_RSA_PKCS1_2048_SHA256 {
        LAT_RSA
    } else {
        LAT_ECDSA
    };
    let result = (|| {
        let digest = board.read_buf(hash_handle, hash_offset, hash_length)?;
        let sig = board.read_buf(sig_handle, sig_offset, sig_length)?;
        let key = board.read_buf(key_handle, key_offset, key_length)?;
        let valid = cryptoimpl::verify(algorithm, &digest, &sig, &key)?;
        // An invalid signature is a successful verification whose payload
        // carries valid = 0 -- never an error status.
        let mut payload = vec![0u8; 48];
        put_be32(&mut payload, 0, u32::from(valid));
        Ok(payload)
    })();
    (result, latency)
}

fn diag_read(board: &Board) -> Vec<u8> {
    let mut payload = vec![0u8; 48];
    put_be32(&mut payload, 0, board.requests_completed);
    put_be32(&mut payload, 4, board.requests_failed);
    put_be32(&mut payload, 8, board.last_status);
    put_be32(&mut payload, 12, board.pending.len() as u32);
    put_be32(&mut payload, 16, board.heap.buffers_used());
    put_be32(&mut payload, 20, board.heap.total());
    put_be32(&mut payload, 24, board.heap.free_bytes());
    put_be32(&mut payload, 28, board.heap.largest_free());
    put_be32(&mut payload, 32, MAILBOX_ARM_ADDRESS);
    put_be32(&mut payload, 36, RING_ENTRIES);
    // surfaces_used and allocator_invalid_slots stay 0.
    payload
}
