; Sites the loader fixture hooks. Bytes are exact so a plugin can find them
; and so a 14-byte redirect either fits in the leading nops or cannot.
.code

align 16
fixture_hookable proc
	rept 16
	nop
	endm
	mov eax, 7
	ret
fixture_hookable endp

fixture_short proc
	mov eax, 1
	ret
fixture_short endp

align 16
fixture_late proc
	rept 16
	nop
	endm
	mov eax, 3
	ret
fixture_late endp

fixture_unique proc
	db 0F0h, 0C4h, 11h, 22h, 33h, 44h, 55h, 66h, 77h, 88h, 99h, 0AAh, 0BBh, 0CCh
	ret
fixture_unique endp

fixture_ambiguous_a proc
	db 0DEh, 0ADh, 0BEh, 0EFh
	mov eax, 1
	ret
fixture_ambiguous_a endp

fixture_ambiguous_b proc
	db 0DEh, 0ADh, 0BEh, 0EFh
	mov eax, 2
	ret
fixture_ambiguous_b endp

EXTERN fixture_country_monthly:PROC
EXTERN fixture_country_after:PROC
EXTERN fixture_state_body:PROC
EXTERN fixture_active_pred_body:PROC
EXTERN fixture_active_g0:DWORD
EXTERN fixture_active_g1:DWORD
EXTERN fixture_active_g2:DWORD

; Same 23-byte opening as the eu4.exe state function. The host steals the
; first 17 bytes and resumes at mov rdi, rcx. The rest records one update.
align 16
fixture_state_entry proc
	db 040h, 053h
	db 055h
	db 056h
	db 057h
	db 041h, 054h
	db 041h, 055h
	db 041h, 056h
	db 041h, 057h
	db 048h, 083h, 0ECh, 028h
	db 048h, 08Bh, 0F9h
	db 045h, 033h, 0EDh
	sub rsp, 20h
	mov rcx, rdi
	call fixture_state_body
	add rsp, 20h
	add rsp, 28h
	db 041h, 05Fh
	db 041h, 05Eh
	db 041h, 05Dh
	db 041h, 05Ch
	db 05Fh
	db 05Eh
	db 05Dh
	db 05Bh
	ret
fixture_state_entry endp

; Same head as eu4.exe RVA 0x75dd30. r15 is the game object, rbx the first
; index, rsi the end index, r12b the constant r8, r13b the constant dl.
; Offset 0x49 is the instruction the serial loop falls into.
fixture_run_countries proc
	movzx eax, byte ptr [rsp+28h]
	push rbp
	push rbx
	push rsi
	push rdi
	push r12
	push r13
	push r14
	push r15
	mov r13d, eax
	mov r15, rcx
	mov rbx, rdx
	mov rsi, r8
	mov r12, r9
	mov edi, edx
	align 16
fixture_country_head::
	db 049h, 08Bh, 087h, 0E8h, 01Ch, 000h, 000h
	db 045h, 00Fh, 0B6h, 0C4h
	db 041h, 00Fh, 0B6h, 0D5h
	db 048h, 08Bh, 00Ch, 0D8h
	call fixture_country_monthly
	rept 39
	nop
	endm
	inc edi
	inc rbx
	cmp rbx, rsi
	db 07Ch, 0B7h
fixture_country_joined::
	mov r14, qword ptr [r15+1CF0h]
	call fixture_country_after
	mov eax, 1
	pop r15
	pop r14
	pop r13
	pop r12
	pop rdi
	pop rsi
	pop rbx
	pop rbp
	ret
fixture_run_countries endp

fixture_country_site proc
	lea rax, fixture_country_head
	ret
fixture_country_site endp

fixture_country_span proc
	lea rax, fixture_country_joined
	lea rcx, fixture_country_head
	sub rax, rcx
	ret
fixture_country_span endp

; BuildActiveCountries loop head, RVA 0x75ac2b. The 17-byte opening is the
; hook. Offsets 0x6F, 0x157 and 0x1E8 stay put so the plugin can read the
; comparisons, the predicate call and the back-edge. Its own segment starts
; on a page, so the 14-byte redirect does not cross one.
ACTIVE SEGMENT ALIGN(4096) READ EXECUTE
fixture_run_active proc
	mov byte ptr [rsp+10h], dl
	push rbp
	push rsi
	push rdi
	push r12
	push r13
	push r14
	push r15
	sub rsp, 20h
	movzx esi, dl
	mov rbp, rcx
	xor edi, edi
	mov dword ptr [rcx+1D14h], edi
	mov dword ptr [rcx+1D2Ch], edi
	lea r15, [rcx+1D00h]
	lea r13, [rbp+1D18h]
	test sil, sil
	jz fixture_active_head
	mov rax, qword ptr [rbp+1D30h]
	mov qword ptr [rbp+1D38h], rax
