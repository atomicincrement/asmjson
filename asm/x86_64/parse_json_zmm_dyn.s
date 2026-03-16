// parse_json_zmm_dyn.s
// Hand-written x86-64 (AVX-512BW) assembly translation of parse_json_impl.
//
// This file translates the Rust parse_json_impl loop into GNU assembler using:
//   • Direct threading   — every state ends with an unconditional jmp to the
//                          next state label; no state variable is stored.
//   • Inlined ZMM classify — the four bitmasks are produced by AVX-512BW
//                          instructions at the top of the outer loop.
//   • Fat-pointer dispatch — writer methods are called via the Rust
//                          dyn-trait vtable.
//
// ---------------------------------------------------------------------------
// Extern C signature
// ---------------------------------------------------------------------------
//
//   bool parse_json_zmm_dyn(
//       const char    *src_ptr,      // rdi  — pointer to source bytes
//       size_t         src_len,      // rsi  — byte count
//       void          *writer_data,  // rdx  — data half of dyn JsonWriter
//       const void   **writer_vtab,  // rcx  — vtable half of dyn JsonWriter
//       uint8_t       *frames_buf,   // r8   — [FrameKind; 64], u8 per entry
//       void          *unescape_buf  // r9   — *mut String for escape decoding
//   );
//   Returns: al = 1 on success, 0 on error.
//
// FrameKind encoding: 0 = Object, 1 = Array  (#[repr(u8)])
//
// ---------------------------------------------------------------------------
// dyn JsonWriter vtable layout (Rust)
// ---------------------------------------------------------------------------
//
//   +  0   drop_in_place(data: *mut ())
//   +  8   size_of: usize
//   + 16   align_of: usize
//   + 24   null        (&mut self)
//   + 32   bool_val    (&mut self, v: bool)
//   + 40   number      (&mut self, s_ptr: *const u8, s_len: usize)
//   + 48   string      (&mut self, s_ptr: *const u8, s_len: usize)
//   + 56   escaped_string(&mut self, box_ptr: *mut u8, box_len: usize)
//   + 64   key         (&mut self, s_ptr: *const u8, s_len: usize)
//   + 72   escaped_key (&mut self, box_ptr: *mut u8, box_len: usize)
//   + 80   start_object(&mut self)
//   + 88   end_object  (&mut self)
//   + 96   start_array (&mut self)
//   +104   end_array   (&mut self)
//   +112   finish      (self) — NOT called from assembly; caller handles it
//
//   NOTE: &str is (ptr, len) in registers: rsi = ptr, rdx = len.
//         Box<str> likewise (ptr, len); no capacity word since Box<str> ≠ String.
//
// ---------------------------------------------------------------------------
// Caller-provided symbols (from Rust lib with C linkage)
// ---------------------------------------------------------------------------
//
//   bool unescape_str(s_ptr, s_len, out_string_ptr)
//     Fills *out_string_ptr (a &mut String) with the decoded text.
//     Declared:  #[unsafe(no_mangle)] #[inline(never)]
//                pub fn unescape_str(s: &str, out: &mut String)
//                → (rdi=s_ptr, rsi=s_len, rdx=out_string_ptr)
//
//   bool is_valid_json_number_c(ptr, len)
//     Returns 1 if bytes[..len] form a valid JSON number, else 0.
//     Declared:  #[unsafe(no_mangle)]
//                pub extern "C" fn is_valid_json_number_c(ptr, len) -> bool
//                → (rdi=ptr, rsi=len)
//
// ---------------------------------------------------------------------------
// Register assignments (persistent across the entire function)
// ---------------------------------------------------------------------------
//
//   rbx  = writer_data  (callee-saved; data half of fat pointer)
//   r12  = src_base     (callee-saved; pointer to byte 0 of source)
//   r13  = src_end      (callee-saved; src_base + src_len, one-past-end)
//   r14  = writer_vtab  (callee-saved; vtable pointer)
//   r15  = frames_buf   (callee-saved; &[u8; 64], one byte per nesting level)
//
// Inner-loop register:
//   rcx  = chunk_offset (0..chunk_len, current bit index within the chunk)
//
// State-return register (set only when the chunk is exhausted and a refetch
// is needed; otherwise each transition uses a direct jb .LSTATE):
//   r10  = address of target state   (loaded only before jmp .Lchunk_fetch)
//   r11  = address of EOF handler    (always set before any .Lchunk_fetch jump)
//
// Scratch: rax, rdx, rsi, rdi, r8, r9  (all caller-saved; free between vtable calls)
//
// ---------------------------------------------------------------------------
// Stack frame layout  (rbp-relative)
// ---------------------------------------------------------------------------
//
// At function entry the call pushes the return address (-8 from caller rsp).
// Prologue: push rbp; mov rbp,rsp; then five push(callee-saved).
// After five pushes rsp = rbp - 40.  sub rsp, 136  →  rsp = rbp - 176.
// 176 % 16 == 0  ✓  (required for ABI-aligned calls)
//
//   [rbp -  8]  saved rbx
//   [rbp - 16]  saved r12
//   [rbp - 24]  saved r13
//   [rbp - 32]  saved r14
//   [rbp - 40]  saved r15
//   [rbp - 48]  unescape_buf ptr         (initialised from arg r9)
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
//   [rbp -168]  saved chunk_offset       (rcx saved around vtable calls)
//   [rbp -176]  (alignment pad)

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
.equ LOC_COFF,        -168     // saved chunk_offset around vtable calls

