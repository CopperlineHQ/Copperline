/* SPDX-License-Identifier: GPL-3.0-or-later */
/* Linked first: AmigaDOS enters at the start of the first code hunk. */
extern int worker(int value);
volatile int result;

void entry(void)
{
    result = worker(7);
    __asm__ volatile ("1: bra.s 1b");
}
