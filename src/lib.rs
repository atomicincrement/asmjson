

//! This module parses JSON strings 64 bytes at a time using AVX-512BW instructions to quickly identify structural characters.
//!
//! Here is some example JSON with corresponding states:
//! ```json
//!  { "key1" : "value1" , "key2": [123, 456 , 768], "key3" : { "nested_key" : true} }
//! vvkkkkkkkkkvsssssssss,kkkkkkkkvvvnnavvnn-avvnnaakkkkkkkkkvvvkkkkkkkkkkkkkkvvnnnooo
//!  ( (cccc)-: (cccccc)-,-(cccc): ((c), (c)-, (c)), (cccc)-: ( (cccccccccc)-: (cc))-)
//! ```
//! 
//! v = JSON value start {, [, ", digit, t, f, n}
//! k = JSON key
//! s = String
//! a = Array
//! o = Object
//! n = number, bool or null.
//!
//! space = leading whitespace
//! s = start of a token
//! c = trailing chars of the start.
//! e = end of a token
//! t = trailing whitespace
//! 

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

    // `{` consumed; skip whitespace then expect `"` (key) or `}`.
    ObjectStart,

    // `[` consumed; skip whitespace then expect a value or `]`.
    ArrayStart,

    // A complete value was produced; skip whitespace then pop the context stack.
    AfterValue,
}

#[derive(PartialEq, Debug, Clone)]
enum Value {
    String(String),
    Number(f64),
    Bool(bool),
    Null,
    Object(Vec<(String, Value)>),
    Array(Vec<Value>)
}

