use crate::JsonWriter;

// ---------------------------------------------------------------------------
// TapeEntryKind — top 4 bits of the tag word
// ---------------------------------------------------------------------------

/// Discriminant stored in bits 63–60 of `TapeEntry::tag_payload`.
///
/// The numeric values are fixed and part of the public ABI (the hand-written
/// assembly in `parse_json_zmm_tape.S` depends on them).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum TapeEntryKind {
    Null = 0,
    Bool = 1,
    Number = 2,
    String = 3,
    EscapedString = 4,
    Key = 5,
    EscapedKey = 6,
    StartObject = 7,
    EndObject = 8,
    StartArray = 9,
    EndArray = 10,
}

// Bit-field constants
const KIND_SHIFT: u64 = 60;
const PAYLOAD_MASK: u64 = u64::MAX >> 4; // low 60 bits

// ---------------------------------------------------------------------------
// TapeEntry — exactly 16 bytes
// ---------------------------------------------------------------------------

/// A single token in a [`Tape`].
///
/// The representation is a fixed-size 16-byte struct:
///
/// | word | bits | meaning |
/// |------|------|---------|
/// | 0 (offset 0) | 63–60 | [`TapeEntryKind`] discriminant |
/// | 0 (offset 0) | 59–0  | string/key length **or** object/array end-index |
/// | 1 (offset 8) | 63–0  | pointer to string bytes (null for non-string kinds) |
///
/// For `EscapedString` / `EscapedKey` the pointer is the data pointer of a
/// `Box<str>` whose ownership is transferred into (and out of) this entry.
/// [`TapeEntry`] implements [`Drop`] to free that allocation.
///
/// For `Bool` the low bit of the payload encodes the value (`0` = false, `1` = true).
/// For `Null`, `EndObject`, `EndArray` both payload and pointer are zero.
#[repr(C)]
pub struct TapeEntry<'a> {
    /// Bits 63–60: kind.  Bits 59–0: length or end-index.
    pub(crate) tag_payload: u64,
    /// Pointer to string bytes, or null.
    pub(crate) ptr: *const u8,
    _marker: std::marker::PhantomData<&'a str>,
}

// SAFETY: the only non-Send/Sync component is the raw pointer; we track
// ownership of the pointed-to data through the 'a lifetime or through the
// Box<str> path (EscapedString/EscapedKey), so sharing is safe.
unsafe impl<'a> Send for TapeEntry<'a> {}
unsafe impl<'a> Sync for TapeEntry<'a> {}

impl<'a> Drop for TapeEntry<'a> {
    fn drop(&mut self) {
        let kind = self.kind();
        if kind == TapeEntryKind::EscapedString || kind == TapeEntryKind::EscapedKey {
            if !self.ptr.is_null() {
                let len = self.payload() as usize;
                // SAFETY: these were originally created by Box::into_raw(s.into_boxed_str()).
                unsafe {
                    let slice = std::slice::from_raw_parts_mut(self.ptr as *mut u8, len);
                    drop(Box::from_raw(slice as *mut [u8] as *mut str));
                }
            }
        }
    }
}

impl<'a> Clone for TapeEntry<'a> {
    fn clone(&self) -> Self {
        let kind = self.kind();
        if kind == TapeEntryKind::EscapedString || kind == TapeEntryKind::EscapedKey {
            // Deep-copy the heap allocation.
            let s = self.as_escaped_str_unchecked();
            let boxed: Box<str> = s.into();
            let len = boxed.len() as u64;
            let ptr = Box::into_raw(boxed) as *mut u8 as *const u8;
            Self {
                tag_payload: ((kind as u64) << KIND_SHIFT) | (len & PAYLOAD_MASK),
                ptr,
                _marker: std::marker::PhantomData,
            }
        } else {
            Self {
                tag_payload: self.tag_payload,
                ptr: self.ptr,
                _marker: std::marker::PhantomData,
            }
        }
    }
}

