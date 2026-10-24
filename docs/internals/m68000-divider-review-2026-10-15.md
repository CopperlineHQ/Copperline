# MC68000 divider ordering review, 15 October 2026

## Conclusion

Follow-up core correction: m68k commit `275b2e6` preserves the MC68000
destination and sets overflow for signed `$80000000 / -1`. It also exposes
the measured early DIVS poll through `AddressBus::ipl_poll_internal`, taking
and retaining a fresh sample two CPU clocks before the unchanged final
prefetch. Copperline implements the hook; it does not reuse the preceding
operand access's sample. Other CPU models retain their existing behavior.
All four m68k contribution checks pass, including the new clocked-bus IRQ
and overflow regressions. The review and counts below describe the preceding
core version unless explicitly marked as a post-fix replay.

The remaining saved-PC differences must not all be attributed to the host.
The probe waits in `TST.W DONE(A5)` and a conditional branch after the marker.
A source comparison also finds an ordinary-instruction poll discrepancy:
WinUAE's cycle-exact memory TST handler samples IPL before the operand read,
whereas m68k's `exec_tst` currently leaves the boundary sample to the final
access. Isolate that waiting-loop poll point separately from the DIV/marker
boundary and from host IRQ propagation. It is not changed by the DIVS fix.

### Post-fix Copperline replay

The rebuilt release executable replayed the unchanged accepted v3 ROM against
all 2,560 physical A500 records, with zero excluded launches, timeouts or skips.
All division results now agree (256 mismatches before, zero after). Saved-PC
and handler-beam mismatches are 452 (446 before), including 40 DIV/marker
boundary mismatches (34 before). Final marker values still all agree. The
early DIVS poll removes an accidental cancellation in six boundary cases;
it does not close the common recognition discrepancy affecting ordinary
DIVU/DIVS. These results must not be described as fully accurate end-to-end
divider interrupts. The core's internal poll placement is separately checked
by clocked-bus tests, including IRQ arrivals between the poll and prefetch.

Capture: `amiga-hardware-probes/captures/divider-irq-copperline-fixed-v3`.
Comparison: `captures/divider-irq-a500-v3-fixed-comparison.json` in that repo.
Packet SHA-256:
`8a1f0991a8b8fec991f7935f4053835ccbad0c955d36904f977d148a2d2b10ae`.
No new hardware run was needed: the ROM and accepted hardware reference are
unchanged. The physical machine remains on its restored Kickstart firmware.

Validation: m68k formatting, all-target/all-feature Clippy, the complete
all-feature test suite and clean-tree packaging all passed. Copperline's
release build, Clippy, formatting and new host-hook test passed. Its full
library suite returned 4,084 passes, five failures and 43 ignored tests.
Three failed tests passed on individual rerun (keypad mapping, disk-swap
netplay and spectator netplay). The picture-scaler comparison and netplay
mouse-convergence test still failed; both failures also appear in prior
10/14 October logs, before these changes. A preserved pre-change test binary
also reproduces the scaler failure. The full host suite is therefore not
claimed clean. Details are in local `tmp/divider-host-tests.log`,
`tmp/divider-host-reruns.json`, and `tmp/divider-host-rerun-*.log`.

The computation-before-final-prefetch correction in m68k commit
`54b862309a0f4a9f150a8d01532cf53d62281473` is supported by the saved One ROM
A500 measurements and matches the ordering in WinUAE's cycle-exact MC68000
DIVU and DIVS handlers. It preserves the existing instruction cycle totals.
It does **not** establish complete DIVU/DIVS equivalence with WinUAE or exact
interrupt timing for every operand.

This review inspected the local WinUAE source at
`a69f1124a3b7e7a68582ae899247373414df78f6`. The previously recorded emulator
captures used WinUAE 6.0.3 Best Compatibility. The initial source review was
followed by the physical verification below, using that same WinUAE version.

## Physical verification following the source review

Two new One ROM runs on the PAL A500 produced byte-identical 2,560-trial
packets. WinUAE Best Compatibility matches every checked observable in all
2,560 matched-launch records: saved interrupt PC, marker at IRQ entry,
handler beam sample, final D0 and final marker. There were no timeouts,
skips or excluded launch pairs. The matrix covers all four DIVS sign pairs,
zero dividend, signed quotient overflow without early absolute overflow,
early absolute overflow and two DIVU controls, with display DMA disabled.

The sign-pair boundary measurements support WinUAE's special early DIVS
poll. They also establish that the current Copperline/m68k combination is
not fully equivalent: 446 saved-PC/handler-beam differences include 34
cases crossing the DIV/following-marker boundary. The remaining saved-PC
differences occur in the subsequent waiting loop. All 256 signed
`$80000000 / -1` results differ: hardware and WinUAE preserve `$80000000`,
while m68k writes zero. That behavior predates the ordering correction.
These are overlapping counts, not additive independent failures.

Ordinary DIVU and DIVS boundaries also differ, so implementing only the
special DIVS sample cannot resolve all observations. Separate the host IRQ
propagation/sample mechanism from CPU-specific poll points before changing
general latency. No CPU code was changed during this verification.

