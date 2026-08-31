/* SPDX-License-Identifier: BSD-2-Clause */
/* Link only the copied DiagArea/bootstrap. The Python builder adds payload. */
SECTIONS {
    rom = .;
    .bootldr : {
        *(CODE)
        *(DATA)
        *(BSS)
    }
    .device ALIGN(16) : {
        device = .;
        *(DEVICE)
    }
}
