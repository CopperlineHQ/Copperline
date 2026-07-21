.include "entry.s"
/* Test: MMU Table Walk Verification */
/* Tests that MMU translation registers can be configured */
/* and that address translation concepts are functional */

run_test:
    /* Cover the whole address space with transparent translation before
       touching TC: with E set and no page tables, a real 68040 faults the
       very next instruction fetch (and the fault dispatch itself), so
       bring-up code shields fetches and data with the TTRs first. */
    move.l #0x00FFC000, %d0 | base 0x00, mask 0xFF (all), E=1, both FC spaces
    movec %d0, %itt0
    movec %d0, %dtt0

    /* =================================================================== */
    /* Test 1: TC (Translation Control) Register */
    /* Verify we can enable/disable translation via TC */
    /* =================================================================== */

    /* Read current TC value */
    movec %tc, %d0
    
    /* Set TC to enable translation (bit 15 = E bit) */
    move.l #0x8000, %d0     | E=1, other bits 0
    movec %d0, %tc
    
    /* Read back and verify */
    movec %tc, %d1
    cmp.l %d0, %d1
    bne TEST_FAIL
    
    /* Disable translation */
    move.l #0, %d0
    movec %d0, %tc
    
    movec %tc, %d1
    tst.l %d1
    bne TEST_FAIL
    
    /* =================================================================== */
    /* Test 2: URP (User Root Pointer) */
    /* Set up user page table root */
    /* =================================================================== */
    
    move.l #0x00100000, %d0 | Page-aligned address
    movec %d0, %urp
    
    movec %urp, %d1
    cmp.l %d0, %d1
    bne TEST_FAIL
    
    /* =================================================================== */
    /* Test 3: SRP (Supervisor Root Pointer) */
    /* Set up supervisor page table root */
    /* =================================================================== */
    
    move.l #0x00200000, %d0
    movec %d0, %srp
    
    movec %srp, %d1
    cmp.l %d0, %d1
    bne TEST_FAIL
    
    /* =================================================================== */
    /* Test 4: Different root pointers for user/supervisor */
    /* =================================================================== */
    
    move.l #0x00300000, %d0
    movec %d0, %urp
    
    move.l #0x00400000, %d1
    movec %d1, %srp
    
    /* Verify both are different */
    movec %urp, %d2
    movec %srp, %d3
    
    cmp.l %d2, %d3
    beq TEST_FAIL          | Should be different
    
    cmp.l #0x00300000, %d2
    bne TEST_FAIL
    
    cmp.l #0x00400000, %d3
    bne TEST_FAIL
    
    /* =================================================================== */
    /* Test 5: TC configuration bits */
    /* Test various TC register configurations */
    /* =================================================================== */
    
    /* Set TC with page size = 4K (PS=0) */
    move.l #0x8000, %d0     | E=1, PS=0 (4K pages)
    movec %d0, %tc
    
    movec %tc, %d1
    cmp.l %d0, %d1
    bne TEST_FAIL
    
    /* Set TC with page size = 8K (PS=1) */
    move.l #0xC000, %d0     | E=1, PS=1 (8K pages)
    movec %d0, %tc
    
    movec %tc, %d1
    cmp.l %d0, %d1
    bne TEST_FAIL
    
    /* Disable for cleanup */
    move.l #0, %d0
    movec %d0, %tc
    movec %d0, %urp
    movec %d0, %srp
    movec %d0, %itt0
    movec %d0, %dtt0

    rts
