#![doc = include_str!("../README.md")]

#[cfg(feature = "serde")]
pub mod de;
pub mod dom;
pub mod sax;

#[cfg(feature = "serde")]
pub use de::from_taperef;
pub use dom::json_ref::JsonRef;
pub use dom::{Dom, DomArrayIter, DomEntry, DomEntryKind, DomObjectIter, DomRef};
pub use sax::Sax;

use dom::DomWriter;

// ---------------------------------------------------------------------------
// Hand-written x86-64 AVX-512BW assembly parser (direct-threading, C vtable)
// ---------------------------------------------------------------------------
//
// Instead of indexing directly into Rust's implementation-defined dyn-trait
// vtable, we supply a *stable* `#[repr(C)]` function-pointer struct.  The
// assembly uses fixed offsets 0, 8, 16, … into this struct.

/// Stable C-layout vtable passed to the assembly parser.
///
/// Every field is an `unsafe extern "C"` function pointer with the calling
/// convention that the assembly uses for each `JsonWriter` method.
#[cfg(target_arch = "x86_64")]
#[repr(C)]
struct ZmmVtab {
    null: unsafe extern "C" fn(*mut ()),
    bool_val: unsafe extern "C" fn(*mut (), bool),
    number: unsafe extern "C" fn(*mut (), *const u8, usize),
    string: unsafe extern "C" fn(*mut (), *const u8, usize),
    escaped_string: unsafe extern "C" fn(*mut (), *const u8, usize),
    key: unsafe extern "C" fn(*mut (), *const u8, usize),
    escaped_key: unsafe extern "C" fn(*mut (), *const u8, usize),
    start_object: unsafe extern "C" fn(*mut ()),
    end_object: unsafe extern "C" fn(*mut ()),
    start_array: unsafe extern "C" fn(*mut ()),
    end_array: unsafe extern "C" fn(*mut ()),
}

// ---------------------------------------------------------------------------
// Generic C-ABI trampolines for any JsonWriter
// ---------------------------------------------------------------------------
//
// `WriterForZmm` is a private bridge trait that exposes every `JsonWriter`
// method via raw pointer / length pairs so that the `extern "C"` trampolines
// below need no lifetime parameters.  It is implemented for every
// `W: JsonWriter<'a>` via the blanket impl; the `transmute` in each `src_*`
// method is sound because the raw pointers always point into the source JSON
// which lives for at least `'a`, matching the lifetime the concrete writer
// expects.

#[cfg(target_arch = "x86_64")]
pub(crate) trait WriterForZmm {
    unsafe fn wfz_null(&mut self);
    unsafe fn wfz_bool_val(&mut self, v: bool);
    unsafe fn wfz_number(&mut self, ptr: *const u8, len: usize);
    unsafe fn wfz_string(&mut self, ptr: *const u8, len: usize);
    unsafe fn wfz_escaped_string(&mut self, ptr: *const u8, len: usize);
    unsafe fn wfz_key(&mut self, ptr: *const u8, len: usize);
    unsafe fn wfz_escaped_key(&mut self, ptr: *const u8, len: usize);
    unsafe fn wfz_start_object(&mut self);
    unsafe fn wfz_end_object(&mut self);
    unsafe fn wfz_start_array(&mut self);
    unsafe fn wfz_end_array(&mut self);
}

#[cfg(target_arch = "x86_64")]
impl<'a, W: Sax<'a>> WriterForZmm for W {
    unsafe fn wfz_null(&mut self) {
        self.null()
    }
    unsafe fn wfz_bool_val(&mut self, v: bool) {
        self.bool_val(v)
    }
    unsafe fn wfz_number(&mut self, ptr: *const u8, len: usize) {
        let s: &'a str = unsafe {
            std::mem::transmute(std::str::from_utf8_unchecked(std::slice::from_raw_parts(
                ptr, len,
            )))
        };
        self.number(s)
    }
    unsafe fn wfz_string(&mut self, ptr: *const u8, len: usize) {
        let s: &'a str = unsafe {
            std::mem::transmute(std::str::from_utf8_unchecked(std::slice::from_raw_parts(
                ptr, len,
            )))
        };
        self.string(s)
    }
    unsafe fn wfz_escaped_string(&mut self, ptr: *const u8, len: usize) {
        let s = unsafe { std::str::from_utf8_unchecked(std::slice::from_raw_parts(ptr, len)) };
        self.escaped_string(s)
    }
    unsafe fn wfz_key(&mut self, ptr: *const u8, len: usize) {
        let s: &'a str = unsafe {
            std::mem::transmute(std::str::from_utf8_unchecked(std::slice::from_raw_parts(
                ptr, len,
            )))
        };
        self.key(s)
    }
    unsafe fn wfz_escaped_key(&mut self, ptr: *const u8, len: usize) {
        let s = unsafe { std::str::from_utf8_unchecked(std::slice::from_raw_parts(ptr, len)) };
        self.escaped_key(s)
    }
    unsafe fn wfz_start_object(&mut self) {
        self.start_object()
    }
    unsafe fn wfz_end_object(&mut self) {
        self.end_object()
    }
    unsafe fn wfz_start_array(&mut self) {
        self.start_array()
    }
    unsafe fn wfz_end_array(&mut self) {
        self.end_array()
    }
}

#[cfg(target_arch = "x86_64")]
unsafe extern "C" fn zw_null<W: WriterForZmm>(data: *mut ()) {
    unsafe { (*(data as *mut W)).wfz_null() }
}
#[cfg(target_arch = "x86_64")]
unsafe extern "C" fn zw_bool_val<W: WriterForZmm>(data: *mut (), v: bool) {
    unsafe { (*(data as *mut W)).wfz_bool_val(v) }
}
#[cfg(target_arch = "x86_64")]
unsafe extern "C" fn zw_number<W: WriterForZmm>(data: *mut (), ptr: *const u8, len: usize) {
    unsafe { (*(data as *mut W)).wfz_number(ptr, len) }
}
#[cfg(target_arch = "x86_64")]
unsafe extern "C" fn zw_string<W: WriterForZmm>(data: *mut (), ptr: *const u8, len: usize) {
    unsafe { (*(data as *mut W)).wfz_string(ptr, len) }
}
#[cfg(target_arch = "x86_64")]
unsafe extern "C" fn zw_escaped_string<W: WriterForZmm>(data: *mut (), ptr: *const u8, len: usize) {
    unsafe { (*(data as *mut W)).wfz_escaped_string(ptr, len) }
}
#[cfg(target_arch = "x86_64")]
unsafe extern "C" fn zw_key<W: WriterForZmm>(data: *mut (), ptr: *const u8, len: usize) {
    unsafe { (*(data as *mut W)).wfz_key(ptr, len) }
}
#[cfg(target_arch = "x86_64")]
unsafe extern "C" fn zw_escaped_key<W: WriterForZmm>(data: *mut (), ptr: *const u8, len: usize) {
    unsafe { (*(data as *mut W)).wfz_escaped_key(ptr, len) }
}
#[cfg(target_arch = "x86_64")]
unsafe extern "C" fn zw_start_object<W: WriterForZmm>(data: *mut ()) {
    unsafe { (*(data as *mut W)).wfz_start_object() }
}
#[cfg(target_arch = "x86_64")]
unsafe extern "C" fn zw_end_object<W: WriterForZmm>(data: *mut ()) {
    unsafe { (*(data as *mut W)).wfz_end_object() }
}
#[cfg(target_arch = "x86_64")]
unsafe extern "C" fn zw_start_array<W: WriterForZmm>(data: *mut ()) {
    unsafe { (*(data as *mut W)).wfz_start_array() }
}
#[cfg(target_arch = "x86_64")]
unsafe extern "C" fn zw_end_array<W: WriterForZmm>(data: *mut ()) {
    unsafe { (*(data as *mut W)).wfz_end_array() }
}