/// Custom `Debug` that renders the same variant names as the old enum.
impl<'a> std::fmt::Debug for TapeEntry<'a> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.kind() {
            TapeEntryKind::Null => write!(f, "Null"),
            TapeEntryKind::Bool => write!(f, "Bool({})", self.payload() != 0),
            TapeEntryKind::Number => write!(f, "Number({:?})", self.as_str_unchecked()),
            TapeEntryKind::String => write!(f, "String({:?})", self.as_str_unchecked()),
            TapeEntryKind::EscapedString => {
                write!(f, "EscapedString({:?})", self.as_escaped_str_unchecked())
            }
            TapeEntryKind::Key => write!(f, "Key({:?})", self.as_str_unchecked()),
            TapeEntryKind::EscapedKey => {
                write!(f, "EscapedKey({:?})", self.as_escaped_str_unchecked())
            }
            TapeEntryKind::StartObject => write!(f, "StartObject({})", self.payload()),
            TapeEntryKind::EndObject => write!(f, "EndObject"),
            TapeEntryKind::StartArray => write!(f, "StartArray({})", self.payload()),
            TapeEntryKind::EndArray => write!(f, "EndArray"),
        }
    }
}

/// Equality.  For `EscapedString`/`EscapedKey` we compare the string content.
impl<'a> PartialEq for TapeEntry<'a> {
    fn eq(&self, other: &Self) -> bool {
        if self.kind() != other.kind() {
            return false;
        }
        match self.kind() {
            TapeEntryKind::Null | TapeEntryKind::EndObject | TapeEntryKind::EndArray => true,
            TapeEntryKind::Bool => self.payload() == other.payload(),
            TapeEntryKind::StartObject | TapeEntryKind::StartArray => {
                self.payload() == other.payload()
            }
            TapeEntryKind::Number | TapeEntryKind::String | TapeEntryKind::Key => {
                self.as_str_unchecked() == other.as_str_unchecked()
            }
            TapeEntryKind::EscapedString | TapeEntryKind::EscapedKey => {
                self.as_escaped_str_unchecked() == other.as_escaped_str_unchecked()
            }
        }
    }
}

// ---------------------------------------------------------------------------
// TapeEntry constructors and accessors
// ---------------------------------------------------------------------------

impl<'a> TapeEntry<'a> {
    // ---- private helpers ----

    #[inline]
    fn make(kind: TapeEntryKind, payload: u64, ptr: *const u8) -> Self {
        Self {
            tag_payload: ((kind as u64) << KIND_SHIFT) | (payload & PAYLOAD_MASK),
            ptr,
            _marker: std::marker::PhantomData,
        }
    }

    /// The discriminant.
    #[inline]
    pub fn kind(&self) -> TapeEntryKind {
        // SAFETY: we only ever write valid TapeEntryKind values into the top 4 bits.
        unsafe { std::mem::transmute((self.tag_payload >> KIND_SHIFT) as u8) }
    }

    /// The payload field (low 28 bits of the tag word).
    #[inline]
    pub(crate) fn payload(&self) -> u64 {
        self.tag_payload & PAYLOAD_MASK
    }

