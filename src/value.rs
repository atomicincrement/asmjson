use std::borrow::Cow;

use crate::JsonWriter;

// ---------------------------------------------------------------------------
// Public Value type
// ---------------------------------------------------------------------------

/// A parsed JSON value (tree representation).
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

// ---------------------------------------------------------------------------
// JSON number validation
// ---------------------------------------------------------------------------

// Validate that `s` is a well-formed JSON number:
//   number = [ "-" ] ( "0" | [1-9][0-9]* ) [ "." [0-9]+ ] [ ("e"|"E") ["+"|"-"] [0-9]+ ]
#[inline(never)]
pub(crate) fn is_valid_json_number(s: &[u8]) -> bool {
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

// ---------------------------------------------------------------------------
// Frame helpers
// ---------------------------------------------------------------------------

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
// ValueWriter — builds the nested Value tree
// ---------------------------------------------------------------------------

pub(crate) struct ValueWriter<'a> {
    frames: Vec<Frame<'a>>,
    result: Option<Value<'a>>,
}

impl<'a> ValueWriter<'a> {
    pub(crate) fn new() -> Self {
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
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::borrow::Cow;

    use crate::{choose_classifier, classify_xmm, classify_ymm, parse_json};

    use super::Value;

    fn run_xmm(json: &'static str) -> Option<Value<'static>> {
        parse_json(json, classify_xmm)
    }

    fn run_ymm(json: &'static str) -> Option<Value<'static>> {
        parse_json(json, classify_ymm)
    }

    fn run_zmm(json: &'static str) -> Option<Value<'static>> {
        parse_json(json, choose_classifier())
    }

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
}