/// Build a [`ZmmVtab`] whose function pointers are monomorphised for writer
/// type `W`.  `W` must implement [`WriterForZmm`], which is blanket-impl'd
/// for every `JsonWriter<'a>`.
#[cfg(target_arch = "x86_64")]
fn build_zmm_vtab<W: WriterForZmm>() -> ZmmVtab {
    ZmmVtab {
        null: zw_null::<W>,
        bool_val: zw_bool_val::<W>,
        number: zw_number::<W>,
        string: zw_string::<W>,
        escaped_string: zw_escaped_string::<W>,
        key: zw_key::<W>,
        escaped_key: zw_escaped_key::<W>,
        start_object: zw_start_object::<W>,
        end_object: zw_end_object::<W>,
        start_array: zw_start_array::<W>,
        end_array: zw_end_array::<W>,
    }
}

#[cfg(target_arch = "x86_64")]
#[allow(improper_ctypes)]
unsafe extern "C" {
    /// Entry point assembled from `asm/x86_64/parse_json_zmm_sax.S`.
    ///
    /// Calls writer methods through the supplied `ZmmVtab`.  Does NOT call
    /// `finish`.  Returns `true` on success.
    fn parse_json_zmm_sax(
        src_ptr: *const u8,
        src_len: usize,
        writer_data: *mut (),
        writer_vtab: *const ZmmVtab,
        frames_buf: *mut u8,
    ) -> bool;

    /// Entry point assembled from `asm/x86_64/parse_json_zmm_dom.S`.
    ///
    /// Writes [`DomEntry`] values directly into the pre-allocated `tape_ptr`
    /// array (up to `tape_cap` entries).  On success sets `*tape_len_out` to
    /// the number of entries written and returns `RESULT_OK` (0).  Sets
    /// `*has_escapes_out` to `true` if any `EscapedString` or `EscapedKey`
    /// entry was written.  Returns `RESULT_PARSE_ERROR` (1) for invalid JSON
    /// or `RESULT_TAPE_OVERFLOW` (2) if `tape_cap` entries are not sufficient.
    fn parse_json_zmm_dom(
        src_ptr: *const u8,
        src_len: usize,
        tape_ptr: *mut DomEntry<'static>,
        tape_len_out: *mut usize,
        frames_buf: *mut u8,
        open_buf: *mut u64,
        has_escapes_out: *mut bool,
        tape_cap: usize,
    ) -> u8;
}

#[derive(PartialEq)]
enum State {
    // Waiting for the first byte of any JSON value.
    ValueWhitespace,

    // Inside a quoted string value.
    StringChars,

    // Inside a key string (left-hand side of an object member).
    KeyChars,
    // Closing `"` of a key consumed; skip whitespace then expect `:`.
    KeyEnd,
    // `:` consumed; skip whitespace then dispatch a value.
    AfterColon,

    // Inside an unquoted atom (number / true / false / null).
    AtomChars,

    // An invalid token was encountered; the parse will return None.
    Error,

    // `{` consumed; skip whitespace then expect `"` (key) or `}`.
    ObjectStart,

    // `[` consumed; skip whitespace then expect a value or `]`.
    ArrayStart,

    // A complete value was produced; skip whitespace then pop the context stack.
    AfterValue,
}

// ---------------------------------------------------------------------------
// Lightweight frame kind — the parser only needs to know Object vs Array for
// routing commas and validating bracket matches.  Value construction lives in
// the writer implementations below.
// ---------------------------------------------------------------------------

#[derive(Copy, Clone, PartialEq)]
#[repr(u8)]
enum FrameKind {
    Object = 0,
    Array = 1,
}

/// Maximum supported JSON nesting depth (objects + arrays combined).
pub const MAX_JSON_DEPTH: usize = 64;

// The Sax trait (SAX-style event sink) lives in the `sax` module.
// Re-exported at crate root as `pub use sax::Sax`.

// ---------------------------------------------------------------------------
// Atom helper — Validate a JSON number.
// ---------------------------------------------------------------------------

fn is_valid_json_number(s: &[u8]) -> bool {
    let mut i = 0;
    let n = s.len();
    if n == 0 {
        return false;
    }
    if s[i] == b'-' {
        i += 1;
        if i == n {
            return false;
        }
    }
    if s[i] == b'0' {
        i += 1;
        if i < n && s[i].is_ascii_digit() {
            return false;
        }
    } else if s[i].is_ascii_digit() {
        while i < n && s[i].is_ascii_digit() {
            i += 1;
        }
    } else {
        return false;
    }
    if i < n && s[i] == b'.' {
        i += 1;
        if i == n || !s[i].is_ascii_digit() {
            return false;
        }
        while i < n && s[i].is_ascii_digit() {
            i += 1;
        }
    }
    if i < n && (s[i] == b'e' || s[i] == b'E') {
        i += 1;
        if i < n && (s[i] == b'+' || s[i] == b'-') {
            i += 1;
        }
        if i == n || !s[i].is_ascii_digit() {
            return false;
        }
        while i < n && s[i].is_ascii_digit() {
            i += 1;
        }
    }
    i == n
}

/// C-linkage entry point for the hand-written assembly parser.
/// Returns 1 if `bytes[..len]` is a valid JSON number, 0 otherwise.
#[doc(hidden)]
#[unsafe(no_mangle)]
pub extern "C" fn is_valid_json_number_c(ptr: *const u8, len: usize) -> bool {
    let s = unsafe { std::slice::from_raw_parts(ptr, len) };
    is_valid_json_number(s)
}

/// Called from `parse_json_zmm_dom` to unescape and box a raw JSON string
/// in one step.
///
/// Decodes the still-escaped bytes at `raw_ptr[..raw_len]` via
/// [`unescape_str`], moves the result into a `Box<str>`, writes the data
/// pointer and length to `*out_ptr` / `*out_len`, then **leaks** the box.
/// Ownership is transferred to the `DomEntry` written immediately after this
/// call, which will free it on `Drop`.
#[doc(hidden)]
#[cfg(target_arch = "x86_64")]
#[unsafe(no_mangle)]
#[inline(never)]
pub extern "C" fn dom_unescape_to_box_str(
    raw_ptr: *const u8,
    raw_len: usize,
    out_ptr: *mut *const u8,
    out_len: *mut usize,
) {
    unsafe {
        let raw = std::str::from_utf8_unchecked(std::slice::from_raw_parts(raw_ptr, raw_len));
        let mut buf = String::new();
        unescape_str(raw, &mut buf);
        let boxed: Box<str> = buf.into_boxed_str();
        let len = boxed.len();
        let raw_out: *mut str = Box::into_raw(boxed);
        *out_ptr = raw_out as *mut u8 as *const u8;
        *out_len = len;
    }
}