// Vtable offsets for dyn JsonWriter
.equ VTAB_NULL,           24
.equ VTAB_BOOL_VAL,       32
.equ VTAB_NUMBER,         40
.equ VTAB_STRING,         48
.equ VTAB_ESCAPED_STRING, 56
.equ VTAB_KEY,            64
.equ VTAB_ESCAPED_KEY,    72
.equ VTAB_START_OBJECT,   80
.equ VTAB_END_OBJECT,     88
.equ VTAB_START_ARRAY,    96
.equ VTAB_END_ARRAY,      104

// MAX_JSON_DEPTH
.equ MAX_JSON_DEPTH, 64

// FrameKind values (#[repr(u8)])
.equ FRAME_OBJECT, 0
.equ FRAME_ARRAY,  1

.section .rodata
.align 64
// Local copy of the classification constants (one 64-byte lane each).
// Must match ByteStateConstants layout in lib.rs.
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
.global parse_json_zmm_dyn
.type   parse_json_zmm_dyn, @function

// ---------------------------------------------------------------------------
// parse_json_zmm_dyn — entry point
// ---------------------------------------------------------------------------
parse_json_zmm_dyn:
    // Prologue: save frame pointer and callee-saved registers.
    push    rbp
    mov     rbp, rsp
    push    rbx
    push    r12
    push    r13
    push    r14
    push    r15
    sub     rsp, 136        // local variables; rsp now 16-byte aligned

    // Stash incoming arguments into callee-saved / stack slots.
    mov     r12, rdi                        // src_base
    mov     r13, rdi
    add     r13, rsi                        // src_end = src_base + src_len
    mov     rbx, rdx                        // writer_data
    mov     r14, rcx                        // writer_vtab
    mov     r15, r8                         // frames_buf
    mov     qword ptr [rbp + LOC_UNESCAPE], r9  // unescape_buf

    // Initialise local variables.
    xor     eax, eax
    mov     qword ptr [rbp + LOC_FDEPTH],    rax   // frames_depth = 0
    mov     qword ptr [rbp + LOC_POS],       rax   // pos = 0
    mov     qword ptr [rbp + LOC_STR_START], rax
    mov     qword ptr [rbp + LOC_ATOM_START],rax
    mov     byte  ptr [rbp + LOC_STR_ESC],   al    // str_escaped = false
    mov     qword ptr [rbp + LOC_KEY_PTR],   rax
    mov     qword ptr [rbp + LOC_KEY_LEN],   rax
    mov     byte  ptr [rbp + LOC_KEY_ESC],   al    // key_escaped = false
    mov     byte  ptr [rbp + LOC_AFT_COMMA], al    // after_comma = false

    // First state is ValueWhitespace; set up r10/r11 and fall into chunk_fetch.
    lea     r10, [rip + .Lvalue_whitespace]
    lea     r11, [rip + .Leof_after_value]   // treat EOF at top-level as success
    jmp     .Lchunk_fetch

