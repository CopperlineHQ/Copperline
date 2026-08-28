| AmigaDOS jumps to the first byte of the first hunk: this object is
| listed first on the link line and only forwards to the C entry point.
    .text
    .globl __probe_start
__probe_start:
    jmp _entry
