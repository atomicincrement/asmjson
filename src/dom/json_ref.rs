use super::{TapeEntryKind, TapeRef, tape_skip};

// ---------------------------------------------------------------------------
// JsonRef trait
// ---------------------------------------------------------------------------

/// Read-only, tree-like access to a parsed JSON value.
///
/// Modelled on `serde_json::Value`'s accessor methods.  The lifetime `'a`
/// is the string-access lifetime: returned `&'a str` slices are valid for at
/// least `'a`.
///
/// Implemented by:
/// - [`TapeRef<'a, _>`] — lightweight cursor into a flat [`super::Tape`].
/// - `Option<J>` where `J: JsonRef<'a>` — transparent wrapper enabling chaining
///   without intermediate `?` or `.and_then`: `root.get("a").get("b").as_str()`.
pub trait JsonRef<'a>: Sized + Copy {
    /// The concrete node type returned by [`get`](JsonRef::get) and
    /// [`index_at`](JsonRef::index_at).
    ///
    /// For concrete node types ([`TapeRef`]) this is `Self`.
    /// For `Option<J>` it is `J::Item`, keeping chains flat:
    /// `opt.get("a")` returns `Option<J::Item>`, not `Option<Option<J>>`.
    type Item: JsonRef<'a>;

    // ------------------------------------------------------------------
    // Type-test helpers (all have default implementations)
    // ------------------------------------------------------------------

    /// Returns `true` if this value is JSON `null`.
    fn is_null(self) -> bool {
        self.as_null().is_some()
    }
    /// Returns `true` if this value is a JSON boolean.
    fn is_bool(self) -> bool {
        self.as_bool().is_some()
    }
    /// Returns `true` if this value is a JSON number.
    fn is_number(self) -> bool {
        self.as_number_str().is_some()
    }
    /// Returns `true` if this value is a JSON string.
    fn is_string(self) -> bool {
        self.as_str().is_some()
    }
    /// Returns `true` if this value is a JSON array.
    fn is_array(self) -> bool;
    /// Returns `true` if this value is a JSON object.
    fn is_object(self) -> bool;

    // ------------------------------------------------------------------
    // Scalar accessors
    // ------------------------------------------------------------------

    /// Returns `Some(())` if this value is `null`, otherwise `None`.
    fn as_null(self) -> Option<()>;
    /// Returns the boolean if this value is a boolean, otherwise `None`.
    fn as_bool(self) -> Option<bool>;
    /// Returns the raw number token if this value is a number, otherwise `None`.
    ///
    /// The slice always contains valid JSON number syntax.
    fn as_number_str(self) -> Option<&'a str>;
    /// Returns the string content if this value is a JSON string, otherwise `None`.
    fn as_str(self) -> Option<&'a str>;

    /// Parse the number as `i64`.
    ///
    /// Returns `None` for non-numbers or when the value is out of range.
    fn as_i64(self) -> Option<i64> {
        self.as_number_str()?.parse().ok()
    }
    /// Parse the number as `u64`.
    ///
    /// Returns `None` for non-numbers or when the value is out of range.
    fn as_u64(self) -> Option<u64> {
        self.as_number_str()?.parse().ok()
    }
    /// Parse the number as `f64`.
    ///
    /// Returns `None` for non-numbers.
    fn as_f64(self) -> Option<f64> {
        self.as_number_str()?.parse().ok()
    }

    // ------------------------------------------------------------------
    // Collection accessors
    // ------------------------------------------------------------------

    /// Look up an object member by key.
    ///
    /// Returns `None` if this value is not an object or the key is absent.
    fn get(self, key: &str) -> Option<Self::Item>;

    /// Index into an array by zero-based position.
    ///
    /// Returns `None` if this value is not an array or `i` is out of bounds.
    fn index_at(self, i: usize) -> Option<Self::Item>;

    /// Returns the number of elements (array) or key-value pairs (object).
    ///
    /// Returns `None` if this value is neither an array nor an object.
    fn len(self) -> Option<usize>;
}

// ---------------------------------------------------------------------------
// JsonRef impl for TapeRef<'t, 'src>
// ---------------------------------------------------------------------------