fixture_active_head::
	db 044h,08Bh,0E7h,048h,08Bh,08Dh,0E8h,01Ch,000h,000h,048h,08Bh,085h,0F0h,01Ch,000h
	db 000h,048h,02Bh,0C1h,048h,0C1h,0F8h,003h,085h,0C0h,00Fh,08Eh,0CEh,001h,000h,000h
	db 00Fh,01Fh,044h,000h,000h,041h,08Bh,0C4h,048h,08Dh,034h,0C5h,000h,000h,000h,000h
	db 048h,089h,074h,024h,060h,04Ch,08Dh,004h,00Eh,04Dh,08Bh,030h,049h,08Bh,05Eh,020h
	db 048h,08Bh,0CBh,048h,0C1h,0E9h,030h,048h,08Bh,0C3h,048h,0C1h,0E8h,030h,03Ch,007h
	db 00Fh,084h,0CFh,000h,000h,000h,041h,083h,0BEh,044h,013h,000h,000h,000h,075h,035h
	db 080h,0F9h,007h,074h,030h,048h,08Bh,0C3h,048h,0C1h,0E8h,020h,00Fh,0BFh,0C8h
	cmp ecx, dword ptr [fixture_active_g0]
	db 074h, 01Eh
	cmp ecx, dword ptr [fixture_active_g1]
	db 074h, 016h
	cmp ecx, dword ptr [fixture_active_g2]
	db 074h, 00Eh
	db 041h,083h,0BEh,028h,0Dh,000h,000h,000h
	db 00Fh,084h,090h,000h,000h,000h
	mov eax, dword ptr [r13+14h]
	mov rcx, qword ptr [r13+8]
	mov qword ptr [rcx+rax*8], rbx
	inc eax
	mov dword ptr [r13+14h], eax
	inc dword ptr [rbp+1D14h]
	rept 120
	nop
	endm
	db 080h,07Ch,024h,068h,000h
	db 00Fh,084h,09Dh,000h,000h,000h
	db 041h,083h,0BEh,044h,013h,000h,000h,000h
	db 075h,00Bh
	db 041h,080h,07Eh,026h,007h
	db 00Fh,085h,088h,000h,000h,000h
	mov rcx, r14
	rept 15
	nop
	endm
	call fixture_active_pred
	test eax, eax
	db 00Fh,084h,069h,000h,000h,000h
	mov rax, qword ptr [rbp+1D38h]
	mov qword ptr [rax], r14
	add qword ptr [rbp+1D38h], 8
	add edi, 3E8h
	rept 81
	nop
	endm
	db 041h,0FFh,0C4h
	db 048h,08Bh,08Dh,0E8h,01Ch,000h,000h
	db 048h,08Bh,085h,0F0h,01Ch,000h,000h
	db 048h,02Bh,0C1h
	db 048h,0C1h,0F8h,003h
	db 044h,03Bh,0E0h
	db 00Fh,08Ch,037h,0FEh,0FFh,0FFh
	mov eax, edi
	add rsp, 20h
	pop r15
	pop r14
	pop r13
	pop r12
	pop rdi
	pop rsi
	pop rbp
	ret
fixture_run_active endp

align 4
	dd 0C0DEF00Dh
fixture_active_pred proc
	sub rsp, 28h
	call fixture_active_pred_body
	add rsp, 28h
	ret
fixture_active_pred endp

fixture_active_site proc
	lea rax, fixture_active_head
	ret
fixture_active_site endp
ACTIVE ENDS









; Stealable wrappers around the C allocator stubs: the plugin hooks the two
; allocation funnels, so their entries must hold a 14-byte redirect.
EXTERN fixture_daily_alloc:PROC
EXTERN fixture_daily_free:PROC

align 16
fixture_daily_alloc_wrap proc
	rept 16
	nop
	endm
	jmp fixture_daily_alloc
