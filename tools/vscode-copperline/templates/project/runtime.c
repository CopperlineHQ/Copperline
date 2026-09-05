/* SPDX-License-Identifier: CC0-1.0 */
/* The bundled Bartman compiler has no libc/libgcc archive. These are the
 * freestanding operations emitted by GCC for this 68000 template. */
void *memcpy(void *destination, const void *source, __SIZE_TYPE__ count)
{
    unsigned char *out = destination;
    const unsigned char *in = source;
    while (count--)
        *out++ = *in++;
    return destination;
}

unsigned int __mulsi3(unsigned int a, unsigned int b)
{
    unsigned int product = 0;
    while (b) {
        if (b & 1u)
            product += a;
        a <<= 1;
        b >>= 1;
    }
    return product;
}

unsigned int __umodsi3(unsigned int numerator, unsigned int denominator)
{
    unsigned int shifted = denominator;
    if (!denominator)
        return 0;
    while (shifted <= (numerator >> 1))
        shifted <<= 1;
    do {
        if (numerator >= shifted)
            numerator -= shifted;
        shifted >>= 1;
    } while (shifted >= denominator);
    return numerator;
}