    /// Borrowed str for Number/String/Key variants (UB if called on others).
    #[inline]
    fn as_str_unchecked(&self) -> &'a str {
        let len = self.payload() as usize;
        unsafe { std::str::from_utf8_unchecked(std::slice::from_raw_parts(self.ptr, len)) }
    }

    /// Borrowed str for EscapedString/EscapedKey variants (UB if called on others).
    #[inline]
    fn as_escaped_str_unchecked(&self) -> &str {
        let len = self.payload() as usize;
        unsafe { std::str::from_utf8_unchecked(std::slice::from_raw_parts(self.ptr, len)) }
    }

    // ---- public constructors matching the old enum variants ----

    #[inline]
    pub fn null_entry() -> Self {
        Self::make(TapeEntryKind::Null, 0, std::ptr::null())
    }
    #[inline]
    pub fn bool_entry(v: bool) -> Self {
        Self::make(TapeEntryKind::Bool, v as u64, std::ptr::null())
    }
    #[inline]
    pub fn number_entry(s: &'a str) -> Self {
        Self::make(TapeEntryKind::Number, s.len() as u64, s.as_ptr())
    }
    #[inline]
    pub fn string_entry(s: &'a str) -> Self {
        Self::make(TapeEntryKind::String, s.len() as u64, s.as_ptr())
    }
    #[inline]
    pub fn escaped_string_entry(s: Box<str>) -> Self {
        let len = s.len() as u64;
        let ptr = Box::into_raw(s) as *mut u8 as *const u8;
        Self::make(TapeEntryKind::EscapedString, len, ptr)
    }
    #[inline]
    pub fn key_entry(s: &'a str) -> Self {
        Self::make(TapeEntryKind::Key, s.len() as u64, s.as_ptr())
    }
    #[inline]
    pub fn escaped_key_entry(s: Box<str>) -> Self {
        let len = s.len() as u64;
        let ptr = Box::into_raw(s) as *mut u8 as *const u8;
        Self::make(TapeEntryKind::EscapedKey, len, ptr)
    }
    /// `payload` will be backfilled with the end-index later.
    #[inline]
    pub fn start_object_entry(end_idx: usize) -> Self {
        Self::make(TapeEntryKind::StartObject, end_idx as u64, std::ptr::null())
    }
    #[inline]
    pub fn end_object_entry() -> Self {
        Self::make(TapeEntryKind::EndObject, 0, std::ptr::null())
    }
    /// `payload` will be backfilled with the end-index later.
    #[inline]
    pub fn start_array_entry(end_idx: usize) -> Self {
        Self::make(TapeEntryKind::StartArray, end_idx as u64, std::ptr::null())
    }
    #[inline]
    pub fn end_array_entry() -> Self {
        Self::make(TapeEntryKind::EndArray, 0, std::ptr::null())
    }

    // ---- backfill helper (used by TapeWriter::end_object / end_array) ----

    /// Overwrite the payload field (low 60 bits) without changing the kind.
    #[inline]
    pub(crate) fn set_payload(&mut self, v: usize) {
        self.tag_payload = (self.tag_payload & !(PAYLOAD_MASK)) | ((v as u64) & PAYLOAD_MASK);
    }

    // ---- pattern-match helpers matching the old enum syntax ----

    /// Returns `Some(end_index)` if this is `StartObject`, else `None`.
    #[inline]
    pub fn as_start_object(&self) -> Option<usize> {
        if self.kind() == TapeEntryKind::StartObject {
            Some(self.payload() as usize)
        } else {
            None
        }
    }
    /// Returns `Some(end_index)` if this is `StartArray`, else `None`.
    #[inline]
    pub fn as_start_array(&self) -> Option<usize> {
        if self.kind() == TapeEntryKind::StartArray {
            Some(self.payload() as usize)
        } else {
            None
        }
    }
    /// Returns `Some(b)` if this is `Bool`, else `None`.
    #[inline]
    pub fn as_bool(&self) -> Option<bool> {
        if self.kind() == TapeEntryKind::Bool {
            Some(self.payload() != 0)
        } else {
            None
        }
    }
    /// Returns the number text if this is `Number`, else `None`.
    #[inline]
    pub fn as_number(&self) -> Option<&'a str> {
        if self.kind() == TapeEntryKind::Number {
            Some(self.as_str_unchecked())
        } else {
            None
        }
    }
    /// Returns the string text if this is `String` or `EscapedString`, else `None`.
    #[inline]
    pub fn as_string(&self) -> Option<&str> {
        match self.kind() {
            TapeEntryKind::String => Some(self.as_str_unchecked()),
            TapeEntryKind::EscapedString => Some(self.as_escaped_str_unchecked()),
            _ => None,
        }
    }
    /// Returns the key text if this is `Key` or `EscapedKey`, else `None`.
    #[inline]
    pub fn as_key(&self) -> Option<&str> {
        match self.kind() {
            TapeEntryKind::Key => Some(self.as_str_unchecked()),
            TapeEntryKind::EscapedKey => Some(self.as_escaped_str_unchecked()),
            _ => None,
        }
    }
}

