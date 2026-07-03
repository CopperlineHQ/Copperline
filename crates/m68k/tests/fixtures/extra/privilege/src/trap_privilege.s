.include "entry.s"
/* Test: Verify SR.S bit set on TRAP */
/* Note: USP must be initialized before switching to user mode */

run_test:
    clr.l %d0
    
    /* Install TRAP handler */
    lea trap_handler, %a0
    move.l %a0, 0x80            | TRAP #0 vector
    
    /* Install TRAP #1 handler for returning to supervisor */
    lea trap_handler_1, %a0
    move.l %a0, 0x84            | TRAP #1 vector
    
    /* Initialize USP for user mode - use a separate area from SSP */
    lea 0x200, %a0          | Set up user stack at 0x200
    move.l %a0, %usp        | Store to USP (privileged instruction)
    
    /* Switch to user mode */
    andi.w #0xDFFF, %sr
    
    /* Trigger TRAP */
    trap #0
    
    /* After RTE: Now in USER mode with USP as stack */
    /* The TRAP handler already verified supervisor mode */
    /* MOVE from SR is privileged on the 68010+, so the user-mode check
       is done by the TRAP #1 handler on the stacked SR. */
    /* The return address is on SSP, not USP: use TRAP #1 to get back
       to supervisor mode and return. */
    trap #1
    
    /* Shouldn't reach here - trap #1 handler does the return */
    bra TEST_FAIL

trap_handler:
    /* Verify we're in supervisor mode */
    move.w %sr, %d1
    btst #13, %d1
    beq TEST_FAIL           | Should be in supervisor mode
    
    move.l #1, %d0
    rte

/* Handler for TRAP #1 - return to caller in supervisor mode */
trap_handler_1:
    /* Stacked SR is the caller's SR: S bit must be clear (user mode) */
    move.w (%sp), %d1
    btst #13, %d1
    bne TEST_FAIL
    /* Return to caller (main) */
    addq.l #8, %sp          | Pop the exception frame (format 0: 8 bytes)
    rts
