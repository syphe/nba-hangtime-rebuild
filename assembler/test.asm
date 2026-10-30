; test.asm - Comprehensive Test Suite with Macros, Bitwise & Control Flow
.include "hardware.asm"

; Define a test macro using parameters
ADD_VAL .macro REG, INCREMENT
    MOVE #INCREMENT, A2
    ADD A2, REG
.endm

INIT:
    NOP
    MOVE #PORT_JOYSTICK, A0    ; Resolves .equ PORT_JOYSTICK -> $0100
    MOVE #MAX_LOOPS, A1        ; Resolves .equ MAX_LOOPS -> 10
    
    ; Invoke our macro
    ADD_VAL A0, 5

MAIN_LOOP:
    ADD #1, A0                 ; Increment counter
    SLL #SHIFT_AMOUNT, A0      ; Resolves .equ SHIFT_AMOUNT -> 2 for scaling
    
    ; Bitwise Operation Tests
    AND #$0FFF, A0             ; Immediate AND mask
    OR A1, A2                  ; Register-to-register OR (A1 OR A2 -> A2)
    XOR #$F0F0, A1             ; Immediate XOR

    SUB #1, A1                 ; Decrement loop threshold
    JNE MAIN_LOOP              ; Loop back if not zero
    
    CALL COMPARE_ROUTINE       ; Call our comparison test routine
    NOP

COMPARE_ROUTINE:
    NOP
    CMP #5, A1                 ; Compare immediate value 5 with A1
    JGT MAIN_LOOP              ; Signed conditional jump if greater than
    JLT SKIP_CHECK             ; Signed conditional jump if less than
    JMP RENDER_FRAME           ; Unconditional jump to frame render block
    
SKIP_CHECK:
    NOP
    RETS                       ; Subroutine return (replaces drop-through)

RENDER_FRAME:
    NOP
    MOVE @SCREEN_WIDTH, A5     ; Direct Memory / Absolute Addressing (@label)
    .WORD SCREEN_WIDTH         ; Data directive
    .WORD SCREEN_HEIGHT        ; Data directive
    .WORD $BEEF                ; Hex marker word