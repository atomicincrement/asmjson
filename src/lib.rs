#![doc = include_str!("../README.md")]

use std::borrow::Cow;

pub mod json_ref;
pub mod tape;
pub mod value;

pub use json_ref::JsonRef;
pub use tape::{Tape, TapeEntry, TapeRef};
pub use value::Value;

use tape::TapeWriter;
use value::{ValueWriter, is_valid_json_number};

// ---------------------------------------------------------------------------
// Optional state-entry statistics (compiled in with --features stats).
// ---------------------------------------------------------------------------

#[cfg(feature = "stats")]
pub mod stats {
    use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

    pub static VALUE_WHITESPACE: AtomicU64 = AtomicU64::new(0);
    pub static STRING_CHARS: AtomicU64 = AtomicU64::new(0);
    pub static STRING_ESCAPE: AtomicU64 = AtomicU64::new(0);
    pub static KEY_CHARS: AtomicU64 = AtomicU64::new(0);
    pub static KEY_ESCAPE: AtomicU64 = AtomicU64::new(0);
    pub static KEY_END: AtomicU64 = AtomicU64::new(0);
    pub static AFTER_COLON: AtomicU64 = AtomicU64::new(0);
    pub static ATOM_CHARS: AtomicU64 = AtomicU64::new(0);
    pub static OBJECT_START: AtomicU64 = AtomicU64::new(0);
    pub static ARRAY_START: AtomicU64 = AtomicU64::new(0);
    pub static AFTER_VALUE: AtomicU64 = AtomicU64::new(0);

    pub fn reset() {
        for s in all() {
            s.store(0, Relaxed);
        }
    }

    fn all() -> [&'static AtomicU64; 11] {
        [
            &VALUE_WHITESPACE,
            &STRING_CHARS,
            &STRING_ESCAPE,
            &KEY_CHARS,
            &KEY_ESCAPE,
            &KEY_END,
            &AFTER_COLON,
            &ATOM_CHARS,
            &OBJECT_START,
            &ARRAY_START,
            &AFTER_VALUE,
        ]
    }

    pub struct StateStats {
        pub value_whitespace: u64,
        pub string_chars: u64,
        pub string_escape: u64,
        pub key_chars: u64,
        pub key_escape: u64,
        pub key_end: u64,
        pub after_colon: u64,
        pub atom_chars: u64,
        pub object_start: u64,
        pub array_start: u64,
        pub after_value: u64,
    }

    pub fn get() -> StateStats {
        StateStats {
            value_whitespace: VALUE_WHITESPACE.load(Relaxed),
            string_chars: STRING_CHARS.load(Relaxed),
            string_escape: STRING_ESCAPE.load(Relaxed),
            key_chars: KEY_CHARS.load(Relaxed),
            key_escape: KEY_ESCAPE.load(Relaxed),
            key_end: KEY_END.load(Relaxed),
            after_colon: AFTER_COLON.load(Relaxed),
            atom_chars: ATOM_CHARS.load(Relaxed),
            object_start: OBJECT_START.load(Relaxed),
            array_start: ARRAY_START.load(Relaxed),
            after_value: AFTER_VALUE.load(Relaxed),
        }
    }
}

/// Increment a state counter when the `stats` feature is enabled; a no-op otherwise.
macro_rules! stat {
    ($counter:path) => {
        #[cfg(feature = "stats")]
        $counter.fetch_add(1, ::std::sync::atomic::Ordering::Relaxed);
    };
}

#[derive(PartialEq)]
enum State {
    // Waiting for the first byte of any JSON value.
    ValueWhitespace,

    // Inside a quoted string value.
    StringChars,
    // After a `\` inside a string value; next byte is consumed unconditionally.
    StringEscape,

    // Inside a key string (left-hand side of an object member).
    KeyChars,
    // After a `\` inside a key string.
    KeyEscape,
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

enum FrameKind {
    Object,
    Array,
}

// ---------------------------------------------------------------------------
// JsonWriter trait — SAX-style event sink
// ---------------------------------------------------------------------------

/// Receives a stream of structural events as the parser walks the input.
///
/// Implement this trait to produce any output from a single pass over the JSON
/// source.  Two built-in implementations are provided — the one used by
/// [`parse_json`] (producing a nested [`Value`] tree) and the one used by
/// [`parse_to_tape`] (producing a flat [`Tape`]).
///
/// A custom writer can use [`parse_with`] to drive the parse.
pub trait JsonWriter<'src> {
    /// The type returned by [`finish`](JsonWriter::finish).
    type Output;

