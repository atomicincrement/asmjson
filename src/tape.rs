use std::borrow::Cow;

use crate::JsonWriter;

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
}

// ---------------------------------------------------------------------------
// TapeWriter — builds the flat Tape
// ---------------------------------------------------------------------------

pub(crate) struct TapeWriter<'a> {
    entries: Vec<TapeEntry<'a>>,
    /// Indices of unmatched `StartObject` / `StartArray` waiting for backfill.
    open: Vec<usize>,
}

impl<'a> TapeWriter<'a> {
    pub(crate) fn new() -> Self {
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
    match &entries[pos] {
        TapeEntry::StartObject(end) => end + 1,
        TapeEntry::StartArray(end) => end + 1,
        _ => pos + 1,
    }
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::borrow::Cow;

    use crate::{choose_classifier, classify_u64, classify_ymm, parse_to_tape};

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