fn write_atom<'a, W: Sax<'a>>(s: &'a str, w: &mut W) -> bool {
    match s {
        "true" => {
            w.bool_val(true);
            true
        }
        "false" => {
            w.bool_val(false);
            true
        }
        "null" => {
            w.null();
            true
        }
        n => {
            if is_valid_json_number(n.as_bytes()) {
                w.number(n);
                true
            } else {
                false
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Public parse entry points
// ---------------------------------------------------------------------------

/// Parse `src` into a flat [`Dom`] using the portable SWAR classifier.
///
/// `initial_capacity` pre-sizes the tape allocation.  Pass `None` to let the
/// parser decide (equivalent to `Some(0)`, i.e. start with a default-sized
/// `Vec`).  The tape grows automatically so this is only a performance hint.
///
/// Returns `None` if the input is not valid JSON.
///
/// For maximum throughput on CPUs with AVX-512BW, use [`parse_to_dom_zmm`] or
/// the safe wrapper returned by [`dom_parser`].
///
/// ```rust
/// use asmjson::{parse_to_dom, JsonRef};
/// let tape = parse_to_dom(r#"{"x":1}"#, None).unwrap();
/// assert_eq!(tape.root().get("x").as_i64(), Some(1));
/// ```
pub fn parse_to_dom<'a>(src: &'a str, initial_capacity: Option<usize>) -> Option<Dom<'a>> {
    let cap = initial_capacity.unwrap_or(0);
    parse_with(src, DomWriter::with_capacity(cap))
}

/// Parse `src` to a [`Dom`] using the hand-written x86-64 AVX-512BW
/// assembly parser that writes [`DomEntry`] values directly into a
/// pre-allocated array, bypassing all virtual dispatch.
///
/// `initial_capacity` controls how many [`DomEntry`] slots the first
/// allocation reserves.  Pass `None` to use the default of `src.len() / 4`,
/// which is large enough for well-formed JSON without triggering a retry on
/// typical inputs.  Pass `Some(n)` to hint a known-good size and avoid any
/// retry allocation.  On overflow the capacity is doubled and the parse is
/// retried automatically regardless of the initial hint.
///
/// Only available on `x86_64` targets.  Returns `None` if the JSON is
/// invalid or nesting exceeds [`MAX_JSON_DEPTH`] levels.
///
/// # Safety
///
/// The caller must ensure the CPU supports AVX-512BW.  Invoking this on a CPU
/// without AVX-512BW support will trigger an illegal instruction fault.  Use
/// [`parse_to_dom`] for portable code.
///
/// ```rust,no_run
/// #[cfg(target_arch = "x86_64")]
/// {
///     use asmjson::parse_to_dom_zmm;
///     // SAFETY: caller must verify AVX-512BW support before calling.
///     let tape = unsafe { parse_to_dom_zmm(r#"{"x":1}"#, None) }.unwrap();
///     use asmjson::JsonRef;
///     assert_eq!(tape.root().get("x").as_i64(), Some(1));
/// }
/// ```
#[cfg(target_arch = "x86_64")]
pub unsafe fn parse_to_dom_zmm<'a>(
    src: &'a str,
    initial_capacity: Option<usize>,
) -> Option<Dom<'a>> {
    // Result codes matching the assembly RESULT_* constants.
    const RESULT_OK: u8 = 0;
    const RESULT_PARSE_ERROR: u8 = 1;
    const RESULT_TAPE_OVERFLOW: u8 = 2;

    let mut frames_buf = [FrameKind::Object; MAX_JSON_DEPTH];
    let mut open_buf = [0u64; MAX_JSON_DEPTH];

    // Start at the caller-supplied hint, or default to src.len()/4 entries.
    // For well-formed JSON this default comfortably exceeds the tape length
    // (each record is ~130 bytes and emits ~22 entries; 130/4 = 32.5 > 22),
    // so no retry should be needed in practice.
    let mut capacity = initial_capacity.unwrap_or_else(|| (src.len() / 4).max(2));

    loop {
        let mut tape_data: Vec<DomEntry<'a>> = Vec::with_capacity(capacity);
        let tape_ptr = tape_data.as_mut_ptr() as *mut DomEntry<'static>;
        let mut tape_len: usize = 0;
        let mut has_escapes: bool = false;

        // SAFETY:
        //   • `tape_data` has exactly `capacity` entries; the assembly checks
        //     bounds before every write and returns RESULT_TAPE_OVERFLOW if
        //     the capacity is exceeded.
        //   • `src` lives for at least `'a`; string pointers stored in tape
        //     entries point into `src`'s bytes and remain valid for `'a`.
        //   • EscapedString / EscapedKey entries own a `Box<str>` allocated by
        //     `dom_unescape_to_box_str`; `DomEntry::drop` frees them.
        //   • `parse_json_zmm_dom` does NOT call `finish`.
        let result = unsafe {
            parse_json_zmm_dom(
                src.as_ptr(),
                src.len(),
                tape_ptr,
                &raw mut tape_len,
                frames_buf.as_mut_ptr() as *mut u8,
                open_buf.as_mut_ptr(),
                &raw mut has_escapes,
                capacity,
            )
        };

        match result {
            RESULT_OK => {
                // SAFETY: assembly wrote exactly `tape_len` initialised entries.
                unsafe { tape_data.set_len(tape_len) };
                return Some(Dom {
                    entries: tape_data,
                    has_escapes,
                });
            }
            RESULT_PARSE_ERROR => return None,
            RESULT_TAPE_OVERFLOW => {
                // The tape was too small; double capacity and retry.
                // First, set the vec length to `tape_len` so that any
                // EscapedString / EscapedKey entries already written (which
                // own a Box<str>) are properly dropped when tape_data goes
                // out of scope at the end of this block.
                unsafe { tape_data.set_len(tape_len) };
                capacity = capacity.saturating_mul(2).max(capacity + 1);
                continue;
            }
            _ => return None, // should not happen
        }
    }
}

// ---------------------------------------------------------------------------
// CPUID-dispatching helpers
// ---------------------------------------------------------------------------

/// Safe trampoline for `parse_to_dom_zmm`; only called when CPUID has
/// confirmed AVX-512BW support (see [`dom_parser`]).
#[cfg(target_arch = "x86_64")]
fn parse_to_dom_zmm_safe<'a>(src: &'a str, cap: Option<usize>) -> Option<Dom<'a>> {
    // SAFETY: dom_parser() only returns this fn after CPUID confirms AVX-512BW.
    unsafe { parse_to_dom_zmm(src, cap) }
}

/// Returns a CPUID-selected DOM parse function.
///
/// On CPUs with AVX-512BW the returned function uses the hand-written
/// assembly parser; otherwise the portable SWAR parser is used.  The CPUID
/// check is performed once when `dom_parser()` is called.
///
/// The returned function has the signature
/// `fn(&str, Option<usize>) -> Option<Dom<'_>>`.
///
/// ```rust
/// use asmjson::{dom_parser, JsonRef};
/// let parse = dom_parser();
/// let tape = parse(r#"{"x":1}"#, None).unwrap();
/// assert_eq!(tape.root().get("x").as_i64(), Some(1));
/// ```
pub fn dom_parser() -> for<'a> fn(&'a str, Option<usize>) -> Option<Dom<'a>> {
    #[cfg(target_arch = "x86_64")]
    if is_x86_feature_detected!("avx512bw") {
        return parse_to_dom_zmm_safe;
    }
    parse_to_dom
}

