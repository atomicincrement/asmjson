# asmjson — development conversation log

This file captures the development history of the `asmjson` project as it
unfolded over two coding sessions.  The earlier session is reconstructed from
a conversation summary; the later session (JsonWriter) is recorded in full.

---

## Session 1 — SIMD classifiers, benchmarks, perf profiling

### Resuming from prior state

The project was `asmjson`, a Rust JSON parser at `/home/amy/andy-thomason/asmjson`
using AVX-512BW SIMD classification.  Prior work had added XMM/YMM/ZMM
classifier variants, CPUID dispatch, benchmarks, and a standalone `.s`
assembly file.

### Fixing orphaned code

Removed orphaned `next_state_xmm` body, added `static ZMM_CONSTANTS`, fixed
Rust 2024 `unsafe {}` blocks inside `unsafe fn imp` bodies.  Tests passed
14/14.

### ISA question: broadcast memory operands

Confirmed that AVX-512 broadcast memory operands (`{1to16}`) do not exist for
byte-granularity instructions (`vpcmpeqb` / `vpcmpub`), so 64-byte needle
vectors in `ZMM_CONSTANTS` are the correct approach.

### Benchmark results (criterion)

Compared XMM / YMM / ZMM variants plus simd-json on three workloads:

| workload      | XMM       | YMM       | ZMM       |
|---------------|-----------|-----------|-----------|
| string_array  | 4.16 GiB/s| 5.53 GiB/s| 6.08 GiB/s|
| string_object | —         | —         | —         |
| mixed         | —         | —         | —         |

State machine dominates on mixed workload.

### Standalone GNU assembly file

Created `src/classify_zmm.s`, compiled via `build.rs` + `cc` crate.  Fixed
SysV sret convention for 32-byte `ByteState` return.  Added `#[repr(C)]` to
`ByteState`.  Added to benchmarks and `classifier_agreement` test.  14/14
passing.

### Perf profiling results

Used `perf record` / `perf report` to compare `classify_zmm_s` (`.s` file)
vs inline-asm `classify_zmm`:

- `.s` version: ~16% of runtime (8.16% body + 7.87% Rust wrapper) — sret
  call overhead for 32-byte return value.
- Inline-asm version: ~10% (folded into `parse_json_impl` by the compiler).
- State machine + allocator dominate at 68–74%.

Conclusion: allocator / tree-building is the real bottleneck, motivating a
flat `Tape` output.

### Revert standalone `.s`

Removed `classify_zmm_gnu`, `build.rs`, `cc` build-dep, bench slots.  14/14
tests still passing.

### Final state of session 1

- `cargo fmt` applied.
- Committed as **`dbf274e`**.
- `push_value` / `close_frame` still present; `parse_json_impl` still builds
  `Vec<Frame>` directly.

---

## Session 2 — JsonWriter trait and Tape output

### Motivation

The perf profile showed that allocation for the `Value` tree is the dominant
cost.  A flat `Tape` representation would let callers avoid or defer
allocation.  Rather than duplicating the parser, we abstract output
construction behind a trait.

### Design

**`JsonWriter<'src>` trait** — SAX-style event sink:

```rust
pub trait JsonWriter<'src> {
    type Output;
    fn null(&mut self);
    fn bool_val(&mut self, v: bool);
    fn number(&mut self, s: &'src str);
    fn string(&mut self, s: Cow<'src, str>);
    fn key(&mut self, s: Cow<'src, str>);
    fn start_object(&mut self);
    fn end_object(&mut self);
    fn start_array(&mut self);
    fn end_array(&mut self);
    fn finish(self) -> Option<Self::Output>;
}
```

**`FrameKind` enum** — lightweight parser-internal discriminant replacing
`Vec<Frame>` in the state machine loop:

```rust
enum FrameKind { Object, Array }
```

**`ValueWriter<'a>`** — private struct implementing `JsonWriter<'a>` with
`Output = Value<'a>`.  Delegates to the existing `push_value` helper.

**`TapeEntry<'a>`** — flat token type:

