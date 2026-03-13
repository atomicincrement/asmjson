# classify_zmm.s — AVX-512BW JSON chunk classifier in standalone GNU assembly.
#
# Implements the same logic as classify_zmm() in lib.rs.  Assembled via
# global_asm!(include_str!("classify_zmm.s")), which routes it through LLVM's
# integrated assembler.
#
# Calling convention (System V AMD64):
#   ByteState is 32 bytes (4 × u64), classified as MEMORY under the ABI.
#   The caller allocates storage and passes its address as a hidden first
#   parameter.  The actual Rust parameters therefore arrive in:
#     rdi  = *mut ByteState          (hidden sret pointer)
#     rsi  = src.ptr  (*const u8)
#     rdx  = src.len  (usize, 1 ≤ len ≤ 64)
#   On return: rax = rdi (SysV sret contract).

.intel_syntax noprefix

# ---------------------------------------------------------------------------
# Read-only constants: six 64-byte needle vectors, 64-byte aligned.
#   +  0 : 0x20 × 64  — whitespace upper-bound (vpcmpub ≤ 0x20)
#   + 64 : '"'  × 64  — quote
#   +128 : '\\' × 64  — backslash
#   +192 : ','  × 64  — comma
#   +256 : '}'  × 64  — close brace
#   +320 : ']'  × 64  — close bracket
# ---------------------------------------------------------------------------
.section .rodata
.balign 64
classify_zmm_s_constants:
    .fill  64, 1, 0x20   # 0x20 = space  (whitespace upper-bound)
    .fill  64, 1, 0x22   # 0x22 = '"'
    .fill  64, 1, 0x5c   # 0x5c = '\\'
    .fill  64, 1, 0x2c   # 0x2c = ','
    .fill  64, 1, 0x7d   # 0x7d = '}'
    .fill  64, 1, 0x5d   # 0x5d = ']'

# ---------------------------------------------------------------------------
# classify_zmm_s(src_ptr: *const u8, src_len: usize) -> ByteState
# ---------------------------------------------------------------------------
.section .text
.globl classify_zmm_s
.type  classify_zmm_s, @function
classify_zmm_s:
    # Build the load mask: ~0 for a full 64-byte chunk, (1<<len)-1 otherwise.
    # Use a branch; the predicted path is len == 64 in hot loops.
    mov     r8, -1                          # assume full chunk
    cmp     rdx, 64
    je      .Lload
    xor     r9, r9
    bts     r9, rdx                         # r9 = 1 << len  (rdx < 64 guaranteed)
    lea     r8, [r9 - 1]                    # r8 = (1 << len) - 1

.Lload:
    kmovq   k1, r8
    vmovdqu8 zmm0{k1}{z}, zmmword ptr [rsi] # masked load; bytes ≥ len are zeroed

    lea     r9, [rip + classify_zmm_s_constants]

    # Issue all six comparisons into distinct k-registers so the CPU can
    # execute them in parallel; collect GP results as a batch at the end.
    vpcmpub  k2, zmm0, zmmword ptr [r9],        2  # a ≤ 0x20  → whitespace
    vpcmpeqb k3, zmm0, zmmword ptr [r9 +  64]      # a == '"'  → quotes
    vpcmpeqb k4, zmm0, zmmword ptr [r9 + 128]      # a == '\\' → backslashes
    vpcmpeqb k5, zmm0, zmmword ptr [r9 + 192]      # a == ','  → comma
    vpcmpeqb k6, zmm0, zmmword ptr [r9 + 256]      # a == '}'  → close_brace
    vpcmpeqb k7, zmm0, zmmword ptr [r9 + 320]      # a == ']'  → close_bracket

    # delimiters = whitespace | comma | '}' | ']'  — all in k-registers.
    korq    k5, k5, k6
    korq    k5, k5, k7
    korq    k5, k5, k2

    # Store the four 64-bit masks into the caller-allocated ByteState.
    kmovq   rax, k2
    mov     qword ptr [rdi],      rax       # whitespace
    kmovq   rax, k3
    mov     qword ptr [rdi +  8], rax       # quotes
    kmovq   rax, k4
    mov     qword ptr [rdi + 16], rax       # backslashes
    kmovq   rax, k5
    mov     qword ptr [rdi + 24], rax       # delimiters

    mov     rax, rdi                        # SysV sret: return the hidden pointer
    ret

.size classify_zmm_s, . - classify_zmm_s
