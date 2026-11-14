# tapetest

Guest-side probe for the SCSI tape target (`src/scsi/tape.rs`), run by
`tests/scsi_tape.rs`. It opens `scsi.device` unit 4 and drives the drive
with SCSI Direct (`HD_SCSICMD`) the way tape software does: INQUIRY, MODE
SELECT/SENSE, REWIND, fixed- and variable-block WRITE and READ, WRITE
FILEMARKS, SPACE, READ BLOCK LIMITS, LOAD/UNLOAD and ERASE, checking the
status and the autosense data (FILEMARK, EOM, ILI, the residue) of each
step.

The program's return code is 0 when every step passed and 100 plus the
number of the first failing step otherwise; the host test reads it through
`--exit-on-return` and then checks the tape files the probe left in the
cartridge directory.

Build with `make` (the dockerized cross-GCC of `../toolchain.mk`); the
binary is committed so `cargo test` needs no toolchain.
