/* SPDX-License-Identifier: GPL-3.0-or-later */
volatile int increment = 3;

int worker(int value)
{
    volatile int adjusted = value + increment;
    return adjusted;
}