/// Handle returned by [`sax_parser`]; call `.parse(src, writer)`.
///
/// Stores the result of a CPUID check performed at construction time so that
/// repeated `.parse()` calls pay only one branch.
#[derive(Copy, Clone)]
pub struct SaxParser {
    #[cfg(target_arch = "x86_64")]
    zmm: bool,
}

impl SaxParser {
    /// Parse `src` with `writer` using the best available CPU path.
    pub fn parse<'a, W: Sax<'a>>(&self, src: &'a str, writer: W) -> Option<W::Output> {
        #[cfg(target_arch = "x86_64")]
        if self.zmm {
            // SAFETY: constructed only when CPUID confirms AVX-512BW.
            return unsafe { parse_with_zmm(src, writer) };
        }
        parse_with(src, writer)
    }
}

/// Returns a CPUID-selected SAX parser handle.
///
/// The CPUID check is performed once; subsequent calls to
/// [`SaxParser::parse`] dispatch to the best available path without
/// repeating it.
///
/// ```rust
/// use asmjson::sax::Sax;
/// use asmjson::sax_parser;
///
/// struct Counter { n: usize }
/// impl<'a> Sax<'a> for Counter {
///     type Output = usize;
///     fn null(&mut self) {}
///     fn bool_val(&mut self, _: bool) {}
///     fn number(&mut self, _: &str) {}
///     fn string(&mut self, _: &str) { self.n += 1; }
///     fn escaped_string(&mut self, _: &str) { self.n += 1; }
///     fn key(&mut self, _: &str) {}
///     fn escaped_key(&mut self, _: &str) {}
///     fn start_object(&mut self) {}
///     fn end_object(&mut self) {}
///     fn start_array(&mut self) {}
///     fn end_array(&mut self) {}
///     fn finish(self) -> Option<usize> { Some(self.n) }
/// }
///
/// let parser = sax_parser();
/// let n = parser.parse(r#"["a","b"]"#, Counter { n: 0 }).unwrap();
/// assert_eq!(n, 2);
/// ```
pub fn sax_parser() -> SaxParser {
    SaxParser {
        #[cfg(target_arch = "x86_64")]
        zmm: is_x86_feature_detected!("avx512bw"),
    }
}

/// Parse `src` using a custom [`JsonWriter`], returning its output.
///
/// This is the generic entry point: supply your own writer to produce any
/// output in a single pass over the source.  Uses the portable SWAR
/// classifier; works on any architecture.
///
/// For maximum throughput on CPUs with AVX-512BW, use [`parse_with_zmm`] or
/// the safe wrapper returned by [`sax_parser`].
pub fn parse_with<'a, W: Sax<'a>>(src: &'a str, writer: W) -> Option<W::Output> {
    let mut frames_buf = [FrameKind::Object; MAX_JSON_DEPTH];
    parse_json_impl(src, writer, &mut frames_buf)
}

/// Parse `src` using a custom [`JsonWriter`] and the hand-written x86-64
/// AVX-512BW assembly parser with direct-threaded state dispatch.
///
/// Only available on `x86_64` targets.  Returns `None` if the JSON is
/// invalid or nesting exceeds [`MAX_JSON_DEPTH`] levels.
///
/// # Safety
///
/// The caller must ensure the CPU supports AVX-512BW.  Invoking this on a CPU
/// without AVX-512BW support will trigger an illegal instruction fault.  Use
/// [`parse_with`] for portable code.
///
#[cfg(target_arch = "x86_64")]
pub unsafe fn parse_with_zmm<'a, W: Sax<'a>>(src: &'a str, mut writer: W) -> Option<W::Output> {
    let vtab = build_zmm_vtab::<W>();
    let mut frames_buf = [FrameKind::Object; MAX_JSON_DEPTH];
    // SAFETY (caller obligation): CPU supports AVX-512BW.
    // SAFETY (internal): writer and src both live for 'a, outlasting this
    // synchronous call.  parse_json_zmm_sax does NOT call finish.
    let ok = unsafe {
        parse_json_zmm_sax(
            src.as_ptr(),
            src.len(),
            &raw mut writer as *mut (),
            &vtab,
            frames_buf.as_mut_ptr() as *mut u8,
        )
    };
    if ok { writer.finish() } else { None }
}