// ===========================================================================
// .Lchunk_fetch  — outer loop: advance pos, classify next 64-byte chunk
// ===========================================================================
// On entry:
//   r10 = target state   (loaded only from refetch labels or the chunk-exhausted
//                         fallthrough path; never set on the jb .LSTATE fast path)
//   r11 = EOF handler    (always set before any jump to .Lchunk_fetch)
// Clobbers: rax, rdx, rsi, zmm0..zmm6 (implicit), k1..k7 (implicit)
// On exit: rcx = 0 (chunk_offset), chunk bitmasks written to stack
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
    //   Inputs:  rdx = chunk pointer, rsi = chunk_len (1..64)
    //   Outputs: bs_whitespace / bs_quotes / bs_backslashes / bs_delimiters
    //            written to stack; k-registers and zmm0 clobbered.
    // -----------------------------------------------------------------------
    // Build load mask: (chunk_len == 64) ? ~0 : (1 << chunk_len) - 1
    cmp     rsi, 64
    je      .Lclassify_full
    mov     rax, 1
    shl     rax, cl             // rax = 1 << chunk_len  (cl = rsi low byte)
    dec     rax                 // rax = (1 << chunk_len) - 1
    jmp     .Lclassify_do
.Lclassify_full:
    mov     rax, -1             // ~0  (all 64 bits set)
.Lclassify_do:
    // cl might have been changed; reload rsi → rcx for shl above used cl.
    // Re-read: rsi still holds chunk_len (not changed since above).
    lea     r9, [rip + .Lzmm_space]     // r9 = base of constants
    kmovq   k1, rax                     // k1 = load mask
    vmovdqu8 zmm0{k1}{z}, zmmword ptr [rdx]   // zero-masked load

    vpcmpub  k2, zmm0, zmmword ptr [r9       ], 2   // whitespace: byte <= 0x20
    vpcmpeqb k3, zmm0, zmmword ptr [r9 +  64]       // quotes
    vpcmpeqb k4, zmm0, zmmword ptr [r9 + 128]       // backslashes
    vpcmpeqb k5, zmm0, zmmword ptr [r9 + 192]       // comma
    vpcmpeqb k6, zmm0, zmmword ptr [r9 + 256]       // '}'
    vpcmpeqb k7, zmm0, zmmword ptr [r9 + 320]       // ']'

    // delimiters = whitespace | comma | '}' | ']'
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
//   Skip whitespace bytes; dispatch on first non-whitespace byte.
// ===========================================================================
.Lvalue_whitespace:
    mov     rax, qword ptr [rbp + LOC_WS]
    not     rax                     // non-whitespace bits
    shr     rax, cl                 // shift to current chunk_offset
    tzcnt   rax, rax                // distance to first non-ws bit
    add     rcx, rax                // advance chunk_offset to non-ws byte
    cmp     rcx, qword ptr [rbp + LOC_CHUNK_LEN]
    jae     .Lrefetch_value_whitespace   // chunk exhausted; all whitespace

    // Dispatch on the non-whitespace byte.
    mov     rax, qword ptr [rbp + LOC_POS]
    movzx   eax, byte ptr [r12 + rax + rcx]   // byte = src[pos + chunk_offset]
    cmp     al, '{'
    je      .Lvw_open_object
    cmp     al, '['
    je      .Lvw_open_array
    cmp     al, '"'
    je      .Lvw_open_string
    // else: atom
    mov     rdx, qword ptr [rbp + LOC_POS]
    add     rdx, rcx
    mov     qword ptr [rbp + LOC_ATOM_START], rdx   // atom_start = pos + chunk_offset
    inc     rcx
    lea     r11, [rip + .Latom_eof_flush]
    cmp     rcx, qword ptr [rbp + LOC_CHUNK_LEN]
    jb      .Latom_chars
    lea     r10, [rip + .Latom_chars]
    jmp     .Lchunk_fetch

