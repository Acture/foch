; First entry splits the country loop and jumps to site+0x49.
; A nested entry, while that split is still running, uses the original bytes.
.code

EXTERN country_split:PROC
EXTERN active_takeover:PROC

country_detour proc
	pushfq
	push rax
	mov eax, 1
	xchg dword ptr [country_splitting], eax
	test eax, eax
	jnz passthrough
	push rcx
	push rdx
	push r8
	push r9
	push r10
	push r11
	push rbp
	mov rbp, rsp
	and rsp, 0FFFFFFFFFFFFFFF0h
	; Own 32-byte home plus a 16-aligned 512-byte legacy FP/SIMD save.
	; REX.W FXSAVE64/FXRSTOR64 retain full x87 pointers, MXCSR, XMM0-15.
	sub rsp, 220h
	db 048h,00Fh,0AEh,044h,024h,020h
	mov rcx, r15
	mov rdx, rbx
	mov r8, rsi
	call country_split
	db 048h,00Fh,0AEh,04Ch,024h,020h
	mov rsp, rbp
	pop rbp
	pop r11
	pop r10
	pop r9
	pop r8
	pop rdx
	pop rcx
	pop rax
	popfq
	; The original complete loop sees its original RSP, home and live EDI.
	jmp qword ptr [country_body]
passthrough:
	pop rax
	popfq
	mov rax, qword ptr [country_next]
	jmp qword ptr [rax]
country_detour endp

country_replay_done proc
	mov dword ptr [states_done], 0
	mov dword ptr [country_splitting], 0
	jmp qword ptr [country_after]
country_replay_done endp
public country_replay_done

; Entered by the call that used to enter the state function. A set flag
; returns to that caller. Otherwise the stolen prologue runs.
state_detour proc
	cmp dword ptr [states_done], 0
	jne state_skip
	mov rax, qword ptr [state_next]
	mov rax, qword ptr [rax]
	jmp rax
state_skip:
	ret
state_detour endp

; First entry owns the active-country loop and jumps to the instruction
; after its back-edge. A nested entry uses the stolen prologue.
; Real mode then runs the relocated loop body. That body is entered by a
; jump, so rcx must already be the country array and rsp stays the game rsp:
; the flag lives at [rsp+68h]. active_invoke is the same frame for the probe.
active_detour proc
	mov eax, 1
	xchg dword ptr [active_running], eax
	test eax, eax
	jnz active_pass
	push rbp
	mov rax, rbp
	mov rbp, rsp
	and rsp, 0FFFFFFFFFFFFFFF0h
	sub rsp, 30h
	mov rcx, rax
	mov edx, edi
	movzx r8d, sil
	call active_takeover
	mov edi, eax
	mov rsp, rbp
	pop rbp
	cmp dword ptr [active_enter_body], 0
	je active_done
	mov r12d, edi
	mov rcx, qword ptr [rbp+1CE8h]
	jmp qword ptr [active_body]
active_done:
	mov dword ptr [active_running], 0
	jmp qword ptr [active_after]
active_pass:
	mov rax, qword ptr [active_next]
	mov rax, qword ptr [rax]
	jmp rax
active_detour endp

active_invoke proc
	mov r11, rcx
	mov qword ptr [rsp+18h], rbx
	mov byte ptr [rsp+10h], r8b
	push rbp
	push rsi
	push rdi
	push r12
	push r13
	push r14
	push r15
	sub rsp, 20h
	mov rbp, rdx
	xor edi, edi
	lea r15, [rdx+1D00h]
	lea r13, [rbp+1D18h]
	xor r12d, r12d
	mov rcx, qword ptr [rbp+1CE8h]
	jmp r11
active_invoke endp

; Entered by a jump from the relocated loop, with the game rsp still in place.
active_body_return proc
	mov edi, eax
	mov dword ptr [active_running], 0
	jmp qword ptr [active_after]
active_body_return endp

; Entered by a jump from the probe loop, with active_invoke's frame still in place.
active_invoke_back proc
	mov rbx, qword ptr [rsp+70h]
	add rsp, 20h
	pop r15
	pop r14
	pop r13
	pop r12
	pop rdi
	pop rsi
	pop rbp
	ret
active_invoke_back endp

; ===== Daily loop A (queue drain at 0x7596a0) =====
; First entry dispatches the slices and jumps to the function epilogue;
; a nested entry runs the stolen prologue through the chain.
EXTRN daily_a_dispatch:PROC
EXTRN daily_a_route_alloc:PROC
EXTRN daily_a_route_free:PROC
EXTRN daily_a_finish_fallback:PROC
public daily_a_worker_ret