fn parse_json_impl<'a, W: Sax<'a>>(
    src: &'a str,
    mut writer: W,
    frames_buf: &mut [FrameKind; MAX_JSON_DEPTH],
) -> Option<W::Output> {
    let bytes = src.as_bytes();
    let mut frames_depth: usize = 0;
    let mut str_start: usize = 0; // absolute byte offset of char after opening '"'
    let mut str_escaped = false; // true if the current string contained any backslash
    let mut bs_count: usize = 0; // consecutive backslashes immediately before current pos
    let mut atom_start: usize = 0; // absolute byte offset of first atom byte
    let mut current_key_raw: &'a str = ""; // raw key slice captured when KeyChars closes
    let mut current_key_escaped = false; // true when the key contained backslash escapes
    let mut after_comma = false; // true when ObjectStart/ArrayStart was reached via a `,`
    let mut state = State::ValueWhitespace;

    let mut pos = 0;
    while pos < bytes.len() {
        let chunk_len = (bytes.len() - pos).min(64);
        let chunk = &bytes[pos..pos + chunk_len];
        let byte_state = classify_u64(chunk);

        let mut chunk_offset = 0;
        'inner: while chunk_offset < chunk_len {
            state = match state {
                State::ValueWhitespace => {
                    let ahead = (!byte_state.whitespace) >> chunk_offset;
                    let skip = ahead.trailing_zeros() as usize;
                    chunk_offset += skip;
                    if chunk_offset >= chunk_len {
                        break 'inner;
                    }
                    let byte = chunk[chunk_offset];
                    match byte {
                        b'{' => {
                            if frames_depth >= MAX_JSON_DEPTH {
                                State::Error
                            } else {
                                frames_buf[frames_depth] = FrameKind::Object;
                                frames_depth += 1;
                                writer.start_object();
                                State::ObjectStart
                            }
                        }
                        b'[' => {
                            if frames_depth >= MAX_JSON_DEPTH {
                                State::Error
                            } else {
                                frames_buf[frames_depth] = FrameKind::Array;
                                frames_depth += 1;
                                writer.start_array();
                                State::ArrayStart
                            }
                        }
                        b'"' => {
                            str_start = pos + chunk_offset + 1;
                            str_escaped = false;
                            bs_count = 0;
                            State::StringChars
                        }
                        _ => {
                            atom_start = pos + chunk_offset;
                            State::AtomChars
                        }
                    }
                }

                State::StringChars => {
                    // Scan for either '\' or '"'; handle runs of backslashes here
                    // rather than via a separate state so that even/odd counting is
                    // correct for sequences like `\\"` (two backslashes + quote).
                    let interesting = (byte_state.backslashes | byte_state.quotes) >> chunk_offset;
                    let skip = interesting.trailing_zeros() as usize;
                    chunk_offset = (chunk_offset + skip).min(chunk_len);
                    if chunk_offset >= chunk_len {
                        break 'inner;
                    }
                    // Any ordinary chars between the last event and here break the run.
                    if skip > 0 {
                        bs_count = 0;
                    }
                    let byte = chunk[chunk_offset];
                    match byte {
                        b'\\' => {
                            // Count consecutive backslashes; parity decides whether
                            // the next quote (if any) is escaped.
                            bs_count += 1;
                            str_escaped = true;
                            State::StringChars
                        }
                        b'"' if bs_count & 1 == 1 => {
                            // Odd run of preceding backslashes: this quote is escaped.
                            bs_count = 0;
                            State::StringChars
                        }
                        _ => {
                            // Even run (0, 2, 4 …): string ends here.
                            bs_count = 0;
                            let raw = &src[str_start..pos + chunk_offset];
                            if str_escaped {
                                writer.escaped_string(raw);
                            } else {
                                writer.string(raw);
                            }
                            State::AfterValue
                        }
                    }
                }

                State::KeyChars => {
                    let interesting = (byte_state.backslashes | byte_state.quotes) >> chunk_offset;
                    let skip = interesting.trailing_zeros() as usize;
                    chunk_offset = (chunk_offset + skip).min(chunk_len);
                    if chunk_offset >= chunk_len {
                        break 'inner;
                    }
                    if skip > 0 {
                        bs_count = 0;
                    }
                    let byte = chunk[chunk_offset];
                    match byte {
                        b'\\' => {
                            bs_count += 1;
                            str_escaped = true;
                            State::KeyChars
                        }
                        b'"' if bs_count & 1 == 1 => {
                            // Odd run of preceding backslashes: this quote is escaped.
                            bs_count = 0;
                            State::KeyChars
                        }
                        _ => {
                            // Even run: key ends here.
                            bs_count = 0;
                            current_key_raw = &src[str_start..pos + chunk_offset];
                            current_key_escaped = str_escaped;
                            State::KeyEnd
                        }
                    }
                }
                State::KeyEnd => {
                    let ahead = (!byte_state.whitespace) >> chunk_offset;
                    let skip = ahead.trailing_zeros() as usize;
                    chunk_offset += skip;
                    if chunk_offset >= chunk_len {
                        break 'inner;
                    }
                    let byte = chunk[chunk_offset];
                    match byte {
                        b':' => {
                            if current_key_escaped {
                                writer.escaped_key(current_key_raw);
                            } else {
                                writer.key(current_key_raw);
                            }
                            State::AfterColon
                        }
                        _ => State::Error,
                    }
                }
                State::AfterColon => {
                    let ahead = (!byte_state.whitespace) >> chunk_offset;
                    let skip = ahead.trailing_zeros() as usize;
                    chunk_offset += skip;
                    if chunk_offset >= chunk_len {
                        break 'inner;
                    }
                    let byte = chunk[chunk_offset];
                    match byte {
                        b'{' => {
                            if frames_depth >= MAX_JSON_DEPTH {
                                State::Error
                            } else {
                                frames_buf[frames_depth] = FrameKind::Object;
                                frames_depth += 1;
                                writer.start_object();
                                State::ObjectStart
                            }
                        }
                        b'[' => {
                            if frames_depth >= MAX_JSON_DEPTH {
                                State::Error
                            } else {
                                frames_buf[frames_depth] = FrameKind::Array;
                                frames_depth += 1;
                                writer.start_array();
                                State::ArrayStart
                            }
                        }
                        b'"' => {
                            str_start = pos + chunk_offset + 1;
                            str_escaped = false;
                            bs_count = 0;
                            State::StringChars
                        }
                        _ => {
                            atom_start = pos + chunk_offset;
                            State::AtomChars
                        }
                    }
                }

                State::AtomChars => {
                    let ahead = byte_state.delimiters >> chunk_offset;
                    let skip = ahead.trailing_zeros() as usize;
                    chunk_offset += skip;
                    if chunk_offset >= chunk_len {
                        break 'inner;
                    }
                    let byte = chunk[chunk_offset];
                    if !write_atom(&src[atom_start..pos + chunk_offset], &mut writer) {
                        State::Error
                    } else {
                        match byte {
                            b'}' => {
                                if frames_depth == 0
                                    || frames_buf[frames_depth - 1] != FrameKind::Object
                                {
                                    State::Error
                                } else {
                                    frames_depth -= 1;
                                    writer.end_object();
                                    State::AfterValue
                                }
                            }
                            b']' => {
                                if frames_depth == 0
                                    || frames_buf[frames_depth - 1] != FrameKind::Array
                                {
                                    State::Error
                                } else {
                                    frames_depth -= 1;
                                    writer.end_array();
                                    State::AfterValue
                                }
                            }
                            b',' => {
                                if frames_depth == 0 {
                                    State::Error
                                } else {
                                    match frames_buf[frames_depth - 1] {
                                        FrameKind::Array => {
                                            after_comma = true;
                                            State::ArrayStart
                                        }
                                        FrameKind::Object => {
                                            after_comma = true;
                                            State::ObjectStart
                                        }
                                    }
                                }
                            }
                            _ => State::AfterValue, // whitespace delimiter
                        }
                    }
                }

                State::Error => break 'inner,

                State::ObjectStart => {
                    let ahead = (!byte_state.whitespace) >> chunk_offset;
                    let skip = ahead.trailing_zeros() as usize;
                    chunk_offset += skip;
                    if chunk_offset >= chunk_len {
                        break 'inner;
                    }
                    let byte = chunk[chunk_offset];
                    match byte {
                        b'"' => {
                            after_comma = false;
                            str_start = pos + chunk_offset + 1;
                            str_escaped = false;
                            bs_count = 0;
                            State::KeyChars
                        }
                        b'}' => {
                            if after_comma {
                                State::Error
                            } else if frames_depth > 0
                                && frames_buf[frames_depth - 1] == FrameKind::Object
                            {
                                frames_depth -= 1;
                                writer.end_object();
                                State::AfterValue
                            } else {
                                State::Error
                            }
                        }
                        _ => State::Error,
                    }
                }

                State::ArrayStart => {
                    let ahead = (!byte_state.whitespace) >> chunk_offset;
                    let skip = ahead.trailing_zeros() as usize;
                    chunk_offset += skip;
                    if chunk_offset >= chunk_len {
                        break 'inner;
                    }
                    let byte = chunk[chunk_offset];
                    match byte {
                        b']' => {
                            if after_comma {
                                State::Error
                            } else if frames_depth > 0
                                && frames_buf[frames_depth - 1] == FrameKind::Array
                            {
                                frames_depth -= 1;
                                writer.end_array();
                                State::AfterValue
                            } else {
                                State::Error
                            }
                        }
                        b'{' => {
                            after_comma = false;
                            if frames_depth >= MAX_JSON_DEPTH {
                                State::Error
                            } else {
                                frames_buf[frames_depth] = FrameKind::Object;
                                frames_depth += 1;
                                writer.start_object();
                                State::ObjectStart
                            }
                        }
                        b'[' => {
                            after_comma = false;
                            if frames_depth >= MAX_JSON_DEPTH {
                                State::Error
                            } else {
                                frames_buf[frames_depth] = FrameKind::Array;
                                frames_depth += 1;
                                writer.start_array();
                                State::ArrayStart
                            }
                        }
                        b'"' => {
                            after_comma = false;
                            str_start = pos + chunk_offset + 1;
                            str_escaped = false;
                            bs_count = 0;
                            State::StringChars
                        }
                        _ => {
                            after_comma = false;
                            atom_start = pos + chunk_offset;
                            State::AtomChars
                        }
                    }
                }

                State::AfterValue => {
                    let ahead = (!byte_state.whitespace) >> chunk_offset;
                    let skip = ahead.trailing_zeros() as usize;
                    chunk_offset += skip;
                    if chunk_offset >= chunk_len {
                        break 'inner;
                    }
                    let byte = chunk[chunk_offset];
                    match byte {
                        b',' => {
                            if frames_depth == 0 {
                                State::Error
                            } else {
                                match frames_buf[frames_depth - 1] {
                                    FrameKind::Object => {
                                        after_comma = true;
                                        State::ObjectStart
                                    }
                                    FrameKind::Array => {
                                        after_comma = true;
                                        State::ArrayStart
                                    }
                                }
                            }
                        }
                        b'}' => {
                            if frames_depth > 0 && frames_buf[frames_depth - 1] == FrameKind::Object
                            {
                                frames_depth -= 1;
                                writer.end_object();
                                State::AfterValue
                            } else {
                                State::Error
                            }
                        }
                        b']' => {
                            if frames_depth > 0 && frames_buf[frames_depth - 1] == FrameKind::Array
                            {
                                frames_depth -= 1;
                                writer.end_array();
                                State::AfterValue
                            } else {
                                State::Error
                            }
                        }
                        _ => State::Error,
                    }
                }
            };
            chunk_offset += 1;
        }
        pos += chunk_len;
    }

    // Flush a trailing atom not followed by a delimiter (e.g. top-level `42`).
    if state == State::AtomChars {
        if !write_atom(&src[atom_start..], &mut writer) {
            return None;
        }
    } else if state != State::AfterValue {
        return None;
    }

    if state == State::Error {
        return None;
    }

    // Unclosed objects or arrays.
    if frames_depth != 0 {
        return None;
    }

    writer.finish()
}

