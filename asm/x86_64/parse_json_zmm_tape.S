// parse_json_zmm_tape.s
// Hand-written x86-64 (AVX-512BW) assembly translation of parse_json_impl.
//
// This variant writes TapeEntry values **directly** into a pre-allocated array
// instead of calling back through a C vtable.  The pre-allocation
//   tape.len() >= src.len() + 2
// guarantees that the tape can never overflow (valid JSON cannot produce more
// tokens than source bytes + 2).
//
// ---------------------------------------------------------------------------
// Extern C signature
// ---------------------------------------------------------------------------
//
//   bool parse_json_zmm_tape(
//       const uint8_t *src_ptr,       // rdi  — pointer to source bytes
//       size_t         src_len,       // rsi  — byte count
//       TapeEntry     *tape_ptr,      // rdx  — pre-allocated TapeEntry array
//       size_t        *tape_len_out,  // rcx  — receives token count on success
//       uint8_t       *frames_buf,    // r8   — [FrameKind; 64], u8 per entry
//       uint64_t      *open_buf,      // r9   — [usize; 64] backfill index stack
//       void          *unescape_buf,  // [rsp+8]  — *mut String for escape decoding
//       bool          *has_escapes_out // [rsp+16] — written to 1 if any EscapedString/EscapedKey
//   );
//   Returns: al = 1 on success, 0 on error.
//
// FrameKind encoding: 0 = Object, 1 = Array  (#[repr(u8)])
//
// ---------------------------------------------------------------------------
// TapeEntry layout  (#[repr(C)], 16 bytes)
// ---------------------------------------------------------------------------
//
//   offset  0 (u64)  tag_payload:  bits 63-60 = TapeEntryKind, bits 27-0 = payload
//   offset  8 (u64)  ptr:          pointer to string bytes, or null
//
// TapeEntryKind values (#[repr(u8)], fixed ABI):
//   Null=0, Bool=1, Number=2, String=3, EscapedString=4,
//   Key=5, EscapedKey=6, StartObject=7, EndObject=8, StartArray=9, EndArray=10
//
// kind << KIND_SHIFT (60) lives in bits 63-60 of tag_payload.
// payload lives in bits 59-0; masked by shl/shr 4 to clear the top 4 kind bits.
//
// ---------------------------------------------------------------------------
// Caller-provided symbols (from Rust lib with C linkage)
// ---------------------------------------------------------------------------
//
//   void unescape_str(s_ptr, s_len, out_string_ptr)
//     Fills *out_string_ptr (a *mut String) with the decoded text.
//     Declared:  #[unsafe(no_mangle)] #[inline(never)]
//                → (rdi=s_ptr, rsi=s_len, rdx=out_string_ptr)
//
//   bool is_valid_json_number_c(ptr, len)
//     Returns 1 if bytes[..len] form a valid JSON number, else 0.
//     → (rdi=ptr, rsi=len)
//
//   void tape_take_box_str(unescape_buf, out_ptr, out_len)
//     Moves the decoded string out of *unescape_buf into a Box<str>,
//     writing its data pointer and length to *out_ptr / *out_len.
//     → (rdi=unescape_buf, rsi=out_ptr, rdx=out_len)
//
// ---------------------------------------------------------------------------
// Register assignments (persistent across the entire function)
// ---------------------------------------------------------------------------
//
//   rbx  = tape_len    (callee-saved; current number of written TapeEntry values)
//   r12  = src_base    (callee-saved; pointer to byte 0 of source)
//   r13  = src_end     (callee-saved; src_base + src_len, one-past-end)
//   r14  = tape_ptr    (callee-saved; base of the TapeEntry array)
//   r15  = frames_buf  (callee-saved; &[u8; 64], one byte per nesting level)
//
// Inner-loop register:
//   rcx  = chunk_offset (0..chunk_len, current bit index within the chunk)
//
// State-return registers (reloaded before every .Lchunk_fetch jump):
//   r10  = address of target state
//   r11  = address of EOF handler
//
// Scratch: rax, rdx, rsi, rdi, r8, r9  (all caller-saved)
//
// ---------------------------------------------------------------------------
// Stack frame layout  (rbp-relative)
// ---------------------------------------------------------------------------
//
// At function entry the call pushes the return address (-8 from caller rsp).
// Prologue: push rbp; mov rbp,rsp; then five callee-saved pushes (rbx…r15).
// After five pushes  rsp = rbp - 40.  sub rsp, 152  →  rsp = rbp - 192.
// 192 % 16 == 0  ✓  (required for ABI-aligned calls)
//
//   [rbp +24]  has_escapes_out ptr      (8th arg, caller's stack slot)
//   [rbp +16]  unescape_buf ptr         (7th arg, caller's stack slot)
//   [rbp + 8]  return address
//   [rbp + 0]  saved rbp
//   [rbp - 8]  saved rbx
//   [rbp -16]  saved r12
//   [rbp -24]  saved r13
//   [rbp -32]  saved r14
//   [rbp -40]  saved r15
//   [rbp - 48]  unescape_buf ptr         (copy of [rbp+16] for convenience)
//   [rbp - 56]  frames_depth             (usize; 0..64)
//   [rbp - 64]  pos                      (usize; bytes consumed so far)
//   [rbp - 72]  str_start                (usize; byte offset after opening '"')
//   [rbp - 80]  atom_start               (usize; byte offset of first atom byte)
//   [rbp - 88]  str_escaped              (u8;  0 or 1)
//   [rbp - 96]  key_raw_ptr              (*const u8; borrows source)
//   [rbp -104]  key_raw_len              (usize)
//   [rbp -112]  key_escaped              (u8;  0 or 1)
//   [rbp -120]  after_comma              (u8;  0 or 1)
//   [rbp -128]  chunk_len                (usize; ≤ 64)
//   [rbp -136]  bs_whitespace            (u64)
//   [rbp -144]  bs_quotes                (u64)
//   [rbp -152]  bs_backslashes           (u64)
//   [rbp -160]  bs_delimiters            (u64)
//   [rbp -168]  saved chunk_offset       (rcx saved around external calls)
//   [rbp -176]  tape_len_out pointer     (rcx arg saved here)
//   [rbp -184]  open_buf pointer         (r9 arg saved here)
//   [rbp -192]  open_depth               (usize; depth of the backfill stack)

.intel_syntax noprefix