fn parse_json(mut get_byte: impl FnMut() -> Option<u8>) -> Option<Value> {
    // Partially-built Object or Array sitting on the frame stack.
    enum Frame {
        Obj { key: String, members: Vec<(String, Value)> },
        Arr { elements: Vec<Value> },
    }

    // Parse a completed atom string into the right Value variant.
    fn parse_atom(s: &str) -> Value {
        match s {
            "true"  => Value::Bool(true),
            "false" => Value::Bool(false),
            "null"  => Value::Null,
            n       => Value::Number(n.parse().unwrap_or(0.0)),
        }
    }

    // Push a completed Value into the top frame, or set the top-level result.
    fn push_value(val: Value, frames: &mut Vec<Frame>, result: &mut Option<Value>) {
        match frames.last_mut() {
            Some(Frame::Arr { elements }) => elements.push(val),
            Some(Frame::Obj { key, members }) => {
                let k = std::mem::take(key);
                members.push((k, val));
            }
            None => *result = Some(val),
        }
    }

    // Close the top frame with `}` or `]` and push the resulting Value.
    fn close_frame(byte: u8, frames: &mut Vec<Frame>, result: &mut Option<Value>) {
        match byte {
            b'}' => {
                if let Some(Frame::Obj { members, .. }) = frames.pop() {
                    push_value(Value::Object(members), frames, result);
                }
            }
            b']' => {
                if let Some(Frame::Arr { elements }) = frames.pop() {
                    push_value(Value::Array(elements), frames, result);
                }
            }
            _ => {}
        }
    }

    let mut frames: Vec<Frame> = Vec::new();
    let mut buf = String::new();
    let mut state = State::ValueWhitespace;
    let mut result: Option<Value> = None;

    while let Some(byte) = get_byte() {
        state = match state {
            State::ValueWhitespace => match byte {
                b if b <= b' ' => State::ValueWhitespace,
                b'{' => { frames.push(Frame::Obj { key: String::new(), members: Vec::new() }); State::ObjectStart }
                b'[' => { frames.push(Frame::Arr { elements: Vec::new() }); State::ArrayStart }
                b'"' => { buf.clear(); State::StringChars }
                _    => { buf.clear(); buf.push(byte as char); State::AtomChars }
            },

            State::StringChars => match byte {
                b'\\' => { buf.push('\\'); State::StringEscape }                b'"'  => {
                    push_value(Value::String(std::mem::take(&mut buf)), &mut frames, &mut result);
                    State::AfterValue
                }
                _ => { buf.push(byte as char); State::StringChars }
            },
            State::StringEscape => { buf.push(byte as char); State::StringChars }

            State::KeyChars => match byte {
                b'\\' => { buf.push('\\'); State::KeyEscape }                b'"'  => State::KeyEnd,
                _ => { buf.push(byte as char); State::KeyChars }
            },
            State::KeyEscape => { buf.push(byte as char); State::KeyChars }
            State::KeyEnd => match byte {
                b if b <= b' ' => State::KeyEnd,
                b':' => {
                    if let Some(Frame::Obj { key, .. }) = frames.last_mut() {
                        *key = std::mem::take(&mut buf);
                    }
                    State::AfterColon
                }
                _ => State::KeyEnd,
            },
            State::AfterColon => match byte {
                b if b <= b' ' => State::AfterColon,
                b'{' => { frames.push(Frame::Obj { key: String::new(), members: Vec::new() }); State::ObjectStart }
                b'[' => { frames.push(Frame::Arr { elements: Vec::new() }); State::ArrayStart }
                b'"' => { buf.clear(); State::StringChars }
                _    => { buf.clear(); buf.push(byte as char); State::AtomChars }
            },

            State::AtomChars => match byte {
                b if b <= b' ' || matches!(b, b',' | b'}' | b']') => {
                    push_value(parse_atom(&buf), &mut frames, &mut result);
                    buf.clear();
                    match byte {
                        b'}' => { close_frame(b'}', &mut frames, &mut result); State::AfterValue }
                        b']' => { close_frame(b']', &mut frames, &mut result); State::AfterValue }
                        b',' => match frames.last() {
                            Some(Frame::Arr { .. }) => State::ArrayStart,
                            Some(Frame::Obj { .. }) => State::ObjectStart,
                            None                    => State::AfterValue,
                        },
                        _ => State::AfterValue, // whitespace delimiter
                    }
                }
                _ => { buf.push(byte as char); State::AtomChars }
            },

            State::ObjectStart => match byte {
                b if b <= b' ' => State::ObjectStart,
                b'"' => { buf.clear(); State::KeyChars }
                b'}' => {
                    close_frame(b'}', &mut frames, &mut result);
                    State::AfterValue
                }
                _ => State::ObjectStart,
            },

            State::ArrayStart => match byte {
                b if b <= b' ' => State::ArrayStart,
                b']' => {
                    close_frame(b']', &mut frames, &mut result);
                    State::AfterValue
                }
                b'{' => { frames.push(Frame::Obj { key: String::new(), members: Vec::new() }); State::ObjectStart }
                b'[' => { frames.push(Frame::Arr { elements: Vec::new() }); State::ArrayStart }
                b'"' => { buf.clear(); State::StringChars }
                _    => { buf.clear(); buf.push(byte as char); State::AtomChars }
            },

            State::AfterValue => match byte {
                b if b <= b' ' => State::AfterValue,
                b',' => match frames.last() {
                    Some(Frame::Obj { .. }) => State::ObjectStart,
                    Some(Frame::Arr { .. }) => State::ArrayStart,
                    None                    => State::AfterValue,
                },
                b'}' => {
                    close_frame(b'}', &mut frames, &mut result);
                    State::AfterValue
                }
                b']' => {
                    close_frame(b']', &mut frames, &mut result);
                    State::AfterValue
                }
                _ => State::AfterValue,
            },
        };
    }

    // Flush a trailing atom not followed by a delimiter (e.g. top-level `42`).
    if state == State::AtomChars && !buf.is_empty() {
        push_value(parse_atom(&buf), &mut frames, &mut result);
    }

    result
}



// /// Input: a complete JSON string.
// fn partition_json(src: &str) {
// }

// struct ByteState {
//     braces: u64,
//     brackets: u64,
//     quotes: u64,
//     backslashes: u64,
//     whitespace: u64,
// }

// struct JsonParser {
//     brace: [u8; 64],
//     bracket: [u8; 64],
//     quote: [u8; 64],
//     backslash: [u8; 64],
//     space: [u8; 64],
// }

// impl JsonParser {
//     fn new() -> Self {
//         Self {
//             brace:     [b'{'; 64],
//             bracket:   [b'['; 64],
//             quote:     [b'"'; 64],
//             backslash: [b'\\'; 64],
//             space:     [b' '; 64],
//         }
//     }
// }

// fn next_state(src: &[u8], parser: &JsonParser) -> ByteState {
//     assert!(src.len() >= 64, "src must be at least 64 bytes");

//     let braces: u64;
//     let brackets: u64;
//     let quotes: u64;
//     let backslashes: u64;
//     let whitespace: u64;

//     // Inline AVX-512BW: load 64 bytes, compare against pre-built 64-byte
//     // needle vectors loaded directly from `needles`, collect 64-bit masks.
//     unsafe {
//         std::arch::asm!(
//             // Load 64 source bytes.
//             "vmovdqu64 zmm0, [{src}]",