impl<'t, 'src: 't> JsonRef<'t> for TapeRef<'t, 'src> {
    type Item = Self;

    fn is_array(self) -> bool {
        self.tape[self.pos].kind() == TapeEntryKind::StartArray
    }

    fn is_object(self) -> bool {
        self.tape[self.pos].kind() == TapeEntryKind::StartObject
    }

    fn as_null(self) -> Option<()> {
        (self.tape[self.pos].kind() == TapeEntryKind::Null).then_some(())
    }

    fn as_bool(self) -> Option<bool> {
        self.tape[self.pos].as_bool()
    }

    fn as_number_str(self) -> Option<&'t str> {
        self.tape[self.pos].as_number()
    }

    fn as_str(self) -> Option<&'t str> {
        self.tape[self.pos].as_string()
    }

    fn get(self, key: &str) -> Option<Self> {
        let end_idx = self.tape[self.pos].as_start_object()?;
        let mut i = self.pos + 1;
        while i < end_idx {
            let k_str: &str = self.tape[i].as_key()?;
            let val_pos = i + 1;
            if k_str == key {
                return Some(TapeRef {
                    tape: self.tape,
                    pos: val_pos,
                });
            }
            i = tape_skip(self.tape, val_pos);
        }
        None
    }

    fn index_at(self, idx: usize) -> Option<Self> {
        let end_idx = self.tape[self.pos].as_start_array()?;
        let mut i = self.pos + 1;
        let mut count = 0usize;
        while i < end_idx {
            if count == idx {
                return Some(TapeRef {
                    tape: self.tape,
                    pos: i,
                });
            }
            i = tape_skip(self.tape, i);
            count += 1;
        }
        None
    }

    fn len(self) -> Option<usize> {
        if let Some(end_idx) = self.tape[self.pos].as_start_array() {
            let mut count = 0usize;
            let mut i = self.pos + 1;
            while i < end_idx {
                i = tape_skip(self.tape, i);
                count += 1;
            }
            return Some(count);
        }
        if let Some(end_idx) = self.tape[self.pos].as_start_object() {
            let mut count = 0usize;
            let mut i = self.pos + 1;
            while i < end_idx {
                // entries[i] is Key, entries[i+1] is value
                i = tape_skip(self.tape, i + 1);
                count += 1;
            }
            return Some(count);
        }
        None
    }
}

// ---------------------------------------------------------------------------
// JsonRef impl for Option<J> — enables x.get("a").get("b") chaining
// ---------------------------------------------------------------------------

