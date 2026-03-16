# asmjson

[![CI](https://github.com/andy-thomason/asmjson/actions/workflows/ci.yml/badge.svg)](https://github.com/andy-thomason/asmjson/actions/workflows/ci.yml)
[![crates.io](https://img.shields.io/crates/v/asmjson.svg)](https://crates.io/crates/asmjson)
[![docs.rs](https://docs.rs/asmjson/badge.svg)](https://docs.rs/asmjson)

A fast JSON parser that classifies 64 bytes at a time using SIMD or portable
SWAR (SIMD-Within-A-Register) bit tricks, enabling entire whitespace runs and
string bodies to be skipped in a single operation.

> **⚠️ Experimental — not production ready.**  
> This crate is a research and benchmarking project.  The API is unstable, test
> coverage is incomplete, and the hand-written assembly has not been audited for
> safety or correctness under adversarial input.  Use `serde_json` or `sonic-rs`
> for production workloads.

## Quick start

```rust
use asmjson::{parse_to_tape, choose_classifier, JsonRef};

let classify = choose_classifier(); // picks best for the current CPU
let tape = parse_to_tape(r#"{"name":"Alice","age":30}"#, classify).unwrap();

assert_eq!(tape.root().get("name").as_str(), Some("Alice"));
assert_eq!(tape.root().get("age").as_i64(), Some(30));
```

For repeated parses, store the result of `choose_classifier` in a static once
cell or pass it through your application rather than calling it on every parse.

## Benchmarks

Measured on a single core with `cargo bench` against 10 MiB of synthetic JSON.
Comparison point is `sonic-rs` (lazy Value, AVX2).

| Parser               | string array | string object | mixed      |
|----------------------|:------------:|:-------------:|:----------:|
| asmjson zmm dyn      | 10.93 GiB/s  | 7.50 GiB/s    | 655 MiB/s  |
| asmjson zmm tape     | 10.75 GiB/s  | 7.10 GiB/s    | 920 MiB/s  |
| asmjson zmm          | 8.39 GiB/s   | 6.16 GiB/s    | 640 MiB/s  |
| sonic-rs             | 7.05 GiB/s   | 4.05 GiB/s    | 483 MiB/s  |
| asmjson u64          | 6.31 GiB/s   | 4.43 GiB/s    | 599 MiB/s  |
| serde_json           | 2.41 GiB/s   | 539 MiB/s     | 83 MiB/s   |
| simd-json †          | 1.94 GiB/s   | 1.20 GiB/s    | 175 MiB/s  |

† simd-json numbers include buffer cloning overhead (see note above).

Note: `asmjson zmm dyn` and `asmjson zmm tape` are implemented entirely in
hand-written x86-64 assembly using AVX-512BW instructions.  They require a
CPU with AVX-512BW support (Ice Lake or later on Intel, Zen 4 or later on AMD)
and are not available on other architectures.

asmjson zmm dyn leads on string-dominated workloads; asmjson zmm tape leads on
mixed JSON by a wide margin (920 MiB/s vs 483 MiB/s for sonic-rs — 90 % ahead).
The zmm tape parser writes a flat `TapeEntry` array directly in assembly — one
entry per value — so subsequent traversal is a single linear scan with no
pointer chasing.  The portable `u64` SWAR classifier beats sonic-rs on string
objects (4.43 vs 4.05 GiB/s) despite using no SIMD instructions.

Each benchmark measures **parse + full traversal**: after parsing, every string
value and object key is visited and its length accumulated.  This is necessary
for a fair comparison because sonic-rs defers decoding string content until the
value is accessed (lazy evaluation); a parse-only measurement would undercount
its work relative to any real use-case where the parsed data is actually read.

Note: simd-json requires a mutable copy of the input buffer to parse in-place,
so each iteration includes a `Vec::clone` of the 10 MiB dataset; it does not
start on a level footing with the other parsers on these workloads.

## Optimisation tips

`TapeRef` is a plain `Copy` cursor — two `usize`s — so it is cheap to store
and reuse.  Holding on to a `TapeRef` you have already located lets you skip
re-scanning work on subsequent accesses.

### Cache field refs from a one-pass object scan

`get(key)` walks the object from the start every time it is called.  If you
need several fields from the same object, iterate once with `object_iter` and
keep the values you care about:

```rust
use asmjson::{parse_to_tape, choose_classifier, JsonRef, TapeRef};

let classify = choose_classifier();
let src = r#"{"items":[1,2,3],"meta":{"count":3}}"#;
let tape = parse_to_tape(src, classify).unwrap();
let root = tape.root().unwrap();

// Single pass — O(n_keys) regardless of how many fields we need.
let mut items_ref: Option<TapeRef> = None;
let mut meta_ref:  Option<TapeRef> = None;
for (key, val) in root.object_iter().unwrap() {
    match key {
        "items" => items_ref = Some(val),
        "meta"  => meta_ref  = Some(val),
        _ => {}
    }
}

// Subsequent accesses go straight to the cached position — no re-scan.
let count = meta_ref.unwrap().get("count").unwrap().as_i64();
assert_eq!(count, Some(3));
```

### Collect array elements for indexed or multi-pass access

`array_iter` yields each element once in document order.  Collecting the
results into a `Vec<TapeRef>` gives you random access and any number of
further passes at zero additional parsing cost:

```rust
use asmjson::{parse_to_tape, choose_classifier, JsonRef, TapeRef};

let classify = choose_classifier();
let src = r#"[{"name":"Alice","score":91},{"name":"Bob","score":78},{"name":"Carol","score":85}]"#;
let tape = parse_to_tape(src, classify).unwrap();
let root = tape.root().unwrap();

// Collect once — O(n) scan.
let rows: Vec<TapeRef> = root.array_iter().unwrap().collect();

// Random access is now O(1) — no re-scanning.
assert_eq!(rows[1].get("name").unwrap().as_str(), Some("Bob"));

// Multiple passes over the same rows are free.
let total: i64 = rows.iter()
    .filter_map(|r| r.get("score").and_then(|s| s.as_i64()))
    .sum();
assert_eq!(total, 91 + 78 + 85);
```

## Output formats

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

## Conformance note

asmjson is slightly permissive: its classifier treats **any byte with value
`< 0x20`** (i.e. all C0 control characters) as whitespace, rather than
strictly the four characters the JSON specification allows (`0x09` HT, `0x0A`
LF, `0x0D` CR, `0x20` SP).  Well-formed JSON is parsed identically; input
that embeds bare control characters other than the four legal ones will be
accepted where a strict parser would reject it.

## License

MIT — see [LICENSE](https://github.com/andy-thomason/asmjson/blob/master/LICENSE).

For internals documentation (state machine annotation, register allocation,
design decisions) see [doc/dev.md](doc/dev.md).