// Stack-frame byte offsets from rbp
.equ LOC_UNESCAPE,    -48
.equ LOC_FDEPTH,      -56
.equ LOC_POS,         -64
.equ LOC_STR_START,   -72
.equ LOC_ATOM_START,  -80
.equ LOC_STR_ESC,     -88
.equ LOC_KEY_PTR,     -96
.equ LOC_KEY_LEN,     -104
.equ LOC_KEY_ESC,     -112
.equ LOC_AFT_COMMA,   -120
.equ LOC_CHUNK_LEN,   -128
.equ LOC_WS,          -136
.equ LOC_QUOTES,      -144
.equ LOC_BSL,         -152
.equ LOC_DELIMS,      -160
.equ LOC_COFF,        -168     // saved chunk_offset around external calls
.equ LOC_TLEN_OUT,    -176     // tape_len_out pointer
.equ LOC_OPEN_BUF,    -184     // open_buf pointer
.equ LOC_OPEN_DEPTH,  -192     // current open-stack depth
.equ LOC_HAS_ESC_OUT, +24      // has_escapes_out pointer (8th arg, caller's stack)

// TapeEntryKind discriminants (bits 63-60 of tag_payload)
.equ TAPE_KIND_NULL,           0
.equ TAPE_KIND_BOOL,           1
.equ TAPE_KIND_NUMBER,         2
.equ TAPE_KIND_STRING,         3
.equ TAPE_KIND_ESCAPED_STRING, 4
.equ TAPE_KIND_KEY,            5
.equ TAPE_KIND_ESCAPED_KEY,    6
.equ TAPE_KIND_START_OBJECT,   7
.equ TAPE_KIND_END_OBJECT,     8
.equ TAPE_KIND_START_ARRAY,    9
.equ TAPE_KIND_END_ARRAY,      10

// Bit-field constants
.equ KIND_SHIFT,     60
// PAYLOAD_MASK: clear top 4 bits via  shl r10, 4 / shr r10, 4  (avoids 64-bit imm)

// MAX_JSON_DEPTH
.equ MAX_JSON_DEPTH, 64

// FrameKind values (#[repr(u8)])
.equ FRAME_OBJECT, 0
.equ FRAME_ARRAY,  1

.section .rodata
.align 64
// Local copy of the classification constants (one 64-byte lane each).
.Lzmm_space:
    .fill 64, 1, 0x20   // ' '  — threshold for whitespace (unsigned <=)
.Lzmm_quote:
    .fill 64, 1, 0x22   // '"'
.Lzmm_backslash:
    .fill 64, 1, 0x5C   // '\\'
.Lzmm_comma:
    .fill 64, 1, 0x2C   // ','
.Lzmm_close_brace:
    .fill 64, 1, 0x7D   // '}'
.Lzmm_close_bracket:
    .fill 64, 1, 0x5D   // ']'

.text
.global parse_json_zmm_tape
.type   parse_json_zmm_tape, @function

// ---------------------------------------------------------------------------
// parse_json_zmm_tape — entry point
// ---------------------------------------------------------------------------
parse_json_zmm_tape:
    // Prologue: save frame pointer and callee-saved registers.
    push    rbp
    mov     rbp, rsp
    push    rbx
    push    r12
    push    r13
    push    r14
    push    r15
    sub     rsp, 152        // local variables; rsp now 16-byte aligned

    // Stash incoming arguments into callee-saved / stack slots.
    mov     r12, rdi                                    // src_base
    mov     r13, rdi
    add     r13, rsi                                    // src_end = src_base + src_len
    mov     r14, rdx                                    // tape_ptr
    mov     qword ptr [rbp + LOC_TLEN_OUT], rcx         // tape_len_out
    mov     r15, r8                                     // frames_buf
    mov     qword ptr [rbp + LOC_OPEN_BUF],  r9         // open_buf
    // unescape_buf is the 7th argument, passed on the caller's stack at [rbp+16].
    mov     rax, qword ptr [rbp + 16]
    mov     qword ptr [rbp + LOC_UNESCAPE], rax

    // Initialise local variables.
    xor     eax, eax
    xor     ebx, ebx                                    // tape_len = 0
    mov     qword ptr [rbp + LOC_FDEPTH],    rax        // frames_depth = 0
    mov     qword ptr [rbp + LOC_POS],       rax        // pos = 0
    mov     qword ptr [rbp + LOC_STR_START], rax
    mov     qword ptr [rbp + LOC_ATOM_START],rax
    mov     byte  ptr [rbp + LOC_STR_ESC],   al         // str_escaped = false
    mov     qword ptr [rbp + LOC_KEY_PTR],   rax
    mov     qword ptr [rbp + LOC_KEY_LEN],   rax
    mov     byte  ptr [rbp + LOC_KEY_ESC],   al         // key_escaped = false
    mov     byte  ptr [rbp + LOC_AFT_COMMA], al         // after_comma = false
    mov     qword ptr [rbp + LOC_CHUNK_LEN], rax        // chunk_len = 0 (first iteration)
    mov     qword ptr [rbp + LOC_OPEN_DEPTH],rax        // open_depth = 0

    // First state is ValueWhitespace; set up r10/r11 and fall into chunk_fetch.
    lea     r10, [rip + .Lvalue_whitespace]
    lea     r11, [rip + .Leof_after_value]   // treat EOF at top-level as success
    jmp     .Lchunk_fetch

// ===========================================================================
// .Lchunk_fetch  — outer loop: advance pos, classify next 64-byte chunk
// ===========================================================================
.Lchunk_fetch:
    // Advance pos by chunk_len (0 on first entry since stack is zeroed).
    mov     rax, qword ptr [rbp + LOC_CHUNK_LEN]
    add     qword ptr [rbp + LOC_POS], rax

    // Compute bytes remaining.
    mov     rax, qword ptr [rbp + LOC_POS]
    lea     rdx, [r12 + rax]        // rdx = src_base + pos  (current chunk start)
    cmp     rdx, r13
    jae     .Lchunk_eof             // pos >= src_len → handle end of source

    // chunk_len = min(64, src_end - chunk_ptr)
    mov     rsi, r13
    sub     rsi, rdx                // rsi = src_len - pos  (bytes left)
    cmp     rsi, 64
    jbe     .Lclassify_partial      // fewer than 64 bytes → use masked load
    mov     rsi, 64
.Lclassify_partial:
    mov     qword ptr [rbp + LOC_CHUNK_LEN], rsi

    // -----------------------------------------------------------------------
    // Inline AVX-512BW classification
    // -----------------------------------------------------------------------
    cmp     rsi, 64
    je      .Lclassify_full
    mov     ecx, esi
    mov     rax, 1
    shl     rax, cl
    dec     rax
    jmp     .Lclassify_do
.Lclassify_full:
    mov     rax, -1