.Lvw_open_object:
    // frames_buf[frames_depth] = FRAME_OBJECT; frames_depth++
    mov     rax, qword ptr [rbp + LOC_FDEPTH]
    cmp     rax, MAX_JSON_DEPTH
    jae     .Lerror
    mov     byte ptr [r15 + rax], FRAME_OBJECT
    inc     qword ptr [rbp + LOC_FDEPTH]
    // call writer.start_object()
    mov     qword ptr [rbp + LOC_COFF], rcx
    mov     rdi, rbx
    call    qword ptr [r14 + VTAB_START_OBJECT]
    mov     rcx, qword ptr [rbp + LOC_COFF]
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
    mov     qword ptr [rbp + LOC_COFF], rcx
    mov     rdi, rbx
    call    qword ptr [r14 + VTAB_START_ARRAY]
    mov     rcx, qword ptr [rbp + LOC_COFF]
    inc     rcx
    lea     r11, [rip + .Lerror_from_r11]
    cmp     rcx, qword ptr [rbp + LOC_CHUNK_LEN]
    jb      .Larray_start
    lea     r10, [rip + .Larray_start]
    jmp     .Lchunk_fetch

.Lvw_open_string:
    // str_start = pos + chunk_offset + 1  (byte after the '"')
    mov     rax, qword ptr [rbp + LOC_POS]
    add     rax, rcx
    inc     rax
    mov     qword ptr [rbp + LOC_STR_START], rax
    mov     byte ptr [rbp + LOC_STR_ESC], 0    // str_escaped = false
    inc     rcx                                 // past the '"'
    lea     r11, [rip + .Lerror_from_r11]       // EOF mid-string = error
    cmp     rcx, qword ptr [rbp + LOC_CHUNK_LEN]
    jb      .Lstring_chars
    lea     r10, [rip + .Lstring_chars]
    jmp     .Lchunk_fetch

.Lrefetch_value_whitespace:
    lea     r10, [rip + .Lvalue_whitespace]
    lea     r11, [rip + .Leof_after_value]      // leading/trailing whitespace OK
    jmp     .Lchunk_fetch

// ===========================================================================
// STATE: string_chars
//   Scan for the first unescaped '"' or '\' within a string value.
// ===========================================================================
.Lstring_chars:
    // interesting = (backslashes | (quotes & ~(backslashes << 1))) >> chunk_offset
    mov     rax, qword ptr [rbp + LOC_BSL]
    mov     rdx, rax
    shl     rdx, 1                      // backslashes << 1
    mov     rsi, qword ptr [rbp + LOC_QUOTES]
    andn    rsi, rdx, rsi               // quotes & ~(backslashes << 1)
    or      rax, rsi                    // backslashes | unescaped_quotes
    shr     rax, cl                     // >> chunk_offset
    tzcnt   rax, rax
    add     rcx, rax                    // advance to first interesting byte
    cmp     rcx, qword ptr [rbp + LOC_CHUNK_LEN]
    jae     .Lrefetch_string_chars      // nothing interesting in this chunk

    // Which byte is it?
    mov     rax, qword ptr [rbp + LOC_POS]
    movzx   eax, byte ptr [r12 + rax + rcx]
    cmp     al, '\\'
    je      .Lsc_escape

    // '"' — end of string.
    // raw slice: [str_start .. pos + chunk_offset]
    mov     rsi, qword ptr [rbp + LOC_STR_START]  // start ptr
    lea     rsi, [r12 + rsi]                       // absolute ptr
    mov     rdx, qword ptr [rbp + LOC_POS]
    add     rdx, rcx                               // end = pos + chunk_offset
    sub     rdx, qword ptr [rbp + LOC_STR_START]  // len = end - str_start
    cmp     byte ptr [rbp + LOC_STR_ESC], 0
    jne     .Lsc_emit_escaped
    // Borrow: call writer.string(ptr, len)
    mov     qword ptr [rbp + LOC_COFF], rcx
    mov     rdi, rbx
    // rsi = ptr, rdx = len already set
    call    qword ptr [r14 + VTAB_STRING]
    mov     rcx, qword ptr [rbp + LOC_COFF]
    inc     rcx
    lea     r11, [rip + .Lerror_from_r11]
    cmp     rcx, qword ptr [rbp + LOC_CHUNK_LEN]
    jb      .Lafter_value
    lea     r10, [rip + .Lafter_value]
    jmp     .Lchunk_fetch