/// Decode all JSON string escape sequences within `s` (the raw content between
/// the opening and closing quotes, with no surrounding quotes).  Clears `out`
/// and writes the decoded text into it.
///
/// Supported escapes: `\"` `\\` `\/` `\b` `\f` `\n` `\r` `\t` `\uXXXX`
/// (including surrogate pairs).  Unknown escapes are passed through verbatim.
#[doc(hidden)]
#[unsafe(no_mangle)]
#[inline(never)]
pub fn unescape_str(s: &str, out: &mut String) {
    out.clear();
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'\\' {
            // Copy one UTF-8 character verbatim.
            let ch = s[i..].chars().next().unwrap();
            out.push(ch);
            i += ch.len_utf8();
            continue;
        }
        // Skip the backslash.
        i += 1;
        if i >= bytes.len() {
            break;
        }
        match bytes[i] {
            b'"' => {
                out.push('"');
                i += 1;
            }
            b'\\' => {
                out.push('\\');
                i += 1;
            }
            b'/' => {
                out.push('/');
                i += 1;
            }
            b'b' => {
                out.push('\x08');
                i += 1;
            }
            b'f' => {
                out.push('\x0C');
                i += 1;
            }
            b'n' => {
                out.push('\n');
                i += 1;
            }
            b'r' => {
                out.push('\r');
                i += 1;
            }
            b't' => {
                out.push('\t');
                i += 1;
            }
            b'u' => {
                i += 1; // skip 'u'
                if i + 4 <= bytes.len() {
                    if let Ok(hi) = u16::from_str_radix(&s[i..i + 4], 16) {
                        i += 4;
                        // Surrogate pair: high surrogate \uD800-\uDBFF + low \uDC00-\uDFFF.
                        if (0xD800..0xDC00).contains(&hi)
                            && i + 6 <= bytes.len()
                            && bytes[i] == b'\\'
                            && bytes[i + 1] == b'u'
                        {
                            if let Ok(lo) = u16::from_str_radix(&s[i + 2..i + 6], 16) {
                                if (0xDC00..=0xDFFF).contains(&lo) {
                                    let cp = 0x1_0000u32
                                        + ((hi as u32 - 0xD800) << 10)
                                        + (lo as u32 - 0xDC00);
                                    if let Some(ch) = char::from_u32(cp) {
                                        out.push(ch);
                                        i += 6;
                                        continue;
                                    }
                                }
                            }
                        }
                        if let Some(ch) = char::from_u32(hi as u32) {
                            out.push(ch);
                        }
                    }
                }
                // i was already advanced past uXXXX inside the block above.
            }
            b => {
                out.push('\\');
                out.push(b as char);
                i += 1;
            }
        }
    }
}

/// Per-chunk classification masks produced by the classifier functions.
#[repr(C)]
#[derive(Debug, PartialEq)]
pub struct ByteState {
    whitespace: u64,  // bit n set => byte n is whitespace (<= 0x20)
    quotes: u64,      // bit n set => byte n is '"'
    backslashes: u64, // bit n set => byte n is '\\'
    delimiters: u64,  // bit n set => byte n ends an atom (whitespace | ',' | '}' | ']')
}

// ---------------------------------------------------------------------------
// U64 (portable SWAR) — 8 × u64 words, no SIMD
// ---------------------------------------------------------------------------