.Lclassify_do:
    lea     r9, [rip + .Lzmm_space]
    kmovq   k1, rax
    vmovdqu8 zmm0{k1}{z}, zmmword ptr [rdx]

    vpcmpub  k2, zmm0, zmmword ptr [r9       ], 2   // whitespace: byte <= 0x20
    vpcmpeqb k3, zmm0, zmmword ptr [r9 +  64]       // quotes
    vpcmpeqb k4, zmm0, zmmword ptr [r9 + 128]       // backslashes
    vpcmpeqb k5, zmm0, zmmword ptr [r9 + 192]       // comma
    vpcmpeqb k6, zmm0, zmmword ptr [r9 + 256]       // '}'
    vpcmpeqb k7, zmm0, zmmword ptr [r9 + 320]       // ']'

    korq    k5, k5, k6
    korq    k5, k5, k7
    korq    k5, k5, k2

    kmovq   rax, k2
    mov     qword ptr [rbp + LOC_WS],     rax
    kmovq   rax, k3
    mov     qword ptr [rbp + LOC_QUOTES], rax
    kmovq   rax, k4
    mov     qword ptr [rbp + LOC_BSL],    rax
    kmovq   rax, k5
    mov     qword ptr [rbp + LOC_DELIMS], rax

    xor     ecx, ecx            // chunk_offset = 0
    jmp     r10                 // direct-thread: resume current state

// ---------------------------------------------------------------------------
// .Lchunk_eof — src exhausted; handle based on current state (via r11)
// ---------------------------------------------------------------------------
.Lchunk_eof:
    jmp     r11

// ===========================================================================
// STATE: value_whitespace
// ===========================================================================
.Lvalue_whitespace:
    mov     rax, qword ptr [rbp + LOC_WS]
    not     rax
    shr     rax, cl
    tzcnt   rax, rax
    add     rcx, rax
    cmp     rcx, qword ptr [rbp + LOC_CHUNK_LEN]
    jae     .Lrefetch_value_whitespace

    mov     rax, qword ptr [rbp + LOC_POS]
    add     rax, r12
    movzx   eax, byte ptr [rax + rcx]
    cmp     al, '{'
    je      .Lvw_open_object
    cmp     al, '['
    je      .Lvw_open_array
    cmp     al, '"'
    je      .Lvw_open_string
    // else: atom
    mov     rdx, qword ptr [rbp + LOC_POS]
    add     rdx, rcx
    mov     qword ptr [rbp + LOC_ATOM_START], rdx
    inc     rcx
    lea     r11, [rip + .Latom_eof_flush]
    cmp     rcx, qword ptr [rbp + LOC_CHUNK_LEN]
    jb      .Latom_chars
    lea     r10, [rip + .Latom_chars]
    jmp     .Lchunk_fetch

.Lvw_open_object:
    mov     rax, qword ptr [rbp + LOC_FDEPTH]
    cmp     rax, MAX_JSON_DEPTH
    jae     .Lerror
    mov     byte ptr [r15 + rax], FRAME_OBJECT
    inc     qword ptr [rbp + LOC_FDEPTH]
    // Push tape_len to open_buf[open_depth]; open_depth++
    mov     r10, qword ptr [rbp + LOC_OPEN_BUF]
    mov     r11, qword ptr [rbp + LOC_OPEN_DEPTH]
    mov     qword ptr [r10 + r11*8], rbx
    inc     qword ptr [rbp + LOC_OPEN_DEPTH]
    // Write StartObject entry (payload backfilled on close)
    lea     rax, [rbx + rbx]
    lea     rax, [r14 + rax*8]
    mov     r10, TAPE_KIND_START_OBJECT
    shl     r10, KIND_SHIFT
    mov     qword ptr [rax], r10
    mov     qword ptr [rax + 8], 0
    inc     rbx
    inc     rcx
    lea     r11, [rip + .Lerror_from_r11]
    cmp     rcx, qword ptr [rbp + LOC_CHUNK_LEN]
    jb      .Lobject_start
    lea     r10, [rip + .Lobject_start]
    jmp     .Lchunk_fetch

.Lvw_open_array:
    mov     rax, qword ptr [rbp + LOC_FDEPTH]
    cmp     rax, MAX_JSON_DEPTH
    jae     .Lerror
    mov     byte ptr [r15 + rax], FRAME_ARRAY
    inc     qword ptr [rbp + LOC_FDEPTH]
    mov     r10, qword ptr [rbp + LOC_OPEN_BUF]
    mov     r11, qword ptr [rbp + LOC_OPEN_DEPTH]
    mov     qword ptr [r10 + r11*8], rbx
    inc     qword ptr [rbp + LOC_OPEN_DEPTH]
    lea     rax, [rbx + rbx]
    lea     rax, [r14 + rax*8]
    mov     r10, TAPE_KIND_START_ARRAY
    shl     r10, KIND_SHIFT
    mov     qword ptr [rax], r10
    mov     qword ptr [rax + 8], 0
    inc     rbx
    inc     rcx
    lea     r11, [rip + .Lerror_from_r11]
    cmp     rcx, qword ptr [rbp + LOC_CHUNK_LEN]
    jb      .Larray_start
    lea     r10, [rip + .Larray_start]
    jmp     .Lchunk_fetch

.Lvw_open_string:
    mov     rax, qword ptr [rbp + LOC_POS]
    add     rax, rcx
    inc     rax
    mov     qword ptr [rbp + LOC_STR_START], rax
    mov     byte ptr [rbp + LOC_STR_ESC], 0
    inc     rcx
    lea     r11, [rip + .Lerror_from_r11]
    cmp     rcx, qword ptr [rbp + LOC_CHUNK_LEN]
    jb      .Lstring_chars
    lea     r10, [rip + .Lstring_chars]
    jmp     .Lchunk_fetch

.Lrefetch_value_whitespace:
    lea     r10, [rip + .Lvalue_whitespace]
    lea     r11, [rip + .Leof_after_value]
    jmp     .Lchunk_fetch

// ===========================================================================
// STATE: string_chars
// ===========================================================================
.Lstring_chars:
    mov     rax, qword ptr [rbp + LOC_BSL]
    mov     rdx, rax
    shl     rdx, 1
    mov     rsi, qword ptr [rbp + LOC_QUOTES]
    andn    rsi, rdx, rsi
    or      rax, rsi
    shr     rax, cl
    tzcnt   rax, rax
    add     rcx, rax
    cmp     rcx, qword ptr [rbp + LOC_CHUNK_LEN]
    jae     .Lrefetch_string_chars

    mov     rax, qword ptr [rbp + LOC_POS]
    add     rax, r12
    movzx   eax, byte ptr [rax + rcx]
    cmp     al, '\\'
    je      .Lsc_escape

    // '"' — end of string.  raw slice: [str_start .. pos + chunk_offset]
    mov     rsi, qword ptr [rbp + LOC_STR_START]
    lea     rsi, [r12 + rsi]                        // absolute ptr
    mov     rdx, qword ptr [rbp + LOC_POS]
    add     rdx, rcx
    sub     rdx, qword ptr [rbp + LOC_STR_START]    // len
    cmp     byte ptr [rbp + LOC_STR_ESC], 0
    jne     .Lsc_emit_escaped
    // Borrow: write String entry directly (rcx preserved — no call)
    lea     rax, [rbx + rbx]
    lea     rax, [r14 + rax*8]
    mov     r10, rdx
    shl     r10, 4
    shr     r10, 4
    mov     r11, TAPE_KIND_STRING
    shl     r11, KIND_SHIFT
    or      r10, r11
    mov     qword ptr [rax], r10
    mov     qword ptr [rax + 8], rsi
    inc     rbx
    inc     rcx
    lea     r11, [rip + .Leof_after_value]
    cmp     rcx, qword ptr [rbp + LOC_CHUNK_LEN]
    jb      .Lafter_value
    lea     r10, [rip + .Lafter_value]
    jmp     .Lchunk_fetch