```rust
pub enum TapeEntry<'a> {
    Null,
    Bool(bool),
    Number(&'a str),
    String(Cow<'a, str>),
    Key(Cow<'a, str>),
    StartObject(usize),   // payload = index of matching EndObject
    EndObject,
    StartArray(usize),    // payload = index of matching EndArray
    EndArray,
}
```

`StartObject(n)` / `StartArray(n)` carry the index of their matching closer,
enabling O(1) structural skips:

```rust
if let TapeEntry::StartObject(end) = tape.entries[i] {
    i = end + 1; // jump past the entire object
}
```

**`Tape<'a>`** — output struct:

```rust
pub struct Tape<'a> { pub entries: Vec<TapeEntry<'a>> }
```

**`TapeWriter<'a>`** — private struct implementing `JsonWriter<'a>` with
`Output = Tape<'a>`.  Maintains an `open: Vec<usize>` of unmatched
`StartObject` / `StartArray` indices that are backfilled when the closer
is emitted.

**`write_atom`** — helper replacing the old `parse_atom` + `push_value`
callsites:

```rust
fn write_atom<'a, W: JsonWriter<'a>>(s: &'a str, w: &mut W) -> bool { … }
```

### New public API

| Symbol | Description |
|--------|-------------|
| `pub trait JsonWriter<'src>` | SAX-style writer trait |
| `pub enum TapeEntry<'a>` | flat token |
| `pub struct Tape<'a>` | flat token sequence |
| `pub fn parse_to_tape(src, classify) -> Option<Tape>` | flat output path |
| `pub fn parse_with(src, classify, writer) -> Option<W::Output>` | generic entry point |
| `pub fn parse_json(src, classify) -> Option<Value>` | **unchanged** |

### Internal changes

- `parse_json_impl` is now generic:
  `fn parse_json_impl<'a, F, W>(src, classify: F, writer: W) -> Option<W::Output>`
- The parser loop uses `Vec<FrameKind>` instead of `Vec<Frame>`.
- `parse_atom` and `close_frame` removed (dead code).

### Implementation

Inserted all new types between `close_frame` and the old `pub fn parse_json`,
then replaced `parse_json_impl` with the generic version.

### Tests

9 new tape tests added:

- `tape_scalar_values` — null, bool, number, string
- `tape_empty_object` — `StartObject(1)` points to `EndObject` at index 1
- `tape_empty_array`
- `tape_simple_object` — `{"a":1}`
- `tape_simple_array` — `[1,2,3]`
- `tape_nested` — `{"a":[1,2]}`, verifies both skip indices
- `tape_multi_key_object` — `{"x":1,"y":2}`
- `tape_invalid_returns_none` — trailing commas, bad structure
- `tape_skip_object` — exercises the O(1) skip idiom

All classifiers (XMM / YMM / ZMM) are compared for each tape test.

**23/23 tests pass, zero warnings.**

### Commit

```
00c27c4  Add JsonWriter trait + Tape output
```

---

## Session 3 — Tape throughput benchmark

### What was done

Added an `asmjson/zmm/tape` bench slot to the `string_array` criterion group
(`benches/parse.rs`) so that `parse_to_tape` (flat Tape output) can be
directly compared against `parse_json` (Value tree output), both using the ZMM
classifier.

### Results

Workload: ~10 MiB array of 95-character ASCII strings.

| variant | throughput |
|---|---|
| `asmjson/zmm` — `parse_json` → `Value` tree | 6.25 GiB/s |
| `asmjson/zmm/tape` — `parse_to_tape` → `Tape` | 8.56 GiB/s |

The flat Tape is **~37% faster** on this workload.  The gain comes almost
entirely from eliminating the per-element heap allocation required to build the
`Vec<Value>` inside `Value::Array` and the `Box<[...]>` at close time.  The
SIMD classifier and state machine costs are identical between the two paths.

### Design decisions

The Tape bench was added only to `string_array` (the allocation-heavy
workload) rather than to all three groups, keeping the benchmark run time
reasonable.  The same pattern can be replicated for `string_object` and
`mixed` when needed.

### Commit

```
3b1f4b2  bench: add asmjson/zmm/tape to string_array group
```