.Lsc_emit_escaped:
    // Owned: call unescape_str(raw_ptr, raw_len, unescape_buf)
    //        then call writer.escaped_string(box_ptr, box_len)
    mov     qword ptr [rbp + LOC_COFF], rcx
    // rsi = raw_ptr, rdx = raw_len already set
    mov     rdi, rsi                    // arg1: raw_ptr
    mov     rsi, rdx                    // arg2: raw_len
    mov     rdx, qword ptr [rbp + LOC_UNESCAPE]  // arg3: &mut String
    call    unescape_str
    // Read the decoded string back from the String struct.
    // String layout (Rust): { ptr: *mut u8, cap: usize, len: usize } = (ptr,cap,len).
    // Actually String = Vec<u8> = { ptr: NonNull<u8>, cap: usize, len: usize }.
    // In memory: [ptr_word, cap_word, len_word] each 8 bytes on 64-bit.
    // The ptr_word is actually a NonNull wrapper = just the pointer value.
    mov     r8,  qword ptr [rbp + LOC_UNESCAPE]  // &String
    mov     rsi, qword ptr [r8]                   // box_ptr  (= String.ptr.pointer())
    mov     rdx, qword ptr [r8 + 16]              // box_len  (= String.len)
    mov     qword ptr [rbp + LOC_COFF], rcx       // already saved but refresh
    mov     rdi, rbx
    call    qword ptr [r14 + VTAB_ESCAPED_STRING]
    mov     rcx, qword ptr [rbp + LOC_COFF]
    inc     rcx
    lea     r11, [rip + .Lerror_from_r11]
    cmp     rcx, qword ptr [rbp + LOC_CHUNK_LEN]
    jb      .Lafter_value
    lea     r10, [rip + .Lafter_value]
    jmp     .Lchunk_fetch

.Lsc_escape:
    // '\' found: mark string as escaped, advance past the backslash.
    mov     byte ptr [rbp + LOC_STR_ESC], 1
    inc     rcx                             // past '\' (escaped char follows)
    lea     r11, [rip + .Lerror_from_r11]
    cmp     rcx, qword ptr [rbp + LOC_CHUNK_LEN]
    jb      .Lstring_chars
    lea     r10, [rip + .Lstring_chars]
    jmp     .Lchunk_fetch

.Lrefetch_string_chars:
    lea     r10, [rip + .Lstring_chars]
    lea     r11, [rip + .Lerror_from_r11]  // EOF inside string = error
    jmp     .Lchunk_fetch

// ===========================================================================
// STATE: key_chars
//   Same pattern as string_chars, but stores result in key_raw_* / key_escaped
//   rather than emitting to the writer immediately (key is emitted in key_end).
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
    movzx   eax, byte ptr [r12 + rax + rcx]
    cmp     al, '\\'
    je      .Lkc_escape

    // '"' — end of key.  Store raw slice for later emission in key_end.
    mov     rax, qword ptr [rbp + LOC_STR_START]
    lea     rax, [r12 + rax]                   // key_raw_ptr (absolute)
    mov     qword ptr [rbp + LOC_KEY_PTR], rax
    mov     rax, qword ptr [rbp + LOC_POS]
    add     rax, rcx
    sub     rax, qword ptr [rbp + LOC_STR_START]  // key_raw_len
    mov     qword ptr [rbp + LOC_KEY_LEN], rax
    // Copy str_escaped → key_escaped
    movzx   eax, byte ptr [rbp + LOC_STR_ESC]
    mov     byte ptr [rbp + LOC_KEY_ESC], al
    inc     rcx                                // past closing '"'
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
//   Skip whitespace; expect ':'; emit the key via the writer.
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
    movzx   eax, byte ptr [r12 + rax + rcx]
    cmp     al, ':'
    jne     .Lerror

    // Emit the key (unescape if needed).
    cmp     byte ptr [rbp + LOC_KEY_ESC], 0
    jne     .Lke_emit_escaped

    // key is borrow: call writer.key(ptr, len)
    mov     qword ptr [rbp + LOC_COFF], rcx
    mov     rdi, rbx
    mov     rsi, qword ptr [rbp + LOC_KEY_PTR]
    mov     rdx, qword ptr [rbp + LOC_KEY_LEN]
    call    qword ptr [r14 + VTAB_KEY]
    mov     rcx, qword ptr [rbp + LOC_COFF]
    inc     rcx
    lea     r11, [rip + .Lerror_from_r11]
    cmp     rcx, qword ptr [rbp + LOC_CHUNK_LEN]
    jb      .Lafter_colon
    lea     r10, [rip + .Lafter_colon]
    jmp     .Lchunk_fetch