.Lsc_emit_escaped:
    // Owned: unescape into buf, then move to Box<str>, write EscapedString entry.
    mov     qword ptr [rbp + LOC_COFF], rcx
    mov     rdi, rsi                               // arg1: raw_ptr
    mov     rsi, rdx                               // arg2: raw_len
    mov     rdx, qword ptr [rbp + LOC_UNESCAPE]   // arg3: *mut String
    call    unescape_str
    // call tape_take_box_str(unescape_buf, &tmp_ptr, &tmp_len)
    // Reuse LOC_ATOM_START as tmp_ptr slot, LOC_STR_START as tmp_len slot.
    mov     rdi, qword ptr [rbp + LOC_UNESCAPE]
    lea     rsi, [rbp + LOC_ATOM_START]
    lea     rdx, [rbp + LOC_STR_START]
    call    tape_take_box_str
    mov     rsi, qword ptr [rbp + LOC_ATOM_START]  // box_ptr
    mov     rdx, qword ptr [rbp + LOC_STR_START]   // box_len
    // Write EscapedString entry
    lea     rax, [rbx + rbx]
    lea     rax, [r14 + rax*8]
    mov     r10, rdx
    shl     r10, 4
    shr     r10, 4
    mov     r11, TAPE_KIND_ESCAPED_STRING
    shl     r11, KIND_SHIFT
    or      r10, r11
    mov     qword ptr [rax], r10
    mov     qword ptr [rax + 8], rsi
    // Signal: tape contains at least one escaped entry.
    mov     rax, qword ptr [rbp + LOC_HAS_ESC_OUT]
    mov     byte ptr [rax], 1
    inc     rbx
    mov     rcx, qword ptr [rbp + LOC_COFF]
    inc     rcx
    lea     r11, [rip + .Leof_after_value]
    cmp     rcx, qword ptr [rbp + LOC_CHUNK_LEN]
    jb      .Lafter_value
    lea     r10, [rip + .Lafter_value]
    jmp     .Lchunk_fetch

.Lsc_escape:
    mov     byte ptr [rbp + LOC_STR_ESC], 1
    inc     rcx
    lea     r11, [rip + .Lerror_from_r11]
    cmp     rcx, qword ptr [rbp + LOC_CHUNK_LEN]
    jb      .Lstring_chars
    lea     r10, [rip + .Lstring_chars]
    jmp     .Lchunk_fetch

.Lrefetch_string_chars:
    lea     r10, [rip + .Lstring_chars]
    lea     r11, [rip + .Lerror_from_r11]
    jmp     .Lchunk_fetch

// ===========================================================================
// STATE: key_chars
// ===========================================================================
.Lkey_chars:
    mov     rax, qword ptr [rbp + LOC_BSL]
    mov     rdx, rax
    shl     rdx, 1
    mov     rsi, qword ptr [rbp + LOC_QUOTES]
    andn    rsi, rdx, rsi
    or      rax, rsi
    shr     rax, cl
    tzcnt   rax, rax
    add     rcx, rax
    cmp     rcx, qword ptr [rbp + LOC_CHUNK_LEN]
    jae     .Lrefetch_key_chars

    mov     rax, qword ptr [rbp + LOC_POS]
    add     rax, r12
    movzx   eax, byte ptr [rax + rcx]
    cmp     al, '\\'
    je      .Lkc_escape

    // '"' — end of key.
    mov     rax, qword ptr [rbp + LOC_STR_START]
    lea     rax, [r12 + rax]
    mov     qword ptr [rbp + LOC_KEY_PTR], rax
    mov     rax, qword ptr [rbp + LOC_POS]
    add     rax, rcx
    sub     rax, qword ptr [rbp + LOC_STR_START]
    mov     qword ptr [rbp + LOC_KEY_LEN], rax
    movzx   eax, byte ptr [rbp + LOC_STR_ESC]
    mov     byte ptr [rbp + LOC_KEY_ESC], al
    inc     rcx
    lea     r11, [rip + .Lerror_from_r11]
    cmp     rcx, qword ptr [rbp + LOC_CHUNK_LEN]
    jb      .Lkey_end
    lea     r10, [rip + .Lkey_end]
    jmp     .Lchunk_fetch

.Lkc_escape:
    mov     byte ptr [rbp + LOC_STR_ESC], 1
    inc     rcx
    lea     r11, [rip + .Lerror_from_r11]
    cmp     rcx, qword ptr [rbp + LOC_CHUNK_LEN]
    jb      .Lkey_chars
    lea     r10, [rip + .Lkey_chars]
    jmp     .Lchunk_fetch

.Lrefetch_key_chars:
    lea     r10, [rip + .Lkey_chars]
    lea     r11, [rip + .Lerror_from_r11]
    jmp     .Lchunk_fetch

// ===========================================================================
// STATE: key_end
// ===========================================================================
.Lkey_end:
    mov     rax, qword ptr [rbp + LOC_WS]
    not     rax
    shr     rax, cl
    tzcnt   rax, rax
    add     rcx, rax
    cmp     rcx, qword ptr [rbp + LOC_CHUNK_LEN]
    jae     .Lrefetch_key_end

    mov     rax, qword ptr [rbp + LOC_POS]
    add     rax, r12
    movzx   eax, byte ptr [rax + rcx]
    cmp     al, ':'
    jne     .Lerror

    cmp     byte ptr [rbp + LOC_KEY_ESC], 0
    jne     .Lke_emit_escaped

    // Borrow key: write Key entry (rcx preserved — no call)
    mov     rsi, qword ptr [rbp + LOC_KEY_PTR]
    mov     rdx, qword ptr [rbp + LOC_KEY_LEN]
    lea     rax, [rbx + rbx]
    lea     rax, [r14 + rax*8]
    mov     r10, rdx
    shl     r10, 4
    shr     r10, 4
    mov     r11, TAPE_KIND_KEY
    shl     r11, KIND_SHIFT
    or      r10, r11
    mov     qword ptr [rax], r10
    mov     qword ptr [rax + 8], rsi
    inc     rbx
    inc     rcx
    lea     r11, [rip + .Lerror_from_r11]
    cmp     rcx, qword ptr [rbp + LOC_CHUNK_LEN]
    jb      .Lafter_colon
    lea     r10, [rip + .Lafter_colon]
    jmp     .Lchunk_fetch