fixture_daily_alloc_wrap endp
public fixture_daily_alloc_wrap

align 16
fixture_daily_free_wrap proc
	rept 16
	nop
	endm
	jmp fixture_daily_free
fixture_daily_free_wrap endp
public fixture_daily_free_wrap

; Loop B stand-in entry. Sixteen single-byte nops give the host a stealable
; window; the jump into the C body stays past that window.
EXTERN fixture_daily_b_body:PROC
align 16
fixture_daily_b proc
	rept 16
	nop
	endm
	jmp fixture_daily_b_body
fixture_daily_b endp
public fixture_daily_b











; Calls fn(world) with stack slack below the return address: the queue
; body writes above its frame, into space its own caller reserves.
.code
da_shim proc
	; Entry RSP is 8 mod 16. Keep the existing 0x210 bytes of caller
	; slack and add eight bytes so RSP is aligned before the normal call.
	sub rsp, 218h
	mov rax, rcx
	mov rcx, rdx
	call rax
	add rsp, 218h
	ret
da_shim endp
public da_shim

; A normal callee sees RSP=8 (mod 16). Only perform the aligned store when
; that contract holds, so the regression can report an incorrect caller.
fixture_daily_stack_probe proc
	mov rax, rsp
	and eax, 0Fh
	cmp eax, 8
	jne stack_probe_done
	pxor xmm0, xmm0
	movaps xmmword ptr [rsp+8], xmm0
stack_probe_done:
	ret
fixture_daily_stack_probe endp
public fixture_daily_stack_probe

; Invoke the actual plugin detour as a mid-function jump, retaining the
; queue header in RAX, its index in RCX, and the live loop count in R11D.
; Both forced passthrough cases must reach the same native continuation.
fixture_daily_fallback_invoke proc
	push rbp
	push rbx
	push rsi
	push rdi
	push r12
	push r13
	push r14
	push r15
	sub rsp, 118h
	mov rbp, rsp
	mov qword ptr [rbp+20h], rsp
	mov qword ptr [rbp+28h], rdx
	mov r10, rcx
	mov rax, rdx
	mov ecx, 1
	mov r11d, 12345678h
	jmp r10
fixture_daily_fallback_return::
	add rsp, 118h
	pop r15
	pop r14
	pop r13
	pop r12
	pop rdi
	pop rsi
	pop rbx
	pop rbp
	ret
fixture_daily_fallback_invoke endp
public fixture_daily_fallback_invoke

fixture_daily_fallback_next proc
	; Check RAX before the real head's dereference, making corruption a
	; deterministic result rather than an unrelated access violation.
	cmp rax, qword ptr [rbp+28h]
	jne fallback_bad_rax
	cmp rsp, qword ptr [rbp+20h]
	jne fallback_bad_rsp
	cmp rcx, 1
	jne fallback_bad_rcx
	cmp r11d, 12345678h
	jne fallback_bad_r11
	; The actual three stolen instructions at the daily loop head.
	mov rdx, qword ptr [rax+8]
	mov r15, qword ptr [rdx+rcx*8]
	mov qword ptr [rbp+0A8h], r15
	cmp qword ptr [rbp+0A8h], 2468h
	jne fallback_bad_load
	xor eax, eax
	jmp fixture_daily_fallback_return
fallback_bad_rax:
	mov eax, 1
	jmp fixture_daily_fallback_return
fallback_bad_rsp:
	mov eax, 2
	jmp fixture_daily_fallback_return
fallback_bad_rcx:
	mov eax, 3
	jmp fixture_daily_fallback_return
fallback_bad_r11:
	mov eax, 4
	jmp fixture_daily_fallback_return
fallback_bad_load:
	mov eax, 5
	jmp fixture_daily_fallback_return
fixture_daily_fallback_next endp
public fixture_daily_fallback_next

; The guard access starts owned single stepping. The first trap at the
; explicit stop label must clear TF before this wrapper returns to C.
fixture_trace_invoke proc
	sub rsp, 38h
	mov qword ptr [rsp+20h], rcx
	mov qword ptr [rsp+28h], rdx
fixture_trace_arm::
	mov al, byte ptr [r8]
	mov rcx, qword ptr [rsp+20h]
	mov rdx, qword ptr [rsp+28h]
	call da_shim
fixture_trace_stop::
	nop
	add rsp, 38h
	ret
