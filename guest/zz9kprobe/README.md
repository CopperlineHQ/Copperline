# zz9kprobe

Guest-side conformance probe for the bundled zz9k crypto board (`[zz9k]`,
contract in `docs/internals/zz9k.md`). It deliberately links the **real
ZZ9000 SDK transport** -- the same `zz9k_host.c` the SDK's `zz9k.library`
and every `zz9k-*` tool are built from -- so a pass means Copperline's
board satisfies the exact Amiga-side code paths real ZZ9000 software runs:
discovery, bootstrap registers, mailbox attach/submit/poll, shared
buffers, all five crypto opcodes against published vectors (FIPS 180,
RFC 4231/8439/7748/5903/6979, and the SDK's own RSA-2048 KAT), and the
completion interrupt's status/ack protocol.

`tests/zz9k.rs` boots the committed `zz9kprobe` binary on the bundled AROS
ROM against the board and asserts on its `ZZ9K: PASS/FAIL` output lines --
the same harness shape as `tests/mhi.rs`.

## Provenance

`vendor/` is copied verbatim from the zz9000-sdk repository
(<https://github.com/BlitterStudio/zz9000-sdk>, GPL-3.0-or-later -- the
same license as Copperline) at commit
`9a7ec6de5069117f08049165e498d1cf6a6f1cab` (2026-08-17), the revision the
board contract is pinned to:

- `vendor/zz9k/` -- `include/zz9k/{abi,caps,crypto,text,audio,compression,shared}.h`
  and `host/include/zz9k/{host,request,reply}.h`
- `vendor/zz9k_host.c` -- `host/src/zz9k_host.c`, the Amiga-side transport
- `vendor/rsa_kat_vector.h` -- `tools/rsa_kat_vector.h`, the RSA-2048 KAT

When bumping the pinned SDK revision, re-copy these files and update the
commit hash here and in `docs/internals/zz9k.md` in the same change.

## Building

```sh
make    # dockerized m68k-amigaos-gcc, see ../toolchain.mk
```

The committed `zz9kprobe` binary is the build artifact (same policy as the
other `guest/` programs: a clean checkout runs the integration test with
no cross-toolchain installed). The Makefile's `-fcommon` is load-bearing;
see its comment.