//             // '{' needle -> braces mask.
//             "vpcmpeqb k1, zmm0, zmmword ptr [{n_brace}]",
//             "kmovq {braces}, k1",

//             // '[' needle -> brackets mask.
//             "vpcmpeqb k1, zmm0, zmmword ptr [{n_bracket}]",
//             "kmovq {brackets}, k1",

//             // '"' needle -> quotes mask.
//             "vpcmpeqb k1, zmm0, zmmword ptr [{n_quote}]",
//             "kmovq {quotes}, k1",

//             // '\\' needle -> backslashes mask.
//             "vpcmpeqb k1, zmm0, zmmword ptr [{n_backslash}]",
//             "kmovq {backslashes}, k1",

//             // Whitespace: any byte <= 0x20 (space).
//             "vpcmpub k1, zmm0, zmmword ptr [{n_space}], 2",
//             "kmovq {whitespace}, k1",

//             src          = in(reg)  src.as_ptr(),
//             n_brace      = in(reg)  parser.brace.as_ptr(),
//             n_bracket    = in(reg)  parser.bracket.as_ptr(),
//             n_quote      = in(reg)  parser.quote.as_ptr(),
//             n_backslash  = in(reg)  parser.backslash.as_ptr(),
//             n_space      = in(reg)  parser.space.as_ptr(),
//             braces       = out(reg) braces,
//             brackets     = out(reg) brackets,
//             quotes       = out(reg) quotes,
//             backslashes  = out(reg) backslashes,
//             whitespace   = out(reg) whitespace,
//             out("zmm0") _,
//             out("k1")   _,
//             options(nostack, readonly),
//         );
//     }

//     ByteState { braces, brackets, quotes, backslashes, whitespace }
// }

#[cfg(test)]
mod tests {
    use super::*;

    fn run(json: &'static str) -> Option<Value> {
        let bytes = json.as_bytes();
        let mut idx = 0usize;
        parse_json(|| {
            if idx < bytes.len() {
                let b = bytes[idx];
                idx += 1;
                Some(b)
            } else {
                None
            }
        })
    }

    fn s(v: &str) -> Value { Value::String(v.to_string()) }
    fn n(v: f64)  -> Value { Value::Number(v) }
    fn obj(members: &[(&str, Value)]) -> Value {
        Value::Object(members.iter().map(|(k, v)| (k.to_string(), v.clone())).collect())
    }
    fn arr(elements: Vec<Value>) -> Value { Value::Array(elements) }

    #[test]
    fn test_string() {
        assert_eq!(run(r#""hello""#), Some(s("hello")));
    }

    #[test]
    fn test_escaped_string() {
        // Escapes are stored raw (backslash + char), not expanded.
        assert_eq!(run(r#""hello \"world\"""#), Some(s(r#"hello \"world\""#)));
        assert_eq!(run(r#""line\\nbreak""#),    Some(s(r#"line\\nbreak"#)));
        assert_eq!(run(r#""tab\there""#),       Some(s(r#"tab\there"#)));
    }

    #[test]
    fn test_number() {
        assert_eq!(run("42"), Some(n(42.0)));
    }

    #[test]
    fn test_bool_null() {
        assert_eq!(run("true"),  Some(Value::Bool(true)));
        assert_eq!(run("false"), Some(Value::Bool(false)));
        assert_eq!(run("null"),  Some(Value::Null));
    }

    #[test]
    fn test_empty_object() {
        assert_eq!(run("{}"), Some(Value::Object(vec![])));
    }

    #[test]
    fn test_empty_array() {
        assert_eq!(run("[]"), Some(Value::Array(vec![])));
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
            run(r#"{ "key1" : "value1" , "key2": [123, 456, 768], "key3" : { "nested_key" : true } }"#),
            Some(obj(&[
                ("key1", s("value1")),
                ("key2", arr(vec![n(123.0), n(456.0), n(768.0)])),
                ("key3", obj(&[("nested_key", Value::Bool(true))])),
            ]))
        );
    }

    #[test]
    fn test_array_of_mixed() {
        assert_eq!(
            run(r#"[1, "two", true, null, {"x": 3}]"#),
            Some(arr(vec![
                n(1.0),
                s("two"),
                Value::Bool(true),
                Value::Null,
                obj(&[("x", n(3.0))]),
            ]))
        );
    }

    #[test]
    fn test_whitespace() {
        assert_eq!(run("  \n  42  \n"), Some(n(42.0)));
    }
}
