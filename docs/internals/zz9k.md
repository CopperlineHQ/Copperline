# The zz9k crypto board: ZZ9000 SDK v2 protocol subset

This page is the **contract**, not an implementation note: it specifies the
register- and opcode-level behavior of Copperline's bundled ZZ9000 SDK
crypto board (`[zz9k]`), precisely enough that the board
(`crates/zz9k-plugin`, hosted by `src/wasmboard.rs` and bundled by
`src/zz9k.rs`) can be verified against the SDK's own Amiga-side software.
Where this document and the implementation disagree, this document wins.

Unlike the other board contracts in this directory, the protocol is not
Copperline's to define: the board implements a subset of the **MNT ZZ9000
"SDK v2" service platform**, whose authoritative definition is the
zz9000-sdk repository (BlitterStudio/zz9000-sdk, GPL-3.0-or-later, the same
license as Copperline). This page pins the exact revision the board was
written against and records every choice the board makes where the ABI
leaves the firmware latitude. The behavioral reference is the SDK's own
Amiga-side transport, `host/src/zz9k_host.c` -- the code every SDK tool and
the accelerated AmiSSL build link.

**Pinned SDK revision:** commit `9a7ec6de5069117f08049165e498d1cf6a6f1cab`
(2026-08-17). File references below (`abi.h`, `zz9k_host.c`, ...) are paths
in that repository.

## What the board is for

The real ZZ9000's ARM core offers a crypto offload service: the Amiga
submits hash/AEAD/key-exchange/signature-verify operations through a ring
mailbox in the board window and the ARM computes them at modern speed,
which is what makes TLS-era cryptography usable from a 68k. Copperline's
board plays the ARM's role with host-native code (pure-Rust RustCrypto
compiled into the plugin), so the **unmodified** SDK stack -- the
`zz9k.library` transport, the `zz9k-info`/`zz9k-hash`/`zz9k-chacha`/
`zz9k-aead`/`zz9k-cryptobench`/`zz9k-irqtest` tools, and the SDK's
accelerated AmiSSL integration -- detects and drives it exactly as it does
real hardware.

Only the **CORE**, **MEMORY**, and **CRYPTO** services (plus the
`DIAG_READ` counter snapshot) are implemented. Everything else the real
board offers -- RTG graphics, surfaces, image/audio/video codecs, USB,
Ethernet -- is absent: those opcodes complete with `UNSUPPORTED`, their
`QUERY_SERVICE` ids report `NOT_FOUND`, and non-SDK registers read zero.
Installing the real board's P96 `zz9000.card` RTG driver against this board
is therefore unsupported; the board guarantees only that the driver's
register probes read zeroes rather than crash the machine.

## Zorro identity

- Manufacturer **0x6D6E** (MNT Research -- the identity the SDK's
  `FindConfigDev` probe looks for; presenting a Copperline product number
  would defeat the point of compatibility).
- Product **4** on Zorro III, **3** on Zorro II, probed in that order by
  the transport (`zz9k_find_board`, zz9k_host.c).
