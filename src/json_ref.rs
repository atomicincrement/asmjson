use crate::tape::{TapeEntry, TapeRef, tape_skip};
use crate::value::Value;

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
/// - `&'a Value<'a>` — borrows a tree [`Value`].
/// - [`TapeRef<'a, _>`] — lightweight cursor into a flat [`crate::tape::Tape`].
/// - `Option<J>` where `J: JsonRef<'a>` — transparent wrapper enabling chaining
///   without intermediate `?` or `.and_then`: `root.get("a").get("b").as_str()`.
pub trait JsonRef<'a>: Sized + Copy {
    /// The concrete node type returned by [`get`](JsonRef::get) and
    /// [`index_at`](JsonRef::index_at).
    ///
    /// For concrete node types (`&'a Value<'a>`, [`TapeRef`]) this is `Self`.
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
// JsonRef impl for &'a Value<'a>
// ---------------------------------------------------------------------------

impl<'a> JsonRef<'a> for &'a Value<'a> {
    type Item = Self;

    fn is_array(self) -> bool {
        matches!(self, Value::Array(_))
    }

    fn is_object(self) -> bool {
        matches!(self, Value::Object(_))
    }

    fn as_null(self) -> Option<()> {
        matches!(self, Value::Null).then_some(())
    }

    fn as_bool(self) -> Option<bool> {
        if let Value::Bool(b) = self {
            Some(*b)
        } else {
            None
        }
    }

    fn as_number_str(self) -> Option<&'a str> {
        if let Value::Number(s) = self {
            Some(s)
        } else {
            None
        }
    }

    fn as_str(self) -> Option<&'a str> {
        if let Value::String(s) = self {
            Some(s.as_ref())
        } else {
            None
        }
    }

    fn get(self, key: &str) -> Option<Self> {
        if let Value::Object(pairs) = self {
            pairs
                .iter()
                .find(|(k, _)| k.as_ref() == key)
                .map(|(_, v)| v)
        } else {
            None
        }
    }

    fn index_at(self, i: usize) -> Option<Self> {
        if let Value::Array(items) = self {
            items.get(i)
        } else {
            None
        }
    }

    fn len(self) -> Option<usize> {
        match self {
            Value::Array(items) => Some(items.len()),
            Value::Object(pairs) => Some(pairs.len()),
            _ => None,
        }
    }
}

// ---------------------------------------------------------------------------
// JsonRef impl for TapeRef<'t, 'src>
// ---------------------------------------------------------------------------

impl<'t, 'src: 't> JsonRef<'t> for TapeRef<'t, 'src> {
    type Item = Self;

    fn is_array(self) -> bool {
        matches!(&self.tape[self.pos], TapeEntry::StartArray(_))
    }

    fn is_object(self) -> bool {
        matches!(&self.tape[self.pos], TapeEntry::StartObject(_))
    }

    fn as_null(self) -> Option<()> {
        matches!(&self.tape[self.pos], TapeEntry::Null).then_some(())
    }

    fn as_bool(self) -> Option<bool> {
        if let TapeEntry::Bool(b) = &self.tape[self.pos] {
            Some(*b)
        } else {
            None
        }
    }