The full method, provenance, comparison and next-step plan are in
[the probe report](https://github.com/HakatomOy/amiga-hardware-probes/blob/main/docs/divider-irq-verification-2026-10-15.md).
Accepted source ID:
`5d1558cd75f40b7e947773e4778873a8e8ccf42876309960aed568e3f2261d40`.
Both hardware result packets have SHA-256
`5cdce1ea1730161e0340709f8f5aae1cd623c6f7efc7bbc9a5d9f379cb81ec4e`.
The original Kickstart firmware was restored and its flash readback verified
byte-for-byte. All 64 probe Python tests pass.

## Source comparison

WinUAE `cpuemu_13.cpp`, `op_80c0_13_ff` and the other DIVU addressing-mode
handlers, advance the divider's internal clocks before the final instruction
prefetch. The DIVS handlers, including `op_81c0_13_ff`, do likewise. The
MC68000 cycle calculations in `newcpu_common.cpp` exclude the final four-clock
prefetch; m68k's calculations include it, and `finish_m68000_div` subtracts
those four clocks before synchronizing. These conventions agree.

There is a significant DIVS exception to the ordinary interrupt poll point.
WinUAE `getDivs68kCycles` reports an extra case when the dividend is
nonnegative and the divisor is negative, after excluding the early absolute
overflow path. Its generated handler advances all but the last two internal
CPU clocks, calls `ipl_fetch_next`, advances the remaining two clocks, and
then performs the final prefetch. Thus this poll is two CPU clocks (one PAL
A500 colour clock) earlier than the ordinary prefetch-associated poll.

m68k's divider helper always advances the entire internal interval before
prefetch. Its AddressBus contract normally takes the boundary IPL sample
from the last bus access. There is no operand-dependent early DIVS sample in
this helper. Consequently the patch matches the final bus-access placement
but does not implement WinUAE's special DIVS IPL timing. An interrupt changing
near that boundary could be taken after a different instruction even though
uncontended division duration and final prefetch timing agree. This is a
source-level difference; the subsequent hardware sweep above supports
WinUAE's boundary behavior.

There are also pre-existing differences outside this ordering correction:

* For signed `$80000000 / -1`, m68k explicitly writes zero to the destination
  and clears overflow. WinUAE's MC68000 path preserves the destination and
  sets overflow. The m68k behavior is present in this commit's parent.
* m68k updates the division result and flags after its final prefetch;
  WinUAE updates them before it. A final-prefetch bus fault therefore needs a
  dedicated exception-state comparison. This relative writeback ordering was
  already present before the correction and was not validated by the A500
  elapsed-time experiments.

These differences should not be presented as regressions introduced by
moving the prefetch. They do prevent a blanket claim that fixed m68k DIVU
and DIVS match WinUAE in every respect.

## Earlier hardware evidence and its limits

The saved aligned CPU sweep contains 1,792 scheduled trials. Against the
physical A500, fixed Copperline has zero elapsed-time mismatches among 1,732
safe same-launch pairs; WinUAE has zero among 1,640; the earlier Copperline
capture has 655 among 1,685. Pair counts differ because unsafe beam samples
and unequal launches are excluded independently for each capture.

The six-bitplane, 64-division loops agree across all three systems: normal
DIVU 4763–4767 colour clocks, worst-case DIVU 5102, and DIVS 5249–5257,
including loop and readback overhead. These figures are saved measurements,
not newly rerun results. See `a500-timing-measurements-2026-10-14.md` and the
probe repository's `docs/rom-timing-isolation-2026-10-14.md`.

The original probe's aligned DIVS kernel uses dividend `$12345678` and divisor
`$789a`. Its original DIVS/IRQ kernel also uses a positive divisor. Neither covers the
negative-divisor early IPL sample. The new m68k bus-sequence tests cover
register, immediate and memory operands and overflow paths, but do not
inject interrupts or test prefetch faults. No external MC68000 bus capture
has yet established the divider's physical prefetch boundary.

## Checks rerun

All four commands from `vendor/m68k/CONTRIBUTING.md` completed successfully:

```text
cargo fmt --all -- --check
cargo clippy --all-targets --all-features --locked -- -D warnings
cargo test --all-features --locked
cargo package --locked
```

Packaging emitted existing default-feature dead-code warnings in
`trace_jit.rs` and warnings that test files are excluded from the package;
package verification still passed. The m68k checkout was not changed by
this review.

## Before claiming fully accurate divider IPL timing

The initial measurement plan below is retained for context. Steps 1-3 were
subsequently executed without display DMA, as recorded above; the probe report
contains the updated remaining plan. Display-DMA contention and external
prefetch capture remain untested by the new sweep.

1. Extend the One ROM IRQ probe with all four operand sign combinations,
   keeping the quotient in range. Include zero dividend with a negative
   divisor, signed quotient overflow without early absolute overflow, and
   early absolute overflow as a separate control.
2. Sweep the IRQ assertion across the last internal clocks and final
   prefetch. Record the saved interrupt PC to distinguish interruption
   immediately after DIV from interruption after its following marker.
   Use identical actual launch phases and inspect both CPU clock parities.
3. Repeat with display DMA disabled first, then with bitplane contention.
   Compare physical A500, WinUAE Best Compatibility, and Copperline. Use
   the scope to confirm final-prefetch placement; do not infer the internal
   IPL latch directly from an external bus waveform.
4. If hardware confirms the early sample, design an explicit timed IPL
   sampling facility in the CPU/bus interface. Do not move the prefetch two
   clocks earlier to imitate the interrupt behavior: WinUAE keeps the
   prefetch at its ordinary position. Add a bus/IRQ regression that tests
   both poll timing and unchanged prefetch timing.
5. Validate the signed minimum-dividend overflow result and final-prefetch
   fault state separately. Keep these independent from the bus-ordering PR
   unless its scope is deliberately expanded.

For the existing PR, describe the validated change as moving the MC68000
divider's internal interval before its final prefetch, with improved ordinary
IPL placement. Explicitly disclose the unimplemented special DIVS poll point.
The present evidence supports that focused correction, not a claim of
complete divider or interrupt equivalence.
