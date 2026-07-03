.include "entry.s"
/* Test: PACK - Pack BCD (68020+) */

run_test:
    /* PACK Dx,Dy,#adj: the adjustment is added to the raw 16-bit source
       BEFORE the digits are packed: dest = pack(src + adj) */
    
    /* Test 1: Pack 0x0302 with adj=0 -> 0x32 */
    move.w #0x0302, %d0
    pack %d0, %d1, #0
    cmp.b #0x32, %d1
    bne TEST_FAIL
    
    /* Test 2: Pack 0x0908 with adj=0 -> 0x98 */
    move.w #0x0908, %d0
    pack %d0, %d1, #0
    cmp.b #0x98, %d1
    bne TEST_FAIL
    
    /* Test 3: With adjustment (applied before packing):
       0x0302 + 0x30 = 0x0332 -> pack -> 0x32 */
    move.w #0x0302, %d0
    pack %d0, %d1, #0x30
    cmp.b #0x32, %d1
    bne TEST_FAIL
    
    /* Test 4: adjustment wraps in 16 bits before packing:
       0x3339 + 0xFFCC = 0x3305 -> pack -> 0x35 */
    move.w #0x3339, %d0
    pack %d0, %d1, #0xFFCC
    cmp.b #0x35, %d1
    bne TEST_FAIL
    
    /* Test 5: Pack 0x0000 -> 0x00 */
    move.w #0x0000, %d0
    pack %d0, %d1, #0
    cmp.b #0x00, %d1
    bne TEST_FAIL
    
    rts
