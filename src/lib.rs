//! This module parses JSON strings 64 bytes at a time using AVX-512BW
//! instructions to quickly identify structural characters, enabling entire
//! whitespace runs and string bodies to be skipped in a single operation.
//!
//! Each byte of the input is labelled below with the state that handles it.
//! States that skip whitespace via `trailing_zeros` handle both the whitespace
//! bytes **and** the following dispatch byte in the same loop iteration.
//!
//! ```text
//! { "key1" : "value1" , "key2": [123, 456 , 768], "key3" : { "nested_key" : true} }
//! VOOKKKKKDDCCSSSSSSSFFOOKKKKKDCCRAAARRAAAFRRAAAFOOKKKKKDDCCOOKKKKKKKKKKKDDCCAAAAFF
//! ```
//!
//! State key:
//! * `V` = `ValueWhitespace` — waiting for the first byte of any value
//! * `O` = `ObjectStart`     — after `{` or `,` in an object; skips whitespace, expects `"` or `}`
//! * `K` = `KeyChars`        — inside a quoted key; bulk-skipped via the backslash/quote masks
//! * `D` = `KeyEnd`          — after closing `"` of a key; skips whitespace, expects `:`
//! * `C` = `AfterColon`      — after `:`; skips whitespace, dispatches to the value type
//! * `S` = `StringChars`     — inside a quoted string value; bulk-skipped via the backslash/quote masks
//! * `F` = `AfterValue`      — after any complete value; skips whitespace, expects `,`/`}`/`]`
//! * `R` = `ArrayStart`      — after `[` or `,` in an array; skips whitespace, dispatches value
//! * `A` = `AtomChars`       — inside a number, `true`, `false`, or `null`
//!
//! A few things to notice in the annotation:
//!
//! * `OO`: `ObjectStart` eats the space *and* the opening `"` of a key in one
//!   shot via the `trailing_zeros` whitespace skip.
//! * `DD` / `CC`: `KeyEnd` eats the space *and* `:` together; `AfterColon`
//!   eats the space *and* the value-start byte — structural punctuation costs
//!   no extra iterations.
//! * `SSSSSSS`: `StringChars` covers the entire `value1"` run including the
//!   closing quote (bulk AVX-512 skip + dispatch in one pass through the chunk).
//! * `RAAARRAAAFRRAAAF`: inside the array `[123, 456 , 768]` each `R` covers
//!   the skip-to-digit hop; `AAA` covers the digit characters plus their
//!   terminating `,` / space / `]`.
//! * `KKKKKKKKKKK` (11 bytes): the 10-character `nested_key` body *and* its
//!   closing `"` are all handled by `KeyChars` in one bulk-skip pass.

use std::borrow::Cow;

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

