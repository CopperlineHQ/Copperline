.include "entry.s"
/* Test: Verify RTE restores SR correctly */
/* Note: MOVE from SR is privileged on the 68010+, so the user-mode leg
   captures CCR (unprivileged MOVE from CCR) and a TRAP #1 handler verifies
   the stacked SR and returns to the supervisor entry code. */

run_test:
    clr.l %d0

    /* Install TRAP handlers */
    lea trap_handler, %a0
    move.l %a0, 0x80        | TRAP #0 vector
    lea verify_handler, %a0
    move.l %a0, 0x84        | TRAP #1 vector

    /* Initialize USP before entering user mode */
    lea 0x200, %a0
    move.l %a0, %usp

    /* Enter user mode with all CCR flags set */
    move.w #0x001F, %sr

    /* Trigger TRAP; the handler trashes SR, RTE must restore it */
    trap #0

    /* Back in user mode: capture the restored CCR, then re-enter
       supervisor mode so the checks and the final rts are legal */
    move.w %ccr, %d1
    trap #1
    bra TEST_FAIL           | Shouldn't reach here

trap_handler:
    /* Modify SR in the handler; RTE must restore the stacked value */
    move.w #0x2700, %sr
    move.l #1, %d0
    rte

verify_handler:
    /* The TRAP #0 handler must have run */
    cmp.l #1, %d0
    bne TEST_FAIL

    /* CCR captured after RTE must be the value RTE restored */
    andi.w #0x001F, %d1
    cmp.w #0x001F, %d1
    bne TEST_FAIL

    /* Stacked SR: S bit must be clear (RTE returned to user mode) */
    move.w (%sp), %d2
    btst #13, %d2
    bne TEST_FAIL

    /* Pop the exception frame (format 0: 8 bytes) and return to main */
    addq.l #8, %sp
    rts