.Lke_emit_escaped:
    // Owned escaped key: unescape, box, write EscapedKey entry.
    mov     qword ptr [rbp + LOC_COFF], rcx
    mov     rdi, qword ptr [rbp + LOC_KEY_PTR]    // raw_ptr
    mov     rsi, qword ptr [rbp + LOC_KEY_LEN]    // raw_len
    mov     rdx, qword ptr [rbp + LOC_UNESCAPE]   // *mut String
    call    unescape_str
    mov     rdi, qword ptr [rbp + LOC_UNESCAPE]
    lea     rsi, [rbp + LOC_ATOM_START]
    lea     rdx, [rbp + LOC_STR_START]
    call    tape_take_box_str
    mov     rsi, qword ptr [rbp + LOC_ATOM_START]  // box_ptr
    mov     rdx, qword ptr [rbp + LOC_STR_START]   // box_len
    // Write EscapedKey entry
    lea     rax, [rbx + rbx]
    lea     rax, [r14 + rax*8]
    mov     r10, rdx
    shl     r10, 4
    shr     r10, 4
    mov     r11, TAPE_KIND_ESCAPED_KEY
    shl     r11, KIND_SHIFT
    or      r10, r11
    mov     qword ptr [rax], r10
    mov     qword ptr [rax + 8], rsi
    // Signal: tape contains at least one escaped entry.
    mov     rax, qword ptr [rbp + LOC_HAS_ESC_OUT]
    mov     byte ptr [rax], 1
    inc     rbx
    mov     rcx, qword ptr [rbp + LOC_COFF]
    inc     rcx
    lea     r11, [rip + .Lerror_from_r11]
    cmp     rcx, qword ptr [rbp + LOC_CHUNK_LEN]
    jb      .Lafter_colon
    lea     r10, [rip + .Lafter_colon]
    jmp     .Lchunk_fetch

.Lrefetch_key_end:
    lea     r10, [rip + .Lkey_end]
    lea     r11, [rip + .Lerror_from_r11]
    jmp     .Lchunk_fetch

// ===========================================================================
// STATE: after_colon
// ===========================================================================
.Lafter_colon:
    mov     rax, qword ptr [rbp + LOC_WS]
    not     rax
    shr     rax, cl
    tzcnt   rax, rax
    add     rcx, rax
    cmp     rcx, qword ptr [rbp + LOC_CHUNK_LEN]
    jae     .Lrefetch_after_colon

    mov     rax, qword ptr [rbp + LOC_POS]
    add     rax, r12
    movzx   eax, byte ptr [rax + rcx]
    cmp     al, '{'
    je      .Lac_open_object
    cmp     al, '['
    je      .Lac_open_array
    cmp     al, '"'
    je      .Lac_open_string
    // atom
    mov     rdx, qword ptr [rbp + LOC_POS]
    add     rdx, rcx
    mov     qword ptr [rbp + LOC_ATOM_START], rdx
    inc     rcx
    lea     r11, [rip + .Latom_eof_flush]
    cmp     rcx, qword ptr [rbp + LOC_CHUNK_LEN]
    jb      .Latom_chars
    lea     r10, [rip + .Latom_chars]
    jmp     .Lchunk_fetch

.Lac_open_object:
    mov     rax, qword ptr [rbp + LOC_FDEPTH]
    cmp     rax, MAX_JSON_DEPTH
    jae     .Lerror
    mov     byte ptr [r15 + rax], FRAME_OBJECT
    inc     qword ptr [rbp + LOC_FDEPTH]
    mov     r10, qword ptr [rbp + LOC_OPEN_BUF]
    mov     r11, qword ptr [rbp + LOC_OPEN_DEPTH]
    mov     qword ptr [r10 + r11*8], rbx
    inc     qword ptr [rbp + LOC_OPEN_DEPTH]
    lea     rax, [rbx + rbx]
    lea     rax, [r14 + rax*8]
    mov     r10, TAPE_KIND_START_OBJECT
    shl     r10, KIND_SHIFT
    mov     qword ptr [rax], r10
    mov     qword ptr [rax + 8], 0
    inc     rbx
    inc     rcx
    lea     r11, [rip + .Lerror_from_r11]
    cmp     rcx, qword ptr [rbp + LOC_CHUNK_LEN]
    jb      .Lobject_start
    lea     r10, [rip + .Lobject_start]
    jmp     .Lchunk_fetch

.Lac_open_array:
    mov     rax, qword ptr [rbp + LOC_FDEPTH]
    cmp     rax, MAX_JSON_DEPTH
    jae     .Lerror
    mov     byte ptr [r15 + rax], FRAME_ARRAY
    inc     qword ptr [rbp + LOC_FDEPTH]
    mov     r10, qword ptr [rbp + LOC_OPEN_BUF]
    mov     r11, qword ptr [rbp + LOC_OPEN_DEPTH]
    mov     qword ptr [r10 + r11*8], rbx
    inc     qword ptr [rbp + LOC_OPEN_DEPTH]
    lea     rax, [rbx + rbx]
    lea     rax, [r14 + rax*8]
    mov     r10, TAPE_KIND_START_ARRAY
    shl     r10, KIND_SHIFT
    mov     qword ptr [rax], r10
    mov     qword ptr [rax + 8], 0
    inc     rbx
    inc     rcx
    lea     r11, [rip + .Lerror_from_r11]
    cmp     rcx, qword ptr [rbp + LOC_CHUNK_LEN]
    jb      .Larray_start
    lea     r10, [rip + .Larray_start]
    jmp     .Lchunk_fetch

.Lac_open_string:
    mov     rax, qword ptr [rbp + LOC_POS]
    add     rax, rcx
    inc     rax
    mov     qword ptr [rbp + LOC_STR_START], rax
    mov     byte ptr [rbp + LOC_STR_ESC], 0
    inc     rcx
    lea     r11, [rip + .Lerror_from_r11]
    cmp     rcx, qword ptr [rbp + LOC_CHUNK_LEN]
    jb      .Lstring_chars
    lea     r10, [rip + .Lstring_chars]
    jmp     .Lchunk_fetch

.Lrefetch_after_colon:
    lea     r10, [rip + .Lafter_colon]
    lea     r11, [rip + .Lerror_from_r11]
    jmp     .Lchunk_fetch

// ===========================================================================
// STATE: atom_chars
// ===========================================================================
.Latom_chars:
    mov     rax, qword ptr [rbp + LOC_DELIMS]
    shr     rax, cl
    tzcnt   rax, rax
    add     rcx, rax
    cmp     rcx, qword ptr [rbp + LOC_CHUNK_LEN]
    jae     .Lrefetch_atom_chars

    // Emit the atom from [atom_start .. pos + chunk_offset].
    mov     rsi, qword ptr [rbp + LOC_ATOM_START]
    lea     rsi, [r12 + rsi]
    mov     rdx, qword ptr [rbp + LOC_POS]
    add     rdx, rcx
    sub     rdx, qword ptr [rbp + LOC_ATOM_START]
    call    .Lemit_atom
    test    al, al
    jz      .Lerror

    // Dispatch on the delimiter byte.
    mov     rax, qword ptr [rbp + LOC_POS]
    add     rax, r12
    movzx   eax, byte ptr [rax + rcx]
    cmp     al, '}'
    je      .Lav_close_brace
    cmp     al, ']'
    je      .Lav_close_bracket
    cmp     al, ','
    je      .Lav_comma
    // whitespace delimiter
    inc     rcx
    lea     r11, [rip + .Leof_after_value]
    cmp     rcx, qword ptr [rbp + LOC_CHUNK_LEN]
    jb      .Lafter_value
    lea     r10, [rip + .Lafter_value]
    jmp     .Lchunk_fetch