/// Convenience constructors using the old enum-variant names so existing
/// test/user code keeps a familiar style.
#[allow(non_snake_case, non_upper_case_globals)]
impl<'a> TapeEntry<'a> {
    /// Alias: `TapeEntry::Null` → `TapeEntry::null_entry()`.
    pub const Null: TapeEntry<'static> = TapeEntry {
        tag_payload: 0,
        ptr: std::ptr::null(),
        _marker: std::marker::PhantomData,
    };
    /// Alias: `TapeEntry::EndObject` → `TapeEntry::end_object_entry()`.
    pub const EndObject: TapeEntry<'static> = TapeEntry {
        tag_payload: (TapeEntryKind::EndObject as u64) << KIND_SHIFT,
        ptr: std::ptr::null(),
        _marker: std::marker::PhantomData,
    };
    /// Alias: `TapeEntry::EndArray` → `TapeEntry::end_array_entry()`.
    pub const EndArray: TapeEntry<'static> = TapeEntry {
        tag_payload: (TapeEntryKind::EndArray as u64) << KIND_SHIFT,
        ptr: std::ptr::null(),
        _marker: std::marker::PhantomData,
    };

    /// Construct a `Bool` entry.  Replaces `TapeEntry::Bool(v)`.
    #[inline]
    pub fn Bool(v: bool) -> Self {
        Self::bool_entry(v)
    }
    /// Construct a `Number` entry.  Replaces `TapeEntry::Number(s)`.
    #[inline]
    pub fn Number(s: &'a str) -> Self {
        Self::number_entry(s)
    }
    /// Construct a `String` entry.  Replaces `TapeEntry::String(s)`.
    #[inline]
    pub fn String(s: &'a str) -> Self {
        Self::string_entry(s)
    }
    /// Construct an `EscapedString` entry.  Replaces `TapeEntry::EscapedString(b)`.
    #[inline]
    pub fn EscapedString(s: Box<str>) -> Self {
        Self::escaped_string_entry(s)
    }
    /// Construct a `Key` entry.  Replaces `TapeEntry::Key(s)`.
    #[inline]
    pub fn Key(s: &'a str) -> Self {
        Self::key_entry(s)
    }
    /// Construct an `EscapedKey` entry.  Replaces `TapeEntry::EscapedKey(b)`.
    #[inline]
    pub fn EscapedKey(s: Box<str>) -> Self {
        Self::escaped_key_entry(s)
    }
    /// Construct a `StartObject` entry.  Replaces `TapeEntry::StartObject(n)`.
    #[inline]
    pub fn StartObject(end_idx: usize) -> Self {
        Self::start_object_entry(end_idx)
    }
    /// Construct a `StartArray` entry.  Replaces `TapeEntry::StartArray(n)`.
    #[inline]
    pub fn StartArray(end_idx: usize) -> Self {
        Self::start_array_entry(end_idx)
    }
}

/// A flat sequence of [`TapeEntry`] tokens produced by [`crate::parse_to_tape`].
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
    /// True if any entry in the tape is `EscapedString` or `EscapedKey`
    /// (i.e. owns a heap-allocated `Box<str>`).  When false, `Drop` can skip
    /// per-element destructor calls and free the backing allocation directly.
    pub(crate) has_escapes: bool,
}

impl<'a> Drop for Tape<'a> {
    fn drop(&mut self) {
        if !self.has_escapes {
            // No entry owns heap memory: skip per-element Drop calls and just
            // free the Vec's backing allocation.
            // SAFETY: every TapeEntry either borrows from the source JSON
            // (String/Key/Number) or contains no pointer (Null/Bool/StartObject
            // etc).  None own a Box<str>, so there is nothing to free per-element.
            unsafe { self.entries.set_len(0) };
        }
        // self.entries drops here; with len==0 no element destructors run.
    }
}

// ---------------------------------------------------------------------------
// TapeWriter — builds the flat Tape
// ---------------------------------------------------------------------------