daily_a_detour proc
	; The native head consumes incoming RAX as its queue-header pointer.
	push rax
	mov eax, 1
	xchg dword ptr [daily_a_running], eax
	test eax, eax
	jnz daily_a_pass
	cmp dword ptr [daily_a_ready], 0
	je daily_a_unready
	push rcx
	push r11
	push rbp
	mov rbp, rsp
	; Save every live native-head input before the fallible preflight call.
	and rsp, 0FFFFFFFFFFFFFFF0h
	sub rsp, 20h
	mov rcx, r12
	call daily_a_dispatch
	test eax, eax
	mov rsp, rbp
	pop rbp
	pop r11
	pop rcx
	pop rax
	jz daily_a_failed
	mov dword ptr [daily_a_running], 0
	jmp qword ptr [daily_a_after]
daily_a_failed:
	; Hold ownership for this native invocation until its epilogue hook.
	mov qword ptr [daily_a_fallback_frame], rsp
	jmp daily_a_native
daily_a_unready:
	; Only this path acquired ownership. A nested entry must leave the
	; outer dispatch's running flag set while it replays the native head.
	mov dword ptr [daily_a_running], 0
daily_a_pass:
	pop rax
daily_a_native:
	; RDX is overwritten by the native head's first mov rdx,[rax+8].
	; Keep RCX (index) and R11D (live loop count) intact through the chain.
	mov rdx, qword ptr [daily_a_next]
	jmp qword ptr [rdx]
daily_a_detour endp

daily_a_end_detour proc
	push rax
	sub rsp, 28h
	lea rcx, [rsp+30h]
	call daily_a_finish_fallback
	add rsp, 28h
	pop rax
	mov rdx, qword ptr [daily_a_end_next]
	jmp qword ptr [rdx]
daily_a_end_detour endp

; The allocator funnels, hooked process-wide. The routing only diverts
; worker threads, so every other caller reaches the original immediately.
daily_a_alloc_detour proc
	sub rsp, 28h
	call daily_a_route_alloc
	add rsp, 28h
	ret
daily_a_alloc_detour endp

daily_a_free_detour proc
	sub rsp, 28h
	call daily_a_route_free
	add rsp, 28h
	ret
daily_a_free_detour endp

; One worker through the relocated copy. rcx = struct daily_a_part*.
; Field offsets must match struct daily_a_part in country_month.c.
; rbp points 0x300 into the shadow block so the frame slots the body
; addresses (rbp-0x80 .. rbp+0x1d4) stay inside it; rsp keeps using the
; pool thread's own stack, with 32 bytes of callee home space plus alignment.
daily_a_worker proc
	push rbp
	push rbx
	push rsi
	push rdi
	push r12
	push r13
	push r14
	push r15
	sub rsp, 28h
	mov r10, qword ptr [rcx+10h]	; copy
	mov r12, qword ptr [rcx+18h]	; game
	mov rbp, qword ptr [rcx+08h]	; shadow base
	add rbp, 300h
	mov rax, qword ptr [rcx+20h]	; inclusive lower index, used by the copy's back-edge
	mov qword ptr [rbp+1E0h], rax
	mov rax, qword ptr [rcx+28h]	; to, one past the highest index
	lea rax, [rax-1]
	mov qword ptr [rbp+168h], rax
	mov qword ptr [rbp+0C8h], rax
	; Enter the native head with its live index/count, after the last part load.
	mov rcx, rax
	mov r11d, eax
	lea rax, [rbp+1C0h]
	; The shift call reloads the same native queue header from its frame slot.
	mov qword ptr [rbp+0F0h], rax
	jmp r10
daily_a_worker_ret::
	add rsp, 28h
	pop r15
	pop r14
	pop r13
	pop r12
	pop rdi
	pop rsi
	pop rbx
	pop rbp
	ret
daily_a_worker endp

.data
public daily_a_ready
public daily_a_after
public daily_a_next
public daily_a_running
public daily_a_alloc_next
public daily_a_free_next
align 8
daily_a_ready dd 0
align 8
daily_a_after dq 0
daily_a_next dq 0
daily_a_running dd 0
align 8
daily_a_alloc_next dq 0
daily_a_free_next dq 0
public daily_a_end_next
public daily_a_fallback_frame
daily_a_end_next dq 0
daily_a_fallback_frame dq 0

public country_site
public country_after
public country_body
public country_next
public country_splitting
public state_next
public states_done
public active_site
public active_after
public active_next
public active_running
public active_body
public active_enter_body
align 8
country_site dq 0
country_after dq 0
country_body dq 0
country_next dq 0
country_splitting dd 0
align 8
state_next dq 0
states_done dd 0
align 8
active_site dq 0
active_after dq 0
active_next dq 0
active_body dq 0
active_running dd 0
active_enter_body dd 0

end