.Lke_emit_escaped:
    // Unescape into unescape_buf, then call writer.escaped_key(box_ptr, box_len).
    mov     qword ptr [rbp + LOC_COFF], rcx
    mov     rdi, qword ptr [rbp + LOC_KEY_PTR]   // raw_ptr
    mov     rsi, qword ptr [rbp + LOC_KEY_LEN]   // raw_len
    mov     rdx, qword ptr [rbp + LOC_UNESCAPE]  // &mut String
    call    unescape_str
    mov     r8,  qword ptr [rbp + LOC_UNESCAPE]
    mov     rsi, qword ptr [r8]                   // box_ptr
    mov     rdx, qword ptr [r8 + 16]              // box_len
    mov     rdi, rbx
    call    qword ptr [r14 + VTAB_ESCAPED_KEY]
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
//   Skip whitespace; dispatch the value that follows ':'.
//   Identical dispatch logic as value_whitespace.
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
    movzx   eax, byte ptr [r12 + rax + rcx]
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
    mov     qword ptr [rbp + LOC_COFF], rcx
    mov     rdi, rbx
    call    qword ptr [r14 + VTAB_START_OBJECT]
    mov     rcx, qword ptr [rbp + LOC_COFF]
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
    mov     qword ptr [rbp + LOC_COFF], rcx
    mov     rdi, rbx
    call    qword ptr [r14 + VTAB_START_ARRAY]
    mov     rcx, qword ptr [rbp + LOC_COFF]
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
//   Scan for the first delimiter byte.  When found, validate and emit the atom
//   then dispatch on the delimiter.
// ===========================================================================
.Latom_chars:
    mov     rax, qword ptr [rbp + LOC_DELIMS]
    shr     rax, cl
    tzcnt   rax, rax
    add     rcx, rax
    cmp     rcx, qword ptr [rbp + LOC_CHUNK_LEN]
    jae     .Lrefetch_atom_chars    // delimiter not in this chunk → fetch more

    // Delimiter found at chunk_offset = rcx.
    // Emit the atom from [atom_start .. pos + chunk_offset].
    mov     rsi, qword ptr [rbp + LOC_ATOM_START]
    lea     rsi, [r12 + rsi]                       // atom_ptr (absolute)
    mov     rdx, qword ptr [rbp + LOC_POS]
    add     rdx, rcx
    sub     rdx, qword ptr [rbp + LOC_ATOM_START]  // atom_len
    call    .Lemit_atom                             // uses rsi=ptr, rdx=len
    test    al, al
    jz      .Lerror

    // Dispatch on the delimiter byte to update structural context.
    mov     rax, qword ptr [rbp + LOC_POS]
    movzx   eax, byte ptr [r12 + rax + rcx]
    cmp     al, '}'
    je      .Lav_close_brace
    cmp     al, ']'
    je      .Lav_close_bracket
    cmp     al, ','
    je      .Lav_comma
    // whitespace delimiter — just an AfterValue transition
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
//   Skip whitespace; expect '"' (next key) or '}' (empty object).
// ===========================================================================
.Lobject_start:
    mov     byte ptr [rbp + LOC_AFT_COMMA], 0   // after_comma = false on fresh entry
    // fall through to .Lobject_start_body
