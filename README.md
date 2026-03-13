# asmjson

[![CI](https://github.com/andy-thomason/asmjson/actions/workflows/ci.yml/badge.svg)](https://github.com/andy-thomason/asmjson/actions/workflows/ci.yml)
[![crates.io](https://img.shields.io/crates/v/asmjson.svg)](https://crates.io/crates/asmjson)
[![docs.rs](https://docs.rs/asmjson/badge.svg)](https://docs.rs/asmjson)

A fast JSON parser that classifies 64 bytes at a time using SIMD or portable
SWAR (SIMD-Within-A-Register) bit tricks, enabling entire whitespace runs and
string bodies to be skipped in a single operation.

## Quick start

```rust
use asmjson::{parse_json, choose_classifier, JsonRef};

let classify = choose_classifier(); // picks best for the current CPU
let value = parse_json(r#"{"name":"Alice","age":30}"#, classify).unwrap();

assert_eq!(value.get("name").as_str(), Some("Alice"));
assert_eq!(value.get("age").as_i64(), Some(30));
```

For repeated parses, store the result of `choose_classifier` in a static once
cell or pass it through your application rather than calling it on every parse.

## Output formats

- `parse_json` — allocates a nested `Value` tree (convenient, heap-allocated).
- `parse_to_tape` — allocates a flat `Tape` of tokens with O(1) structural skips.
- `parse_with` — drives a custom `JsonWriter` sink; zero extra allocation.

## Classifiers

The classifier is a plain function pointer that labels 64 bytes at a time.
Three are provided:

| Classifier      | ISA           | Speed   |
|-----------------|---------------|---------|
| `classify_zmm`  | AVX-512BW     | fastest |
| `classify_ymm`  | AVX2          | fast    |
| `classify_u64`  | portable SWAR | good    |

Use `choose_classifier` to select automatically at runtime.

## Benchmarks

Measured on a single core with `cargo bench` against 10 MiB of synthetic JSON.
Comparison points are `simd-json` (borrowed output, AVX2) and `serde_json`.

| Parser              | string array | string object | mixed        |
|---------------------|:------------:|:-------------:|:------------:|
| asmjson zmm (tape)  | 8.44 GiB/s   | 5.64 GiB/s    | 397.5 MiB/s  |
| asmjson zmm         | 6.06 GiB/s   | 5.23 GiB/s    | 265.3 MiB/s  |
| asmjson u64         | 5.86 GiB/s   | 4.21 GiB/s    | 261.3 MiB/s  |
| asmjson ymm         | 5.49 GiB/s   | 4.43 GiB/s    | 265.1 MiB/s  |
| serde_json          | 2.51 GiB/s   | 0.59 GiB/s    |  95.5 MiB/s  |
| simd-json borrowed  | 2.14 GiB/s   | 1.33 GiB/s    | 183.1 MiB/s  |

The tape output is consistently the fastest because it skips object/array
construction entirely.  The portable `u64` SWAR classifier matches or beats
AVX2 (`ymm`) on string-heavy workloads and is competitive on mixed JSON.

## Internal state machine

Each byte of the input is labelled below with the state that handles it.
States that skip whitespace via `trailing_zeros` handle both the whitespace
bytes **and** the following dispatch byte in the same loop iteration.

```text
{ "key1" : "value1" , "key2": [123, 456 , 768], "key3" : { "nested_key" : true} }
VOOKKKKKDDCCSSSSSSSFFOOKKKKKDCCRAAARRAAAFRRAAAFOOKKKKKDDCCOOKKKKKKKKKKKDDCCAAAAFF
```

State key:
* `V` = `ValueWhitespace` — waiting for the first byte of any value
* `O` = `ObjectStart`     — after `{` or `,` in an object; skips whitespace, expects `"` or `}`
* `K` = `KeyChars`        — inside a quoted key; bulk-skipped via the backslash/quote masks
* `D` = `KeyEnd`          — after closing `"` of a key; skips whitespace, expects `:`
* `C` = `AfterColon`      — after `:`; skips whitespace, dispatches to the value type
* `S` = `StringChars`     — inside a quoted string value; bulk-skipped via the backslash/quote masks
* `F` = `AfterValue`      — after any complete value; skips whitespace, expects `,`/`}`/`]`
* `R` = `ArrayStart`      — after `[` or `,` in an array; skips whitespace, dispatches value
* `A` = `AtomChars`       — inside a number, `true`, `false`, or `null`

A few things to notice in the annotation:

* `OO`: `ObjectStart` eats the space *and* the opening `"` of a key in one
  shot via the `trailing_zeros` whitespace skip.
* `DD` / `CC`: `KeyEnd` eats the space *and* `:` together; `AfterColon`
  eats the space *and* the value-start byte — structural punctuation costs
  no extra iterations.
* `SSSSSSS`: `StringChars` covers the entire `value1"` run including the
  closing quote (bulk AVX-512 skip + dispatch in one pass through the chunk).
* `RAAARRAAAFRRAAAF`: inside the array `[123, 456 , 768]` each `R` covers
  the skip-to-digit hop; `AAA` covers the digit characters plus their
  terminating `,` / space / `]`.
* `KKKKKKKKKKK` (11 bytes): the 10-character `nested_key` body *and* its
  closing `"` are all handled by `KeyChars` in one bulk-skip pass.

## License

MIT — see [LICENSE](https://github.com/andy-thomason/asmjson/blob/master/LICENSE).
