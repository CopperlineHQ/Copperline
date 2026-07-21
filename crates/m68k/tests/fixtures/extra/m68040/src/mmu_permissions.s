.include "entry.s"
/* Test: MMU Access Permission Checks */
/* Tests that MMU blocks writes to read-only pages and access to supervisor pages */
/* Currently expected to FAIL until access permissions are implemented */

run_test:
    /* For now, just verify basic PMOVE and TC manipulation works */
    /* Full permission tests require working MMU table setup */
    
    /* Cover the whole address space with transparent translation before
       touching TC: with E set and no page tables, a real 68040 faults the
       very next instruction fetch, so bring-up code shields itself with
       the TTRs first. */
    move.l #0x00FFC000, %d0     | base 0x00, mask 0xFF (all), E=1, both FCs
    movec %d0, %itt0
    movec %d0, %dtt0

    /* =================================================================== */
    /* Test 1: Enable MMU via TC register */
    /* =================================================================== */
    /* Set up a simple translation control register */
    /* For 68040: TC bit 15 (E) enables translation */
    move.l #0x00008000, %d0     | E=1 (enable), other bits default
    movec %d0, %tc              | Write to TC

    /* Read back and verify */
    movec %tc, %d1
    cmp.l %d0, %d1
    bne TEST_FAIL

    /* Disable MMU before continuing */
    move.l #0, %d0
    movec %d0, %tc
    movec %d0, %itt0
    movec %d0, %dtt0
    
    /* =================================================================== */
    /* Test 2: Verify URP/SRP registers work */
    /* =================================================================== */
    move.l #0x12340000, %d0     | Page table pointer
    movec %d0, %urp             | User Root Pointer
    movec %urp, %d1
    cmp.l %d0, %d1
    bne TEST_FAIL
    
    move.l #0x56780000, %d0
    movec %d0, %srp             | Supervisor Root Pointer
    movec %srp, %d1
    cmp.l %d0, %d1
    bne TEST_FAIL
    
    rts