pub(crate) struct TapeWriter<'a> {
    entries: Vec<TapeEntry<'a>>,
    /// Indices of unmatched `StartObject` / `StartArray` waiting for backfill.
    open: Vec<usize>,
    /// Set to `true` when any escaped string or key is pushed.
    has_escapes: bool,
}

impl<'a> TapeWriter<'a> {
    pub(crate) fn new() -> Self {
        Self {
            entries: Vec::new(),
            open: Vec::new(),
            has_escapes: false,
        }
    }
}

impl<'a> JsonWriter<'a> for TapeWriter<'a> {
    type Output = Tape<'a>;

    fn null(&mut self) {
        self.entries.push(TapeEntry::null_entry());
    }
    fn bool_val(&mut self, v: bool) {
        self.entries.push(TapeEntry::bool_entry(v));
    }
    fn number(&mut self, s: &'a str) {
        self.entries.push(TapeEntry::number_entry(s));
    }
    fn string(&mut self, s: &'a str) {
        self.entries.push(TapeEntry::string_entry(s));
    }
    fn escaped_string(&mut self, s: Box<str>) {
        self.has_escapes = true;
        self.entries.push(TapeEntry::escaped_string_entry(s));
    }
    fn key(&mut self, s: &'a str) {
        self.entries.push(TapeEntry::key_entry(s));
    }
    fn escaped_key(&mut self, s: Box<str>) {
        self.has_escapes = true;
        self.entries.push(TapeEntry::escaped_key_entry(s));
    }
    fn start_object(&mut self) {
        let idx = self.entries.len();
        self.open.push(idx);
        self.entries.push(TapeEntry::start_object_entry(0)); // backfilled in end_object
    }
    fn end_object(&mut self) {
        let end_idx = self.entries.len();
        self.entries.push(TapeEntry::end_object_entry());
        if let Some(start_idx) = self.open.pop() {
            self.entries[start_idx].set_payload(end_idx);
        }
    }
    fn start_array(&mut self) {
        let idx = self.entries.len();
        self.open.push(idx);
        self.entries.push(TapeEntry::start_array_entry(0)); // backfilled in end_array
    }
    fn end_array(&mut self) {
        let end_idx = self.entries.len();
        self.entries.push(TapeEntry::end_array_entry());
        if let Some(start_idx) = self.open.pop() {
            self.entries[start_idx].set_payload(end_idx);
        }
    }
    fn finish(self) -> Option<Tape<'a>> {
        if self.open.is_empty() {
            Some(Tape {
                entries: self.entries,
                has_escapes: self.has_escapes,
            })
        } else {
            None
        }
    }
}

// ---------------------------------------------------------------------------
// TapeRef — lightweight cursor into a Tape
// ---------------------------------------------------------------------------

/// A lightweight cursor into a [`Tape`], pointing at a single entry by index.
///
/// `'t` is the lifetime of the borrow of the tape; `'src` is the lifetime of
/// the source JSON bytes (`'src: 't`).  Both lifetimes collapse to the same
/// `'a` in the common case where you borrow the tape and the source in the
/// same scope.
///
/// Created via [`Tape::root`].  Implements [`crate::JsonRef`] alongside
/// `&'t Value<'src>`.
#[derive(Clone, Copy)]
pub struct TapeRef<'t, 'src: 't> {
    pub(crate) tape: &'t [TapeEntry<'src>],
    pub(crate) pos: usize,
}

impl<'src> Tape<'src> {
    /// Returns a [`TapeRef`] cursor at the root (entry 0), or `None` if the
    /// tape is empty.
    pub fn root<'t>(&'t self) -> Option<TapeRef<'t, 'src>> {
        if self.entries.is_empty() {
            None
        } else {
            Some(TapeRef {
                tape: &self.entries,
                pos: 0,
            })
        }
    }
}