/// Blanket implementation of [`JsonRef`] for `Option<J>` where `J: JsonRef`.
///
/// Every accessor returns the same result as calling it on the inner value,
/// or the appropriate "absent" answer when `self` is `None`:
///
/// - Boolean tests (`is_*`) return `false`.
/// - `as_*` accessors return `None`.
/// - `get`, `index_at`, `len` return `None`.
///
/// This allows chaining without intermediate `?` or `.and_then`:
///
/// ```rust,ignore
/// let city = root.get("address").get("city").as_str();
/// let first = root.get("items").index_at(0).as_i64();
/// ```
impl<'a, J: JsonRef<'a>> JsonRef<'a> for Option<J> {
    /// Chaining stays flat: `Option<J>` produces the same item type as `J`.
    type Item = J::Item;

    fn is_array(self) -> bool {
        self.is_some_and(|v| v.is_array())
    }
    fn is_object(self) -> bool {
        self.is_some_and(|v| v.is_object())
    }
    fn as_null(self) -> Option<()> {
        self?.as_null()
    }
    fn as_bool(self) -> Option<bool> {
        self?.as_bool()
    }
    fn as_number_str(self) -> Option<&'a str> {
        self?.as_number_str()
    }
    fn as_str(self) -> Option<&'a str> {
        self?.as_str()
    }
    fn get(self, key: &str) -> Option<J::Item> {
        self?.get(key)
    }
    fn index_at(self, i: usize) -> Option<J::Item> {
        self?.index_at(i)
    }
    fn len(self) -> Option<usize> {
        self?.len()
    }
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::super::Tape;
    use crate::parse_to_tape;

    use super::JsonRef;

    fn run_tape(json: &'static str) -> Option<Tape<'static>> {
        parse_to_tape(json)
    }

    // -----------------------------------------------------------------------
    // JsonRef tests — exercise the trait on TapeRef
    // -----------------------------------------------------------------------

    #[test]
    fn jsonref_scalars_tape() {
        let t = run_tape("null").unwrap();
        let r = t.root().unwrap();
        assert!(r.is_null());
        assert!(r.as_null().is_some());

        let t = run_tape("true").unwrap();
        assert_eq!(t.root().unwrap().as_bool(), Some(true));

        let t = run_tape("false").unwrap();
        assert_eq!(t.root().unwrap().as_bool(), Some(false));

        let t = run_tape("42").unwrap();
        let r = t.root().unwrap();
        assert!(r.is_number());
        assert_eq!(r.as_number_str(), Some("42"));
        assert_eq!(r.as_i64(), Some(42));
        assert_eq!(r.as_u64(), Some(42));
        assert_eq!(r.as_f64(), Some(42.0));

        let t = run_tape(r#""hello""#).unwrap();
        assert_eq!(t.root().unwrap().as_str(), Some("hello"));
    }

    #[test]
    fn jsonref_object_get() {
        let src = r#"{"x":1,"y":"hi","z":true}"#;
        let t = run_tape(src).unwrap();
        let tr = t.root().unwrap();
        assert!(tr.is_object());
        assert_eq!(tr.get("x").and_then(|r| r.as_i64()), Some(1));
        assert_eq!(tr.get("y").and_then(|r| r.as_str()), Some("hi"));
        assert_eq!(tr.get("z").and_then(|r| r.as_bool()), Some(true));
        assert!(tr.get("missing").is_none());
        assert_eq!(tr.len(), Some(3));
    }

    #[test]
    fn jsonref_array_index() {
        let src = r#"[1,"two",false,null]"#;
        let t = run_tape(src).unwrap();
        let tr = t.root().unwrap();
        assert!(tr.is_array());
        assert_eq!(tr.len(), Some(4));
        assert_eq!(tr.index_at(0).and_then(|r| r.as_i64()), Some(1));
        assert_eq!(tr.index_at(1).and_then(|r| r.as_str()), Some("two"));
        assert_eq!(tr.index_at(2).and_then(|r| r.as_bool()), Some(false));
        assert!(tr.index_at(3).unwrap().is_null());
        assert!(tr.index_at(4).is_none());
    }

    #[test]
    fn jsonref_nested() {
        let src = r#"{"items":[10,20,30],"meta":{"count":3}}"#;
        let t = run_tape(src).unwrap();
        let tr = t.root().unwrap();
        assert_eq!(
            tr.get("items")
                .and_then(|a| a.index_at(1))
                .and_then(|n| n.as_i64()),
            Some(20)
        );
        assert_eq!(
            tr.get("meta")
                .and_then(|o| o.get("count"))
                .and_then(|n| n.as_i64()),
            Some(3)
        );
    }

    #[test]
    fn jsonref_generic_fn() {
        fn first_item<'a, J: JsonRef<'a>>(val: J) -> Option<i64> {
            val.index_at(0)?.as_i64()
        }
        let src = "[7,8,9]";
        let t = run_tape(src).unwrap();
        assert_eq!(first_item(t.root().unwrap()), Some(7));
    }

    #[test]
    fn jsonref_option_chaining() {
        let src = r#"{"a":{"b":{"c":42}},"arr":[10,20,30]}"#;
        let t = run_tape(src).unwrap();
        let tr = t.root().unwrap();
        assert_eq!(tr.get("a").get("b").get("c").as_i64(), Some(42));
        assert!(tr.get("a").get("missing").get("c").as_i64().is_none());
        assert!(tr.get("none").get("b").as_i64().is_none());

        let src2 = r#"{"items":[{"val":1},{"val":2},{"val":3}]}"#;
        let t2 = run_tape(src2).unwrap();
        assert_eq!(
            t2.root()
                .unwrap()
                .get("items")
                .index_at(1)
                .get("val")
                .as_i64(),
            Some(2)
        );
    }
}
