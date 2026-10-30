; perturb_test.asm - PC-tracking regression test
; Adds extra .equ directives (which emit no bytes) so that a pass-2 PC
; counter that advances +2 per line diverges from pass-1 addresses.
; With the old flat +2 accounting, JNE/CALL offsets decode to the wrong
; targets. With correct per-instruction sizing, they must still pass.
.include "hardware.asm"

; Two extra equates: no bytes emitted, so they must not move the PC.
.equ EXTRA_CONST_A, 7
.equ EXTRA_CONST_B, 9

INIT:
    NOP
    MOVE #PORT_JOYSTICK, A0
    MOVE #MAX_LOOPS, A1

MAIN_LOOP:
    ADD #1, A0
    SLL #SHIFT_AMOUNT, A0
    SUB #1, A1
    JNE MAIN_LOOP              ; must land on MAIN_LOOP
    CALL RENDER_FRAME          ; must land on RENDER_FRAME
    NOP

RENDER_FRAME:
    NOP
    .WORD SCREEN_WIDTH
    .WORD SCREEN_HEIGHT
    .WORD $BEEF
