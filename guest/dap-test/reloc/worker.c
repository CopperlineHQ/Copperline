/* SPDX-License-Identifier: GPL-3.0-or-later */
volatile int increment = 3;

/* A separate code section exercises ELF section-to-hunk identity even
 * when every section has VMA zero in the relocatable ELF. */
__attribute__((section(".text.worker"), noinline))
int worker(int value)
{
    return value + increment;
}