/// Advance past the entry at `pos`, returning the index of the next sibling.
///
/// `StartObject(end)` / `StartArray(end)` jump over the entire subtree.
pub(crate) fn tape_skip(entries: &[TapeEntry<'_>], pos: usize) -> usize {
    let e = &entries[pos];
    match e.kind() {
        TapeEntryKind::StartObject | TapeEntryKind::StartArray => e.payload() as usize + 1,
        _ => pos + 1,
    }
}

// ---------------------------------------------------------------------------
// TapeObjectIter / TapeArrayIter
// ---------------------------------------------------------------------------

/// Iterator over the key-value pairs of a JSON object in a [`Tape`].
///
/// Yields `(&str, TapeRef)` pairs in document order.  Created by
/// [`TapeRef::object_iter`].
pub struct TapeObjectIter<'t, 'src: 't> {
    tape: &'t [TapeEntry<'src>],
    pos: usize,
    end: usize,
}

impl<'t, 'src: 't> Iterator for TapeObjectIter<'t, 'src> {
    type Item = (&'t str, TapeRef<'t, 'src>);

    fn next(&mut self) -> Option<Self::Item> {
        if self.pos >= self.end {
            return None;
        }
        let key: &'t str = self.tape[self.pos].as_key()?;
        let val_pos = self.pos + 1;
        self.pos = tape_skip(self.tape, val_pos);
        Some((
            key,
            TapeRef {
                tape: self.tape,
                pos: val_pos,
            },
        ))
    }
}

/// Iterator over the elements of a JSON array in a [`Tape`].
///
/// Yields one [`TapeRef`] per element in document order.  Created by
/// [`TapeRef::array_iter`].
pub struct TapeArrayIter<'t, 'src: 't> {
    tape: &'t [TapeEntry<'src>],
    pos: usize,
    end: usize,
}

impl<'t, 'src: 't> Iterator for TapeArrayIter<'t, 'src> {
    type Item = TapeRef<'t, 'src>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.pos >= self.end {
            return None;
        }
        let item = TapeRef {
            tape: self.tape,
            pos: self.pos,
        };
        self.pos = tape_skip(self.tape, self.pos);
        Some(item)
    }
}

// ---------------------------------------------------------------------------
// TapeRef inherent methods
// ---------------------------------------------------------------------------