#[derive(PartialEq, Debug, Clone)]
pub enum Value<'a> {
    String(Cow<'a, str>),
    Number(&'a str),
    Bool(bool),
    Null,
    Object(Box<[(Cow<'a, str>, Value<'a>)]>),
    Array(Box<[Value<'a>]>),
}

// Partially-built Object or Array sitting on the frame stack.
enum Frame<'a> {
    Obj {
        key: Cow<'a, str>,
        members: Vec<(Cow<'a, str>, Value<'a>)>,
    },
    Arr {
        elements: Vec<Value<'a>>,
    },
}

// Validate that `s` is a well-formed JSON number:
//   number = [ "-" ] ( "0" | [1-9][0-9]* ) [ "." [0-9]+ ] [ ("e"|"E") ["+"|"-"] [0-9]+ ]
#[inline(never)]
fn is_valid_json_number(s: &[u8]) -> bool {
    let mut i = 0;
    let n = s.len();
    if n == 0 {
        return false;
    }
    // Optional minus.
    if s[i] == b'-' {
        i += 1;
        if i == n {
            return false;
        }
    }
    // Integer part.
    if s[i] == b'0' {
        i += 1;
        // Leading zero must not be followed by another digit.
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
    // Optional fraction.
    if i < n && s[i] == b'.' {
        i += 1;
        if i == n || !s[i].is_ascii_digit() {
            return false;
        }
        while i < n && s[i].is_ascii_digit() {
            i += 1;
        }
    }
    // Optional exponent.
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

// Push a completed Value into the top frame, or set the top-level result.
#[inline(never)]
fn push_value<'a>(val: Value<'a>, frames: &mut Vec<Frame<'a>>, result: &mut Option<Value<'a>>) {
    match frames.last_mut() {
        Some(Frame::Arr { elements }) => elements.push(val),
        Some(Frame::Obj { key, members }) => {
            members.push((std::mem::replace(key, Cow::Borrowed("")), val))
        }
        None => *result = Some(val),
    }
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
/// source.  Two built-in implementations are provided:
///
/// * [`ValueWriter`] — produces a nested [`Value`] tree (used by [`parse_json`]).
/// * [`TapeWriter`] — produces a flat [`Tape`] (used by [`parse_to_tape`]).
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
// ValueWriter — builds the nested Value tree
// ---------------------------------------------------------------------------

struct ValueWriter<'a> {
    frames: Vec<Frame<'a>>,
    result: Option<Value<'a>>,
}

impl<'a> ValueWriter<'a> {
    fn new() -> Self {
        Self {
            frames: Vec::new(),
            result: None,
        }
    }
}

impl<'a> JsonWriter<'a> for ValueWriter<'a> {
    type Output = Value<'a>;

    fn null(&mut self) {
        push_value(Value::Null, &mut self.frames, &mut self.result);
    }
    fn bool_val(&mut self, v: bool) {
        push_value(Value::Bool(v), &mut self.frames, &mut self.result);
    }
    fn number(&mut self, s: &'a str) {
        push_value(Value::Number(s), &mut self.frames, &mut self.result);
    }
    fn string(&mut self, s: Cow<'a, str>) {
        push_value(Value::String(s), &mut self.frames, &mut self.result);
    }
    fn key(&mut self, s: Cow<'a, str>) {
        if let Some(Frame::Obj { key, .. }) = self.frames.last_mut() {
            *key = s;
        }
    }
    fn start_object(&mut self) {
        self.frames.push(Frame::Obj {
            key: Cow::Borrowed(""),
            members: Vec::new(),
        });
    }
    fn end_object(&mut self) {
        if let Some(Frame::Obj { members, .. }) = self.frames.pop() {
            push_value(
                Value::Object(members.into_boxed_slice()),
                &mut self.frames,
                &mut self.result,
            );
        }
    }
    fn start_array(&mut self) {
        self.frames.push(Frame::Arr {
            elements: Vec::new(),
        });
    }
    fn end_array(&mut self) {
        if let Some(Frame::Arr { elements }) = self.frames.pop() {
            push_value(
                Value::Array(elements.into_boxed_slice()),
                &mut self.frames,
                &mut self.result,
            );
        }
    }
    fn finish(self) -> Option<Value<'a>> {
        if self.frames.is_empty() {
            self.result
        } else {
            None
        }
    }
}

// ---------------------------------------------------------------------------
// Tape types — flat, O(1)-skip representation
// ---------------------------------------------------------------------------

/// A single token in a [`Tape`].
///
/// `StartObject(n)` and `StartArray(n)` carry the **index** of their matching
/// `EndObject` / `EndArray` entry, so an entire object or array can be skipped
/// in O(1) without recursion.
#[derive(Debug, PartialEq, Clone)]
pub enum TapeEntry<'a> {
    Null,
    Bool(bool),
    /// A number token; the slice borrows directly from the source string.
    Number(&'a str),
    /// A string value; borrowed when no escapes were present, owned otherwise.
    String(Cow<'a, str>),
    /// An object key; always immediately followed by the key's value entry/entries.
    Key(Cow<'a, str>),
    /// Start of an object; payload is the index of the matching [`TapeEntry::EndObject`].
    StartObject(usize),
    EndObject,
    /// Start of an array; payload is the index of the matching [`TapeEntry::EndArray`].
    StartArray(usize),
    EndArray,
}

/// A flat sequence of [`TapeEntry`] tokens produced by [`parse_to_tape`].
///
/// Each `StartObject(n)` / `StartArray(n)` carries the index of its matching
/// closer, enabling O(1) structural skips:
///
/// ```ignore
/// if let TapeEntry::StartObject(end) = tape.entries[i] {
///     i = end + 1; // jump past the entire object
/// }
/// ```
#[derive(Debug)]
pub struct Tape<'a> {
    pub entries: Vec<TapeEntry<'a>>,
}

// ---------------------------------------------------------------------------
// TapeWriter — builds the flat Tape
// ---------------------------------------------------------------------------

struct TapeWriter<'a> {
    entries: Vec<TapeEntry<'a>>,
    /// Indices of unmatched `StartObject` / `StartArray` waiting for backfill.
    open: Vec<usize>,
}

impl<'a> TapeWriter<'a> {
    fn new() -> Self {
        Self {
            entries: Vec::new(),
            open: Vec::new(),
        }
    }
}

impl<'a> JsonWriter<'a> for TapeWriter<'a> {
    type Output = Tape<'a>;

    fn null(&mut self) {
        self.entries.push(TapeEntry::Null);
    }
    fn bool_val(&mut self, v: bool) {
        self.entries.push(TapeEntry::Bool(v));
    }
    fn number(&mut self, s: &'a str) {
        self.entries.push(TapeEntry::Number(s));
    }
    fn string(&mut self, s: Cow<'a, str>) {
        self.entries.push(TapeEntry::String(s));
    }
    fn key(&mut self, s: Cow<'a, str>) {
        self.entries.push(TapeEntry::Key(s));
    }
    fn start_object(&mut self) {
        let idx = self.entries.len();
        self.open.push(idx);
        self.entries.push(TapeEntry::StartObject(0)); // backfilled in end_object
    }
    fn end_object(&mut self) {
        let end_idx = self.entries.len();
        self.entries.push(TapeEntry::EndObject);
        if let Some(start_idx) = self.open.pop() {
            self.entries[start_idx] = TapeEntry::StartObject(end_idx);
        }
    }
    fn start_array(&mut self) {
        let idx = self.entries.len();
        self.open.push(idx);
        self.entries.push(TapeEntry::StartArray(0)); // backfilled in end_array
    }
    fn end_array(&mut self) {
        let end_idx = self.entries.len();
        self.entries.push(TapeEntry::EndArray);
        if let Some(start_idx) = self.open.pop() {
            self.entries[start_idx] = TapeEntry::StartArray(end_idx);
        }
    }
    fn finish(self) -> Option<Tape<'a>> {
        if self.open.is_empty() {
            Some(Tape {
                entries: self.entries,
            })
        } else {
            None
        }
    }
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
/// This is a convenience wrapper around [`parse_with`] that uses the built-in
/// [`ValueWriter`].
pub fn parse_json<'a>(src: &'a str, classify: ClassifyFn) -> Option<Value<'a>> {
    parse_with(src, classify, ValueWriter::new())
}