fixture_trace_invoke endp
public fixture_trace_invoke
public fixture_trace_arm
public fixture_trace_stop

fixture_trace_work proc
	inc qword ptr [rcx]
	inc qword ptr [rcx]
	inc qword ptr [rcx]
	ret
fixture_trace_work endp
public fixture_trace_work

fixture_trace_flags proc
	pushfq
	pop rax
	ret
fixture_trace_flags endp
public fixture_trace_flags

EXTERN fixture_worker_return:QWORD
EXTERN fixture_worker_result:DWORD
fixture_daily_worker_entry_probe proc
	mov dword ptr [fixture_worker_result], 1
	cmp rcx, qword ptr [rbp+0C8h]
	jne worker_entry_done
	mov dword ptr [fixture_worker_result], 2
	cmp r11d, dword ptr [rbp+168h]
	jne worker_entry_done
	mov dword ptr [fixture_worker_result], 3
	lea rdx, [rbp+1C0h]
	cmp rax, rdx
	jne worker_entry_done
	mov dword ptr [fixture_worker_result], 5
	cmp rax, qword ptr [rbp+0F0h]
	jne worker_entry_done
	mov dword ptr [fixture_worker_result], 4
	; Execute the real entry's load/store shape after validating its inputs.
	mov rdx, qword ptr [rax+8]
	mov r15, qword ptr [rdx+rcx*8]
	mov qword ptr [rbp+0A8h], r15
	cmp r15, 2468h
	jne worker_entry_done
	mov dword ptr [fixture_worker_result], 0
worker_entry_done:
	jmp qword ptr [fixture_worker_return]
fixture_daily_worker_entry_probe endp
public fixture_daily_worker_entry_probe

; Invoke the actual built worker with nonvolatile canaries and a saved RSP.
fixture_daily_worker_frame proc
	push rbp
	push rbx
	push rsi
	push rdi
	push r12
	push r13
	push r14
	push r15
	sub rsp, 38h
	mov qword ptr [rsp+20h], rcx
	mov qword ptr [rsp+28h], rdx
	mov qword ptr [rsp+30h], rsp
	mov ebp, 09090909h
	mov ebx, 11111111h
	mov esi, 22222222h
	mov edi, 33333333h
	mov r12d, 44444444h
	mov r13d, 55555555h
	mov r14d, 66666666h
	mov r15d, 77777777h
	mov rcx, qword ptr [rsp+28h]
	call qword ptr [rsp+20h]
	xor eax, eax
	cmp rbp, 09090909h
	je frame_rbx
	or eax, 1
frame_rbx:
	cmp rbx, 11111111h
	je frame_rsi
	or eax, 2
frame_rsi:
	cmp rsi, 22222222h
	je frame_rdi
	or eax, 4
frame_rdi:
	cmp rdi, 33333333h
	je frame_r12
	or eax, 8
frame_r12:
	cmp r12, 44444444h
	je frame_r13
	or eax, 16
frame_r13:
	cmp r13, 55555555h
	je frame_r14
	or eax, 32
frame_r14:
	cmp r14, 66666666h
	je frame_r15
	or eax, 64
frame_r15:
	cmp r15, 77777777h
	je frame_rsp
	or eax, 128
frame_rsp:
	cmp rsp, qword ptr [rsp+30h]
	je frame_done
	or eax, 256
frame_done:
	add rsp, 38h
	pop r15
	pop r14
	pop r13
	pop r12
	pop rdi
	pop rsi
	pop rbx
	pop rbp
	ret
fixture_daily_worker_frame endp
public fixture_daily_worker_frame

fixture_daily_bad_worker proc
	mov r12d, 0DEADh
	ret
fixture_daily_bad_worker endp
public fixture_daily_bad_worker

EXTERN fixture_heavy_work:PROC
fixture_daily_home_work proc
	mov qword ptr [rsp+8h], 0
	mov qword ptr [rsp+10h], 0
	mov qword ptr [rsp+18h], 0
	mov qword ptr [rsp+20h], 0
	jmp fixture_heavy_work
fixture_daily_home_work endp
public fixture_daily_home_work