    /// A `null` literal was parsed.
    fn null(&mut self);
    /// A `true` or `false` literal was parsed.
    fn bool_val(&mut self, v: bool);
    /// A JSON number; `s` is a slice of the original source string.
    fn number(&mut self, s: &'src str);
    /// A JSON string value (borrowed when escape-free, owned otherwise).
    fn string(&mut self, s: Cow<'src, str>);
    /// An object key; always immediately followed by the key's value events.
    fn key(&mut self, s: Cow<'src, str>);
    /// Opening `{` of an object.
    fn start_object(&mut self);
    /// Closing `}` of an object.
    fn end_object(&mut self);
    /// Opening `[` of an array.
    fn start_array(&mut self);
    /// Closing `]` of an array.
    fn end_array(&mut self);
    /// Called once after the last token; returns the final output or `None` on
    /// internal error.
    fn finish(self) -> Option<Self::Output>;
}

// ---------------------------------------------------------------------------
// Atom helper — writes a number / bool / null through any JsonWriter
// ---------------------------------------------------------------------------

fn write_atom<'a, W: JsonWriter<'a>>(s: &'a str, w: &mut W) -> bool {
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

/// Parse `src` into a [`Value`] tree using the given classifier.
///
/// Returns `None` if the input is not valid JSON.
///
/// ```rust
/// use asmjson::{parse_json, choose_classifier, JsonRef};
/// let v = parse_json(r#"[1, "two", true]"#, choose_classifier()).unwrap();
/// assert_eq!(v.index_at(1).as_str(), Some("two"));
/// ```
pub fn parse_json<'a>(src: &'a str, classify: ClassifyFn) -> Option<Value<'a>> {
    parse_with(src, classify, ValueWriter::new())
}

/// Parse `src` into a flat [`Tape`] using the given classifier.
///
/// Returns `None` if the input is not valid JSON.
///
/// The tape is more efficient than a [`Value`] tree for large inputs because it
/// avoids recursive allocation.  `StartObject(n)` / `StartArray(n)` entries
/// carry the index of the matching closer so entire subtrees can be skipped in
/// O(1).  Access the tape via [`Tape::root`] which returns a [`TapeRef`] cursor
/// that implements [`JsonRef`].
///
/// ```rust
/// use asmjson::{parse_to_tape, choose_classifier, JsonRef};
/// let tape = parse_to_tape(r#"{"x":1}"#, choose_classifier()).unwrap();
/// assert_eq!(tape.root().get("x").as_i64(), Some(1));
/// ```
pub fn parse_to_tape<'a>(src: &'a str, classify: ClassifyFn) -> Option<Tape<'a>> {
    parse_with(src, classify, TapeWriter::new())
}

/// Parse `src` using a custom [`JsonWriter`], returning its output.
///
/// This is the generic entry point: supply your own writer to produce any
/// output in a single pass over the source.
pub fn parse_with<'a, W: JsonWriter<'a>>(
    src: &'a str,
    classify: ClassifyFn,
    writer: W,
) -> Option<W::Output> {
    parse_json_impl(src, classify, writer)
}