.Lrefetch_atom_chars:
    lea     r10, [rip + .Latom_chars]
    lea     r11, [rip + .Latom_eof_flush]
    jmp     .Lchunk_fetch

// ===========================================================================
// STATE: object_start
// ===========================================================================
.Lobject_start:
    mov     byte ptr [rbp + LOC_AFT_COMMA], 0
.Lobject_start_body:
    mov     rax, qword ptr [rbp + LOC_WS]
    not     rax
    shr     rax, cl
    tzcnt   rax, rax
    add     rcx, rax
    cmp     rcx, qword ptr [rbp + LOC_CHUNK_LEN]
    jae     .Lrefetch_object_start

    mov     rax, qword ptr [rbp + LOC_POS]
    add     rax, r12
    movzx   eax, byte ptr [rax + rcx]
    cmp     al, '"'
    je      .Los_key
    cmp     al, '}'
    je      .Los_close_brace
    jmp     .Lerror

.Los_key:
    mov     rax, qword ptr [rbp + LOC_POS]
    add     rax, rcx
    inc     rax
    mov     qword ptr [rbp + LOC_STR_START], rax
    mov     byte ptr [rbp + LOC_STR_ESC], 0
    inc     rcx
    lea     r11, [rip + .Lerror_from_r11]
    cmp     rcx, qword ptr [rbp + LOC_CHUNK_LEN]
    jb      .Lkey_chars
    lea     r10, [rip + .Lkey_chars]
    jmp     .Lchunk_fetch

.Los_close_brace:
    cmp     byte ptr [rbp + LOC_AFT_COMMA], 0
    jne     .Lerror
    mov     rax, qword ptr [rbp + LOC_FDEPTH]
    test    rax, rax
    jz      .Lerror
    dec     rax
    cmp     byte ptr [r15 + rax], FRAME_OBJECT
    jne     .Lerror
    mov     qword ptr [rbp + LOC_FDEPTH], rax
    // Pop start_idx from open_buf; patch StartObject.payload with end_idx=rbx
    mov     r10, qword ptr [rbp + LOC_OPEN_BUF]
    mov     r11, qword ptr [rbp + LOC_OPEN_DEPTH]
    dec     r11
    mov     qword ptr [rbp + LOC_OPEN_DEPTH], r11
    mov     r11, qword ptr [r10 + r11*8]          // r11 = start_idx
    lea     rax, [r11 + r11]
    lea     rax, [r14 + rax*8]                    // &tape[start_idx]
    mov     r10, rbx
    shl     r10, 4
    shr     r10, 4
    or      qword ptr [rax], r10                  // patch end_idx into StartObject
    // Write EndObject entry at tape[rbx]
    lea     rax, [rbx + rbx]
    lea     rax, [r14 + rax*8]
    mov     r10, TAPE_KIND_END_OBJECT
    shl     r10, KIND_SHIFT
    mov     qword ptr [rax], r10
    mov     qword ptr [rax + 8], 0
    inc     rbx
    inc     rcx
    lea     r11, [rip + .Leof_after_value]
    cmp     rcx, qword ptr [rbp + LOC_CHUNK_LEN]
    jb      .Lafter_value
    lea     r10, [rip + .Lafter_value]
    jmp     .Lchunk_fetch

.Lrefetch_object_start:
    lea     r10, [rip + .Lobject_start_body]
    lea     r11, [rip + .Lerror_from_r11]
    jmp     .Lchunk_fetch

// ===========================================================================
// STATE: array_start
// ===========================================================================
.Larray_start:
    mov     byte ptr [rbp + LOC_AFT_COMMA], 0
.Larray_start_body:
    mov     rax, qword ptr [rbp + LOC_WS]
    not     rax
    shr     rax, cl
    tzcnt   rax, rax
    add     rcx, rax
    cmp     rcx, qword ptr [rbp + LOC_CHUNK_LEN]
    jae     .Lrefetch_array_start

    mov     rax, qword ptr [rbp + LOC_POS]
    add     rax, r12
    movzx   eax, byte ptr [rax + rcx]
    cmp     al, ']'
    je      .Las_close_bracket
    cmp     al, '{'
    je      .Las_open_object
    cmp     al, '['
    je      .Las_open_array
    cmp     al, '"'
    je      .Las_open_string
    // atom
    mov     rdx, qword ptr [rbp + LOC_POS]
    add     rdx, rcx
    mov     qword ptr [rbp + LOC_ATOM_START], rdx
    inc     rcx
    lea     r11, [rip + .Latom_eof_flush]
    cmp     rcx, qword ptr [rbp + LOC_CHUNK_LEN]
    jb      .Latom_chars
    lea     r10, [rip + .Latom_chars]
    jmp     .Lchunk_fetch

.Las_close_bracket:
    cmp     byte ptr [rbp + LOC_AFT_COMMA], 0
    jne     .Lerror
    mov     rax, qword ptr [rbp + LOC_FDEPTH]
    test    rax, rax
    jz      .Lerror
    dec     rax
    cmp     byte ptr [r15 + rax], FRAME_ARRAY
    jne     .Lerror
    mov     qword ptr [rbp + LOC_FDEPTH], rax
    // Pop start_idx from open_buf; patch StartArray.payload with end_idx=rbx
    mov     r10, qword ptr [rbp + LOC_OPEN_BUF]
    mov     r11, qword ptr [rbp + LOC_OPEN_DEPTH]
    dec     r11
    mov     qword ptr [rbp + LOC_OPEN_DEPTH], r11
    mov     r11, qword ptr [r10 + r11*8]          // r11 = start_idx
    lea     rax, [r11 + r11]
    lea     rax, [r14 + rax*8]                    // &tape[start_idx]
    mov     r10, rbx
    shl     r10, 4
    shr     r10, 4
    or      qword ptr [rax], r10                  // patch end_idx into StartArray
    // Write EndArray entry at tape[rbx]
    lea     rax, [rbx + rbx]
    lea     rax, [r14 + rax*8]
    mov     r10, TAPE_KIND_END_ARRAY
    shl     r10, KIND_SHIFT
    mov     qword ptr [rax], r10
    mov     qword ptr [rax + 8], 0
    inc     rbx
    inc     rcx
    lea     r11, [rip + .Leof_after_value]
    cmp     rcx, qword ptr [rbp + LOC_CHUNK_LEN]
    jb      .Lafter_value
    lea     r10, [rip + .Lafter_value]
    jmp     .Lchunk_fetch