; The daily queue-drain stand-in: same head pattern, patch-site shapes and
; offsets as the eu4.exe body at 0x7596a0. The P4 block is the real exe block
; verbatim; its allocator call and four other call sites carry zero rel32
; placeholders until game.c patches them at startup.
align 16
fixture_queue proc
	db 048h,089h,04Ch,024h,008h,055h,053h,056h,057h,041h,054h,041h,055h,041h,056h,041h
	db 057h,048h,08Dh,0ACh,024h,0F8h,0FEh,0FFh,0FFh,048h,081h,0ECh,008h,002h,000h,000h
	db 04Ch,08Bh,0E1h,044h,08Bh,099h,03Ch,01Eh,000h,000h,041h,083h,0EBh,001h,044h,089h
	db 09Dh,068h,001h,000h,000h,00Fh,088h,067h,014h,000h,000h,049h,063h,0CBh,048h,089h
	db 08Dh,0C8h,000h,000h,000h,049h,08Dh,084h,024h,028h,01Eh,000h,000h,048h,089h,085h
	db 0F0h,000h,000h,000h,045h,033h,0C9h,066h,00Fh,01Fh,084h,000h,000h,000h,000h,000h
	db 048h,08Bh,050h,008h,04Ch,08Bh,03Ch,0CAh,04Ch,089h,0BDh,0A8h,000h,000h,000h,04Ch
	db 089h,0BDh,0F8h,000h,000h,000h,04Dh,08Dh,077h,048h,049h,08Bh,046h,008h,049h,02Bh
	db 006h,048h,083h,0F8h,008h,00Fh,083h,0CCh,000h,000h,000h,049h,08Bh,087h,080h,000h
	db 000h,000h,049h,02Bh,047h,078h,048h,083h,0F8h,008h,00Fh,083h,0B7h,000h,000h,000h
	db 045h,08Dh,053h,001h,04Ch,08Dh,049h,001h,049h,03Bh,0C9h,07Dh,04Ah,045h,08Bh,084h
	db 024h,03Ch,01Eh,000h,000h,041h,08Bh,0C0h,041h,02Bh,0C2h,048h,08Dh,00Ch,0CAh,04Eh
	db 08Dh,00Ch,0CAh,048h,063h,0D0h,085h,0C0h,07Eh,01Fh,04Ch,02Bh,0C9h,00Fh,01Fh,000h
	db 049h,08Bh,004h,009h,048h,089h,001h,048h,08Dh,049h,008h,048h,083h,0EAh,001h,075h
	db 0EFh,045h,08Bh,084h,024h,03Ch,01Eh,000h,000h,045h,02Bh,0C2h,045h,003h,0C3h,045h
	db 089h,084h,024h,03Ch,01Eh,000h,000h,049h,08Bh,09Ch,024h,048h,01Eh,000h,000h,0B9h
	db 020h,000h,000h,000h,0E8h,000h,000h,000h,000h,04Ch,089h,038h,048h,089h,058h,008h
	db 045h,033h,0C9h,04Ch,089h,048h,010h,044h,088h,048h,018h,049h,089h,084h,024h,048h
	db 01Eh,000h,000h,041h,08Bh,094h,024h,050h,01Eh,000h,000h,08Dh,04Ah,001h,041h,089h
	db 08Ch,024h,050h,01Eh,000h,000h,044h,08Bh,09Dh,068h,001h,000h,000h,085h,0D2h,074h
	db 009h,048h,089h,043h,010h,0E9h,0D6h,012h,000h,000h,049h,089h,084h,024h,040h,01Eh
	db 000h,000h,0E9h,0C9h,012h,000h,000h,04Ch,089h,0F9h,0E8h,000h,000h,000h,000h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,08Bh,095h,068h,001h,000h,000h,048h,08Bh,08Dh,0F0h,000h,000h,000h,0E8h,000h
	db 000h,000h,000h,090h,090h,090h,090h,090h,090h,090h,049h,08Dh,08Ch,024h,040h,01Eh
	db 000h,000h,048h,08Dh,095h,0F8h,000h,000h,000h,0E8h,000h,000h,000h,000h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 044h,08Bh,09Dh,068h,001h,000h,000h,048h,08Bh,08Dh,0C8h,000h,000h,000h,041h,0FFh
	db 0CBh,044h,089h,09Dh,068h,001h,000h,000h,048h,0FFh,0C9h,048h,089h,08Dh,0C8h,000h
	db 000h,000h,045h,085h,0DBh,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h,090h,090h,078h,00Dh,049h,08Dh,084h,024h,028h,01Eh,000h,000h,0E9h,0BEh,0EBh
	db 0FFh,0FFh,048h,081h,0C4h,008h,002h,000h,000h,041h,05Fh,041h,05Eh,041h,05Dh,041h
	db 05Ch,05Fh,05Eh,05Bh,05Dh,0C3h,090h,090h,090h,090h,090h,090h,090h,090h,090h,090h
	db 090h
