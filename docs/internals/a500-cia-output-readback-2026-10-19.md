# CIA timer output readback during E-high

The A500 kind66 probe samples Timer A, PB6, Paula INTREQR, Timer A again and
ICR. Exact independent PRA, launch and observation beam brackets qualify each
comparison. Both CIAs, plain/PBON controls and16 one-shot latch values are
included. CPU interrupts are masked, separating readback from recognition.

All counters and ICR flags agree. Twenty PBON cases read PB6 high in Copperline
while hardware already reads low. Forty different cases see the Paula request
early. These are separate paths: the PB6 difference does not justify moving the
timer or imposing a common interrupt delay.

`Cia::read_at_transfer` now previews the impending Phi2 underflow output when a
live zero counter is sampled during E-high, as the existing timer read previews
its reload mux. The preview does not commit the counter, interrupt flag, physical
pin, or pulse state. Pulse/toggle behavior and consecutive-underflow merging use
the existing output rules. Stopped and CNT-driven counters do not predict a
Phi2 underflow. The same transfer path handles both timer output bits; the new
hardware boundary evidence directly covers Timer A/PB6 on both CIAs.

References: `amiga-hardware-probes/references/cia-source-v3/run{1,2}`;
ROM SHA-256 `045adb8f6329d940cfb9b039ae2374c7f4aa41e249c77ba7f51bbbb2ba0e3112`.
The two physical runs have different phase brackets, with five repetitions per
descriptor and identical measured-value vectors; they are not byte-identical
independent phase cohorts. The fix repairs all20 PB6 cases without a new field
or qualification difference. An independent four-request sampling sweep checks
the result at neighboring observation phases. Request propagation remains
unresolved; `DEFAULT_IRQ_LATENCY_CCK` is unchanged.

Further evidence and limitations:
`amiga-hardware-probes/docs/cia-source-investigation-2026-10-19.md`.