/// Parse `src` into a flat [`Tape`] using the given classifier.
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
// XMM (SSE2) — 4 × 16-byte registers, 64 bytes total
// ---------------------------------------------------------------------------

/// Classify 64 bytes using 4 × 16-byte XMM registers (SSE2).
///
/// Bytes beyond `src.len()` are zeroed before classification so their bits
/// are well-defined (they come out as whitespace and are never visited by the
/// inner parser loop).
///
/// Whitespace detection uses the identity:
///   `unsigned a <= 0x20`  ↔  `psubusb(a, 0x20) == 0`
/// which avoids signed-comparison pitfalls with high UTF-8 bytes (≥ 0x80).
pub fn classify_xmm(src: &[u8]) -> ByteState {
    #[target_feature(enable = "sse2")]
    unsafe fn imp(src: &[u8]) -> ByteState {
        unsafe {
            use std::arch::x86_64::{
                __m128i, _mm_cmpeq_epi8, _mm_loadu_si128, _mm_movemask_epi8, _mm_set1_epi8,
                _mm_setzero_si128, _mm_subs_epu8,
            };
            assert!(!src.is_empty() && src.len() <= 64);

            // Zero-pad to 64 bytes so all four 16-byte loads are fully defined.
            let mut buf = [0u8; 64];
            buf[..src.len()].copy_from_slice(src);
            let p = buf.as_ptr();

            let v0 = _mm_loadu_si128(p.add(0).cast::<__m128i>());
            let v1 = _mm_loadu_si128(p.add(16).cast::<__m128i>());
            let v2 = _mm_loadu_si128(p.add(32).cast::<__m128i>());
            let v3 = _mm_loadu_si128(p.add(48).cast::<__m128i>());

            let c_ws = _mm_set1_epi8(0x20_u8 as i8); // upper bound for whitespace
            let c_q = _mm_set1_epi8(b'"' as i8);
            let c_bs = _mm_set1_epi8(b'\\' as i8);
            let c_co = _mm_set1_epi8(b',' as i8);
            let c_cb = _mm_set1_epi8(b'}' as i8);
            let c_sb = _mm_set1_epi8(b']' as i8);
            let zero = _mm_setzero_si128();

            // _mm_movemask_epi8 returns i32; only the low 16 bits are meaningful.
            // Cast via u16 to zero-extend cleanly to u64.
            macro_rules! movmsk {
                ($x:expr) => {
                    _mm_movemask_epi8($x) as u16 as u64
                };
            }
            // unsigned a <= 0x20 via saturating subtract: psubusb(a, 0x20) == 0
            macro_rules! ws {
                ($v:expr) => {
                    movmsk!(_mm_cmpeq_epi8(_mm_subs_epu8($v, c_ws), zero))
                };
            }
            macro_rules! eq {
                ($v:expr, $c:expr) => {
                    movmsk!(_mm_cmpeq_epi8($v, $c))
                };
            }
            // Combine four 16-bit masks into one 64-bit mask.
            macro_rules! combine4 {
                ($m0:expr, $m1:expr, $m2:expr, $m3:expr) => {
                    $m0 | ($m1 << 16) | ($m2 << 32) | ($m3 << 48)
                };
            }

            let whitespace = combine4!(ws!(v0), ws!(v1), ws!(v2), ws!(v3));
            let quotes = combine4!(eq!(v0, c_q), eq!(v1, c_q), eq!(v2, c_q), eq!(v3, c_q));
            let backslashes = combine4!(eq!(v0, c_bs), eq!(v1, c_bs), eq!(v2, c_bs), eq!(v3, c_bs));
            let commas = combine4!(eq!(v0, c_co), eq!(v1, c_co), eq!(v2, c_co), eq!(v3, c_co));
            let cl_braces = combine4!(eq!(v0, c_cb), eq!(v1, c_cb), eq!(v2, c_cb), eq!(v3, c_cb));
            let cl_brackets = combine4!(eq!(v0, c_sb), eq!(v1, c_sb), eq!(v2, c_sb), eq!(v3, c_sb));
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
/// The precedence is: AVX-512BW → AVX2 → SSE2 (always available on x86-64).
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
    classify_xmm
}

#[cfg(test)]
mod tests {
    use super::*;

    // -----------------------------------------------------------------------
    // Classifier helpers
    // -----------------------------------------------------------------------

    /// Run parse_json using XMM (SSE2) classifier.
    fn run_xmm(json: &'static str) -> Option<Value<'static>> {
        parse_json(json, classify_xmm)
    }

    /// Run parse_json using YMM (AVX2) classifier.
    fn run_ymm(json: &'static str) -> Option<Value<'static>> {
        parse_json(json, classify_ymm)
    }

    /// Run parse_json using the best classifier chosen via CPUID.
    fn run_zmm(json: &'static str) -> Option<Value<'static>> {
        parse_json(json, choose_classifier())
    }

    /// Run all three classifiers and assert they agree.
    fn run(json: &'static str) -> Option<Value<'static>> {
        let x = run_xmm(json);
        let y = run_ymm(json);
        let z = run_zmm(json);
        assert_eq!(x, y, "XMM vs YMM differ for: {json:?}");
        assert_eq!(y, z, "YMM vs ZMM differ for: {json:?}");
        z
    }

    fn s(v: &'static str) -> Value<'static> {
        Value::String(Cow::Borrowed(v))
    }
    fn so(v: &str) -> Value<'static> {
        Value::String(Cow::Owned(v.to_string()))
    }
    fn n(v: &'static str) -> Value<'static> {
        Value::Number(v)
    }
    fn obj(members: &[(&'static str, Value<'static>)]) -> Value<'static> {
        Value::Object(
            members
                .iter()
                .map(|(k, v)| (Cow::Borrowed(*k), v.clone()))
                .collect::<Vec<_>>()
                .into_boxed_slice(),
        )
    }
    fn arr(elements: Vec<Value<'static>>) -> Value<'static> {
        Value::Array(elements.into_boxed_slice())
    }

    // -----------------------------------------------------------------------
    // Classifier unit tests: compare XMM and YMM bitmasks against ZMM
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
            // Short slice (16 bytes — one XMM register exactly)
            b"0123456789abcdef",
            // Short slice (32 bytes — one YMM register exactly)
            b"0123456789abcdef0123456789abcdef",
        ];

        for &input in inputs {
            // Truncate to max 64 bytes (all are ≤ 64 here)
            let src = &input[..input.len().min(64)];
            let zmm = classify_zmm(src);
            let xmm = classify_xmm(src);
            let ymm = classify_ymm(src);

            assert_eq!(
                xmm, zmm,
                "XMM vs ZMM mismatch on input {:?}\n  xmm ws={:#018x} zmm ws={:#018x}",
                input, xmm.whitespace, zmm.whitespace
            );
            assert_eq!(
                ymm, zmm,
                "YMM vs ZMM mismatch on input {:?}\n  ymm ws={:#018x} zmm ws={:#018x}",
                input, ymm.whitespace, zmm.whitespace
            );
        }
    }

    // -----------------------------------------------------------------------
    // Full parser tests — each assertion exercises all three variants via run()
    // -----------------------------------------------------------------------

    #[test]
    fn test_string() {
        assert_eq!(run(r#""hello""#), Some(s("hello")));
    }

    #[test]
    fn test_escaped_string() {
        // Escapes are decoded: the Value holds the final unescaped string.
        assert_eq!(run(r#""hello \"world\"""#), Some(so("hello \"world\"")));
        assert_eq!(run(r#""line\\nbreak""#), Some(so("line\\nbreak"))); // \\\\ -> \\ then n literal
        assert_eq!(run(r#""tab\there""#), Some(so("tab\there"))); // \t -> tab
    }

    #[test]
    fn test_number() {
        assert_eq!(run("42"), Some(n("42")));
    }

    #[test]
    fn test_bool_null() {
        assert_eq!(run("true"), Some(Value::Bool(true)));
        assert_eq!(run("false"), Some(Value::Bool(false)));
        assert_eq!(run("null"), Some(Value::Null));
    }

    #[test]
    fn test_empty_object() {
        assert_eq!(run("{}"), Some(Value::Object(Box::new([]))));
    }

    #[test]
    fn test_empty_array() {
        assert_eq!(run("[]"), Some(Value::Array(Box::new([]))));
    }

    #[test]
    fn test_simple_object() {
        assert_eq!(
            run(r#"{"key": "value"}"#),
            Some(obj(&[("key", s("value"))]))
        );
    }

    #[test]
    fn test_multi_key_object() {
        assert_eq!(
            run(
                r#"{ "key1" : "value1" , "key2": [123, 456, 768], "key3" : { "nested_key" : true } }"#
            ),
            Some(obj(&[
                ("key1", s("value1")),
                ("key2", arr(vec![n("123"), n("456"), n("768")])),
                ("key3", obj(&[("nested_key", Value::Bool(true))])),
            ]))
        );
    }

    #[test]
    fn test_array_of_mixed() {
        assert_eq!(
            run(r#"[1, "two", true, null, {"x": 3}]"#),
            Some(arr(vec![
                n("1"),
                s("two"),
                Value::Bool(true),
                Value::Null,
                obj(&[("x", n("3"))]),
            ]))
        );
    }

    #[test]
    fn test_whitespace() {
        assert_eq!(run("  \n  42  \n"), Some(n("42")));
    }

    #[test]
    fn test_valid_numbers() {
        assert_eq!(run("0"), Some(n("0")));
        assert_eq!(run("-0"), Some(n("-0")));
        assert_eq!(run("123"), Some(n("123")));
        assert_eq!(run("-456"), Some(n("-456")));
        assert_eq!(run("1.5"), Some(n("1.5")));
        assert_eq!(run("-1.5"), Some(n("-1.5")));
        assert_eq!(run("1e10"), Some(n("1e10")));
        assert_eq!(run("1E+10"), Some(n("1E+10")));
        assert_eq!(run("1.5e-3"), Some(n("1.5e-3")));
        assert_eq!(run("0.001"), Some(n("0.001")));
    }

    #[test]
    fn test_invalid_numbers() {
        assert_eq!(run("01"), None); // leading zero
        assert_eq!(run("1."), None); // trailing dot
        assert_eq!(run(".5"), None); // leading dot
        assert_eq!(run("1e"), None); // bare exponent
        assert_eq!(run("1e+"), None); // exponent sign only
        assert_eq!(run("--1"), None); // double minus
        assert_eq!(run("1.2.3"), None); // two dots
        assert_eq!(run("abc"), None); // not a keyword
        assert_eq!(run("tru"), None); // truncated keyword
        assert_eq!(run("nul"), None); // truncated keyword
    }

    #[test]
    fn test_structural_errors() {
        // Trailing junk after a valid value.
        assert_eq!(run("42 garbage"), None);
        assert_eq!(run(r#""hi" !"#), None);

        // Multiple top-level values.
        assert_eq!(run("1 2"), None);
        assert_eq!(run("true false"), None);

        // Top-level comma.
        assert_eq!(run("42,"), None);

        // Trailing commas in containers.
        assert_eq!(run("[1, 2,]"), None);
        assert_eq!(run(r#"{"a": 1,}"#), None);

        // Mismatched brackets.
        assert_eq!(run("[1, 2}"), None);
        assert_eq!(run(r#"{"a": 1]"#), None);

        // Unclosed containers.
        assert_eq!(run("[1, 2"), None);
        assert_eq!(run(r#"{"a": 1"#), None);

        // Missing colon.
        assert_eq!(run(r#"{"a" 1}"#), None);

        // Extra closing bracket at top level.
        assert_eq!(run("1}"), None);
        assert_eq!(run("1]"), None);
    }

    // -----------------------------------------------------------------------
    // Tape tests
    // -----------------------------------------------------------------------

    fn run_tape(json: &'static str) -> Option<Tape<'static>> {
        let x = parse_to_tape(json, classify_xmm);
        let y = parse_to_tape(json, classify_ymm);
        let z = parse_to_tape(json, choose_classifier());
        assert_eq!(
            x.as_ref().map(|t| &t.entries),
            z.as_ref().map(|t| &t.entries),
            "XMM vs ZMM tape differ for: {json:?}"
        );
        assert_eq!(
            y.as_ref().map(|t| &t.entries),
            z.as_ref().map(|t| &t.entries),
            "YMM vs ZMM tape differ for: {json:?}"
        );
        z
    }

    fn te_str(s: &'static str) -> TapeEntry<'static> {
        TapeEntry::String(Cow::Borrowed(s))
    }
    fn te_key(s: &'static str) -> TapeEntry<'static> {
        TapeEntry::Key(Cow::Borrowed(s))
    }
    fn te_num(s: &'static str) -> TapeEntry<'static> {
        TapeEntry::Number(s)
    }

    #[test]
    fn tape_scalar_values() {
        assert_eq!(run_tape("null").unwrap().entries, vec![TapeEntry::Null]);
        assert_eq!(
            run_tape("true").unwrap().entries,
            vec![TapeEntry::Bool(true)]
        );
        assert_eq!(
            run_tape("false").unwrap().entries,
            vec![TapeEntry::Bool(false)]
        );
        assert_eq!(run_tape("42").unwrap().entries, vec![te_num("42")]);
        assert_eq!(run_tape(r#""hi""#).unwrap().entries, vec![te_str("hi")]);
    }

    #[test]
    fn tape_empty_object() {
        let t = run_tape("{}").unwrap();
        // StartObject(1) EndObject
        assert_eq!(
            t.entries,
            vec![TapeEntry::StartObject(1), TapeEntry::EndObject]
        );
        // StartObject payload points at EndObject
        assert_eq!(t.entries[0], TapeEntry::StartObject(1));
    }

    #[test]
    fn tape_empty_array() {
        let t = run_tape("[]").unwrap();
        assert_eq!(
            t.entries,
            vec![TapeEntry::StartArray(1), TapeEntry::EndArray]
        );
        assert_eq!(t.entries[0], TapeEntry::StartArray(1));
    }

    #[test]
    fn tape_simple_object() {
        // {"a":1} → StartObject Key("a") Number("1") EndObject
        let t = run_tape(r#"{"a":1}"#).unwrap();
        assert_eq!(
            t.entries,
            vec![
                TapeEntry::StartObject(3),
                te_key("a"),
                te_num("1"),
                TapeEntry::EndObject,
            ]
        );
        // StartObject carries index of EndObject
        assert_eq!(t.entries[0], TapeEntry::StartObject(3));
    }

    #[test]
    fn tape_simple_array() {
        // [1,2,3] → StartArray Num Num Num EndArray
        let t = run_tape(r#"[1,2,3]"#).unwrap();
        assert_eq!(
            t.entries,
            vec![
                TapeEntry::StartArray(4),
                te_num("1"),
                te_num("2"),
                te_num("3"),
                TapeEntry::EndArray,
            ]
        );
    }

    #[test]
    fn tape_nested() {
        // {"a":[1,2]} → StartObject Key StartArray Num Num EndArray EndObject
        let t = run_tape(r#"{"a":[1,2]}"#).unwrap();
        use TapeEntry::*;
        assert_eq!(
            t.entries,
            vec![
                StartObject(6), // 0
                te_key("a"),    // 1
                StartArray(5),  // 2
                te_num("1"),    // 3
                te_num("2"),    // 4
                EndArray,       // 5
                EndObject,      // 6
            ]
        );
        assert_eq!(t.entries[0], StartObject(6));
        assert_eq!(t.entries[2], StartArray(5));
    }

    #[test]
    fn tape_multi_key_object() {
        let t = run_tape(r#"{"x":1,"y":2}"#).unwrap();
        use TapeEntry::*;
        assert_eq!(
            t.entries,
            vec![
                StartObject(5), // 0 — points to EndObject at index 5
                te_key("x"),    // 1
                te_num("1"),    // 2
                te_key("y"),    // 3
                te_num("2"),    // 4
                EndObject,      // 5
            ]
        );
        assert_eq!(t.entries[0], StartObject(5));
    }

    #[test]
    fn tape_invalid_returns_none() {
        assert!(run_tape("[1,2,]").is_none());
        assert!(run_tape(r#"{"a":1,}"#).is_none());
        assert!(run_tape("{bad}").is_none());
    }

    #[test]
    fn tape_skip_object() {
        // Verify the skip-forward idiom works.
        let t = run_tape(r#"[{"x":1},2]"#).unwrap();
        // entries: StartArray StartObject Key Num EndObject Num EndArray
        //          0          1           2   3   4         5   6
        assert_eq!(t.entries.len(), 7);
        // Skip from StartObject(4) to index 5 (after EndObject).
        if let TapeEntry::StartObject(end) = t.entries[1] {
            assert_eq!(end, 4);
            // After the object the next item is at end + 1 = 5.
            assert_eq!(t.entries[5], te_num("2"));
        } else {
            panic!("expected StartObject at index 1");
        }
    }
}