.Las_open_object:
    mov     rax, qword ptr [rbp + LOC_FDEPTH]
    cmp     rax, MAX_JSON_DEPTH
    jae     .Lerror
    mov     byte ptr [r15 + rax], FRAME_OBJECT
    inc     qword ptr [rbp + LOC_FDEPTH]
    mov     r10, qword ptr [rbp + LOC_OPEN_BUF]
    mov     r11, qword ptr [rbp + LOC_OPEN_DEPTH]
    mov     qword ptr [r10 + r11*8], rbx
    inc     qword ptr [rbp + LOC_OPEN_DEPTH]
    lea     rax, [rbx + rbx]
    lea     rax, [r14 + rax*8]
    mov     r10, TAPE_KIND_START_OBJECT
    shl     r10, KIND_SHIFT
    mov     qword ptr [rax], r10
    mov     qword ptr [rax + 8], 0
    inc     rbx
    inc     rcx
    lea     r11, [rip + .Lerror_from_r11]
    cmp     rcx, qword ptr [rbp + LOC_CHUNK_LEN]
    jb      .Lobject_start
    lea     r10, [rip + .Lobject_start]
    jmp     .Lchunk_fetch

.Las_open_array:
    mov     rax, qword ptr [rbp + LOC_FDEPTH]
    cmp     rax, MAX_JSON_DEPTH
    jae     .Lerror
    mov     byte ptr [r15 + rax], FRAME_ARRAY
    inc     qword ptr [rbp + LOC_FDEPTH]
    mov     r10, qword ptr [rbp + LOC_OPEN_BUF]
    mov     r11, qword ptr [rbp + LOC_OPEN_DEPTH]
    mov     qword ptr [r10 + r11*8], rbx
    inc     qword ptr [rbp + LOC_OPEN_DEPTH]
    lea     rax, [rbx + rbx]
    lea     rax, [r14 + rax*8]
    mov     r10, TAPE_KIND_START_ARRAY
    shl     r10, KIND_SHIFT
    mov     qword ptr [rax], r10
    mov     qword ptr [rax + 8], 0
    inc     rbx
    inc     rcx
    lea     r11, [rip + .Lerror_from_r11]
    cmp     rcx, qword ptr [rbp + LOC_CHUNK_LEN]
    jb      .Larray_start
    lea     r10, [rip + .Larray_start]
    jmp     .Lchunk_fetch

.Las_open_string:
    mov     rax, qword ptr [rbp + LOC_POS]
    add     rax, rcx
    inc     rax
    mov     qword ptr [rbp + LOC_STR_START], rax
    mov     byte ptr [rbp + LOC_STR_ESC], 0
    inc     rcx
    lea     r11, [rip + .Lerror_from_r11]
    cmp     rcx, qword ptr [rbp + LOC_CHUNK_LEN]
    jb      .Lstring_chars
    lea     r10, [rip + .Lstring_chars]
    jmp     .Lchunk_fetch

.Lrefetch_array_start:
    lea     r10, [rip + .Larray_start_body]
    lea     r11, [rip + .Lerror_from_r11]
    jmp     .Lchunk_fetch

// ===========================================================================
// STATE: after_value
// ===========================================================================
.Lafter_value:
    mov     rax, qword ptr [rbp + LOC_WS]
    not     rax
    shr     rax, cl
    tzcnt   rax, rax
    add     rcx, rax
    cmp     rcx, qword ptr [rbp + LOC_CHUNK_LEN]
    jae     .Lrefetch_after_value

    mov     rax, qword ptr [rbp + LOC_POS]
    add     rax, r12
    movzx   eax, byte ptr [rax + rcx]

.Lav_comma:
    cmp     al, ','
    jne     .Lav_close_brace
    mov     rax, qword ptr [rbp + LOC_FDEPTH]
    test    rax, rax
    jz      .Lerror
    dec     rax
    mov     byte ptr [rbp + LOC_AFT_COMMA], 1
    cmp     byte ptr [r15 + rax], FRAME_OBJECT
    je      .Lav_comma_obj
    cmp     byte ptr [r15 + rax], FRAME_ARRAY
    je      .Lav_comma_arr
    jmp     .Lerror
.Lav_comma_obj:
    inc     rcx
    lea     r11, [rip + .Lerror_from_r11]
    cmp     rcx, qword ptr [rbp + LOC_CHUNK_LEN]
    jb      .Lobject_start_body
    lea     r10, [rip + .Lobject_start_body]
    jmp     .Lchunk_fetch
.Lav_comma_arr:
    inc     rcx
    lea     r11, [rip + .Lerror_from_r11]
    cmp     rcx, qword ptr [rbp + LOC_CHUNK_LEN]
    jb      .Larray_start_body
    lea     r10, [rip + .Larray_start_body]
    jmp     .Lchunk_fetch

.Lav_close_brace:
    cmp     al, '}'
    jne     .Lav_close_bracket
    mov     rax, qword ptr [rbp + LOC_FDEPTH]
    test    rax, rax
    jz      .Lerror
    dec     rax
    cmp     byte ptr [r15 + rax], FRAME_OBJECT
    jne     .Lerror
    mov     qword ptr [rbp + LOC_FDEPTH], rax
    // Pop start_idx from open_buf; patch StartObject.payload with end_idx=rbx
    mov     r10, qword ptr [rbp + LOC_OPEN_BUF]
    mov     r11, qword ptr [rbp + LOC_OPEN_DEPTH]
    dec     r11
    mov     qword ptr [rbp + LOC_OPEN_DEPTH], r11
    mov     r11, qword ptr [r10 + r11*8]          // r11 = start_idx
    lea     rax, [r11 + r11]
    lea     rax, [r14 + rax*8]
    mov     r10, rbx
    shl     r10, 4
    shr     r10, 4
    or      qword ptr [rax], r10
    // Write EndObject
    lea     rax, [rbx + rbx]
    lea     rax, [r14 + rax*8]
    mov     r10, TAPE_KIND_END_OBJECT
    shl     r10, KIND_SHIFT
    mov     qword ptr [rax], r10
    mov     qword ptr [rax + 8], 0
    inc     rbx
    inc     rcx
    lea     r11, [rip + .Leof_after_value]
    cmp     rcx, qword ptr [rbp + LOC_CHUNK_LEN]
    jb      .Lafter_value
    lea     r10, [rip + .Lafter_value]
    jmp     .Lchunk_fetch