impl<'t, 'src: 't> TapeRef<'t, 'src> {
    /// Returns an iterator over the key-value pairs if this value is a JSON
    /// object, or `None` otherwise.
    ///
    /// # Example
    ///
    /// ```rust
    /// use asmjson::{parse_to_tape, choose_classifier, JsonRef};
    ///
    /// let tape = parse_to_tape(r#"{"a":1,"b":2}"#, choose_classifier()).unwrap();
    /// let root = tape.root().unwrap();
    /// for (key, val) in root.object_iter().unwrap() {
    ///     println!("{key}: {}", val.as_number_str().unwrap());
    /// }
    /// ```
    pub fn object_iter(self) -> Option<TapeObjectIter<'t, 'src>> {
        self.tape[self.pos]
            .as_start_object()
            .map(|end| TapeObjectIter {
                tape: self.tape,
                pos: self.pos + 1,
                end,
            })
    }

    /// Returns an iterator over the elements if this value is a JSON array,
    /// or `None` otherwise.
    ///
    /// # Example
    ///
    /// ```rust
    /// use asmjson::{parse_to_tape, choose_classifier, JsonRef};
    ///
    /// let tape = parse_to_tape(r#"[1,2,3]"#, choose_classifier()).unwrap();
    /// let root = tape.root().unwrap();
    /// for elem in root.array_iter().unwrap() {
    ///     println!("{}", elem.as_number_str().unwrap());
    /// }
    /// ```
    pub fn array_iter(self) -> Option<TapeArrayIter<'t, 'src>> {
        self.tape[self.pos]
            .as_start_array()
            .map(|end| TapeArrayIter {
                tape: self.tape,
                pos: self.pos + 1,
                end,
            })
    }
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use crate::{JsonRef, choose_classifier, classify_u64, classify_ymm, parse_to_tape};

    use super::{Tape, TapeEntry};

    fn run_tape(json: &'static str) -> Option<Tape<'static>> {
        let x = parse_to_tape(json, classify_u64);
        let y = parse_to_tape(json, classify_ymm);
        let z = parse_to_tape(json, choose_classifier());
        assert_eq!(
            x.as_ref().map(|t| &t.entries),
            z.as_ref().map(|t| &t.entries),
            "U64 vs ZMM tape differ for: {json:?}"
        );
        assert_eq!(
            y.as_ref().map(|t| &t.entries),
            z.as_ref().map(|t| &t.entries),
            "YMM vs ZMM tape differ for: {json:?}"
        );
        z
    }

    fn te_str(s: &'static str) -> TapeEntry<'static> {
        TapeEntry::String(s)
    }
    fn te_key(s: &'static str) -> TapeEntry<'static> {
        TapeEntry::Key(s)
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
        assert_eq!(
            t.entries,
            vec![
                TapeEntry::StartObject(6), // 0
                te_key("a"),               // 1
                TapeEntry::StartArray(5),  // 2
                te_num("1"),               // 3
                te_num("2"),               // 4
                TapeEntry::EndArray,       // 5
                TapeEntry::EndObject,      // 6
            ]
        );
        assert_eq!(t.entries[0], TapeEntry::StartObject(6));
        assert_eq!(t.entries[2], TapeEntry::StartArray(5));
    }

    #[test]
    fn tape_multi_key_object() {
        let t = run_tape(r#"{"x":1,"y":2}"#).unwrap();
        assert_eq!(
            t.entries,
            vec![
                TapeEntry::StartObject(5), // 0 — points to EndObject at index 5
                te_key("x"),               // 1
                te_num("1"),               // 2
                te_key("y"),               // 3
                te_num("2"),               // 4
                TapeEntry::EndObject,      // 5
            ]
        );
        assert_eq!(t.entries[0], TapeEntry::StartObject(5));
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
        let end = t.entries[1]
            .as_start_object()
            .expect("expected StartObject at index 1");
        assert_eq!(end, 4);
        // After the object the next item is at end + 1 = 5.
        assert_eq!(t.entries[5], te_num("2"));
    }

    #[test]
    fn tape_object_iter() {
        let t = run_tape(r#"{"x":1,"y":true,"z":"hi"}"#).unwrap();
        let root = t.root().unwrap();
        let pairs: Vec<_> = root
            .object_iter()
            .expect("should be object")
            .map(|(k, v)| (k.to_string(), (v.as_number_str(), v.as_bool(), v.as_str())))
            .collect();
        assert_eq!(pairs.len(), 3);
        assert_eq!(pairs[0].0, "x");
        assert_eq!(pairs[0].1, (Some("1"), None, None));
        assert_eq!(pairs[1].0, "y");
        assert_eq!(pairs[1].1, (None, Some(true), None));
        assert_eq!(pairs[2].0, "z");
        assert_eq!(pairs[2].1, (None, None, Some("hi")));
        // Non-object returns None.
        let at = parse_to_tape("[1]", classify_u64).unwrap();
        assert!(at.root().unwrap().object_iter().is_none());
    }

    #[test]
    fn tape_array_iter() {
        let t = run_tape(r#"[1,"two",false,null]"#).unwrap();
        let root = t.root().unwrap();
        let items: Vec<_> = root.array_iter().expect("should be array").collect();
        assert_eq!(items.len(), 4);
        assert_eq!(items[0].as_number_str(), Some("1"));
        assert_eq!(items[1].as_str(), Some("two"));
        assert_eq!(items[2].as_bool(), Some(false));
        assert!(items[3].is_null());
        // Nested structures count as single elements.
        let nt = run_tape(r#"[[1,2],{"a":3}]"#).unwrap();
        let nelems: Vec<_> = nt.root().unwrap().array_iter().unwrap().collect();
        assert_eq!(nelems.len(), 2);
        assert!(nelems[0].is_array());
        assert!(nelems[1].is_object());
        // Non-array returns None.
        let ot = parse_to_tape(r#"{"a":1}"#, classify_u64).unwrap();
        assert!(ot.root().unwrap().array_iter().is_none());
    }
}