    fn as_number_str(self) -> Option<&'t str> {
        if let TapeEntry::Number(s) = &self.tape[self.pos] {
            Some(s)
        } else {
            None
        }
    }

    fn as_str(self) -> Option<&'t str> {
        if let TapeEntry::String(s) = &self.tape[self.pos] {
            Some(s.as_ref())
        } else {
            None
        }
    }

    fn get(self, key: &str) -> Option<Self> {
        let end_idx = match &self.tape[self.pos] {
            TapeEntry::StartObject(e) => *e,
            _ => return None,
        };
        let mut i = self.pos + 1;
        while i < end_idx {
            if let TapeEntry::Key(k) = &self.tape[i] {
                let val_pos = i + 1;
                if k.as_ref() == key {
                    return Some(TapeRef {
                        tape: self.tape,
                        pos: val_pos,
                    });
                }
                i = tape_skip(self.tape, val_pos);
            } else {
                break;
            }
        }
        None
    }

    fn index_at(self, idx: usize) -> Option<Self> {
        let end_idx = match &self.tape[self.pos] {
            TapeEntry::StartArray(e) => *e,
            _ => return None,
        };
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
        match &self.tape[self.pos] {
            TapeEntry::StartArray(end_idx) => {
                let end_idx = *end_idx;
                let mut count = 0usize;
                let mut i = self.pos + 1;
                while i < end_idx {
                    i = tape_skip(self.tape, i);
                    count += 1;
                }
                Some(count)
            }
            TapeEntry::StartObject(end_idx) => {
                let end_idx = *end_idx;
                let mut count = 0usize;
                let mut i = self.pos + 1;
                while i < end_idx {
                    // entries[i] is Key, entries[i+1] is value
                    i = tape_skip(self.tape, i + 1);
                    count += 1;
                }
                Some(count)
            }
            _ => None,
        }
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
    use crate::tape::Tape;
    use crate::value::Value;
    use crate::{choose_classifier, classify_xmm, classify_ymm, parse_json, parse_to_tape};

    use super::JsonRef;

    // -----------------------------------------------------------------------
    // Helpers duplicated from value/tape test modules
    // -----------------------------------------------------------------------

    fn run(json: &'static str) -> Option<Value<'static>> {
        let x = parse_json(json, classify_xmm);
        let y = parse_json(json, classify_ymm);
        let z = parse_json(json, choose_classifier());
        assert_eq!(x, y, "XMM vs YMM differ for: {json:?}");
        assert_eq!(y, z, "YMM vs ZMM differ for: {json:?}");
        z
    }

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

    fn run_both(src: &'static str) -> (Value<'static>, Tape<'static>) {
        let v = run(src).unwrap();
        let t = run_tape(src).unwrap();
        (v, t)
    }

    // -----------------------------------------------------------------------
    // JsonRef tests — exercise the trait on both &Value and TapeRef
    // -----------------------------------------------------------------------

    #[test]
    fn jsonref_scalars_value() {
        let (v, _) = run_both("null");
        assert!((&v).is_null());
        assert!((&v).as_null().is_some());

        let (v, _) = run_both("true");
        assert!((&v).is_bool());
        assert_eq!((&v).as_bool(), Some(true));

        let (v, _) = run_both("false");
        assert_eq!((&v).as_bool(), Some(false));

        let (v, _) = run_both("42");
        assert!((&v).is_number());
        assert_eq!((&v).as_number_str(), Some("42"));
        assert_eq!((&v).as_i64(), Some(42));
        assert_eq!((&v).as_u64(), Some(42));
        assert_eq!((&v).as_f64(), Some(42.0));

        let (v, _) = run_both(r#""hello""#);
        assert!((&v).is_string());
        assert_eq!((&v).as_str(), Some("hello"));
    }

    #[test]
    fn jsonref_scalars_tape() {
        let (_, t) = run_both("null");
        let r = t.root().unwrap();
        assert!(r.is_null());
        assert!(r.as_null().is_some());

        let (_, t) = run_both("true");
        assert_eq!(t.root().unwrap().as_bool(), Some(true));

        let (_, t) = run_both("false");
        assert_eq!(t.root().unwrap().as_bool(), Some(false));

        let (_, t) = run_both("42");
        let r = t.root().unwrap();
        assert!(r.is_number());
        assert_eq!(r.as_number_str(), Some("42"));
        assert_eq!(r.as_i64(), Some(42));
        assert_eq!(r.as_u64(), Some(42));
        assert_eq!(r.as_f64(), Some(42.0));

        let (_, t) = run_both(r#""hello""#);
        assert_eq!(t.root().unwrap().as_str(), Some("hello"));
    }

    #[test]
    fn jsonref_object_get() {
        let src = r#"{"x":1,"y":"hi","z":true}"#;
        let (v, t) = run_both(src);

        // &Value
        let vr = &v;
        assert!(vr.is_object());
        assert_eq!(vr.get("x").and_then(|v| v.as_i64()), Some(1));
        assert_eq!(vr.get("y").and_then(|v| v.as_str()), Some("hi"));
        assert_eq!(vr.get("z").and_then(|v| v.as_bool()), Some(true));
        assert!(vr.get("missing").is_none());
        assert_eq!(vr.len(), Some(3));

        // TapeRef
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
        let (v, t) = run_both(src);

        // &Value
        let vr = &v;
        assert!(vr.is_array());
        assert_eq!(vr.len(), Some(4));
        assert_eq!(vr.index_at(0).and_then(|v| v.as_i64()), Some(1));
        assert_eq!(vr.index_at(1).and_then(|v| v.as_str()), Some("two"));
        assert_eq!(vr.index_at(2).and_then(|v| v.as_bool()), Some(false));
        assert!(vr.index_at(3).unwrap().is_null());
        assert!(vr.index_at(4).is_none());

        // TapeRef
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
        let (v, t) = run_both(src);

        // &Value path: v["items"][1] == 20
        let vr = &v;
        assert_eq!(
            vr.get("items")
                .and_then(|a| a.index_at(1))
                .and_then(|n| n.as_i64()),
            Some(20)
        );
        assert_eq!(
            vr.get("meta")
                .and_then(|o| o.get("count"))
                .and_then(|n| n.as_i64()),
            Some(3)
        );

        // TapeRef same paths
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
        // Verify a generic function works over both representations.
        fn first_item<'a, J: JsonRef<'a>>(val: J) -> Option<i64> {
            val.index_at(0)?.as_i64()
        }
        let src = "[7,8,9]";
        let (v, t) = run_both(src);
        assert_eq!(first_item(&v), Some(7));
        assert_eq!(first_item(t.root().unwrap()), Some(7));
    }

    #[test]
    fn jsonref_option_chaining() {
        // The key feature: x.get("a").get("b") without intermediate ? or and_then.
        let src = r#"{"a":{"b":{"c":42}},"arr":[10,20,30]}"#;
        let (v, t) = run_both(src);

        // Three-level object chain — &Value
        assert_eq!((&v).get("a").get("b").get("c").as_i64(), Some(42));
        // Missing key at any point short-circuits to None
        assert!((&v).get("a").get("missing").get("c").as_i64().is_none());
        assert!((&v).get("none").get("b").as_i64().is_none());

        // Three-level object chain — TapeRef
        let tr = t.root().unwrap();
        assert_eq!(tr.get("a").get("b").get("c").as_i64(), Some(42));
        assert!(tr.get("a").get("missing").get("c").as_i64().is_none());
        assert!(tr.get("none").get("b").as_i64().is_none());

        // Mix get + index_at chaining
        let src2 = r#"{"items":[{"val":1},{"val":2},{"val":3}]}"#;
        let (v2, t2) = run_both(src2);
        assert_eq!((&v2).get("items").index_at(1).get("val").as_i64(), Some(2));
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
