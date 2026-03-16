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
    /// A string value with no escape sequences; borrowed directly from the source.
    String(&'a str),
    /// A string value that contained escape sequences; owns the decoded text.
    EscapedString(Box<str>),
    /// An object key with no escape sequences; borrowed directly from the source.
    Key(&'a str),
    /// An object key that contained escape sequences; owns the decoded text.
    EscapedKey(Box<str>),
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
    fn string(&mut self, s: &'a str) {
        self.entries.push(TapeEntry::String(s));
    }
    fn escaped_string(&mut self, s: Box<str>) {
        self.entries.push(TapeEntry::EscapedString(s));
    }
    fn key(&mut self, s: &'a str) {
        self.entries.push(TapeEntry::Key(s));
    }
    fn escaped_key(&mut self, s: Box<str>) {
        self.entries.push(TapeEntry::EscapedKey(s));
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
        let key: &'t str = match &self.tape[self.pos] {
            TapeEntry::Key(k) => k,
            TapeEntry::EscapedKey(k) => k.as_ref(),
            _ => return None,
        };
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
        if let TapeEntry::StartObject(end) = self.tape[self.pos] {
            Some(TapeObjectIter {
                tape: self.tape,
                pos: self.pos + 1,
                end,
            })
        } else {
            None
        }
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
        if let TapeEntry::StartArray(end) = self.tape[self.pos] {
            Some(TapeArrayIter {
                tape: self.tape,
                pos: self.pos + 1,
                end,
            })
        } else {
            None
        }
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