fn parse_json_impl<'a, F, W>(src: &'a str, classify: F, mut writer: W) -> Option<W::Output>
where
    F: Fn(&[u8]) -> ByteState,
    W: JsonWriter<'a>,
{
    let bytes = src.as_bytes();
    let mut frames: Vec<FrameKind> = Vec::new();
    let mut str_start: usize = 0; // absolute byte offset of char after opening '"'
    let mut str_escaped = false; // true if the current string contained a backslash escape
    let mut atom_start: usize = 0; // absolute byte offset of first atom byte
    let mut current_key: Cow<'a, str> = Cow::Borrowed(""); // key slice captured when KeyChars closes
    let mut after_comma = false; // true when ObjectStart/ArrayStart was reached via a `,`
    let mut state = State::ValueWhitespace;

    let mut pos = 0;
    while pos < bytes.len() {
        let chunk_len = (bytes.len() - pos).min(64);
        let chunk = &bytes[pos..pos + chunk_len];
        let byte_state = classify(chunk);

        let mut chunk_offset = 0;
        'inner: while chunk_offset < chunk_len {
            state = match state {
                State::ValueWhitespace => {
                    stat!(crate::stats::VALUE_WHITESPACE);
                    let ahead = (!byte_state.whitespace) >> chunk_offset;
                    let skip = ahead.trailing_zeros() as usize;
                    chunk_offset += skip;
                    if chunk_offset >= chunk_len {
                        break 'inner;
                    }
                    let byte = chunk[chunk_offset];
                    match byte {
                        b'{' => {
                            frames.push(FrameKind::Object);
                            writer.start_object();
                            State::ObjectStart
                        }
                        b'[' => {
                            frames.push(FrameKind::Array);
                            writer.start_array();
                            State::ArrayStart
                        }
                        b'"' => {
                            str_start = pos + chunk_offset + 1;
                            str_escaped = false;
                            State::StringChars
                        }
                        _ => {
                            atom_start = pos + chunk_offset;
                            State::AtomChars
                        }
                    }
                }

                State::StringChars => {
                    stat!(crate::stats::STRING_CHARS);
                    let unescaped_quotes = byte_state.quotes & !(byte_state.backslashes << 1);
                    let interesting = (byte_state.backslashes | unescaped_quotes) >> chunk_offset;
                    let skip = interesting.trailing_zeros() as usize;
                    chunk_offset = (chunk_offset + skip).min(chunk_len);
                    if chunk_offset >= chunk_len {
                        break 'inner;
                    }
                    let byte = chunk[chunk_offset];
                    match byte {
                        b'\\' => State::StringEscape,
                        b'"' => {
                            let raw = &src[str_start..pos + chunk_offset];
                            let cow = if str_escaped {
                                Cow::Owned(unescape_str(raw))
                            } else {
                                Cow::Borrowed(raw)
                            };
                            writer.string(cow);
                            State::AfterValue
                        }
                        _ => State::StringChars,
                    }
                }
                State::StringEscape => {
                    stat!(crate::stats::STRING_ESCAPE);
                    str_escaped = true;
                    State::StringChars
                }

                State::KeyChars => {
                    stat!(crate::stats::KEY_CHARS);
                    let unescaped_quotes = byte_state.quotes & !(byte_state.backslashes << 1);
                    let interesting = (byte_state.backslashes | unescaped_quotes) >> chunk_offset;
                    let skip = interesting.trailing_zeros() as usize;
                    chunk_offset = (chunk_offset + skip).min(chunk_len);
                    if chunk_offset >= chunk_len {
                        break 'inner;
                    }
                    let byte = chunk[chunk_offset];
                    match byte {
                        b'\\' => State::KeyEscape,
                        b'"' => {
                            let raw = &src[str_start..pos + chunk_offset];
                            current_key = if str_escaped {
                                Cow::Owned(unescape_str(raw))
                            } else {
                                Cow::Borrowed(raw)
                            };
                            State::KeyEnd
                        }
                        _ => State::KeyChars,
                    }
                }
                State::KeyEscape => {
                    stat!(crate::stats::KEY_ESCAPE);
                    str_escaped = true;
                    State::KeyChars
                }
                State::KeyEnd => {
                    stat!(crate::stats::KEY_END);
                    let ahead = (!byte_state.whitespace) >> chunk_offset;
                    let skip = ahead.trailing_zeros() as usize;
                    chunk_offset += skip;
                    if chunk_offset >= chunk_len {
                        break 'inner;
                    }
                    let byte = chunk[chunk_offset];
                    match byte {
                        b':' => {
                            writer.key(std::mem::replace(&mut current_key, Cow::Borrowed("")));
                            State::AfterColon
                        }
                        _ => State::Error,
                    }
                }
                State::AfterColon => {
                    stat!(crate::stats::AFTER_COLON);
                    let ahead = (!byte_state.whitespace) >> chunk_offset;
                    let skip = ahead.trailing_zeros() as usize;
                    chunk_offset += skip;
                    if chunk_offset >= chunk_len {
                        break 'inner;
                    }
                    let byte = chunk[chunk_offset];
                    match byte {
                        b'{' => {
                            frames.push(FrameKind::Object);
                            writer.start_object();
                            State::ObjectStart
                        }
                        b'[' => {
                            frames.push(FrameKind::Array);
                            writer.start_array();
                            State::ArrayStart
                        }
                        b'"' => {
                            str_start = pos + chunk_offset + 1;
                            str_escaped = false;
                            State::StringChars
                        }
                        _ => {
                            atom_start = pos + chunk_offset;
                            State::AtomChars
                        }
                    }
                }

                State::AtomChars => {
                    stat!(crate::stats::ATOM_CHARS);
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
                            b'}' => match frames.pop() {
                                Some(FrameKind::Object) => {
                                    writer.end_object();
                                    State::AfterValue
                                }
                                _ => State::Error,
                            },
                            b']' => match frames.pop() {
                                Some(FrameKind::Array) => {
                                    writer.end_array();
                                    State::AfterValue
                                }
                                _ => State::Error,
                            },
                            b',' => match frames.last() {
                                Some(FrameKind::Array) => {
                                    after_comma = true;
                                    State::ArrayStart
                                }
                                Some(FrameKind::Object) => {
                                    after_comma = true;
                                    State::ObjectStart
                                }
                                None => State::Error,
                            },
                            _ => State::AfterValue, // whitespace delimiter
                        }
                    }
                }

                State::Error => break 'inner,

                State::ObjectStart => {
                    stat!(crate::stats::OBJECT_START);
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
                            State::KeyChars
                        }
                        b'}' => {
                            if after_comma {
                                State::Error
                            } else {
                                match frames.pop() {
                                    Some(FrameKind::Object) => {
                                        writer.end_object();
                                        State::AfterValue
                                    }
                                    _ => State::Error,
                                }
                            }
                        }
                        _ => State::Error,
                    }
                }

                State::ArrayStart => {
                    stat!(crate::stats::ARRAY_START);
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
                            } else {
                                match frames.pop() {
                                    Some(FrameKind::Array) => {
                                        writer.end_array();
                                        State::AfterValue
                                    }
                                    _ => State::Error,
                                }
                            }
                        }
                        b'{' => {
                            after_comma = false;
                            frames.push(FrameKind::Object);
                            writer.start_object();
                            State::ObjectStart
                        }
                        b'[' => {
                            after_comma = false;
                            frames.push(FrameKind::Array);
                            writer.start_array();
                            State::ArrayStart
                        }
                        b'"' => {
                            after_comma = false;
                            str_start = pos + chunk_offset + 1;
                            str_escaped = false;
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
                    stat!(crate::stats::AFTER_VALUE);
                    let ahead = (!byte_state.whitespace) >> chunk_offset;
                    let skip = ahead.trailing_zeros() as usize;
                    chunk_offset += skip;
                    if chunk_offset >= chunk_len {
                        break 'inner;
                    }
                    let byte = chunk[chunk_offset];
                    match byte {
                        b',' => match frames.last() {
                            Some(FrameKind::Object) => {
                                after_comma = true;
                                State::ObjectStart
                            }
                            Some(FrameKind::Array) => {
                                after_comma = true;
                                State::ArrayStart
                            }
                            None => State::Error,
                        },
                        b'}' => match frames.pop() {
                            Some(FrameKind::Object) => {
                                writer.end_object();
                                State::AfterValue
                            }
                            _ => State::Error,
                        },
                        b']' => match frames.pop() {
                            Some(FrameKind::Array) => {
                                writer.end_array();
                                State::AfterValue
                            }
                            _ => State::Error,
                        },
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
    if !frames.is_empty() {
        return None;
    }

    writer.finish()
}

/// Decode all JSON string escape sequences within `s` (the raw content between
/// the opening and closing quotes, with no surrounding quotes).  Returns the
/// decoded `String`.
///
/// Supported escapes: `\"` `\\` `\/` `\b` `\f` `\n` `\r` `\t` `\uXXXX`
/// (including surrogate pairs).  Unknown escapes are passed through verbatim.
#[inline(never)]
fn unescape_str(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
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
    out
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

/// Pre-built 64-byte needle vectors for AVX-512 comparisons.
/// Use repr(C) to guarantee the layout is exactly as defined, so we can safely take
/// pointers to the fields for the inline assembly.
#[repr(C)]
struct ByteStateConstants {
    space: [u8; 64],
    quote: [u8; 64],
    backslash: [u8; 64],
    comma: [u8; 64],
    close_brace: [u8; 64],
    close_bracket: [u8; 64],
}

impl ByteStateConstants {
    const fn new() -> Self {
        Self {
            space: [b' '; 64],
            quote: [b'"'; 64],
            backslash: [b'\\'; 64],
            comma: [b','; 64],
            close_brace: [b'}'; 64],
            close_bracket: [b']'; 64],
        }
    }
}

/// Pre-built constant vectors for `classify_zmm`; placed in a `static` so
/// the address is stable and can be handed to inline assembly.
static ZMM_CONSTANTS: ByteStateConstants = ByteStateConstants::new();

// ---------------------------------------------------------------------------
// Classifier wrappers, type alias, and CPUID-based selection
// ---------------------------------------------------------------------------

/// The type of a chunk classifier: takes a 1–64 byte slice and returns the
/// four bitmasks the parser needs.  All three register-width variants share
/// this signature, so the choice can be stored as a plain function pointer.
pub type ClassifyFn = fn(&[u8]) -> ByteState;

// ---------------------------------------------------------------------------
// YMM (AVX2) — 2 × 32-byte registers, 64 bytes total
// ---------------------------------------------------------------------------

/// Classify 64 bytes using 2 × 32-byte YMM registers (AVX2).
///
/// Whitespace detection uses the identity:
///   `unsigned a <= 0x20`  ↔  `max_epu8(a, 0x20) == 0x20`
pub fn classify_ymm(src: &[u8]) -> ByteState {
    #[target_feature(enable = "avx2")]
    unsafe fn imp(src: &[u8]) -> ByteState {
        unsafe {
            use std::arch::x86_64::{
                __m256i, _mm256_cmpeq_epi8, _mm256_loadu_si256, _mm256_max_epu8,
                _mm256_movemask_epi8, _mm256_set1_epi8,
            };
            assert!(!src.is_empty() && src.len() <= 64);

            // Zero-pad to 64 bytes so both 32-byte loads are fully defined.
            let mut buf = [0u8; 64];
            buf[..src.len()].copy_from_slice(src);
            let p = buf.as_ptr();

            let v0 = _mm256_loadu_si256(p.add(0).cast::<__m256i>());
            let v1 = _mm256_loadu_si256(p.add(32).cast::<__m256i>());

            let c_ws = _mm256_set1_epi8(0x20_u8 as i8);
            let c_q = _mm256_set1_epi8(b'"' as i8);
            let c_bs = _mm256_set1_epi8(b'\\' as i8);
            let c_co = _mm256_set1_epi8(b',' as i8);
            let c_cb = _mm256_set1_epi8(b'}' as i8);
            let c_sb = _mm256_set1_epi8(b']' as i8);

            // _mm256_movemask_epi8 returns i32 with all 32 bits used; cast via
            // u32 to zero-extend to u64 without sign-extending.
            macro_rules! movmsk {
                ($x:expr) => {
                    _mm256_movemask_epi8($x) as u32 as u64
                };
            }
            // unsigned a <= 0x20 via max trick: max(a, 0x20) == 0x20 iff a <= 0x20
            macro_rules! ws {
                ($v:expr) => {
                    movmsk!(_mm256_cmpeq_epi8(_mm256_max_epu8($v, c_ws), c_ws))
                };
            }
            macro_rules! eq {
                ($v:expr, $c:expr) => {
                    movmsk!(_mm256_cmpeq_epi8($v, $c))
                };
            }
            // Combine two 32-bit masks into one 64-bit mask.
            macro_rules! combine2 {
                ($m0:expr, $m1:expr) => {
                    $m0 | ($m1 << 32)
                };
            }

            let whitespace = combine2!(ws!(v0), ws!(v1));
            let quotes = combine2!(eq!(v0, c_q), eq!(v1, c_q));
            let backslashes = combine2!(eq!(v0, c_bs), eq!(v1, c_bs));
            let commas = combine2!(eq!(v0, c_co), eq!(v1, c_co));
            let cl_braces = combine2!(eq!(v0, c_cb), eq!(v1, c_cb));
            let cl_brackets = combine2!(eq!(v0, c_sb), eq!(v1, c_sb));
            let delimiters = whitespace | commas | cl_braces | cl_brackets;

            ByteState {
                whitespace,
                quotes,
                backslashes,
                delimiters,
            }
        }
    }
    unsafe { imp(src) }
}

// ---------------------------------------------------------------------------
// ZMM (AVX-512BW) — 1 × 64-byte register
// ---------------------------------------------------------------------------

/// Classify up to 64 bytes from `src` using AVX-512BW.
/// Bytes beyond `src.len()` are zeroed via masked load; their whitespace bits
/// are set to 1 (0 <= 0x20) but are never visited by the inner loop.
pub fn classify_zmm(src: &[u8]) -> ByteState {
    #[target_feature(enable = "avx512bw")]
    unsafe fn imp(src: &[u8]) -> ByteState {
        assert!(!src.is_empty() && src.len() <= 64);
        // Bits 0..len-1 set, rest clear.
        let load_mask: u64 = if src.len() == 64 {
            !0u64
        } else {
            (1u64 << src.len()) - 1
        };
        let whitespace: u64;
        let quotes: u64;
        let backslashes: u64;
        let delimiters: u64;
        // ByteStateConstants layout (each field is [u8; 64]):
        //   +  0 : space
        //   + 64 : quote
        //   +128 : backslash
        //   +192 : comma
        //   +256 : close_brace
        //   +320 : close_bracket
        unsafe {
            std::arch::asm!(
                // Masked byte load: only load src.len() bytes, zero the rest.
                "kmovq k1, {load_mask}",
                "vmovdqu8 zmm0 {{k1}}{{z}}, zmmword ptr [{src}]",
                // Issue all six comparisons into distinct k registers so the CPU
                // can execute them in parallel, then move the results to GP
                // registers as a batch at the end.
                "vpcmpub  k2, zmm0, zmmword ptr [{base}      ], 2", // whitespace (<= 0x20) : space   +  0
                "vpcmpeqb k3, zmm0, zmmword ptr [{base} +  64]",    // quotes               : quote   + 64
                "vpcmpeqb k4, zmm0, zmmword ptr [{base} + 128]",    // backslashes          : bslash  +128
                "vpcmpeqb k5, zmm0, zmmword ptr [{base} + 192]",    // comma                : comma   +192
                "vpcmpeqb k6, zmm0, zmmword ptr [{base} + 256]",    // '}'                  : cbrace  +256
                "vpcmpeqb k7, zmm0, zmmword ptr [{base} + 320]",    // ']'                  : cbrack  +320
                // Combine delimiter masks in k-registers (no GP round-trip needed).
                "korq k5, k5, k6",   // comma | '}'
                "korq k5, k5, k7",   // | ']'
                "korq k5, k5, k2",   // | whitespace
                // Move all results to GP registers.
                "kmovq {whitespace},  k2",
                "kmovq {quotes},      k3",
                "kmovq {backslashes}, k4",
                "kmovq {delimiters},  k5",
                src         = in(reg)  src.as_ptr(),
                base        = in(reg)  &ZMM_CONSTANTS as *const ByteStateConstants,
                load_mask   = in(reg)  load_mask,
                whitespace  = out(reg) whitespace,
                quotes      = out(reg) quotes,
                backslashes = out(reg) backslashes,
                delimiters  = out(reg) delimiters,
                out("zmm0") _,
                out("k1") _, out("k2") _, out("k3") _,
                out("k4") _, out("k5") _, out("k6") _, out("k7") _,
                options(nostack, readonly),
            );
        }
        ByteState {
            whitespace,
            quotes,
            backslashes,
            delimiters,
        }
    }
    unsafe { imp(src) }
}

// ---------------------------------------------------------------------------
// U64 (portable SWAR) — 8 × u64 words, no SIMD
// ---------------------------------------------------------------------------

/// Classify up to 64 bytes purely in software using SWAR
/// (SIMD Within A Register) bit-manipulation on eight `u64` words.
///
/// Three tricks are used:
///
/// * **Whitespace (`byte ≤ 0x20`)**: mask off the high bit with `v & 0x7f…`,
///   then add `0x5f` per byte.  The sum overflows into bit 7 exactly when the
///   original byte is ≥ 0x21; OR-ing back the original high bit excludes
///   bytes ≥ 0x80 (not whitespace).  Invert and mask to get the flag.
///
/// * **Byte equality**: XOR the word with a broadcast of the target byte
///   (`b * 0x0101_0101_0101_0101`), then test for a zero byte via
///   `(v − 0x0101…) & ∼v & 0x8080…`.
///
/// * **Movemask**: collect the MSB of each byte into the low 8 bits by
///   multiplying `(v & 0x8080…)` by `0x0002_0408_1020_4081` and taking the
///   top byte (shift right 56).
pub fn classify_u64(src: &[u8]) -> ByteState {
    assert!(!src.is_empty() && src.len() <= 64);
    let mut buf = [0u8; 64];
    buf[..src.len()].copy_from_slice(src);

    #[inline(always)]
    fn has_zero_byte(v: u64) -> u64 {
        v.wrapping_sub(0x0101_0101_0101_0101_u64) & !v & 0x8080_8080_8080_8080_u64
    }

    /// Produce a u64 with bit 7 of each byte set where that byte equals `b`.
    #[inline(always)]
    fn eq_byte(v: u64, b: u8) -> u64 {
        has_zero_byte(v ^ (b as u64 * 0x0101_0101_0101_0101_u64))
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

/// Choose the best available classifier for the current CPU using CPUID.
///
/// Call this once at program start (or use it to initialise a `static`) and
/// pass the returned function pointer to every [`parse_json`] call:
///
/// ```ignore
/// let classify = choose_classifier();
/// let value = parse_json(json, classify);
/// ```
///
/// The precedence is: AVX-512BW → AVX2 → portable SWAR u64.
pub fn choose_classifier() -> ClassifyFn {
    #[cfg(target_arch = "x86_64")]
    {
        if std::is_x86_feature_detected!("avx512bw") {
            return classify_zmm;
        }
        if std::is_x86_feature_detected!("avx2") {
            return classify_ymm;
        }
    }
    #[allow(unreachable_code)]
    classify_u64
}

#[cfg(test)]
mod tests {
    use super::*;

    // -----------------------------------------------------------------------
    // Classifier unit tests: compare YMM and U64 bitmasks against ZMM
    // -----------------------------------------------------------------------

    /// Test that all three next_state_* functions produce identical ByteState
    /// for the same 64-byte input, covering all six character classes.
    #[test]
    fn classifier_agreement() {
        let inputs: &[&[u8]] = &[
            // Structural characters
            b"{}[]:,",
            // Quotes and backslashes
            b"\"hello\\world\"",
            // Whitespace (space, tab, CR, LF)
            b"   \t\r\n   ",
            // Mix of everything in one 64-byte chunk
            b"{ \"key\" : \"val\\ue\" , [ 1, true, false, null ] }   \x01",
            // High bytes (must NOT be treated as whitespace)
            b"\x80\x81\x82\xff\xfe\xfd\xaa\xbb",
            // Exactly 64 bytes — exercises the full-chunk path
            b"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            // Short slice (1 byte)
            b"x",
            // Short slice (16 bytes)
            b"0123456789abcdef",
            // Short slice (32 bytes — one YMM register exactly)
            b"0123456789abcdef0123456789abcdef",
        ];

        for &input in inputs {
            // Truncate to max 64 bytes (all are ≤ 64 here)
            let src = &input[..input.len().min(64)];
            let ymm = classify_ymm(src);
            let u64_result = classify_u64(src);

            assert_eq!(
                u64_result, ymm,
                "U64 vs YMM mismatch on input {:?}\n  u64 ws={:#018x} ymm ws={:#018x}",
                input, u64_result.whitespace, ymm.whitespace
            );

            // Only test ZMM when AVX-512BW is available at runtime.
            #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
            if std::is_x86_feature_detected!("avx512bw") {
                let zmm = classify_zmm(src);
                assert_eq!(
                    ymm, zmm,
                    "YMM vs ZMM mismatch on input {:?}\n  ymm ws={:#018x} zmm ws={:#018x}",
                    input, ymm.whitespace, zmm.whitespace
                );
                assert_eq!(
                    u64_result, zmm,
                    "U64 vs ZMM mismatch on input {:?}\n  u64 ws={:#018x} zmm ws={:#018x}",
                    input, u64_result.whitespace, zmm.whitespace
                );
            }
        }
    }
}