- Window: Zorro III any power of two from 1M to 256M (default **4M**);
  Zorro II **exactly 4M** -- the SDK transport forwards shared-buffer
  allocations through a Zorro II window only when the autoconfig size is
  exactly the historical 4 MB profile (`zz9k_alloc_shared`'s
  `ZZ9K_HOST_WINDOW_MIN_BOARD_SIZE` check), and this board deliberately
  does not advertise the newer aperture-layout negotiation that would lift
  that restriction (see [Capability bits](#capability-bits)).
- I/O window (never in the Exec memory list), no autoboot ROM, no
  `diag_vec`: the SDK software finds the board itself.
- The default bus generation follows the machine: Zorro III on a 32-bit
  CPU, Zorro II otherwise (`[zz9k] zorro` overrides).

## Address map (board-relative)

| Offset | Contents |
|---|---|
| `0x0000`-`0x0FFF` | Registers (below); everything unlisted reads 0, writes ignored |
| `0x1000`-`0x1FFF` | Zorro III register aperture: aliases `0x0000`-`0x0FFF` (the transport writes the doorbell through it on Z3) |
| `0xA000`-`0xFFFF` | Legacy mapped-IO window = ARM `0x3FE40000`-`0x3FE46000` |
| `0xD000`-`0xD07F` | Mailbox descriptor (inside the mapped-IO window, = ARM `0x3FE43000`) |
| `0xD080`-`0xD87F` | Request ring, 32 x 64-byte entries |
| `0xD880`-`0xE07F` | Completion ring, 32 x 64-byte entries |
| `0x10000`-size | Shared-buffer heap = ARM `0x00200000` upward |

ARM addresses appear on the wire (the mailbox pointer registers, shared
buffer `arm_addr` replies); the transport maps them back to board offsets
through exactly the two windows above (`zz9k_arm_range_to_board_offset`).

All wire data -- registers, descriptor fields, ring entries, inline
payloads -- is **big-endian**. The guest accesses registers with 16-bit
cycles, writes ring entries as 16-bit stores (plus a trailing byte for odd
payload lengths), and reads completions byte-wise; the board serves any
access size 1/2/4 at any offset, composing bytes big-endian.

## Registers

| Offset | Name | Behavior |
|---|---|---|
| `0x0004` | CONFIG | Read: interrupt status, bit `0x0008` = SDK completion pending. Write `0x0088` (ACK_MODE\|ACK_SDK): acknowledge |
| `0x00E8` | CONFIG_KEY | Latched key query: write a key id, read back the key's value. Key **5** = `int2` (nonzero: completion IRQ on INT2/PORTS instead of INT6/EXTER) |
| `0x00EA` | CONFIG_PRESENT | Read: nonzero if the last key id written to CONFIG_KEY is known |
| `0x0100` | SDK_MAGIC | Reads `0x5A39` |
| `0x0102` | SDK_VERSION | Reads `0x0203` (ABI 2.3; the transport checks only major = 2) |
| `0x0104`/`0x0106` | SDK_MAILBOX_HI/LO | Read: ARM address of the mailbox descriptor, `0x3FE43000` |
| `0x0108` | SDK_DOORBELL | Write: request-ring kick. Accepted but **redundant**: the board scans the request ring from its own clock regardless, because the Zorro II transport never rings the doorbell at all (`zz9k_z3_register_window_reg16` returns NULL off Z3) |
| `0x010A` | SDK_STATUS | Reads 0 |
| `0x010C` | SDK_IRQ_CTRL | Write `0x0001` ack, `0x0002` enable, `0x0004` disable the completion interrupt |
| `0x0110`-`0x011E` | DIAG / APERTURE_INFO | Read 0 (aperture state LEGACY), writes ignored |

## Mailbox

The 128-byte descriptor at `0xD000` follows `ZZ9KMailboxDescriptor`
(abi.h): magic `0x5A5A394B` ("ZZ9K"), ABI 2.3, ring offsets **relative to
the descriptor base** (`0x080` and `0x880`), 32 entries each, guest-owned
`request_tail`/`completion_head` and board-owned
`request_head`/`completion_tail` indices, and the capability bits.

Wire entries are 64-byte `ZZ9KMailboxWireEntry`s: `request_id` u32,
`opcode` u16, `status` u16, `flags` u16, `payload_len` u16, `user_cookie`
u32, 48-byte inline payload. The board echoes `request_id`, `opcode`, and
`user_cookie` into the completion (the transport matches on all three) and
sets `status` to the result. Error completions carry an empty payload.

### Capability bits

The descriptor and `QUERY_CAPS` advertise `0x7D07`: MAILBOX (1<<0),
IRQ_COMPLETION (1<<1), SHARED_ALLOC (1<<2), CRYPTO (1<<8), MEMORY_OPS
(1<<10), DIAGNOSTICS (1<<11), DOORBELL (1<<12), POLLING_COMPLETION (1<<13),
SERVICE_DISCOVERY (1<<14). **Deliberately clear:** HOST_WINDOW_HEAP
(1<<20) and APERTURE_LAYOUT (1<<24) -- their absence selects the
transport's simple "historical fixed 4 MB" Zorro II path and skips the
aperture-layout acknowledgement handshake entirely.

### Request pickup and completion timing

The board consumes **at most one request per emulated tick** (one CPU
instruction boundary), computes it immediately -- inputs are read at
dispatch, so mutating a buffer after submission does not affect an
in-flight op, matching real firmware -- and publishes the completion after
a deterministic latency in colour clocks (CCK = 3,546,895 per emulated
second), modelling a serial coprocessor:

| Operation | Latency |
|---|---|
| CORE / MEMORY / DIAG ops | 50 us |
| HASH / STREAM / AEAD | 200 us + length / 50 MB/s |
| KX X25519 | 1 ms |
| KX P-256 (derive or keygen) | 2 ms |
| VERIFY ECDSA-P256 | 2 ms |
| VERIFY RSA | 1 ms |

Completions publish in submission order; a full completion ring holds the
queue until the guest consumes. When the completion interrupt is enabled
(`0x010C` = 2), publishing a completion raises the selected line (INT6 by
default, INT2 when the `int2` config key says so) level-sensitively until
either acknowledge form (`0x010C` = 1 or CONFIG = `0x0088`) clears it.

## Services

### CORE (0x0000)

- `NOP`: OK, empty. `PING`: echoes its payload.
- `QUERY_CAPS`: 40-byte reply -- magic, ABI 2.3, capability bits, max
  inline payload 48, max shared buffers 64, max surfaces 0, firmware
  version `0x00020003` (rendered "2.3" by zz9k-info), request/completion
  ring entries 32/32, host-window heap size 0.
- `QUERY_SERVICE`: 48-byte `ZZ9KServiceInfo` for CORE ("core", 6 ops),
  MEMORY ("memory", 4 ops), CRYPTO ("crypto", 5 ops, flags below); any
  other id -- including DIAG -- reports `NOT_FOUND`, which `zz9k-info`
  skips silently. The AmiSSL provider refuses to load unless
  `QUERY_SERVICE(CRYPTO)` succeeds.
- `CANCEL`: `NOT_FOUND` (nothing is cancellable at these latencies).
  `QUERY_APERTURE_LAYOUT`: `UNSUPPORTED`.

The CRYPTO service flags are `0x003F0001`: FIRMWARE plus X25519 (1<<16),
P256 (1<<17), ECDSA_P256 (1<<18), RSA_2048 (1<<19), AES_GCM (1<<20),
P256_KEYGEN (1<<21) -- the exact gates the provider and `zz9k-cryptobench`
check before offloading each primitive.

### MEMORY (0x0100)

`ALLOC_SHARED` allocates from the heap at `0x10000`..size (16-byte
granules, minimum alignment 16, requested alignment honoured), replying
{handle, arm_addr, rounded length, flags echo}. Handles are
generation-tagged, never 0 (the SDK tools use `handle != 0` as their
cleanup guard) and never `0xFFFFFFFF` (`ZZ9K_INVALID_HANDLE`); a freed
handle goes stale (`BAD_HANDLE` forever after). The HOST_WINDOW and
CARD_ONLY flags are accepted as no-op placement hints: the whole heap is
Amiga-visible. Exhaustion (space or the 64-slot table) is `NO_MEMORY`.
`FREE_SHARED` coalesces. `MEM_FILL`/`MEM_COPY` operate on
handle+offset+length triples (copies may overlap).

### CRYPTO (0x0800)

Descriptors reference (handle, offset, length) triples into shared
buffers; every triple is bounds-checked (`BAD_HANDLE` on any violation).
Results are the 48-byte big-endian `{bytes_written, algorithm, flags}`
payload; the SDK reply decoder treats `bytes_written == 0` on an OK status
as an internal error, so successful results are always nonzero.

- `HASH` (+0x00): SHA-1 (1, 20 bytes), SHA-256 (2, 32), SHA-384 (3, 48),
  SHA-512 (4, 64), BLAKE2s-256 (5, 32), Poly1305 (6, 16-byte MAC, requires
  key length 32 and flags 0). Flag bit 0 = HMAC using the key triple
  (SHA-family only; HMAC-BLAKE2s is `UNSUPPORTED`, as on the real
  firmware). Result: {digest length, algorithm, 0}.
- `STREAM` (+0x01): ChaCha20 (RFC 8439), 32-byte key, 12-byte nonce,
  explicit initial block counter. Result: {src length, 1, 0}.
- `AEAD` (+0x02): the algorithm travels in **flags bits 8-15** (0 = legacy
  default = ChaCha20-Poly1305 (1); AES-128-GCM (2); AES-256-GCM (3));
  flag bit 0 = decrypt. Key length is implied by the algorithm (16 or 32);
  the 12-byte nonce is read from offset 0 of `nonce_handle` (the
  descriptor has no nonce offset field). Encrypt writes
  ciphertext||16-byte tag (`bytes_written` = length + 16); decrypt reads
  src length **+ 16** (tag appended after the ciphertext) and writes the
  plaintext (`bytes_written` = length). The result reports the *resolved*
  algorithm id and echoes **only** the DECRYPT flag bit -- `zz9k-aead`
  asserts both. A tag mismatch completes with status `IO_ERROR`.
- `KX` (+0x03): X25519 (1): 32-byte scalar x 32-byte point -> 32 bytes,
  with the RFC 7748 all-zero-output check (`BAD_REQUEST`). P-256 (2):
  scalar x 65-byte uncompressed SEC1 point -> 32-byte X coordinate. The
  KEYGEN flag (1) is valid **only** with P-256: scalar*G -> 65-byte
  uncompressed point, `point_handle` invalid by design. Any other nonzero
  flags word is `UNSUPPORTED` (the SDK gates keygen offload on the
  P256_KEYGEN service flag for exactly this reason). No other flag/algorithm
  combination exists; the board never generates randomness -- scalars
  always come from the guest.
- `VERIFY` (+0x04): ECDSA-P256-SHA256 (1): 32-byte digest, raw 64-byte
  r||s signature, 65-byte uncompressed key. RSA-PKCS1-SHA256 (2): key =
  modulus (big-endian) followed by a 4-byte big-endian exponent, with
  2048/3072/4096-bit moduli all accepted under the one id and the
  signature exactly modulus-sized. The reply payload's first u32 is the
  valid flag: **an invalid signature is a successful verification with
  valid = 0, never an error status** (malformed keys and signatures verify
  as invalid too -- the cryptographically correct answer).

### DIAG (0x0900)

`DIAG_READ` only: the 48-byte counter payload (requests completed/failed,
last status, pending count, buffers used, heap total/free/largest,
mailbox ARM address, ring entries, 0, 0). The other DIAG ops are
`UNSUPPORTED` and `QUERY_SERVICE(DIAG)` is `NOT_FOUND`; `zz9k-info` prints
exactly the counters and skips the rest.

## Determinism, randomness, and save states

The board is pure compute: its only host imports are `log` and
`config_get`, so fitting it keeps a machine fully deterministic and
replay-safe (the same guarantee as any DMA-and-interrupts-only WASM
board -- see [](../zorro.md)). No operation draws randomness; the KEYGEN
primitive is a deterministic scalar multiplication and TLS ephemeral
scalars are generated guest-side by the SDK/AmiSSL themselves. A ChaCha20
DRBG seeded from the `[zz9k] seed` config key exists **reserved and
dormant** for any future entropy-consuming op; its default seed is a fixed
constant, so runs stay byte-reproducible either way.

All board state -- the window (registers, mailbox, heap contents), the
allocator, pending completions and their remaining latencies -- lives in
the plugin's linear memory, so save states snapshot and resume the board
exactly, including mid-operation (a completion whose latency had not
elapsed at save time arrives on schedule after load).

## Verification

- `crates/zz9k-plugin` native tests: RFC/NIST/FIPS known-answer vectors
  for every primitive (including the SDK's own RSA-2048 KAT, ported from
  `tools/rsa_kat_vector.h`), the allocator, and the full mailbox protocol
  driven through 16-bit accesses like the 68k's.
- `src/wasmboard.rs` tests drive the committed artifact through wasmtime:
  bootstrap detection, a mailbox hash round trip, IRQ line selection, and
  the mid-operation save-state resume.
- `tests/zz9k.rs` (ignored; needs the bundled AROS only) boots the
  in-repo guest probe `guest/zz9kprobe` -- the SDK's real transport code
  compiled for m68k -- against the board.
- `tests/zz9k_sdk_tools.rs` (ignored; needs SDK tools built into
  `test-assets/zz9k/`) runs the unmodified SDK tools.
- **AmiSSL end-to-end (verified manually, 2026-08-18):** the SDK's AmiSSL
  provider self-test (`amiga/provider/zz9k_amissl_selftest.c`, built per
  `docs/zz9k-amissl-provider.md` against the AmiSSL SDK) reports
  `ALL PASS` with every offloadable operation `via 'zz9000'` against this
  board, on a 68030 + Kickstart 3.1 machine with the official AmiSSL 5.27
  OS3 runtime -- both with the stock `amissl_v362.library` (application-
  registered provider) and with the SDK CI's provider-baked drop-in
  library. X25519 key agreement and the AES-256-GCM / ChaCha20-Poly1305
  record round trips are cross-checked against OpenSSL's own software
  provider in both directions, DIAG_READ's `requests_completed` advances
  by the offloaded operations, and `ZZ9K_DISABLE_OFFLOAD` (a shell `Set`
  variable, or `ENV:`) flips every operation to `served by: default` with
  zero board traffic. Staging notes for reproducing it: the guest needs
  the Workbench `mathieee*` libraries (AmiSSL's init opens them, and its
  error path guru-crashes the library loader when they are missing -- not
  a board issue), an `AmiSSL:` volume/assign pointing at the extracted
  release (certificate store), an `ENV:` volume so environment probes do
  not raise insert-volume requesters, and `Stack 65536`.