fixture_queue endp
public fixture_queue

; Exact monthly body receives its native caller frame and live EDI.
EXTERN fixture_monthly_current:QWORD
EXTERN fixture_monthly_primary_record:PROC
EXTERN fixture_monthly_helper_record:PROC
fixture_monthly_invoke proc
	push rbp
	push rbx
	push rsi
	push rdi
	push r12
	push r13
	push r14
	push r15
	sub rsp, 48h
	mov r14, rcx
	mov r15, [r14+8]
	mov rbx, [r14+16]
	mov rsi, [r14+24]
	mov edi, [r14+32]
	mov rbp, 1111222233334444h
	mov r12, 12121212121212A5h
	mov r13, 131313131313133Ch
	mov rax, 5555666677778888h
	mov [rsp+20h], rax
	pcmpeqd xmm0, xmm0
	movdqa xmm1, xmm0
	movdqa xmm2, xmm0
	movdqa xmm3, xmm0
	movdqa xmm4, xmm0
	movdqa xmm5, xmm0
	mov r9, 9999999999999909h
	jmp qword ptr [r14]
fixture_monthly_invoke endp
public fixture_monthly_invoke
fixture_monthly_join proc
	mov [r14+40], rax
	mov [r14+48], rcx
	mov [r14+56], rdx
	mov [r14+64], r8
	mov [r14+72], r9
	mov [r14+80], r10
	mov [r14+88], r11
	pushfq
	pop rax
	and eax, 8D5h
	mov [r14+96], rax
	mov [r14+104], rdi
	mov [r14+112], rbx
	mov [r14+120], rsi
	mov [r14+128], r12
	mov [r14+136], r13
	mov [r14+144], rbp
	mov [r14+152], r15
	mov rax, [rsp+20h]
	mov [r14+160], rax
	add rsp, 48h
	pop r15
	pop r14
	pop r13
	pop r12
	pop rdi
	pop rsi
	pop rbx
	pop rbp
	ret
fixture_monthly_join endp
public fixture_monthly_join
fixture_monthly_primary_bridge proc
	push rax
	mov rax, fixture_monthly_current
	cmp qword ptr [rax+168], 0
	jne monthly_xmm_saved
	movdqu [rax+168], xmm0
	movdqu [rax+184], xmm1
	movdqu [rax+200], xmm2
	movdqu [rax+216], xmm3
	movdqu [rax+232], xmm4
	movdqu [rax+248], xmm5
monthly_xmm_saved:
	pop rax
	sub rsp, 28h
	call fixture_monthly_primary_record
	add rsp, 28h
	mov r8, 88888888888888A5h
	mov r9, 9999999999999909h
	mov r10, 0AAAAAAAAAAAAAA10h
	mov r11, 0BBBBBBBBBBBBBB11h
	mov rax, 1111111111111101h
	mov rcx, 2222222222222202h
	mov rdx, 3333333333333303h
	ret
fixture_monthly_primary_bridge endp
public fixture_monthly_primary_bridge
fixture_monthly_helper_bridge proc
	push rax
	mov rax, fixture_monthly_current
	mov [rax+264], r8
	mov [rax+272], r10
	mov [rax+280], r11
	mov r10, [rsp+30h]
	mov [rax+288], r10
	lea r10, [rsp+8]
	mov [rax+296], r10
	pop rax
	mov qword ptr [rsp+8], 0
	mov qword ptr [rsp+10h], 0
	mov qword ptr [rsp+18h], 0
	mov qword ptr [rsp+20h], 0
	sub rsp, 28h
	call fixture_monthly_helper_record
	add rsp, 28h
	mov r8, 88888888888888A5h
	mov r9, 9999999999999909h
	mov r10, 0AAAAAAAAAAAAAA10h
	mov r11, 0BBBBBBBBBBBBBB11h
	mov rax, 1111111111111101h
	mov rcx, 2222222222222202h
	mov rdx, 3333333333333303h
	ret
fixture_monthly_helper_bridge endp
public fixture_monthly_helper_bridge

end
