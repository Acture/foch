; Ordinary Windows x64 functions: RCX and RDX are arguments, RAX is the
; result. The 17-byte function uses the caller-provided 32-byte home area.
.code
fixture_inline14 proc
	rept 11
	nop
	endm
	mov rax, rcx
	add rax, rdx
	ret
fixture_inline14 endp

fixture_inline15 proc
	rept 12
	nop
	endm
	mov rax, rcx
	add rax, rdx
	ret
fixture_inline15 endp

fixture_inline17 proc
	mov qword ptr [rsp+8], rcx
	rept 7
	nop
	endm
	mov rax, qword ptr [rsp+8]
	add rax, rdx
	ret
fixture_inline17 endp
end