/// Classify up to 64 bytes purely in software using SWAR
/// (SIMD Within A Register) bit-manipulation on eight `u64` words.
/// The Rust parse path always uses this classifier.
///
/// Three tricks are used:
///
/// * **Whitespace (`byte ≤ 0x20`)**: mask off the high bit with `v & 0x7f…`,
///   then add `0x5f` per byte.  The sum overflows into bit 7 exactly when the
///   original byte is ≥ 0x21; OR-ing back the original high bit excludes
///   bytes ≥ 0x80 (not whitespace).  Invert and mask to get the flag.
///
/// * **Byte equality**: XOR the word with a broadcast of the target byte
///   (`b * 0x0101_0101_0101_0101`), leaving zero in every matching byte.
///   Detect zeros with the carry-free formula
///   `!( ((x & 0x7f…) + 0x7f…) | x ) & 0x8080…`, which avoids the false
///   positives of the classic `(v−0x01…) & ∼v & 0x80…` trick (that trick
///   flags a byte with value 0x01 when the immediately preceding byte is
///   zero, causing a spurious `"` detection for e.g. `"#`).
///
/// * **Movemask**: collect the MSB of each byte into the low 8 bits by
///   multiplying `(v & 0x8080…)` by `0x0002_0408_1020_4081` and taking the
///   top byte (shift right 56).
fn classify_u64(src: &[u8]) -> ByteState {
    assert!(!src.is_empty() && src.len() <= 64);
    let mut buf = [0u8; 64];
    buf[..src.len()].copy_from_slice(src);

    /// Produce a u64 with bit 7 of each byte set where that byte equals `b`.
    ///
    /// Uses a carry-free formula that has no false positives, unlike the
    /// classic `(v - 0x01…) & !v & 0x80…` trick which falsely flags a byte
    /// with value 0x01 when the preceding byte is zero.
    #[inline(always)]
    fn eq_byte(v: u64, b: u8) -> u64 {
        let x = v ^ (b as u64 * 0x0101_0101_0101_0101_u64);
        // (x & 0x7f…) + 0x7f… sets bit 7 iff the low-7 bits of x are non-zero;
        // OR-ing with x itself propagates bit 7 if the byte was 0x80.
        // Inverting and masking gives bit 7 set exactly where x == 0.
        !(((x & 0x7f7f_7f7f_7f7f_7f7f_u64).wrapping_add(0x7f7f_7f7f_7f7f_7f7f_u64)) | x)
            & 0x8080_8080_8080_8080_u64
    }

    /// Collect the MSB of each byte into the low 8 bits.
    #[inline(always)]
    fn movemask8(v: u64) -> u8 {
        ((v & 0x8080_8080_8080_8080_u64).wrapping_mul(0x0002_0408_1020_4081_u64) >> 56) as u8
    }

    let mut ws = [0u8; 8];
    let mut q = [0u8; 8];
    let mut bs = [0u8; 8];
    let mut dl = [0u8; 8];

    for i in 0..8 {
        let v = u64::from_le_bytes(buf[i * 8..][..8].try_into().unwrap());

        // Whitespace: byte ≤ 0x20.
        // (v & 0x7f…) + 0x5f… overflows into bit 7 iff byte ≥ 0x21 (low-7 range);
        // OR-ing the original v excludes bytes ≥ 0x80.
        let masked = v & 0x7f7f_7f7f_7f7f_7f7f_u64;
        let sum = masked.wrapping_add(0x5f5f_5f5f_5f5f_5f5f_u64);
        let w = !(sum | v) & 0x8080_8080_8080_8080_u64;

        let quotes = eq_byte(v, b'"');
        let backslashes = eq_byte(v, b'\\');
        let commas = eq_byte(v, b',');
        let cl_brace = eq_byte(v, b'}');
        let cl_bracket = eq_byte(v, b']');
        let delims = w | commas | cl_brace | cl_bracket;

        ws[i] = movemask8(w);
        q[i] = movemask8(quotes);
        bs[i] = movemask8(backslashes);
        dl[i] = movemask8(delims);
    }

    ByteState {
        whitespace: u64::from_le_bytes(ws),
        quotes: u64::from_le_bytes(q),
        backslashes: u64::from_le_bytes(bs),
        delimiters: u64::from_le_bytes(dl),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // -----------------------------------------------------------------------
    // zmm_tape correctness: compare parse_to_dom_zmm against the Rust
    // reference parser across a range of JSON inputs.
    // -----------------------------------------------------------------------

    #[cfg(target_arch = "x86_64")]
    fn zmm_dom_matches(src: &str) {
        if !is_x86_feature_detected!("avx512bw") {
            return;
        }
        let ref_tape =
            parse_to_dom(src, None).unwrap_or_else(|| panic!("reference rejected: {src:?}"));
        let asm_tape = unsafe { parse_to_dom_zmm(src, None) }
            .unwrap_or_else(|| panic!("zmm_tape rejected: {src:?}"));
        assert_eq!(
            ref_tape.entries, asm_tape.entries,
            "tape mismatch for {src:?}"
        );
    }

    #[cfg(target_arch = "x86_64")]
    fn zmm_dom_rejects(src: &str) {
        if !is_x86_feature_detected!("avx512bw") {
            return;
        }
        assert!(
            unsafe { parse_to_dom_zmm(src, None) }.is_none(),
            "zmm_tape should reject {src:?}"
        );
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn zmm_dom_atoms() {
        for src in &[
            "null",
            "true",
            "false",
            "0",
            "42",
            "-7",
            "3.14",
            "1e10",
            "-0.5e-3",
            // SWAR fast-path boundary cases: pure integers up to 8 bytes
            "1",
            "12",
            "123",
            "1234",
            "12345",
            "123456",
            "1234567",
            "12345678",
            // Integers just beyond 8 bytes (validator path)
            "123456789",
        ] {
            zmm_dom_matches(src);
        }
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn zmm_dom_strings() {
        for src in &[
            r#""hello""#,
            r#""""#,
            r#""with \"escape\"""#,
            r#""newline\nand\ttab""#,
            r#""\u0041\u0042\u0043""#,
            r#""\u0000""#,
            r#""surrogate \uD83D\uDE00""#,
        ] {
            zmm_dom_matches(src);
        }
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn zmm_dom_simple_object() {
        zmm_dom_matches(r#"{"x":1}"#);
        zmm_dom_matches(r#"{"a":1,"b":2,"c":3}"#);
        zmm_dom_matches(r#"{}"#);
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn zmm_dom_simple_array() {
        zmm_dom_matches(r#"[1,2,3]"#);
        zmm_dom_matches(r#"[]"#);
        zmm_dom_matches(r#"[null,true,false,"x",42]"#);
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn zmm_dom_nested() {
        zmm_dom_matches(r#"{"a":{"b":[1,true,null]}}"#);
        zmm_dom_matches(r#"[[1,[2,[3]]]]"#);
        zmm_dom_matches(r#"{"k":{"k":{"k":{}}}}"#);
        zmm_dom_matches(r#"[{"a":1},{"b":2}]"#);
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn zmm_dom_escaped_keys() {
        zmm_dom_matches(r#"{"key\nname":1}"#);
        zmm_dom_matches(r#"{"key\u0041":true}"#);
        zmm_dom_matches(r#"{"a\"b":null}"#);
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn zmm_dom_whitespace() {
        zmm_dom_matches("  { \"x\" : 1 }  ");
        zmm_dom_matches("[ 1 , 2 , 3 ]");
        zmm_dom_matches("\t\r\nnull\t\r\n");
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn zmm_dom_long_string() {
        // String that spans more than one 64-byte chunk.
        let long = format!(r#""{}""#, "a".repeat(200));
        zmm_dom_matches(&long);
        let long_esc = format!(r#""{}\n{}""#, "b".repeat(100), "c".repeat(100));
        zmm_dom_matches(&long_esc);
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn zmm_dom_reject_invalid() {
        zmm_dom_rejects("");
        zmm_dom_rejects("{");
        zmm_dom_rejects("[");
        zmm_dom_rejects("}");
        zmm_dom_rejects(r#"{"a":}"#);
        zmm_dom_rejects(r#"{"a":1"#);
        // Leading zeros must be rejected (SWAR fast path must not bypass this).
        zmm_dom_rejects("01");
        zmm_dom_rejects("00");
        zmm_dom_rejects("007");
        zmm_dom_rejects("01234567"); // exactly 8 bytes, leading zero
    }

    // -----------------------------------------------------------------------
    // parse_with_zmm SAX: compare against the Rust reference on escape inputs.
    // -----------------------------------------------------------------------

    #[cfg(target_arch = "x86_64")]
    fn zmm_sax_matches(src: &str) {
        // Collect events from both parsers into a comparable string.
        #[derive(Default)]
        struct EventLog(String);

        impl<'s> Sax<'s> for EventLog {
            type Output = String;
            fn null(&mut self) {
                self.0.push_str("null;");
            }
            fn bool_val(&mut self, v: bool) {
                self.0.push_str(if v { "true;" } else { "false;" });
            }
            fn number(&mut self, s: &str) {
                self.0.push_str(s);
                self.0.push(';');
            }
            fn string(&mut self, s: &str) {
                self.0.push_str("s:");
                self.0.push_str(s);
                self.0.push(';');
            }
            fn escaped_string(&mut self, s: &str) {
                self.0.push_str("es:");
                self.0.push_str(s);
                self.0.push(';');
            }
            fn key(&mut self, s: &str) {
                self.0.push_str("k:");
                self.0.push_str(s);
                self.0.push(';');
            }
            fn escaped_key(&mut self, s: &str) {
                self.0.push_str("ek:");
                self.0.push_str(s);
                self.0.push(';');
            }
            fn start_object(&mut self) {
                self.0.push('{');
            }
            fn end_object(&mut self) {
                self.0.push('}');
            }
            fn start_array(&mut self) {
                self.0.push('[');
            }
            fn end_array(&mut self) {
                self.0.push(']');
            }
            fn finish(self) -> Option<String> {
                Some(self.0)
            }
        }

        if !is_x86_feature_detected!("avx512bw") {
            return;
        }
        let ref_log = parse_with(src, EventLog::default())
            .unwrap_or_else(|| panic!("reference rejected: {src:?}"));
        let asm_log = unsafe { parse_with_zmm(src, EventLog::default()) }
            .unwrap_or_else(|| panic!("parse_with_zmm rejected: {src:?}"));
        assert_eq!(ref_log, asm_log, "event log mismatch for {src:?}");
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn zmm_sax_escaped_strings() {
        // Single-backslash escapes and \uXXXX — the assembly handles these correctly.
        zmm_sax_matches(r#"{"key":"\n\t\r\""}"#);
        zmm_sax_matches(r#"{"key\nname":"val\u0041"}"#);
        zmm_sax_matches(r#"["\u0041","\u0042\u0043"]"#);
        zmm_sax_matches(r#"{"a\"b":"c\"d"}"#);
        // String that spans more than one 64-byte chunk and contains an escape.
        let long = format!(r#"{{"{}\n":"{}\t"}}"#, "x".repeat(70), "y".repeat(70));
        zmm_sax_matches(&long);
        // Note: inputs with even runs of backslashes before a closing quote (e.g.
        // `\\"`) require the parity-counting fix in the assembly too; tested via
        // parse_with in rust_even_backslash_before_quote below.
    }

    // Rust-path-only test for even backslash runs before a closing quote.
    // The assembly SAX path has not yet been updated to count backslash parity,
    // so this test drives parse_to_dom (SWAR) directly.
    #[test]
    fn rust_even_backslash_before_quote() {
        use crate::JsonRef;
        // `\\` = one literal backslash, then `"` terminates string → decoded = `\`
        let t = parse_to_dom(r#"{"k":"\\"}"#, None).expect("parse failed");
        assert_eq!(t.root().get("k").as_str(), Some("\\"));
        // `\\\\` = two literal backslashes → decoded = `\\`
        let t = parse_to_dom(r#"{"k":"\\\\"}"#, None).expect("parse failed");
        assert_eq!(t.root().get("k").as_str(), Some("\\\\"));
        // `\\` inside array
        let t = parse_to_dom(r#"["\\"]"#, None).expect("parse failed");
        assert_eq!(t.root().index_at(0).as_str(), Some("\\"));
        // Mixed content: `abc\\` followed by closing quote → decoded = `abc\`
        let t = parse_to_dom(r#"{"k":"abc\\"}"#, None).expect("parse failed");
        assert_eq!(t.root().get("k").as_str(), Some("abc\\"));
        // Three backslashes before `"`: `\\` escapes itself, `\"` escapes the quote.
        // So `\\\"` does NOT close the string; the outer `"` closes it.
        // Decoded value = `\"` (backslash + quote).
        let t = parse_to_dom("{\"k\":\"\\\\\\\"\"}", None).expect("parse failed");
        assert_eq!(t.root().get("k").as_str(), Some("\\\""));
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn zmm_dom_overflow_retry() {
        if !is_x86_feature_detected!("avx512bw") {
            return;
        }
        // A 200-element array of objects produces ~800+ tape entries.
        // Initial capacity is src.len()/4 which is far smaller, so the
        // function must handle at least one TapeOverflow retry automatically.
        let big: String = {
            let mut s = String::from("[");
            for i in 0..200u32 {
                if i > 0 {
                    s.push(',');
                }
                s.push_str(&format!(r#"{{"k":{i}}}"#));
            }
            s.push(']');
            s
        };
        // Use Some(4) to guarantee at least one overflow retry regardless of input size.
        let tape =
            unsafe { parse_to_dom_zmm(&big, Some(4)) }.expect("overflow retry should succeed");
        assert_eq!(tape.root().unwrap().array_iter().unwrap().count(), 200);
    }

    /// Regression test for the carry-borrow false positive in `eq_byte`.
    ///
    /// The classic `has_zero_byte` SWAR trick falsely reports byte `n` as a
    /// match when byte `n-1` is a genuine match (XORs to 0x00) *and* byte `n`
    /// XORs to 0x01.  For `target = '"'` (0x22), this fires whenever `'#'`
    /// (0x23 = 0x22 + 1) appears immediately after a real `'"'`, causing the
    /// parser to see a phantom closing quote inside the string.
    ///
    /// All three inputs below are the exact tokens from
    /// `Qwen2.5-0.5B/tokenizer.json` that were rejected by the pre-fix parser.
    #[test]
    fn swar_eq_byte_quote_false_positive_regression() {
        use crate::JsonRef;

        // `"#\\"` — '#' at chunk position 1 is 0x22+1, false positive fires
        // at position 1 of the first chunk.  Decoded value: `#\`
        let dom = parse_to_dom("\"#\\\\\"", None).expect("\"#\\\\\" should parse");
        assert_eq!(dom.root().as_str(), Some("#\\"));

        // `"# \\"` — same root cause; '#' at position 1.  Decoded value: `# \`
        let dom = parse_to_dom("\"# \\\\\"", None).expect("\"# \\\\\" should parse");
        assert_eq!(dom.root().as_str(), Some("# \\"));

        // `"=\\\""#"` — the genuine '"' lands at chunk position 5; '#' at
        // position 6 triggers the false positive mid-string.
        // Decoded value: `=\"#`
        let dom = parse_to_dom("\"=\\\\\\\"#\"", None).expect("\"=\\\\\\\"#\\\" should parse");
        assert_eq!(dom.root().as_str(), Some("=\\\"#"));
    }

    /// SAX-path regression for the same `eq_byte` false-positive bug.
    ///
    /// Uses `parse_with` to drive a minimal [`Sax`] writer and confirms that
    /// the events emitted for each of the three formerly-failing inputs carry
    /// the correct raw (still-escaped) string content.
    #[test]
    fn swar_eq_byte_quote_false_positive_regression_sax() {
        struct Capture(Option<String>);

        impl<'s> Sax<'s> for Capture {
            type Output = String;

            fn null(&mut self) {}
            fn bool_val(&mut self, _: bool) {}
            fn number(&mut self, _: &str) {}
            fn string(&mut self, s: &str) {
                self.0 = Some(s.to_owned());
            }
            fn escaped_string(&mut self, s: &str) {
                self.0 = Some(s.to_owned());
            }
            fn key(&mut self, _: &str) {}
            fn escaped_key(&mut self, _: &str) {}
            fn start_object(&mut self) {}
            fn end_object(&mut self) {}
            fn start_array(&mut self) {}
            fn end_array(&mut self) {}
            fn finish(self) -> Option<String> {
                self.0
            }
        }

        // Each tuple: (raw JSON input, expected raw content between the quotes)
        let cases: &[(&str, &str)] = &[
            ("\"#\\\\\"", "#\\\\"),
            ("\"# \\\\\"", "# \\\\"),
            ("\"=\\\\\\\"#\"", "=\\\\\\\"#"),
        ];

        for &(src, expected_raw) in cases {
            let got = parse_with(src, Capture(None))
                .unwrap_or_else(|| panic!("parse_with rejected {src:?}"));
            assert_eq!(got, expected_raw, "SAX raw string mismatch for {src:?}");
        }
    }
}