.Lobject_start_body:
    mov     rax, qword ptr [rbp + LOC_WS]
    not     rax
    shr     rax, cl
    tzcnt   rax, rax
    add     rcx, rax
    cmp     rcx, qword ptr [rbp + LOC_CHUNK_LEN]
    jae     .Lrefetch_object_start

    mov     rax, qword ptr [rbp + LOC_POS]
    movzx   eax, byte ptr [r12 + rax + rcx]
    cmp     al, '"'
    je      .Los_key
    cmp     al, '}'
    je      .Los_close_brace
    jmp     .Lerror

.Los_key:
    // str_start = pos + chunk_offset + 1; str_escaped = false; → key_chars
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
    // '}' on ObjectStart: valid only if after_comma == false
    cmp     byte ptr [rbp + LOC_AFT_COMMA], 0
    jne     .Lerror
    // pop frame; must be Object
    mov     rax, qword ptr [rbp + LOC_FDEPTH]
    test    rax, rax
    jz      .Lerror
    dec     rax
    cmp     byte ptr [r15 + rax], FRAME_OBJECT
    jne     .Lerror
    mov     qword ptr [rbp + LOC_FDEPTH], rax
    mov     qword ptr [rbp + LOC_COFF], rcx
    mov     rdi, rbx
    call    qword ptr [r14 + VTAB_END_OBJECT]
    mov     rcx, qword ptr [rbp + LOC_COFF]
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
//   Skip whitespace; expect any value start or ']' (empty array).
// ===========================================================================
.Larray_start:
    mov     byte ptr [rbp + LOC_AFT_COMMA], 0
    // fall through
.Larray_start_body:
    mov     rax, qword ptr [rbp + LOC_WS]
    not     rax
    shr     rax, cl
    tzcnt   rax, rax
    add     rcx, rax
    cmp     rcx, qword ptr [rbp + LOC_CHUNK_LEN]
    jae     .Lrefetch_array_start

    mov     rax, qword ptr [rbp + LOC_POS]
    movzx   eax, byte ptr [r12 + rax + rcx]
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
    mov     qword ptr [rbp + LOC_COFF], rcx
    mov     rdi, rbx
    call    qword ptr [r14 + VTAB_END_ARRAY]
    mov     rcx, qword ptr [rbp + LOC_COFF]
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
    mov     qword ptr [rbp + LOC_COFF], rcx
    mov     rdi, rbx
    call    qword ptr [r14 + VTAB_START_OBJECT]
    mov     rcx, qword ptr [rbp + LOC_COFF]
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
    mov     qword ptr [rbp + LOC_COFF], rcx
    mov     rdi, rbx
    call    qword ptr [r14 + VTAB_START_ARRAY]
    mov     rcx, qword ptr [rbp + LOC_COFF]
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
//   Skip whitespace; expect ',' (next sibling), '}' or ']' (close container).
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
    movzx   eax, byte ptr [r12 + rax + rcx]

.Lav_comma:
    cmp     al, ','
    jne     .Lav_close_brace
    // comma: route to ObjectStart or ArrayStart based on top frame
    mov     rax, qword ptr [rbp + LOC_FDEPTH]
    test    rax, rax
    jz      .Lerror
    dec     rax                                 // index of top frame
    mov     byte ptr [rbp + LOC_AFT_COMMA], 1  // after_comma = true
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
    mov     qword ptr [rbp + LOC_COFF], rcx
    mov     rdi, rbx
    call    qword ptr [r14 + VTAB_END_OBJECT]
    mov     rcx, qword ptr [rbp + LOC_COFF]
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
    mov     qword ptr [rbp + LOC_COFF], rcx
    mov     rdi, rbx
    call    qword ptr [r14 + VTAB_END_ARRAY]
    mov     rcx, qword ptr [rbp + LOC_COFF]
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
// .Latom_eof_flush — trailing atom at EOF (e.g., top-level `42` with no newline)
// ===========================================================================
.Latom_eof_flush:
    // atom slice: [r12 + atom_start .. r12 + pos]  (pos = src_len here)
    mov     rsi, qword ptr [rbp + LOC_ATOM_START]
    lea     rsi, [r12 + rsi]                       // atom_ptr
    mov     rdx, qword ptr [rbp + LOC_POS]
    sub     rdx, qword ptr [rbp + LOC_ATOM_START]  // atom_len
    call    .Lemit_atom
    test    al, al
    jz      .Lerror
    // fall through to after_value EOF check

