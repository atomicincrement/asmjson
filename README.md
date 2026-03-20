# asmjson

[![CI](https://github.com/atomicincrement/asmjson/actions/workflows/ci.yml/badge.svg)](https://github.com/atomicincrement/asmjson/actions/workflows/ci.yml)
[![crates.io](https://img.shields.io/crates/v/asmjson.svg)](https://crates.io/crates/asmjson)
[![docs.rs](https://docs.rs/asmjson/badge.svg)](https://docs.rs/asmjson)

A JSON parser that reaches **10.93 GiB/s** single-threaded and **26.6 GiB/s**
across Rayon tasks on an AVX-512BW CPU — roughly **1.6× faster than sonic-rs**
on string-heavy workloads.  It classifies 64 bytes at a time using hand-written
AVX-512BW assembly or portable SWAR (SIMD-Within-A-Register) bit tricks,
enabling entire whitespace runs and string bodies to be skipped in a single
operation.

> **⚠️ Experimental — not production ready.**  
> This crate is a research and benchmarking project.  The API is unstable, test
> coverage is incomplete, and the hand-written assembly has not been audited for
> safety or correctness under adversarial input.  Use `serde_json` or `sonic-rs`
> for production workloads.

## Quick start

Add to your `Cargo.toml`:

```toml
# without serde
asmjson = "0.2"

# with serde deserialization support
asmjson = { version = "0.2", features = ["serde"] }
```

```rust
use asmjson::{dom_parser, JsonRef};

let parse = dom_parser(); // selects AVX-512BW or SWAR at runtime
let tape = parse(r#"{"name":"Alice","age":30}"#, None).unwrap();

assert_eq!(tape.root().get("name").as_str(), Some("Alice"));
assert_eq!(tape.root().get("age").as_i64(), Some(30));
```

For maximum throughput on CPUs with AVX-512BW (Ice Lake+, Zen 4+), use the
unsafe assembly entry points `parse_to_dom_zmm` / `parse_with_zmm`.  It is
the caller's responsibility to ensure AVX-512BW support before calling them.

## Benchmarks

Measured on a single core with `cargo bench` against 10 MiB of synthetic JSON.
Comparison point is `sonic-rs` (lazy Value, AVX2).

| Parser               | string array | string object | mixed      |
|----------------------|:------------:|:-------------:|:----------:|
| asmjson/sax          | 10.78 GiB/s  | 8.29 GiB/s    | 1.17 GiB/s |
| asmjson/dom          | 10.93 GiB/s  | 6.94 GiB/s    | 897 MiB/s  |
| asmjson/u64          | 7.02 GiB/s   | 4.91 GiB/s    | 607 MiB/s  |
| sonic-rs             | 6.92 GiB/s   | 4.06 GiB/s    | 478 MiB/s  |
| serde_json           | 2.41 GiB/s   | 534 MiB/s     | 78 MiB/s   |
| simd-json †          | 1.91 GiB/s   | 1.19 GiB/s    | 174 MiB/s  |

† simd-json numbers include buffer cloning overhead (see note above).

### Parallel JSON Lines throughput (`mmap_parallel` example)

Measured on a single machine parsing a 1 GiB JSON Lines file
(`/tmp/file.jsonl`, ~10.7 million lines, ~100 bytes each) memory-mapped with
`memmap2` and split into 1 024 × ~1 MiB chunks processed in parallel with
Rayon.  CPUID auto-selected the AVX-512BW assembly path.

| Configuration                     | Throughput   |
|-----------------------------------|:------------:|
| Rayon + AVX-512BW (`parse_with_zmm`) | **26.6 GiB/s** |
| ZMM temporal loads (single thread, 2 GiB buffer) | 47.5 GiB/s |
| ZMM non-temporal loads (`vmovntdqa`, single thread) | 49.5 GiB/s |

The parser reaches ~56 % of the single-threaded DRAM read ceiling while
doing real work (structure parsing, string counting) across 1 024 parallel
Rayon tasks.

Run it yourself:

```sh
cargo run --release --example mmap_parallel
```

### Serde deserialization (`serde_example`)

End-to-end throughput for parsing a ~1 MiB mixed JSON array and deserialising
it into a `Vec<Record>` via `serde` + `from_taperef` (AVX-512BW path,
Ryzen 9 9955HX):

| Phase           | Time     | Throughput  |
|-----------------|:--------:|:-----------:|
| `parse_to_dom_zmm` | 1.1 ms | **769 MiB/s** |
| `from_taperef`  | 1.9 ms   | **428 MiB/s** |

```sh
cargo run --release --features serde --example serde_example
```

Note: `asmjson/sax` and `asmjson/dom` are implemented entirely in hand-written
x86-64 assembly using AVX-512BW instructions.  They require a CPU with
AVX-512BW support (Ice Lake or later on Intel, Zen 4 or later on AMD) and are
not available on other architectures.

`asmjson/sax` (`parse_with_zmm` + a `JsonWriter` sink) is the fastest overall:
it leads on string objects (8.29 GiB/s, +20 % over `asmjson/dom`) and mixed
JSON (1.17 GiB/s, +30 % over `asmjson/dom`, +145 % over sonic-rs) because it
requires no tape allocation.  `asmjson/dom` (`parse_to_dom_zmm`) writes a flat
`TapeEntry` array directly in assembly — one entry per value — so subsequent
traversal is a single linear scan with no pointer chasing; it leads marginally
on string arrays (10.93 GiB/s) where the tape scan overhead is negligible.
The portable `asmjson/u64` SWAR classifier beats sonic-rs on every workload
(e.g. 4.91 vs 4.06 GiB/s on string objects) despite using no SIMD instructions.

Each benchmark measures **parse + full traversal**: after parsing, every string
value and object key is visited and its length accumulated.  This is necessary
for a fair comparison because sonic-rs defers decoding string content until the
value is accessed (lazy evaluation); a parse-only measurement would undercount
its work relative to any real use-case where the parsed data is actually read.

Note: simd-json requires a mutable copy of the input buffer to parse in-place,
so each iteration includes a `Vec::clone` of the 10 MiB dataset; it does not
start on a level footing with the other parsers on these workloads.

## Optimisation tips

`DomRef` is a plain `Copy` cursor — two `usize`s — so it is cheap to store
and reuse.  Holding on to a `DomRef` you have already located lets you skip
re-scanning work on subsequent accesses.

### Cache field refs from a one-pass object scan

`get(key)` walks the object from the start every time it is called.  If you
need several fields from the same object, iterate once with `object_iter` and
keep the values you care about:

```rust
use asmjson::{parse_to_dom, JsonRef, DomRef};

let src = r#"{"items":[1,2,3],"meta":{"count":3}}"#;
let tape = parse_to_dom(src, None).unwrap();
let root = tape.root().unwrap();

// Single pass — O(n_keys) regardless of how many fields we need.
let mut items_ref: Option<DomRef> = None;
let mut meta_ref:  Option<DomRef> = None;
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
results into a `Vec<DomRef>` gives you random access and any number of
further passes at zero additional parsing cost:

```rust
use asmjson::{parse_to_dom, JsonRef, DomRef};

let src = r#"[{"name":"Alice","score":91},{"name":"Bob","score":78},{"name":"Carol","score":85}]"#;
let tape = parse_to_dom(src, None).unwrap();
let root = tape.root().unwrap();

// Collect once — O(n) scan.
let rows: Vec<DomRef> = root.array_iter().unwrap().collect();

// Random access is now O(1) — no re-scanning.
assert_eq!(rows[1].get("name").unwrap().as_str(), Some("Bob"));

// Multiple passes over the same rows are free.
let total: i64 = rows.iter()
    .filter_map(|r| r.get("score").and_then(|s| s.as_i64()))
    .sum();
assert_eq!(total, 91 + 78 + 85);
```

## Output formats

- `parse_to_dom` — allocates a flat `Tape` of tokens with O(1) structural skips.
- `parse_with` — drives a custom `JsonWriter` sink; zero extra allocation.

## API

| Function                   | Safety   | Description |
|----------------------------|----------|-------------|
| `dom_parser()`             | safe     | Returns a CPUID-selected `fn(&str, Option<usize>) -> Option<Dom<'_>>`. |
| `sax_parser()`             | safe     | Returns a `SaxParser`; call `.parse(src, writer)` to drive any `Sax` writer. |
| `parse_to_dom(src, cap)`   | safe     | Parse to a flat `Dom`; portable SWAR classifier. |
| `parse_with(src, writer)`  | safe     | Drive a custom `Sax` writer; portable SWAR classifier. |
| `unsafe parse_to_dom_zmm(src, cap)` | **unsafe** | Parse to `Dom`; AVX-512BW assembly (direct tape write). |
| `unsafe parse_with_zmm(src, writer)` | **unsafe** | Drive a `Sax` writer; AVX-512BW assembly (vtable dispatch). |

Prefer `dom_parser()` / `sax_parser()` for new code — they perform a one-time
CPUID check and return either the AVX-512BW or the SWAR path; no `unsafe` is
required at the call site.

The `unsafe` low-level variants require a CPU with AVX-512BW support.  Calling
them on an unsupported CPU will trigger an illegal instruction fault.

## Conformance note

asmjson is slightly permissive: its classifier treats **any byte with value
`< 0x20`** (i.e. all C0 control characters) as whitespace, rather than
strictly the four characters the JSON specification allows (`0x09` HT, `0x0A`
LF, `0x0D` CR, `0x20` SP).  Well-formed JSON is parsed identically; input
that embeds bare control characters other than the four legal ones will be
accepted where a strict parser would reject it.

Additionally, asmjson does **not** scan string contents for unescaped control
characters (U+0000–U+001F), which the JSON specification forbids inside string
values.  If your use-case requires this check it can be performed as an extra
pass over the raw string bytes after parsing — for example, rejecting any
string whose raw byte span contains a byte `< 0x20`.

## Contributing

Bug reports, correctness fixes, and performance improvements are welcome.
See [CONTRIBUTING.md](CONTRIBUTING.md) for how to report issues, submit
patches, and seek support.

## License

MIT — see [LICENSE](https://github.com/atomicincrement/asmjson/blob/master/LICENSE).

For internals documentation (state machine annotation, register allocation,
design decisions) see [doc/dev.md](doc/dev.md).