.Lav_close_bracket:
    cmp     al, ']'
    jne     .Lerror
    mov     rax, qword ptr [rbp + LOC_FDEPTH]
    test    rax, rax
    jz      .Lerror
    dec     rax
    cmp     byte ptr [r15 + rax], FRAME_ARRAY
    jne     .Lerror
    mov     qword ptr [rbp + LOC_FDEPTH], rax
    // Pop start_idx from open_buf; patch StartArray.payload with end_idx=rbx
    mov     r10, qword ptr [rbp + LOC_OPEN_BUF]
    mov     r11, qword ptr [rbp + LOC_OPEN_DEPTH]
    dec     r11
    mov     qword ptr [rbp + LOC_OPEN_DEPTH], r11
    mov     r11, qword ptr [r10 + r11*8]          // r11 = start_idx
    lea     rax, [r11 + r11]
    lea     rax, [r14 + rax*8]
    mov     r10, rbx
    shl     r10, 4
    shr     r10, 4
    or      qword ptr [rax], r10
    // Write EndArray
    lea     rax, [rbx + rbx]
    lea     rax, [r14 + rax*8]
    mov     r10, TAPE_KIND_END_ARRAY
    shl     r10, KIND_SHIFT
    mov     qword ptr [rax], r10
    mov     qword ptr [rax + 8], 0
    inc     rbx
    inc     rcx
    lea     r11, [rip + .Leof_after_value]
    cmp     rcx, qword ptr [rbp + LOC_CHUNK_LEN]
    jb      .Lafter_value
    lea     r10, [rip + .Lafter_value]
    jmp     .Lchunk_fetch

.Lrefetch_after_value:
    lea     r10, [rip + .Lafter_value]
    lea     r11, [rip + .Leof_after_value]
    jmp     .Lchunk_fetch

// ===========================================================================
// .Latom_eof_flush — trailing atom at EOF
// ===========================================================================
.Latom_eof_flush:
    mov     rsi, qword ptr [rbp + LOC_ATOM_START]
    lea     rsi, [r12 + rsi]
    mov     rdx, qword ptr [rbp + LOC_POS]
    sub     rdx, qword ptr [rbp + LOC_ATOM_START]
    call    .Lemit_atom
    test    al, al
    jz      .Lerror
    // fall through

// ===========================================================================
// .Leof_after_value — EOF with valid post-value state
// ===========================================================================
.Leof_after_value:
    test    rbx, rbx
    jz      .Lerror                  // empty input is invalid JSON
    cmp     qword ptr [rbp + LOC_FDEPTH], 0
    jne     .Lerror
    // Store tape_len to *tape_len_out
    mov     rax, qword ptr [rbp + LOC_TLEN_OUT]
    mov     qword ptr [rax], rbx
    mov     al, 1           // return true
    jmp     .Lepilogue

// ===========================================================================
// .Lerror / .Lerror_from_r11
// ===========================================================================
.Lerror_from_r11:
.Lerror:
    xor     eax, eax        // return false
    jmp     .Lepilogue

// ===========================================================================
// .Lepilogue
// ===========================================================================
.Lepilogue:
    add     rsp, 152
    pop     r15
    pop     r14
    pop     r13
    pop     r12
    pop     rbx
    pop     rbp
    ret

// ===========================================================================
// .Lemit_atom — local helper (not exported)
//   Entry:  rsi = atom_ptr (*const u8)
//           rdx = atom_len (usize)
//   Exit:   al  = 1 on success, 0 on error
//           rbx = tape_len incremented by 1 on success
//   Clobbers: rax, rdi, rsi, rdx, r10, r11 (all caller-saved or scratch)
//   Preserves: rcx (via LOC_COFF save/restore)
// ===========================================================================
.Lemit_atom:
    mov     qword ptr [rbp + LOC_COFF], rcx

    cmp     rdx, 4
    je      .Lea_check4
    cmp     rdx, 5
    je      .Lea_check5
    jmp     .Lea_number

.Lea_check4:
    mov     eax, dword ptr [rsi]
    cmp     eax, 0x6C6C756E     // 'null' LE
    je      .Lea_null
    cmp     eax, 0x65757274     // 'true' LE
    je      .Lea_true
    jmp     .Lea_number

.Lea_check5:
    mov     eax, dword ptr [rsi]
    cmp     eax, 0x736C6166     // 'fals' LE
    jne     .Lea_number
    cmp     byte ptr [rsi + 4], 'e'
    jne     .Lea_number
    // "false" — write Bool/false entry (payload=0)
    lea     rax, [rbx + rbx]
    lea     rax, [r14 + rax*8]
    mov     r10, TAPE_KIND_BOOL
    shl     r10, KIND_SHIFT         // tag = 1<<60 | 0  (false)
    mov     qword ptr [rax], r10
    mov     qword ptr [rax + 8], 0
    inc     rbx
    mov     al, 1
    mov     rcx, qword ptr [rbp + LOC_COFF]
    ret

.Lea_null:
    // Write Null entry (tag=0, ptr=0)
    lea     rax, [rbx + rbx]
    lea     rax, [r14 + rax*8]
    mov     qword ptr [rax], 0
    mov     qword ptr [rax + 8], 0
    inc     rbx
    mov     al, 1
    mov     rcx, qword ptr [rbp + LOC_COFF]
    ret

.Lea_true:
    // Write Bool/true entry (payload=1)
    lea     rax, [rbx + rbx]
    lea     rax, [r14 + rax*8]
    mov     r10, TAPE_KIND_BOOL
    shl     r10, KIND_SHIFT
    or      r10, 1                  // payload=1 (true)
    mov     qword ptr [rax], r10
    mov     qword ptr [rax + 8], 0
    inc     rbx
    mov     al, 1
    mov     rcx, qword ptr [rbp + LOC_COFF]
    ret

.Lea_number:
    // Validate the number bytes, then write a Number entry.
    mov     qword ptr [rbp + LOC_ATOM_START], rsi   // save ptr (r8/r9 clobbered by call)
    mov     qword ptr [rbp + LOC_STR_START],  rdx   // save len
    mov     rdi, rsi
    mov     rsi, rdx
    call    is_valid_json_number_c
    test    al, al
    jz      .Lea_fail
    // Reload ptr/len
    mov     rsi, qword ptr [rbp + LOC_ATOM_START]
    mov     rdx, qword ptr [rbp + LOC_STR_START]
    // Write Number entry
    lea     rax, [rbx + rbx]
    lea     rax, [r14 + rax*8]
    mov     r10, rdx
    shl     r10, 4
    shr     r10, 4
    mov     r11, TAPE_KIND_NUMBER
    shl     r11, KIND_SHIFT
    or      r10, r11
    mov     qword ptr [rax], r10
    mov     qword ptr [rax + 8], rsi
    inc     rbx
    mov     al, 1
    mov     rcx, qword ptr [rbp + LOC_COFF]
    ret

.Lea_fail:
    xor     eax, eax
    mov     rcx, qword ptr [rbp + LOC_COFF]
    ret

.size parse_json_zmm_tape, . - parse_json_zmm_tape