// ===========================================================================
// .Leof_after_value — EOF while in AfterValue (or after top-level atom).
//   Success only if all containers are closed.
// ===========================================================================
.Leof_after_value:
    cmp     qword ptr [rbp + LOC_FDEPTH], 0
    jne     .Lerror
    mov     al, 1               // return true
    jmp     .Lepilogue

// ===========================================================================
// .Lerror / .Lerror_from_r11
// ===========================================================================
.Lerror_from_r11:
    // jumped from .Lchunk_eof via invalid-state r11
.Lerror:
    xor     eax, eax            // return false
    jmp     .Lepilogue

// ===========================================================================
// .Lepilogue — restore callee-saved registers and return.
// ===========================================================================
.Lepilogue:
    // al already set (0 or 1)
    add     rsp, 136
    pop     r15
    pop     r14
    pop     r13
    pop     r12
    pop     rbx
    pop     rbp
    ret

// ===========================================================================
// .Lemit_atom — local helper (not exported)
//   Identify and emit one JSON atom (null / true / false / number).
//   Entry:  rsi = atom_ptr (*const u8)
//           rdx = atom_len (usize)
//   Exit:   al  = 1 on success, 0 on error
//   Clobbers: rax, rdi, r8, r9  (all caller-saved)
//   Does NOT clobber: rcx, rbx, r12-r15, rbp (callee-saved are untouched)
//   NOTE: chunk_offset (rcx) must be saved by the caller around this call
//         only if the call sequence modifies rcx (it does not in practice,
//         but the vtable calls inside DO clobber rcx; we save via LOC_COFF).
// ===========================================================================
.Lemit_atom:
    // Save rcx (chunk_offset) since vtable calls will clobber it.
    mov     qword ptr [rbp + LOC_COFF], rcx

    cmp     rdx, 4
    je      .Lea_check4
    cmp     rdx, 5
    je      .Lea_check5
    // not "null", "true", "false" — must be a number
    jmp     .Lea_number

.Lea_check4:
    // Load 4 bytes as a little-endian u32 and compare.
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
    // "false"
    mov     rdi, rbx
    xor     esi, esi                            // bool = false (0)
    call    qword ptr [r14 + VTAB_BOOL_VAL]
    mov     al, 1
    mov     rcx, qword ptr [rbp + LOC_COFF]
    ret

.Lea_null:
    mov     rdi, rbx
    call    qword ptr [r14 + VTAB_NULL]
    mov     al, 1
    mov     rcx, qword ptr [rbp + LOC_COFF]
    ret

.Lea_true:
    mov     rdi, rbx
    mov     esi, 1                              // bool = true (1)
    call    qword ptr [r14 + VTAB_BOOL_VAL]
    mov     al, 1
    mov     rcx, qword ptr [rbp + LOC_COFF]
    ret

.Lea_number:
    // Validate: call is_valid_json_number_c(ptr=rsi, len=rdx)
    mov     r8,  rsi            // save ptr
    mov     r9,  rdx            // save len
    mov     rdi, rsi
    mov     rsi, rdx
    call    is_valid_json_number_c
    test    al, al
    jz      .Lea_fail
    // Emit: call writer.number(self, ptr, len)
    mov     rdi, rbx
    mov     rsi, r8
    mov     rdx, r9
    call    qword ptr [r14 + VTAB_NUMBER]
    mov     al, 1
    mov     rcx, qword ptr [rbp + LOC_COFF]
    ret

.Lea_fail:
    xor     eax, eax
    mov     rcx, qword ptr [rbp + LOC_COFF]
    ret

.size parse_json_zmm_dyn, . - parse_json_zmm_dyn
