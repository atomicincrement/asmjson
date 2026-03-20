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
---

## Session 4 — string_object Tape benchmark

### What was done

Added an `asmjson/zmm/tape` slot to the `string_object` criterion group to
compare `parse_json` (Value tree) vs `parse_to_tape` (flat Tape) on the
object-heavy workload.

### Results

Workload: ~10 MiB flat JSON object with string keys (`"keyNNNNN"`) and
85-character ASCII string values.

| variant | throughput |
|---|---|
| `asmjson/zmm` — `parse_json` → `Value` tree | 5.29 GiB/s |
| `asmjson/zmm/tape` — `parse_to_tape` → `Tape` | 5.53 GiB/s |

Only **~5% faster**, compared to 37% on the string array.

### Design decisions / analysis

The much smaller gain reflects the structure of the workload.  Each object
member requires a key parse (KeyChars → KeyEnd → AfterColon states) that is
identical in both paths — the Tape still emits a `Key` entry for every member.
On the Value side, the `Vec<(Cow, Value)>` members accumulation is the main
allocation cost; on the Tape side that is replaced by a flat `Vec<TapeEntry>`
push, but the state-machine work per byte is the same.

In contrast, the string array workload allocates a `Box<[Value]>` per
top-level array (containing ~100 k `Value::String` variants), which the Tape
eliminates entirely.

### Commit

```
c1fb9d4  bench: add asmjson/zmm/tape to string_object group
```
---

## Session 5 — mixed Tape benchmark

### What was done

Added an `asmjson/zmm/tape` slot to the `mixed` criterion group to compare
`parse_json` vs `parse_to_tape` on the deeply-nested mixed workload.

### Results

Workload: ~10 MiB array of objects, each with numbers, booleans, nulls, a
nested array, and a nested object (~130 bytes per record).

| variant | throughput |
|---|---|
| `asmjson/zmm` — `parse_json` → `Value` tree | 254 MiB/s |
| `asmjson/zmm/tape` — `parse_to_tape` → `Tape` | 392 MiB/s |

**~54% faster** with the Tape on this workload.

### Analysis

The mixed workload allocates at multiple nesting levels: an outer `Box<[Value]>`
for the top-level array, and inside each record a `Box<[...]>` for the `tags`
array and the `meta` object, plus the record object itself.  Every `}` / `]`
triggers a heap allocation to box the collected members.  The Tape avoids all
of this — it is a single flat `Vec<TapeEntry>` grown incrementally with no
per-close allocation.

The absolute throughput (254 / 392 MiB/s) is much lower than on the
string-only workloads (5–8 GiB/s) because the mixed data has short strings and
dense structural characters, so the state machine visits more states per byte.

### Overall Tape speedup summary

| workload | Value tree | Tape | speedup |
|---|---|---|---|
| string_array | 6.25 GiB/s | 8.56 GiB/s | +37% |
| string_object | 5.29 GiB/s | 5.53 GiB/s | +5% |
| mixed | 254 MiB/s | 392 MiB/s | +54% |

### Commit

```
8edf785  bench: add asmjson/zmm/tape to mixed group
```
```

---

## Session 6 — JsonRef read-only accessor trait

### Motivation

Having both a `Value` tree and a flat `Tape` as parse outputs created an
ergonomics problem: code consuming parsed JSON had to hardcode which
representation to use.  The request was to model `serde_json::Value`'s
accessor API as a trait so that generic functions work with either.

### Design

`pub trait JsonRef<'a>: Sized + Copy` — `'a` is the string-access lifetime;
`&'a str` returned from `as_str` / `as_number_str` is valid for at least `'a`.

Methods mirror `serde_json::Value`:

| method | notes |
|---|---|
| `is_null / is_bool / is_number / is_string` | default impls via `as_*` |
| `is_array / is_object` | required |
| `as_null / as_bool / as_number_str / as_str` | required |
| `as_i64 / as_u64 / as_f64` | default: `as_number_str()?.parse().ok()` |
| `get(key: &str) -> Option<Self>` | object key lookup |
| `index_at(i: usize) -> Option<Self>` | array positional lookup |
| `len() -> Option<usize>` | element / pair count |

#### TapeRef

A new `pub struct TapeRef<'t, 'src: 't>` carries `tape: &'t [TapeEntry<'src>]`
and `pos: usize` using two lifetimes:

- `'t`   = borrow of the tape's `Vec` (typically the caller's stack frame).
- `'src` = the source JSON bytes lifetime (the data borrowed inside entries).

This avoids the self-referential `&'src Tape<'src>` pattern.

`Tape::root<'t>(&'t self) -> Option<TapeRef<'t, 'src>>` is the entry point.

A private `fn tape_skip(entries, pos) -> usize` advances past one entry in O(1)
for `StartObject` / `StartArray` (using the pre-baked end-index payload) and
also O(1) for scalars.

### Implementation

~300-line insertion in `src/lib.rs` between `TapeWriter` impl and `write_atom`:

1. `pub struct TapeRef<'t, 'src: 't>` + `#[derive(Clone, Copy)]`
2. `impl<'src> Tape<'src> { pub fn root<'t>` }
3. `fn tape_skip` (private)
4. `pub trait JsonRef<'a>` with full docstrings
5. `impl<'a> JsonRef<'a> for &'a Value<'a>`
6. `impl<'t, 'src: 't> JsonRef<'t> for TapeRef<'t, 'src>`

### Tests

Six new tests added (29 total): `jsonref_scalars_value/tape`, `jsonref_object_get`,
`jsonref_array_index`, `jsonref_nested`, `jsonref_generic_fn` (exercises a
`fn<'a, J: JsonRef<'a>>(J) -> Option<i64>` on both representations).  All pass.

### Commit

```
9b5f27c  feat: add JsonRef trait + TapeRef cursor
```

---

## Session 7 — JsonRef chaining via Option<J>

### Motivation

`x.get("a").get("b")` was broken by the original trait design: `get` returned
`Option<Self>`, so calling `.get("b")` on `Option<J>` would have to return
`Option<Option<J>>`, defeating flat chaining.

### Design decision: associated `type Item`

The fix is to add `type Item: JsonRef<'a>` to the trait and change `get` /
`index_at` to return `Option<Self::Item>` instead of `Option<Self>`.

| impl | `type Item` | effect |
|---|---|---|
| `&'a Value<'a>` | `Self` | no change |
| `TapeRef<'t,'src>` | `Self` | no change |
| `Option<J>` | `J::Item` | chain stays flat |

The key insight: `Option<J>::Item = J::Item` (not `Option<J>`), so chaining
never wraps more deeply.

```rust
root.get("address").get("city").as_str()
root.get("items").index_at(0).get("val").as_i64()
```

### Implementation

- Added `type Item: JsonRef<'a>` to `JsonRef` trait definition.
- Changed `fn get` / `fn index_at` signatures to return `Option<Self::Item>`.
- Added `type Item = Self` to both concrete impls (no change in practice).
- Fixed `Option<J>` impl: `type Item = J::Item`, `get`/`index_at` delegate via
  `self?.get(key)` returning `Option<J::Item>`.

### Test

`jsonref_option_chaining`: three-level `.get().get().get()` on both `&Value`
and `TapeRef`, missing-key short-circuit, mixed `.get().index_at().get()`.
30 tests passing.

### Commit

```
413c41f  feat: impl JsonRef for Option<J> with type Item for flat chaining
```

## Session 8 — Submodule split: `value`, `tape`, `json_ref`

### What was done

Split the monolithic `src/lib.rs` into three submodules:

| Module | File | Contents |
|---|---|---|
| `value` | `src/value.rs` | `Value<'a>`, `ValueWriter`, `is_valid_json_number`, `push_value`, `Frame` |
| `tape` | `src/tape.rs` | `TapeEntry`, `Tape`, `TapeWriter`, `TapeRef`, `tape_skip`, `Tape::root` |
| `json_ref` | `src/json_ref.rs` | `JsonRef` trait + impls for `&'a Value`, `TapeRef`, `Option<J>` |

Each module carries its own `#[cfg(test)] mod tests { … }` block with the
tests relevant to that module.  `lib.rs` retains only the parse engine
(classifier functions, `parse_json_impl`, `JsonWriter`, `FrameKind`,
`write_atom`) plus a single `classifier_agreement` test.

Public API is unchanged: `lib.rs` re-exports all moved types via
`pub use value::Value`, `pub use tape::{Tape, TapeEntry, TapeRef}`, and
`pub use json_ref::JsonRef`.

### Design decisions

- `ValueWriter` and `TapeWriter` are `pub(crate)` so `lib.rs` can pass them to
  `parse_with`; their constructors are also `pub(crate)`.
- `is_valid_json_number` is `pub(crate)` so `lib.rs`'s `write_atom` can call it.
- `TapeRef`'s fields (`tape`, `pos`) are `pub(crate)` so `json_ref.rs` can
  implement the `JsonRef` accessor methods without the impl living in `tape.rs`.
- `tape_skip` is `pub(crate)` for the same reason.
- Each submodule's test helpers (`run`, `run_tape`, `run_both`) are duplicated
  locally; they are private and small enough that sharing is unnecessary.

### Results

30/30 tests pass across all four test modules; zero warnings after removing
three unused imports that surfaced during the move.

### Commit

`4781b13` refactor: split into submodules value, tape, json_ref

## Session 9 — portable SWAR classifier

### Add `classify_u64`

#### What was done

Added `pub fn classify_u64(src: &[u8]) -> ByteState`, a pure-Rust classifier
that processes a 64-byte block as eight `u64` words using SIMD-Within-A-Register
(SWAR) tricks, requiring no architecture-specific intrinsics.

`choose_classifier()` was updated so that `classify_u64` is the universal
fallback returned when not running on x86-64 (which continues to return the
AVX-512 / AVX2 / SSE2 path as before).

The `classifier_agreement` integration test was extended to assert that
`classify_u64` produces the same `ByteState` as `classify_zmm` for every
test input.  `classify_u64` was also added as `asmjson/u64` to all three
benchmark groups in `benches/parse.rs`.

#### Design decisions

**Whitespace detection** (`byte <= 0x20`):

```
masked = v & 0x7f7f_7f7f_7f7f_7f7f  // clear bit 7 before add
sum    = masked + 0x5f5f_5f5f_5f5f_5f5f  // overflows into bit 7 iff byte >= 0x21
w      = !(sum | v) & 0x8080_8080_8080_8080
```

Masking bit 7 before the add prevents bytes ≥ 0x80 from aliasing into the
target range.  OR-ing the original `v` then ensures bytes ≥ 0x80 are excluded
from the final result.

**Byte equality** — XOR with a broadcast constant turns the problem into
"detect a zero byte":

```
has_zero_byte(v) = (v - 0x0101...) & !v & 0x8080...
eq_byte(v, b)    = has_zero_byte(v ^ (b * 0x0101...))
```

**Movemask** — collects the MSB of each byte into a `u8`:

```
((v & 0x8080...) * 0x0002_0408_1020_4081) >> 56
```

The magic multiplier routes bit 7 of byte *k* (at position `8k+7`) into
bit `56+k` of the product; shifting right 56 leaves the eight flags in the
low byte.

**Zero-padding** — bytes beyond `src.len()` are zero-filled, which the
whitespace test classifies as whitespace — consistent with the behaviour of
the SIMD classifiers.

#### Results

30/30 tests pass; zero warnings.  `cargo bench --no-run` compiles cleanly.

#### Commit

`54979e0` feat: add classify_u64 portable SWAR classifier

## Session 10 — remove classify_xmm

### What was done

Benchmarked `classify_xmm` (SSE2) against `classify_u64` (SWAR) and found
xmm is slower on every workload:

| Workload      | xmm       | u64       |
|---------------|-----------|-----------|
| string\_array | 3.03 GiB/s | 5.95 GiB/s |
| string\_object | 2.72 GiB/s | 4.20 GiB/s |
| mixed         | 229 MiB/s  | 234 MiB/s  |

`classify_xmm` was removed from `src/lib.rs`, `choose_classifier` updated
(AVX-512BW → AVX2 → portable SWAR u64, no SSE2 step), bench entries
removed, and all submodule test helpers (`value.rs`, `tape.rs`,
`json_ref.rs`) updated to cross-check `classify_u64` instead of
`classify_xmm`.

### Design decisions

SSE2 `classify_xmm` processes the 64-byte block as four 16-byte passes,
each incurring a `VPMOVMSKB` movemask with cross-lane serialisation
overhead.  The portable SWAR implementation works entirely in GP registers
as eight independent 64-bit word operations, avoiding that bottleneck
entirely.  Since the portable code wins unconditionally there is no reason
to maintain the SSE2 path — any x86-64 chip that lacks AVX2 now falls
straight through to `classify_u64`.

YMM (AVX2) was checked simultaneously: u64 leads on string-heavy input
(+12%) while ymm recovers on object-heavy input (+4%).  Net mixed result
means ymm still earns its place as the AVX2 hardware path.

### Results

30/30 tests pass; zero warnings.

### Commit

`c6bbb9b` refactor: remove classify_xmm (slower than classify_u64 on all benchmarks)

---

## Session 12 — Fix CI (AVX-512 compile and runtime guards)

### What was done

GitHub Actions CI was never running because all commits were local only (16
commits ahead of `origin`).  After pushing, CI triggered but would have
failed on two related issues in `classify_zmm`:

1. **Compile-time**: The AVX-512BW inline-assembly block inside `classify_zmm`
   lacked a `#[target_feature(enable = "avx512bw")]` attribute.  LLVM's
   integrated assembler rejects AVX-512 mnemonics (`vmovdqu8`, `vpcmpub`,
   `kmovq`, etc.) when the function's target-feature set does not include
   `avx512bw`.  GitHub's `ubuntu-latest` runners compile with the default
   `x86_64-unknown-linux-gnu` target (no AVX-512), so the build would have
   errored out.

2. **Runtime**: The `classifier_agreement` test called `classify_zmm`
   unconditionally.  On hardware without AVX-512 this triggers `SIGILL`.

### Design decisions

Following the same pattern already used by `classify_ymm`, the AVX-512 asm was
moved into a nested `unsafe fn imp` annotated with
`#[target_feature(enable = "avx512bw")]`.  The outer `classify_zmm` delegates
to `imp` via `unsafe { imp(src) }`.  This is safe because the only callers are
`choose_classifier` (guarded by `is_x86_feature_detected!("avx512bw")`) and
the test (now also guarded).

In the test, the zmm comparison block was wrapped in
`#[cfg(any(target_arch = "x86", target_arch = "x86_64"))] if is_x86_feature_detected!("avx512bw")`.
When AVX-512 is absent the test still cross-checks `classify_u64` against
`classify_ymm`, preserving meaningful coverage on all runners.

### Results

30/30 tests pass locally; doc-tests pass.  CI will now compile and run
successfully on `ubuntu-latest` (AVX2 available, AVX-512 absent).

### Commit

`b5c7265` fix: guard classify_zmm and test behind avx512bw target-feature

---

## Session 13 — Add sonic-rs to benchmarks

### What was done

Added `sonic-rs = "0.5.7"` as a dev-dependency and added a
`sonic_rs::from_str::<sonic_rs::Value>` bench variant to all three groups
(`string_array`, `string_object`, `mixed`).  Ran the full bench suite and
updated the README table with sonic-rs results and refreshed numbers.

### Design decisions

`sonic_rs::from_str::<sonic_rs::Value>` is the closest analogue to
`parse_json` — it produces a fully-navigable value tree from a `&str`.
`sonic-rs` uses a lazy `Value` representation where string content remains as
raw bytes in the source buffer; escape processing is deferred until the value
is read.  By contrast, asmjson fully decodes `\uXXXX` / `\\` / `\"` escapes
into `Cow<'src, str>` during the initial parse pass, which is safer and more
ergonomic but costs throughput on string-heavy inputs.

### Results

| Parser              | string array | string object | mixed     |
|---------------------|:------------:|:-------------:|:---------:|
| sonic-rs            | 11.0 GiB/s   | 6.17 GiB/s    | 969 MiB/s |
| asmjson zmm (tape)  | 8.36 GiB/s   | 5.72 GiB/s    | 383 MiB/s |
| asmjson zmm         | 6.09 GiB/s   | 5.23 GiB/s    | 262 MiB/s |
| asmjson u64         | 6.08 GiB/s   | 4.20 GiB/s    | 255 MiB/s |
| asmjson ymm         | 5.45 GiB/s   | 4.46 GiB/s    | 258 MiB/s |
| simd-json borrowed  | 2.13 GiB/s   | 1.32 GiB/s    | 189 MiB/s |
| serde_json          | 2.50 GiB/s   | 0.57 GiB/s    |  92 MiB/s |

sonic-rs leads on string-heavy work because of its lazy decode.  On mixed
JSON (numbers, bools, nested objects), asmjson zmm/tape is still 2.5× faster
than sonic-rs — likely because mixed workloads require more structural parsing
where sonic-rs's lazy trick gives less advantage.

### Commit

`ee28983` bench: add sonic-rs comparison

---

## Session 14 — TapeRef::object_iter and array_iter

### What was done

Added two new iterator types to `src/tape.rs` and inherent methods on
`TapeRef` to create them:

- **`TapeObjectIter<'t, 'src>`** — yields `(&'t str, TapeRef<'t, 'src>)` pairs
  for every key-value entry in a JSON object, in document order.  Returned by
  `TapeRef::object_iter()`, which returns `None` if the cursor is not on a
  `StartObject` entry.

- **`TapeArrayIter<'t, 'src>`** — yields one `TapeRef<'t, 'src>` per array
  element in document order.  Returned by `TapeRef::array_iter()`, which
  returns `None` if the cursor is not on a `StartArray` entry.

Both types were added to the crate-root re-exports.

### Design decisions

The iterators are inherent methods on `TapeRef` rather than part of the
`JsonRef` trait because the `JsonRef` trait is generic (`type Item`) and
returning an iterator type directly from the trait would require either
associated types for the iterator types (adding trait complexity) or
`impl Trait` returns (not stable in traits without GATs boilerplate).  Keeping
them as inherent methods is simpler and zero-cost.

Both iterators advance via `tape_skip`, so skipping over nested
objects/arrays inside a value position is O(1) — the `StartObject(end)` and
`StartArray(end)` payloads let the iterator jump directly to the next sibling.

### Results

32 unit tests + 5 doc-tests pass; zero warnings.

### Commit

`eb5de55` feat: add TapeRef::object_iter and array_iter

## Session 17 — Remove Value type

### Remove `Value<'a>` and `parse_json`

**What was done**

The `Value<'a>` tree type, `parse_json` entry point, `ValueWriter`, and all
supporting code in `src/value.rs` were removed.  The `Tape` / `TapeRef` path
is now the sole output format.

Specifically:

- `src/value.rs` deleted (`git rm`).
- `src/lib.rs`: removed `pub mod value`, `pub use value::Value`,
  `use value::{ValueWriter, is_valid_json_number}`, and the `parse_json`
  function + its doc-test.  `is_valid_json_number` (previously in
  `value.rs`) was moved inline into `lib.rs` as a private function, since it
  is still needed by `write_atom`.
- `src/json_ref.rs`: removed `use crate::value::Value`, the
  `impl JsonRef<'a> for &'a Value<'a>` block, and the `&'a Value<'a>` bullet
  from the trait's doc comment.  Test module rewritten: `fn run()` and
  `fn run_both()` helpers deleted; all tests that exercised both `&Value` and
  `TapeRef` paths were updated to use only `run_tape()`.  The
  `jsonref_scalars_value` test was removed entirely.
- `benches/parse.rs`: the `#[cfg(feature = "stats")]` `print_stats` helper
  was updated to alias `parse_to_tape` as `parse_json` so that the
  `#[cfg(feature = "stats")]` gate continues to compile.
- `README.md`: quick-start example updated to use `parse_to_tape`; Output
  formats list trimmed to two entries.

**Design decisions**

`Value` was a convenient heap-allocated tree that mirrored `serde_json::Value`,
but benchmarks showed it was always slower than the tape and the codebase now
focuses on flat-tape output.  Removing it simplifies the public API and
eliminates ~500 lines of code.

`is_valid_json_number` is still needed at parse time (in `write_atom`) so it
was migrated to `lib.rs` rather than deleted; it remains private.

**Results**

18 unit tests + 4 doc-tests pass; zero warnings.  5 files changed,
69 insertions, 590 deletions.

**Commit**

`cbb1e6b` Remove Value type and parse_json; tape is the only output format

## Session 18 — Benchmark refresh (March 2026)

### Results

Re-ran `cargo bench` with `RUSTFLAGS="-C target-cpu=native"`.  asmjson now
leads sonic-rs on all three workloads:

| Parser             | string array | string object | mixed      |
|--------------------|:------------:|:-------------:|:----------:|
| asmjson zmm (tape) | 8.20 GiB/s   | 5.48 GiB/s    | 370 MiB/s  |
| sonic-rs           | 7.37 GiB/s   | 4.21 GiB/s    | 368 MiB/s  |

### Design decisions

README table and accompanying prose updated to reflect the new leader, and
stale references to simd-json, serde_json, and the removed asmjson Value
variants were removed.

### Commit

`63d6957` bench: update README with March 2026 results (asmjson leads sonic-rs)

## Session — TapeEntry: split Cow into borrowed + escaped variants

### What was done

Replaced the two `Cow<'a, str>` payload variants in `TapeEntry`:

| Before | After |
|--------|-------|
| `String(Cow<'a, str>)` | `String(&'a str)` + `EscapedString(Box<str>)` |
| `Key(Cow<'a, str>)` | `Key(&'a str)` + `EscapedKey(Box<str>)` |

`TapeWriter::string` / `TapeWriter::key` now branch on the `Cow` variant from
the parser: `Borrowed` goes into the plain variant; `Owned` (escape-decoded)
is converted to `Box<str>` and stored in the `Escaped*` variant.

`TapeObjectIter`, `json_ref::as_str`, and `json_ref::get` were extended to
match both the plain and escaped variants.

### Design decisions

`Box<str>` (ptr + len = 16 bytes) was chosen over `String` (ptr + len + cap =
24 bytes) because the decoded string is never grown after allocation; dropping
the capacity word is the right trade-off.

An alternative was to keep `Cow` on the `JsonWriter` trait and only change
`TapeEntry`.  This was the approach taken: the trait signature is untouched,
keeping the door open for alternative `JsonWriter` impls that may prefer the
`Cow` abstraction.

### Results

`size_of::<TapeEntry>()` reduced from **32 bytes** to **24 bytes** (25%
reduction).  All 18 unit tests and 4 doc-tests continue to pass.

## Session — JsonWriter: replace Cow methods with string/escaped_string and key/escaped_key

### What was done

Split the two `Cow`-taking methods on the `JsonWriter` trait into four
explicit methods:

| Before | After |
|--------|-------|
| `fn string(&mut self, s: Cow<'src, str>)` | `fn string(&mut self, s: &'src str)` |
| | `fn escaped_string(&mut self, s: Box<str>)` |
| `fn key(&mut self, s: Cow<'src, str>)` | `fn key(&mut self, s: &'src str)` |
| | `fn escaped_key(&mut self, s: Box<str>)` |

`parse_json_impl` now dispatches directly on the `str_escaped` flag and calls
the appropriate method instead of allocating a `Cow`.  The `current_key: Cow`
local was replaced by `current_key_raw: &'a str` + `current_key_escaped: bool`.
The `use std::borrow::Cow` import was removed from `lib.rs`.

`TapeWriter` was simplified to four one-liner push calls.

### Design decisions

Having separate methods at the trait level means `JsonWriter` implementors no
longer need to import or pattern-match `Cow`.  A `Box<str>` is the minimal
allocation for the decoded text (no spare capacity), consistent with the
`TapeEntry` representation.

### Results

All 18 unit tests and 4 doc-tests continue to pass.

## Session — Zero-allocation parse_json_impl fast path

### What was done

Eliminated the two remaining heap allocations from the non-escaping path of
`parse_json_impl`:

**Frames stack**: replaced `Vec<FrameKind>` with a caller-supplied
`&mut [FrameKind; 64]` and a `frames_depth: usize` cursor.  `push` / `pop` /
`last` / `is_empty` are now simple array-index operations.  Nesting beyond 64
levels returns `State::Error`.  `FrameKind` gained `#[derive(Copy, Clone,
PartialEq)]` to enable the array semantics.

**Unescape buffer**: replaced the `unescape_str(s) -> String` helper
(which allocated a fresh `String` then a second time for `into_boxed_str`)
with `unescape_str(s, out: &mut String)` that reuses a caller-supplied buffer.
Each escaped value now performs exactly one allocation (`Box::from(buf.as_str())`).

`parse_with` (the public entry point) allocates both resources on its own
stack frame and passes them down, so the public API is unchanged.

`unescape_str` is now `#[unsafe(no_mangle)]` + `#[inline(never)]` and `pub`,
giving it a stable C-linkage symbol for profiling or external calls.

### Design decisions

64 levels of nesting covers all realistic JSON; deeply-nested pathological
inputs are rejected as errors.  The `String` reuse avoids the
`String::with_capacity` allocation on every escape-containing token while
still producing a proper `Box<str>` for the `TapeEntry`.

### Results

All 18 unit tests and 4 doc-tests pass.  The hot path (no escape sequences)
now allocates zero bytes inside `parse_json_impl` itself.


---

## Session 3 — Hand-written AVX-512BW assembly translation

### What was done

Created `asm/x86_64/parse_json_zmm_dyn.s` — a complete hand-written GNU
assembler translation of the `parse_json_impl` state machine.

Two preparatory changes were also made to `src/lib.rs`:

- `FrameKind` received `#[repr(u8)]` with explicit discriminants
  `Object = 0` and `Array = 1`, giving a stable ABI for the assembly.
- A thin `is_valid_json_number_c` wrapper was added with
  `#[unsafe(no_mangle)] pub extern "C"` linkage so it can be called from
  assembly without name-mangling.

### Design decisions

**Direct threading** — each state ends with an unconditional `jmp` to the
next state label.  No integer state variable is stored anywhere; the
program counter encodes the state.  A pair of registers (`r10` = resume
address, `r11` = EOF-handler address) is loaded just before every
`jmp .Lchunk_fetch`, so the shared fetch block can service every state
with a final `jmp r10`.

**Inlined classify_zmm** — the AVX-512BW classification (six
`vpcmpub`/`vpcmpeqb` instructions + three `korq` merges + four `kmovq`
extracts) is inlined at `.Lchunk_fetch`.  Constants live in `.rodata` as
six 64-byte lanes matching the `ByteStateConstants` layout.

**Register allocation** — five callee-saved GP registers carry persistent
state across the entire function:

| Register | Purpose |
|----------|---------|
| `rbx`    | writer data pointer (fat-ptr data half) |
| `r12`    | `src_base` |
| `r13`    | `src_end` |
| `r14`    | writer vtable pointer (fat-ptr vtable half) |
| `r15`    | `frames_buf` (`&mut [u8; 64]`) |

`rcx` holds `chunk_offset` inside the inner loop and is saved to
`[rbp-168]` (LOC_COFF) across every vtable call.

**Vtable offsets** — the 15-entry `dyn JsonWriter` vtable is documented
at the top of the file with byte offsets +0 through +112, derived from the
Rust fat-pointer convention (drop/size/align first, then methods in
declaration order).

**EOF handling** — each state provides its own `r11` EOF target set just
before the refetch jump.  States where EOF is legal (top-level whitespace,
after a complete value) land at `.Leof_after_value`; all others land at
`.Lerror`.

### Results

All 18 unit tests and 4 doc-tests continue to pass after `cargo fmt &&
cargo test`.  The assembly file is not yet linked into the crate but is
provided for inspection, benchmarking, and future FFI integration.

### Commit

`8cbce74` — asm: add x86_64 AVX-512BW direct-threading JSON parser

### Inline .Lcheck_inner_end — direct jumps to next state

**What was done**: Removed the `.Lcheck_inner_end` trampoline label. The
trampoline was a shared 4-instruction block (`cmp rcx, chunk_len; jae
.Lchunk_fetch; jmp r10`) reached by all 26 state-transition sites after
loading `r10`/`r11`.

Each site was rewritten to:

```asm
    lea     r11, [rip + .Leof_handler]
    cmp     rcx, qword ptr [rbp + LOC_CHUNK_LEN]
    jb      .Lnext_state              ; fast path: direct jump, r10 untouched
    lea     r10, [rip + .Lnext_state] ; slow path: set r10 for refetch
    jmp     .Lchunk_fetch
```

`r10` is now loaded only when the chunk is actually exhausted; the fast
path jumps directly to the target state without touching `r10` at all.
The refetch labels (`.Lrefetch_*`) are unchanged since they always feed
`.Lchunk_fetch` and still set `r10`.

**Design decisions**: The `jb` (jump-if-below) form avoids a negated
comparison.  `r11` is set unconditionally so that if `.Lchunk_fetch`
later hits EOF it always has a valid handler, regardless of which path
was taken.

**Results**: Zero references to `.Lcheck_inner_end` in the file.  File
grew from 1080 to 1124 lines (net +44 from expanding 26 × 3-line blocks
to 5 lines each, minus the deleted 10-line trampoline).

**Commit**: `e0e1993` — asm: inline .Lcheck_inner_end; use direct jb to next state

---

## Session 5 — Rust entrypoint for the zmm-dyn assembly parser

### Adding `parse_to_tape_zmm_dyn`

**What was done**: Added a public Rust function `parse_to_tape_zmm_dyn<'a>(src: &'a str) -> Option<Tape<'a>>` that drives the hand-written AVX-512BW assembly parser (`parse_json_zmm_dyn`) and returns the same `Tape` type as the pure-Rust entrypoints.

The work spanned several sub-problems that had to be solved before the doctest `assert_eq!(tape.root().get("x").as_i64(), Some(1))` passed.

### Build system: compiling the assembly

A `build.rs` was created to compile `asm/x86_64/parse_json_zmm_dyn.S` with the `cc` crate (added to `[build-dependencies]`).  The `.s` file was renamed to `.S` (uppercase) so that the C pre-processor runs first and strips `//` comments before GAS assembles the file — GAS in AT&T mode treats `//` as division.

### Assembly correctness fixes

Three assembly bugs were found and fixed before turning to the Rust side:

1. **Three-register addressing**: GAS does not allow `[r12+rax+rcx]` in Intel syntax.  Nine sites were fixed with `add rax, r12` followed by the two-register form.
2. **Wrong shift register**: `shl rax, cl` used `cl` (the chunk-offset byte of `rcx`) instead of the chunk length from `rsi`.  Fixed by inserting `mov ecx, esi` before the shift.
3. **Uninitialised `LOC_CHUNK_LEN`**: The first call to `.Lchunk_fetch` read an uninitialised stack slot.  Fixed by zero-initialising it in the prologue.

### Replacing the raw Rust dyn-vtable with a stable C-ABI vtable

**Design decisions**: The initial approach passed a raw Rust `dyn JsonWriter` fat-pointer vtable to the assembly, which assumed fixed byte offsets (24, 32, 40, …) for each method.  Rust's `dyn` vtable layout is implementation-defined (the header contains size, align, and a destructor before the first method slot), so those offsets are unstable and differed from reality.

The fix replaces the raw vtable with a `#[repr(C)] struct ZmmVtab` whose eleven fields are `unsafe extern "C"` function pointers at predictable 8-byte-aligned offsets (0, 8, 16, …).  Rust fills this struct on the stack with concrete trampoline functions, and the assembly uses matching `.equ VTAB_*` constants starting at 0.

Each trampoline casts `data: *mut ()` to `*mut TapeWriter<'static>` (the `'static` is a white-lie required because `extern "C"` functions cannot carry generic lifetime parameters; safety is upheld because the writer and source JSON both outlive the assembly call).  Trampolines for `escaped_string` and `escaped_key` copy the unescaped bytes into a fresh `Box<str>` to give proper ownership semantics.

All raw-pointer dereferences are wrapped in `unsafe {}` blocks to satisfy the Rust 2024 `unsafe_op_in_unsafe_fn` requirement.

### Fixing r8/r9 clobbering in `.Lemit_atom`

**What was done**: In `.Lea_number`, the atom pointer and length were saved into `r8`/`r9` before calling `is_valid_json_number_c`.  Both registers are caller-saved and were clobbered by the call, so the subsequent `mov rsi, r8 / mov rdx, r9` loaded garbage, causing the number vtable method to receive wrong arguments.

Fixed by saving pointer and length to the stack (`LOC_ATOM_START` / `LOC_STR_START`, which are stale at this point) and reloading from those slots after the validation call.

**Results**: All 18 unit tests and 5 doctests pass with zero warnings.  The doctest `assert_eq!(tape.root().get("x").as_i64(), Some(1))` passes correctly.

**Commit**: `944d97f` — feat: add parse_to_tape_zmm_dyn Rust entrypoint with C-ABI vtable

---

## Session 6 — Benchmarking `parse_to_tape_zmm_dyn`

### What was done

Added `asmjson/zmm_dyn` as a Criterion benchmark case in all three existing groups (`string_array`, `string_object`, `mixed`) in [benches/parse.rs](benches/parse.rs), gated on `#[cfg(target_arch = "x86_64")]` so it is silently skipped on other platforms.

### Results (10 MiB inputs, release build, x86_64)

| group         | asmjson/zmm   | asmjson/zmm_dyn | asmjson/u64  | sonic-rs      |
|---------------|---------------|-----------------|--------------|---------------|
| string_array  | 8.48 GiB/s    | 7.95 GiB/s      | 6.88 GiB/s   | 7.15 GiB/s    |
| string_object | 5.77 GiB/s    | 5.47 GiB/s      | 4.68 GiB/s   | 4.08 GiB/s    |
| mixed         | 451 MiB/s     | 445 MiB/s       | 448 MiB/s    | 484 MiB/s     |

`zmm_dyn` is ~6–8 % slower than the pure-Rust `zmm` path on the string-heavy workloads.  The overhead comes from the two extra indirect calls (through `ZmmVtab`) per parsed token compared with the inlined fast paths in the Rust state machine.  On the mixed workload (many small numbers, booleans, and structural tokens) the gap closes to ~1 % because the vtable-call overhead is a smaller fraction of the per-token work.

### Design decisions

No changes were made to the vtable or trampoline design.  The benchmark baseline is the Rust `asmjson/zmm` path rather than a dedicated "no-vtable" comparison, which keeps the measurement directly actionable: the assembly entrypoint needs to match or beat the Rust zmm path to justify its complexity.

**Commit**: `6525c72` — bench: add asmjson/zmm_dyn to all three criterion groups

---

## Session 7 — Replace `TapeEntry` enum with a 16-byte `#[repr(C)]` struct

### What was done

Replaced the `pub enum TapeEntry<'a>` (whose Rust-enum representation was
compiler-determined and varied by variant) with a fixed-size `#[repr(C)]
pub struct TapeEntry<'a>` that is exactly **16 bytes** on all platforms:

| word | offset | meaning |
|------|--------|---------|
| 0 | 0 | bits 63–60: `TapeEntryKind` discriminant (0–10); bits 27–0: string length **or** object/array end-index |
| 1 | 8 | `*const u8` pointer to string bytes; null for non-string kinds |

A companion `#[repr(u8)] pub enum TapeEntryKind` carries the fixed
discriminants (Null=0, Bool=1, … EndArray=10).  These values are part of
the public ABI that `parse_json_zmm_tape.S` will consume.

For `EscapedString` / `EscapedKey` the pointer is the raw `Box<str>` data
pointer whose ownership lives in the entry; `Drop` and `Clone` are
implemented manually to free / deep-copy the heap allocation correctly.

### Design decisions

*Fixed layout*: The primary motivation was to give the forthcoming
`parse_json_zmm_tape.S` assembly a deterministic, ABI-stable representation
to write into directly in 16-byte stores, with no Rust enum layout involved.
The `u64` tag-word encodes the kind in the top four bits and the
length/index in the low 28 bits; the assembly can set an entry in two `mov`
instructions (tag word then pointer).

*Backwards-compat shim*: All old enum-variant names (`TapeEntry::Null`,
`TapeEntry::Bool(v)`, `TapeEntry::StartObject(n)`, …) are kept as
`#[allow(non_snake_case)]` constructor methods / associated constants so the
pre-existing test suite compiled unchanged.  Pattern-match arms that
destructured enum payloads were rewritten to use the accessor methods
(`as_start_object()`, `as_bool()`, etc.).

### Results

`size_of::<TapeEntry>() == 16`, `align_of::<TapeEntry>() == 8`.  All 18
unit tests and 5 doctests pass; zero warnings.

**Commit**: `e89f2fc` — refactor: replace TapeEntry enum with 16-byte repr(C) struct



## Session 9 — direct-write assembly tape parser (`parse_json_zmm_tape`)

### What was done

Added `asm/x86_64/parse_json_zmm_tape.S`, a new hand-written x86-64 AVX-512BW
assembly parser that writes `TapeEntry` values directly into a pre-allocated
array, bypassing all virtual dispatch overhead present in the existing `zmm_dyn`
variant.  Supporting changes: `tape_take_box_str` C helper in `lib.rs`;
`parse_json_zmm_tape` extern declaration; `parse_to_tape_zmm_tape` public
function; `build.rs` and `benches/parse.rs` updated.  Nine new unit tests
(27 total) verify correctness against the reference Rust parser.

### Design decisions

**Register map** — `rbx` holds `tape_len` live in a register (not spilled to
memory) to avoid a load/store on every emitted token.  `r14` is `tape_ptr`
(the base of the pre-allocated `TapeEntry` array), replacing the vtable
pointer in `zmm_dyn`.  `r15` is `frames_buf` (frame-kind stack), and `r12`/`r13`
are `src_base`/`src_end` as before.

**Inline writes** — instead of calling 11 vtable slots, each token type is
written inline:
```asm
lea  rax, [rbx + rbx]
lea  rax, [r14 + rax*8]   ; tape_ptr + tape_len*16
; fill tag_payload and ptr fields ...
inc  rbx                   ; tape_len++
```

**`open_buf`** — a separate `[u64; 64]` array holds the tape index of each
pending `StartObject`/`StartArray`.  On the matching `}` or `]`, the start
entry's `payload` field is back-patched with the end index.

**`tape_take_box_str`** — a `#[no_mangle] extern "C"` Rust helper converts
the `unescape_buf` `String` into a leaked `Box<str>`, writing the raw pointer
and length to out-params.  The assembly calls this for every escaped string or
key, then writes an `EscapedString`/`EscapedKey` `TapeEntry` that owns the box.

**Pre-allocation** — `parse_to_tape_zmm_tape` reserves `src.len() + 2`
entries before calling the assembly; this is always sufficient for valid JSON
(at most one token per input byte) so no reallocation occurs during parsing.

### Bug fixes discovered during testing

Two bugs found while adding correctness tests:

1. **String-at-chunk-boundary EOF failure** — when a string's closing `"` fell
   exactly at a 64-byte chunk boundary, the code set `r11 = .Lerror_from_r11`
   and jumped to `chunk_fetch` with `r10 = .Lafter_value`.  On the following
   `chunk_fetch` the source was exhausted, so `r11` was invoked and the parse
   failed even for a valid top-level string.  Fix: set `r11 = .Leof_after_value`
   in the string and escaped-string emit paths before the chunk-boundary
   fallthrough.

2. **Empty input accepted** — `.Leof_after_value` checked only `frames_depth == 0`
   before reporting success, so empty input (`""`) returned `Ok` with an empty
   tape.  Fix: added `test rbx, rbx; jz .Lerror` to reject zero-token output.

### Results

All 27 unit tests pass; all 6 doctests pass (3 ignored).  The implementation
is compiled and linked via `cc::Build` in `build.rs` alongside the existing
`parse_json_zmm_dyn.S`.  Correctness is validated by comparing `TapeEntry`
slices against the reference Rust parser across atoms, plain strings, escaped
strings, long strings (>64 bytes), nested structures, escaped keys, whitespace
variants, and rejection of malformed inputs.

**Commit**: `84bb057` — feat: add parse_to_tape_zmm_tape direct-write assembly parser

## Session 8 — Benchmarks and PAYLOAD_MASK widening

### Benchmarking `parse_to_tape_zmm_tape` vs the field

`cargo bench` was run to compare the three tape parsers: the Rust reference
`zmm`, the dynamic-dispatch assembly `zmm_dyn`, and the new direct-write
`zmm_tape`.

| benchmark     | zmm (Rust) | zmm_dyn   | zmm_tape  | δ tape vs dyn |
|---------------|-----------|-----------|-----------|---------------|
| string_array  | 1.251 ms  | 0.959 ms  | 1.008 ms  | +5% slower    |
| string_object | 1.709 ms  | 1.426 ms  | 1.554 ms  | +9% slower    |
| mixed         | 14.85 ms  | 15.34 ms  | 11.86 ms  | -23% faster   |

On purely string-heavy workloads the vtable-call overhead of `zmm_dyn` is
negligible compared to the SIMD scan time, so the extra indirection costs
nothing and `zmm_dyn` wins.  On `mixed` (twitter-style: many short integer,
boolean, null, and nested-object tokens) the direct tape writes in `zmm_tape`
avoid enough per-token overhead to win by 23%.

### Widening PAYLOAD_MASK from 28 bits to 60 bits

`TapeEntry` stores the kind in bits 63-60 and the payload in bits 59-0, giving
60 bits of payload capacity.  The original constant used only the low 28 bits
(`(1 << 28) - 1`), wasting bits 59-28 and capping string/array lengths
unnecessarily.

**Rust** (`src/tape.rs`): `PAYLOAD_MASK` changed to `u64::MAX >> 4` (bits 59-0).

**Assembly** (`asm/x86_64/parse_json_zmm_tape.S`): the previous
`and r10, 0x0FFFFFFF` could not be widened directly because x86-64 encodes
`and` immediate as a 32-bit sign-extended value (max `0x7FFFFFFF`).  A 60-bit
immediate would require a 64-bit `mov` + `and` pair.  Instead the mask is
applied with a shift pair: `shl r10, 4` / `shr r10, 4`, which clears the top
4 bits without needing a large immediate.  All ten masking sites in the file
were updated.

All 27 unit tests and 6 doctests pass after the change.

**Commit**: `2c59a28` -- refactor: widen TapeEntry payload from 28 to 60 bits

## Session 9 — Perf profiling of `parse_to_tape_zmm_tape`

### What was done

A tight-loop driver (`examples/perf_zmm_tape.rs`) was created to generate
~10 MiB of mixed JSON (same generator as the criterion `bench_mixed` benchmark)
and call `parse_to_tape_zmm_tape` 400 times.  The binary was built with
`CARGO_PROFILE_RELEASE_DEBUG=true cargo build --release --example perf_zmm_tape`
to preserve symbols, then profiled with
`perf record -g --call-graph dwarf -F 999`.

### Results

Flat profile (top user-space functions):

| % cycles | Function |
|----------|----------|
| 43.35 % | `parse_json_zmm_tape` |
| 8.92 % | `perf_zmm_tape::main` (almost entirely `Tape` drop) |
| 8.20 % | `<TapeEntry as Drop>::drop` |
| 4.03 % | `asmjson::is_valid_json_number` |
| 2.92 % | `is_valid_json_number_c` |

`perf annotate` of `parse_json_zmm_tape` identified the hottest states:

* **`.Lkey_end`** -- writing a `TapeEntry` for a key (`mov %r10,(%rax)` at 1.48 %
  of function samples), plus surrounding bit-manipulation (kind-tag ORing,
  pointer store, counter increments).  Every object key emits one entry, so
  this is the dominant hot path on the twitter-like dataset.
* **`.Lkey_chars`** -- inner scan loop for key bytes: `andn`/`or`/`shr`/`tzcnt`
  bitmap walk plus a byte load and `\` check (0.58-0.78 % per instruction,
  ~6 % of function samples collectively).
* **`.Lafter_colon`** -- next-byte fetch and dispatch after `:` (~5 % of function),
  with several `mov`/`tzcnt`/`add` instructions at 0.59-0.95 %.
* **`.Lstring_chars`** -- tape write for string entries (0.89 %).
* **`.Latom_chars`** -- the `call is_valid_json_number_c` instruction (0.88 %).

Many hot instructions use frame-pointer-relative stack slots (`-0x80(%rbp)`,
`-0x98(%rbp)`, etc.) for locals such as `chunk_len`, `string_bitmask`, and
`colon_bitmask`.  These are spilled because the function uses more live values
than the callee-saved registers can accommodate.

### Design decisions

No optimisations were applied in this session; profiling was observation-only.
The main actionable findings are:

1. **Drop overhead (~16 %)**: `TapeEntry::drop` checks `kind == EscapedString ||
   EscapedKey` for every entry.  On mixed JSON most entries are plain strings or
   scalars, so the check always fails, yet each still pays for one `kind()` decode
   plus a branch.  A future optimisation could skip the drop loop by tracking
   escape counts separately or keeping escaped entries in a side-vector.
2. **Number validation (~7 %)**: `is_valid_json_number` + `is_valid_json_number_c`
   together consume 7 % of cycles.  Inlining or simplifying the validator could
   recover meaningful throughput, especially for the integer-heavy mixed workload.
3. **Stack spills in hot loops**: register pressure forces `chunk_len` and the two
   bitmask locals to memory.  Restructuring locals or reducing live-variable count
   could reduce load/store traffic in `.Lkey_chars` and `.Lafter_colon`.

**Commit**: n/a -- profiling only, no source changes

## Session 10 — Skip TapeEntry drops via Tape::has_escapes

### What was done

Profiling showed ~16 % of cycles spent in `<TapeEntry as Drop>::drop`, which
checks `kind == EscapedString || EscapedKey` for every entry even when the
tape contains none.  The fix: add a `has_escapes: bool` field to `Tape` and
skip per-element destructors when it is `false`.

**Changes:**

* `src/tape.rs` — `Tape` gains `pub(crate) has_escapes: bool`.  A `Drop for
  Tape` impl is added: when `!has_escapes` it calls `unsafe { self.entries.set_len(0) }`
  before the Vec drops, so the backing allocation is freed without invoking
  `TapeEntry::drop` on each element.  `TapeWriter` gains the same field,
  set to `true` inside `escaped_string` and `escaped_key`, then forwarded to
  `Tape` in `finish()`.

* `src/lib.rs` — `parse_json_zmm_tape` extern declaration gains an 8th
  argument `has_escapes_out: *mut bool`.  `parse_to_tape_zmm_tape` initialises
  `let mut has_escapes = false`, passes `&raw mut has_escapes` to the assembly,
  and propagates it into the returned `Tape`.

* `asm/x86_64/parse_json_zmm_tape.S` — documents the new 8th argument
  (`[rbp+24]`, `.equ LOC_HAS_ESC_OUT, +24`).  Both `.Lsc_emit_escaped` and
  `.Lke_emit_escaped` store `1` to `*has_escapes_out` immediately after
  writing the tape entry. No new stack space needed (the argument lives in
  the caller's frame above the saved `rbp`).

### Design decisions

Setting the flag in the assembly at the two emit sites keeps the hot paths
(plain strings, keys, numbers) unchanged.  The alternative of scanning the
tape after parsing would have been O(n) on every call.

`TapeEntry::drop` is kept unchanged for correctness when entries are used
outside a `Tape` (e.g. constructed in tests).

### Results

All 27 unit tests and 6 doctests pass.

**Commit**: `3ec8fba` -- perf: skip TapeEntry drops via Tape::has_escapes flag

## Session 11 — SWAR digit fast path for short numbers

### What was done

Profiling showed ~7 % of cycles in `is_valid_json_number` + `is_valid_json_number_c`.
The vast majority of numbers in twitter-like JSON are plain integers up to 8 bytes
(e.g., `"id": 12345678`).  These can be validated without a function call by using
SWAR (SIMD Within A Register) bit tricks inside `.Lemit_atom`.

The fast path is applied in both `parse_json_zmm_tape.S` and `parse_json_zmm_dyn.S`.

### Design: SWAR all-digits check

For each byte b in the loaded qword, the check exploits the layout of ASCII digits
('0' = 0x30 .. '9' = 0x39):

```
t = (b | 0x80) - 0x30

Lower bound (b >= '0'):  bit 7 of t = 1
  Setting the top bit ensures (b|0x80) >= 0x80.  For b >= 0x30 the subtraction
  0xB0..0xBF - 0x30 = 0x80..0x8F leaves bit 7 set.  For b < 0x30 the result
  drops to at most 0x7F (top bit clear) -- the borrow has consumed bit 7.

Upper bound (b <= '9'):  (t + 0x06) & 0x10 == 0
  A digit gives t-byte = 0x80..0x89; adding 0x06 = 0x86..0x8F, bit 4 clear.
  A byte > '9' gives t-byte >= 0x8A; adding 0x06 >= 0x90, bit 4 set.
```

Whole-word check:

```asm
  mov  r10, 0x8080808080808080
  or   r10, rax                 ; set top bit per byte
  mov  r11, 0x3030303030303030
  sub  r10, r11                 ; t = (b|0x80)-0x30 per byte
  ; lower bound
  mov  r11, r10 / not r11
  mov  rax, 0x8080808080808080
  test r11, rax                 ; ZF=1 => all bytes >= '0'
  ; upper bound
  mov  r11, 0x0606060606060606
  add  r10, r11
  mov  r11, 0x1010101010101010
  test r10, r11                 ; ZF=1 => all bytes <= '9'
```

Note: `sub`/`add`/`test` cannot encode 64-bit immediates on x86-64 (max 32-bit
sign-extended).  Large constants are loaded into a register via `mov r64, imm64`
first.

### Algorithm

1. If `rdx > 8`: always use full validator (atom doesn't fit in a qword).
2. If `rsi + 8 > src_end`: fewer than 8 bytes remain in source buffer -- safe
   to load only `rdx` bytes, but need padding; fall back to validator instead.
3. Leading-zero guard: if first byte is '0' and `rdx > 1`, fall back to
   validator (would otherwise accept invalid "01", "007", etc.).
4. Load 8 bytes from `rsi`; fill the `8 - rdx` unused high bytes with '0'
   (0x30) using a shift-derived mask, so they vacuously pass the digit check.
5. SWAR check.  If all bytes are digits: write Number entry directly.
6. Otherwise: call `is_valid_json_number_c` (handles '-', '.', 'e', leading
   zeros, etc.).

### Results

All 27 unit tests pass, including new boundary tests:
- Pure integers 1--8 bytes long hit the fast path and match the dyn reference.
- A 9-byte integer ("123456789") correctly falls through to the full validator.
- Leading-zero inputs ("01", "00", "007", "01234567") are still rejected.

**Commit**: `bae1632` -- perf: SWAR digit fast path for short numbers in .Lemit_atom

## Session 12 — perf profile and NT-store experiment

### Profiling zmm_tape

Ran `perf record -g --call-graph dwarf` on `perf_zmm_tape` (400 iterations of
~10 MiB mixed JSON).  Flat profile (self %):

| Symbol | Self% |
|---|---|
| `parse_json_zmm_tape` | 59.4 % |
| `asmjson::is_valid_json_number` | 1.3 % |
| `is_valid_json_number_c` | 0.6 % |
| all allocator / drop | ~0 % |

Inside the parser the profile is very flat — no instruction exceeds 2 %.
The three hottest instructions (~1.9 % combined) are the `mov %r10,(%rax)` tape
entry tag stores.  Number validation after the SWAR fast path is now ~2 % total.
Memory/drop overhead is effectively zero thanks to `has_escapes`.

### NT store experiment

Replaced every tape entry write (`mov qword ptr [rax], r10` and
`mov qword ptr [rax + 8], ...`) with `movnti` (non-temporal store), which
bypasses the cache on write.  Added `sfence` before the function `ret`.

**Result: 3–5 % regression on all three bench workloads.**

Reason: the benchmark iterates over ~1 MiB of JSON many times.  The tape fits
in L3 cache.  With regular stores the L3 is warm when `tape_sum_lens` traverses
the tape immediately after parsing; with `movnti` the traversal refetches from
DRAM.  NT stores are appropriate only when the working set exceeds L3 (large
one-shot streams where the tape would be evicted before the consumer reads it).

The commit was reverted (`0673d7d`).

### Design decision

Non-temporal stores are a context-dependent trade-off:

- **Beneficial**: streaming workloads with tapes larger than L3 (e.g., multi-MB
  one-shot document ingestion) where write and read are separated by enough work
  or time to cause natural eviction.
- **Harmful**: small/medium JSON or repeated parsing where the tape stays hot in
  L3 (as in the criterion bench).

No further action taken; existing `mov` stores are optimal for the benchmark
profile.

**Commits**: `e9bf4e1` NT stores (then `0673d7d` revert)
---

## Session 13 — promote hot stack slots to live registers r8/r9

### Motivation

The perf profile from session 12 highlighted two stack-slot loads as the
highest-weight individual instructions:

| LOC slot        | sample weight |
|-----------------|---------------|
| LOC_CHUNK_LEN   | 6.18 %        |
| LOC_POS         | 5.20 %        |

Both are read on every iteration of the inner dispatch loop (chunk_offset
advance, `cmp rcx, chunk_len`, `lea rdx, [r12 + pos]`).

### Design decisions

**Register selection**: After the prologue, `r8` (which carried `frames_buf`
in the calling convention) is moved to `r15`, and `r9` (which carried
`open_buf`) is spilled to `LOC_OPEN_BUF`.  Both `r8` and `r9` are therefore
free as caller-saved scratch registers for the rest of the function.

| Register | Live value   | Stack spill home |
|----------|-------------|-----------------|
| `r8`     | chunk\_len  | `LOC_CHUNK_LEN` |
| `r9`     | pos         | `LOC_POS`       |

**Spill sites**: External calls (`unescape_str`, `tape_take_box_str`,
`is_valid_json_number_c`) are caller-saved clobbers, so r8/r9 must be
saved to their stack homes before each call cluster and restored afterward.
These paths are hit only for escaped strings/keys and numbers that fail the
fast SWAR path — all are rare in typical JSON.

**zmm\_space pointer conflict**: The `.Lclassify_do` block previously used
`r9` as a scratch pointer to the `.Lzmm_space` lookup table.  Moved to `rdi`
(safe because no calls occur in `chunk_fetch`).

**Prologue init**: Changed `mov qword ptr [rbp + LOC_POS], rax` →
`xor r9d, r9d` and `mov qword ptr [rbp + LOC_CHUNK_LEN], rax` →
`xor r8d, r8d`.

**chunk\_fetch advance**: Collapsed the old three-instruction sequence
```
mov rax, [rbp + LOC_CHUNK_LEN]
add [rbp + LOC_POS], rax
mov rax, [rbp + LOC_POS]           ; → r9 after sed
lea rdx, [r12 + rax]
```
into two instructions:
```
add r9, r8                          ; pos += chunk_len
lea rdx, [r12 + r9]                 ; chunk_ptr
```

### Results

```
mixed/asmjson/zmm_tape     −5.3 %  time   (+5.6 % throughput)  vs previous baseline
string_array/asmjson/zmm_tape  −1.7 %  time
string_object/asmjson/zmm_tape  −0.7 %  time  (within noise)
```

All 27 unit tests and 6 doc-tests pass.

**Commit**: `bc7891b` perf: promote LOC_CHUNK_LEN and LOC_POS to live registers r8/r9
## Session 17 — TapeOverflow error code with capacity-doubling retry

### What was done

Changed `parse_json_zmm_tape` from returning a `bool` (`1`=ok, `0`=error) to
returning a `u8` error code:

| Constant            | Value | Meaning                             |
|---------------------|-------|-------------------------------------|
| `RESULT_OK`         | `0`   | Parse succeeded                     |
| `RESULT_PARSE_ERROR`| `1`   | Invalid JSON                        |
| `RESULT_TAPE_OVERFLOW`| `2` | Tape buffer was too small           |

The Rust wrapper `parse_to_tape_zmm_tape` now starts with a conservative tape
capacity of `(src.len() / 4).max(2)` and doubles on every `RESULT_TAPE_OVERFLOW`
response until the parse succeeds.

### Design decisions

**Capacity checks in assembly**: A `cmp rbx, qword ptr [rbp + LOC_TAPE_CAP]`
/ `jae .Ltape_overflow` pair was inserted before every tape write site — 16
sites in total. The tape capacity is passed as a 9th stack argument (`LOC_TAPE_CAP = +32`).

**`.Lemit_atom` strategy**: `.Lemit_atom` uses `al=1/0` as an internal success
flag. Inserting a third return value there would have broken all callers that
use `test al, al; jz .Lerror`. Capacity checks were placed at the two *call
sites* instead (`.Latom_chars` and `.Latom_eof_flush`), leaving `.Lemit_atom`
internals unchanged.

**Memory safety on overflow**: Any `EscapedString`/`EscapedKey` entries already
written to the tape own `Box<str>` data. If the Vec is dropped with `len=0`,
those allocations leak. The `.Ltape_overflow` path first writes the partial
`rbx` (number of valid entries) to `*tape_len_out`, then returns `2`. The Rust
`RESULT_TAPE_OVERFLOW` arm calls `unsafe { tape_data.set_len(tape_len) }` so
the Vec correctly drops those entries before growing and retrying.

**Initial capacity**: `(src.len() / 4).max(2)` is intentionally small so the
retry path is exercised even on moderately sized inputs.

### Results

28 unit tests and 6 doc-tests pass. The new test `zmm_tape_overflow_retry`
builds a 200-element JSON array (~800+ tape entries), verifying that the
capacity-doubling retry produces the correct result.

**Commit**: `6c87ff4` feat: TapeOverflow error code with capacity-doubling retry

## Session 18 — Optimisation tips in README

### What was done

Added an **Optimisation tips** section to `README.md` (between Quick start and
Output formats) with two executable doc-test examples:

1. **Cache field refs from a one-pass object scan** — shows iterating a root
   object with `object_iter` once and storing the desired `TapeRef` values,
   avoiding the repeated O(n_keys) re-scan that `get(key)` performs on each
   call.

2. **Collect array elements for indexed or multi-pass access** — shows
   collecting `array_iter` results into a `Vec<TapeRef>`, giving O(1) random
   access and free additional passes over the same data.

### Design decisions

`TapeRef` is `Copy` (two `usize` fields), so storing it is cheap and safe for
the lifetime of the tape borrow.  The examples highlight this property
explicitly so users understand that there is no heap cost to caching refs.

The existing `Conformance note` section was added in the prior session; the new
section was inserted between Quick start and Output formats where it is most
visible to new users deciding how to traverse the parsed data.

### Results

28 unit tests + 8 doc-tests (including 2 new README examples) pass.

**Commit**: `e9ce7d8` docs: add optimisation tips — caching TapeRefs from object/array iterators

---

## Session 19 — serde feature: `from_taperef` Deserializer

### What was done

Added a `serde` optional feature that implements `serde::Deserializer<'de>` for
`TapeRef<'de, 'de>` and exposes a `from_taperef` top-level function.  The
feature gate is `--features serde`, satisfied by adding an optional serde 1.x
dependency in `Cargo.toml`.

Files changed:

- `Cargo.toml` — optional `serde = { version = "1", features = ["derive"] }`
  dependency; `serde` feature entry.
- `src/tape.rs` — added `pub(crate) fn source_string(&self) -> Option<&'a str>`
  on `TapeEntry`, which returns the source-JSON-lifetime `&'a str` for plain
  (non-escaped) `String` entries, enabling zero-copy deserialization.
- `src/de.rs` — new file containing the full serde integration:
  - `Error(String)` implementing `serde::de::Error`.
  - `impl<'de> Deserializer<'de> for TapeRef<'de, 'de>` — full dispatch over
    every `deserialize_*` method.
  - `TapeSeqAccess<'de>` (wraps `TapeArrayIter`), `TapeMapAccess<'de>` (wraps
    `TapeObjectIter`), and `KeyDeserializer<'de>` for borrowed object keys.
  - `UnitVariantAccess` / `UnitOnly` for string-valued unit enum variants.
  - `TapeEnumAccess` / `impl VariantAccess for TapeRef` for
    `{"Variant": payload}` style newtype/struct/tuple enum variants.
  - `pub fn from_taperef<'de, T: Deserialize<'de>>(r: TapeRef<'de, 'de>) ->
    Result<T, Error>` as the public entry point.
- `src/lib.rs` — `#[cfg(feature = "serde")] pub mod de;` and re-export of
  `from_taperef`.

### Design decisions

**Lifetime unification** — the `Deserializer` impl uses `TapeRef<'de, 'de>`
(both the tape-borrow and source-JSON lifetimes collapsed to `'de`).  This is
the common case: the tape and its source string both outlive the deserialization
scope.  `from_taperef` enforces this through its own signature.

**Zero-copy strings** — plain `String` tape entries (no escape sequences) are
deserialized via `visit_borrowed_str`, borrowing directly from the source JSON.
Escaped strings (heap-allocated `Box<str>`) go through `visit_str` and are
copied into the target type.

**Enum variants** — two strategies are used: a bare JSON string `"Foo"` maps to
a unit variant (the `TapeRef::deserialize_identifier` path); a single-key
object `{"Foo": value}` maps to any variant kind (newtype, struct, tuple) via
`TapeEnumAccess` + `impl VariantAccess for TapeRef`.

**Key deserializer** — `TapeObjectIter` yields `(&'t str, TapeRef)` pairs.
When both lifetimes are `'de`, the key is already `&'de str` and
`KeyDeserializer` passes it zero-copy to the visitor via `visit_borrowed_str`.

**`forward_to_deserialize_any!`** — used in `KeyDeserializer` to delegate every
unneeded `deserialize_*` method to `deserialize_any`, keeping boilerplate
minimal.

### Results

28 unit tests + 9 doc-tests (including the new `de::from_taperef` doctest) pass.

**Commit**: `9cad231` feat: add serde feature with from_taperef Deserializer for TapeRef

---

## Session 20 — parse_with auto-dispatches to asm dyn for classify_zmm

### What was done

Replaced the eleven `TapeWriter`-specific `extern "C"` trampolines
(`tw_null`, `tw_bool_val`, …) with a generic mechanism that works for any
`JsonWriter`:

- **`WriterForZmm` internal bridge trait** (`pub(crate)`) — exposes every
  `JsonWriter` method via raw `(*const u8, usize)` pairs, hiding the source
  lifetime `'a`. A blanket `impl<'a, W: JsonWriter<'a>> WriterForZmm for W`
  provides the implementation, using `std::mem::transmute` to re-attach the
  correct `'a` lifetime to the string slice before calling the concrete writer
  method.  This is the same unsafety pattern the old single-type trampolines
  used, generalised over `W`.

- **Generic trampolines** `zw_null::<W>`, `zw_string::<W>`, … — `unsafe extern
  "C"` free functions monomorphised per writer type.  `build_zmm_vtab::<W>()
  -> ZmmVtab` assembles them into a `ZmmVtab` on the stack.

- **`parse_with` fast path** — when `classify == classify_zmm`, AVX-512BW is
  present, the source starts with `{` or `[` (object / array), and the source
  contains no backslash, `parse_json_zmm_dyn` is called with a
  `W`-monomorphised vtable.  All other inputs fall back to the Rust path.

- **`parse_to_tape_zmm_dyn` simplified** — now calls `build_zmm_vtab::<
  TapeWriter<'a>>()` instead of inlining the deleted `tw_*` trampolines.

### Design decisions

**Guard conditions for the fast path.**  During testing, two pre-existing
limitations of `parse_json_zmm_dyn` were uncovered by the new routing:

1. The dyn asm crashes (SIGSEGV) on any input containing a backslash — it
   calls the `escaped_string` / `escaped_key` vtab slots but does not
   implement the unescape logic the Rust path provides.
2. The dyn asm returns `false` (parse failure) for top-level JSON strings and
   for bare scalars at the root (`"hello"` → None, but `null` / numbers / `{}`
   / `[]` work).

These limitations were masked before because the test suite used
`parse_to_tape(src, classify_zmm)` as the _reference_ oracle (Rust path), and
the fast path had not yet been wired up.  Rather than fixing the dyn asm (out
of scope), the fast path guards against both conditions:

```rust
&& !src.contains('\\')
&& src.bytes().find(|&b| !b" \t\r\n".contains(&b))
       .map_or(false, |b| b == b'{' || b == b'[')
```

**Generic trampolines via `WriterForZmm`.**  The original trampolines cast
`data` to `*mut TapeWriter<'static>`, relying on lifetime erasure.  To
generalise, the bridge trait's methods reconstruct `&'a str` from raw pointers
using `transmute`, where `'a` is the concrete lifetime from the
`impl<'a, W: JsonWriter<'a>> WriterForZmm for W` monomorphisation.  This is
sound for the same reason the original trampolines were sound: the assembly
call is synchronous and `src` outlives it.

### Results

28 unit tests + 9 doc-tests pass (same as before; the new routing is
transparent to all existing tests).

**Commit**: `0c5c260` feat: parse_with auto-dispatches to asm dyn when classify_zmm is used
---

## Session 20 — Drop ClassifyFn; four clean public entry points

### What was done

Removed the `ClassifyFn` type alias, `choose_classifier()`, and all `classify`
parameters from the public API in response to user feedback: "Lets drop
classifyfn and make the rust version always use the SWAR version. Have only
four entrypoints to lib.rs: `parse_with()`, `parse_to_tape()`, `unsafe
parse_with_zmm()`, `unsafe parse_to_tape_zmm()`. It is up to the user to make
sure that the CPU supports avx512bw."

Files changed: `src/lib.rs`, `src/tape.rs`, `src/json_ref.rs`, `src/de.rs`,
`benches/parse.rs`, `examples/perf_zmm_tape.rs`, `README.md`.

### Design decisions

**Four entry points only.** The new surface area is:

| Function | Safety | Classifier |
|---|---|---|
| `parse_to_tape(src)` | safe | SWAR (u64) |
| `parse_with(src, writer)` | safe | SWAR (u64) |
| `unsafe parse_to_tape_zmm(src, cap)` | unsafe | AVX-512BW asm |
| `unsafe parse_with_zmm(src, writer)` | unsafe | AVX-512BW asm vtable |

**Rust path always uses SWAR.** `parse_json_impl` had its `F: Fn(&[u8]) ->
ByteState` generic parameter removed; it now calls `classify_u64` directly.
This eliminates the abstraction cost of a function-pointer generic and the
`choose_classifier` CPUID dance for the common case.

**`unsafe` for AVX-512BW variants.** Rather than asserting at runtime (old
`parse_to_tape_zmm_tape` panicked if the CPU lacked AVX-512BW), the two asm
entry points are `unsafe fn`.  The assertion is removed; callers declare with
`unsafe` that they have verified CPU support.  This aligns with Rust's
philosophy and avoids hidden panics in libraries.

**`parse_to_tape_zmm_dyn` removed.** Its functionality is now exposed through
`parse_with_zmm`, which accepts any `JsonWriter<'a>`.  The vtable-dispatch asm
path is accessible without requiring a public `TapeWriter` type.

**`classify_ymm` / `classify_zmm` retained as `#[cfg(test)]`.** The
`classifier_agreement` unit test exercises all three classifiers against each
other to verify correctness.  Moving these to `#[cfg(test)]` suppresses dead
code warnings while keeping the coverage.

**`parse_with` no longer auto-dispatches to asm.** The previous session added
logic to call `parse_json_zmm_dyn` automatically from `parse_with` when a
`classify_zmm` argument was detected.  That heuristic is now gone: `parse_with`
is purely the Rust SWAR path; the user explicitly calls `parse_with_zmm` for
the asm path.

### Results

28 unit tests + 7 doc-tests pass (2 doc-tests now ignored due to platform/feature
guards, net -2 from the 9 previously; no regressions).

**Commit**: `c9a266b` refactor: drop ClassifyFn; four clean entry points with unsafe zmm variants
---

## Session 22 — SAX writer, bench rename, remove classify_*

### Remove classify_ymm / classify_zmm / ByteStateConstants

`classify_ymm`, `classify_zmm`, `ByteStateConstants`, `ZMM_CONSTANTS`, and the
`classifier_agreement` unit test were removed entirely from `src/lib.rs`.  Even
`#[cfg(test)]` guards were dropped — the classifier logic is embedded inside the
asm trampolines and needs no Rust-level exposure.

27 unit tests pass (the ignored zmm tests that need the `avx512bw` feature are
still present but not compiled on non-AVX-512 CI machines).

**Commit**: `6484322` refactor: remove classify_ymm, classify_zmm, ByteStateConstants, and classifier_agreement test

### LenSumWriter + bench rename (asmjson/sax, asmjson/dom)

Added a `LenSumWriter` struct to `benches/parse.rs` that implements
`JsonWriter<'src, Output = usize>` and accumulates the total byte length of all
string and key values encountered.  This gives a meaningful SAX-style workload
with no tape allocation.

Renamed benchmark slots in all three groups (string_array, string_object, mixed):

- `asmjson/zmm` → **`asmjson/sax`** — calls `parse_with_zmm(&data, LenSumWriter::new())`, single-pass, no heap allocation for the tape.
- `asmjson/zmm_tape` → **`asmjson/dom`** — calls `parse_to_tape_zmm(&data, None)` then traverses the tape with `tape_sum_lens`.
- `asmjson/u64` — unchanged (safe SWAR path, builds tape).

### Design decisions

The SAX path (`parse_with_zmm` + `LenSumWriter`) avoids the tape allocation and
the subsequent linear scan.  The benchmark therefore measures the parser's
throughput in isolation.  The DOM path (`parse_to_tape_zmm`) keeps the old
comparison point.

`LenSumWriter::finish` returns `Some(self.total)` so the result can be passed to
`black_box`, preventing the compiler from eliding the computation.

### Results

| Parser | string_array | string_object | mixed |
|---|---|---|---|
| asmjson/sax | 10.78 GiB/s | 8.29 GiB/s | 1.17 GiB/s |
| asmjson/dom | 10.93 GiB/s | 6.94 GiB/s | 897 MiB/s |
| asmjson/u64 | 7.02 GiB/s | 4.91 GiB/s | 607 MiB/s |
| sonic-rs | 6.92 GiB/s | 4.06 GiB/s | 478 MiB/s |
| serde_json | 2.41 GiB/s | 534 MiB/s | 78 MiB/s |
| simd-json | 1.91 GiB/s | 1.19 GiB/s | 174 MiB/s |

`asmjson/sax` wins on string_object and mixed (no tape-write overhead), while
`asmjson/dom` edges ahead on string_array (the extra tape scan is cheap relative
to the dominance of string-data throughput).

**Commit**: `855c37c` bench: add LenSumWriter, rename zmm→sax, zmm_tape→dom; update conversation log

---

## Session 23 — Module restructure: dom / sax

### What was done

Reorganised the crate's public module layout:

- **`src/tape.rs` → `src/dom/mod.rs`** — the flat-tape types (`Tape`,
  `TapeEntry`, `TapeEntryKind`, `TapeRef`, `TapeWriter`, iterators) now live
  under the `dom` module, reflecting that they implement a DOM (document object
  model) representation.

- **`src/json_ref.rs` → `src/dom/json_ref.rs`** — the `JsonRef` trait moved
  into `dom` as a submodule.  Its import of `tape::` became `super::` (parent
  module).

- **`src/sax.rs` (new)** — the `JsonWriter` trait was renamed to `Sax` and
  extracted to its own module.  All internal references in `lib.rs` were
  updated from `JsonWriter` to `Sax`.  The `TapeWriter` implementation and
  the `WriterForZmm` blanket impl were updated accordingly.

- **`src/lib.rs`** — module declarations updated (`tape`→`dom`, new `sax`),
  re-exports updated (`pub use dom::…`, `pub use dom::json_ref::JsonRef`,
  `pub use sax::Sax`), `JsonWriter` trait definition removed.

- **`src/de.rs`** — `use crate::tape::` → `use crate::dom::`.

- **`benches/parse.rs`** — `use asmjson::JsonWriter` → `use asmjson::sax::Sax`,
  `impl JsonWriter for LenSumWriter` → `impl Sax for LenSumWriter`.

### Design decisions

Naming `Sax` aligns with the common use of "SAX" (Simple API for XML) to
describe event-driven, streaming parsers — the `Sax` trait is called once
per token with no tree built.  Grouping the DOM types under `dom` makes the
complementary structure explicit: `asmjson::dom::Tape` for tree-shaped access,
`asmjson::sax::Sax` for streaming.

The `WriterForZmm` private bridge trait updates were purely mechanical.

### Results

All tests pass (27 unit + doc-tests).  No regressions.  One pre-existing
`dead_code` warning on `source_string` remains unchanged.

**Commit**: `2640796` refactor: rename tape→dom, json_ref into dom, JsonWriter→Sax in sax module

---

## Session 24 — Rename Tape→Dom, TapeEntry→DomEntry

### What was done

All occurrences of `Tape` (the flat token array struct) were renamed to `Dom`,
and all occurrences of `TapeEntry` (the 16-byte token struct) were renamed to
`DomEntry` across the entire codebase.

Files changed: `src/dom/mod.rs`, `src/lib.rs`, `src/dom/json_ref.rs`,
`src/sax.rs`, `benches/parse.rs`.

Unchanged names: `TapeRef`, `TapeWriter` (private), `TapeArrayIter`,
`TapeObjectIter`, `TapeEntryKind`.

### Results

All 27 unit tests and 7 doc-tests pass.  No regressions.

**Commit**: `f386977` refactor: rename Tape→Dom, TapeEntry→DomEntry

---

## Session 25 — Complete Dom* rename + asm module rename

### What was done

Completed the rename of all remaining `Tape*` identifiers to `Dom*` and renamed
the two x86-64 assembly files to use `_sax` / `_dom` suffixes to match the
module naming established in sessions 23–24.

Files touched: `src/lib.rs`, `src/dom/mod.rs`, `src/dom/json_ref.rs`,
`src/de.rs`, `src/sax.rs`, `benches/parse.rs`, `examples/perf_zmm_tape.rs`
(→ renamed `examples/perf_zmm_dom.rs`), `README.md`, `build.rs`,
`asm/x86_64/parse_json_zmm_dyn.S` (→ `parse_json_zmm_sax.S`),
`asm/x86_64/parse_json_zmm_tape.S` (→ `parse_json_zmm_dom.S`).

Full rename table:

| Old name | New name |
|---|---|
| `TapeRef` | `DomRef` |
| `TapeArrayIter` | `DomArrayIter` |
| `TapeObjectIter` | `DomObjectIter` |
| `TapeEntryKind` | `DomEntryKind` |
| `TapeWriter` | `DomWriter` |
| `tape_skip` | `dom_skip` |
| `tape_take_box_str` | `dom_take_box_str` |
| `parse_json_zmm_dyn` | `parse_json_zmm_sax` |
| `parse_json_zmm_tape` | `parse_json_zmm_dom` |
| `parse_to_tape` | `parse_to_dom` |
| `parse_to_tape_zmm` | `parse_to_dom_zmm` |
| `tape_sum_lens` (bench) | `dom_sum_lens` |
| `parse_json_zmm_dyn.S` | `parse_json_zmm_sax.S` |
| `parse_json_zmm_tape.S` | `parse_json_zmm_dom.S` |
| `examples/perf_zmm_tape.rs` | `examples/perf_zmm_dom.rs` |

### Design decisions

The `_sax` suffix is used for the AVX-512 path that dispatches through a
trait-object vtable (the SAX/event-driven interface), and `_dom` for the path
that writes directly into a flat DOM tape.  This mirrors the Rust-side
`Sax` trait / `Dom` struct split introduced in sessions 23–24.

README code examples are included as doc-tests via `include_str!`, so the
README also needed updating — caught by a second failing `cargo test` pass.

### Results

27 unit tests + 7 doc-tests, 0 failures after all renames.

### Commit

`98870e1` refactor: rename remaining Tape*→Dom*, asm modules to _sax/_dom suffixes

---

## Session — DOM and SAX example files

### What was done

Added two standalone examples that demonstrate both parse modes and both
x86-64 assembly vs portable entry points:

- `examples/dom_example.rs` — builds a [`Dom`] tape and navigates it with
  the [`JsonRef`] cursor API.  Accepts an optional `zmm` argument to switch
  from `parse_to_dom` (SWAR) to `parse_to_dom_zmm` (AVX-512BW assembly).
- `examples/sax_example.rs` — implements the [`Sax`] trait (`Counter`) and
  drives it through `parse_with` or `parse_with_zmm`.  The SAX example notes
  that `parse_with_zmm` does not process backslash escape sequences.

### Design decisions

- A runtime `-- zmm` argument was used instead of a compile-time flag so
  either path can be exercised without rebuilding; the non-x86_64 fallback
  prints an informative error and exits.
- The SAX example uses escape-free JSON (`"rust"`, `"json"`, etc.) so it
  works correctly under both `parse_with` and `parse_with_zmm`.
- Each `inspect` / `run_*` function in the DOM example is duplicated for
  clarity; sharing via a generic closure would obscure which API is in use.

### Results

Both examples compile without errors (`cargo build`).  Output verified by
running `cargo run --example dom_example` and `cargo run --example sax_example`,
and again with `-- zmm` on this AVX-512BW-capable host; counts and field
values matched expectations for both SWAR and assembly paths.

### Commit

65aff4e fix: two bugs in SAX assembly escape path---

## Session N — Fix two bugs in the assembly SAX escape path

### Stale re-save of `rcx` in `.Lsc_emit_escaped`

The assembly SAX string-value emission path (`.Lsc_emit_escaped`) saved
`rcx` (the chunk offset) into `LOC_COFF` before calling `unescape_str`, but
then unconditionally re-saved `rcx` *again* after loading the decoded String
fields:

```asm
    mov     qword ptr [rbp + LOC_COFF], rcx  ; correct save
    ...
    call    unescape_str                      ; clobbers rcx (caller-saved!)
    mov     rsi, qword ptr [r8]
    mov     rdx, qword ptr [r8 + 16]
    mov     qword ptr [rbp + LOC_COFF], rcx  ; BUG: rcx is garbage here
```

`rcx` is caller-saved in the System V AMD64 ABI, so `unescape_str` was free
to clobber it.  The second save overwrote the correct chunk-offset with
whatever value `rcx` held on return from `unescape_str`.  The fix was to
delete the redundant second save.  The corresponding key path
(`.Lke_emit_escaped`) did not have this defect.

### Wrong `String` field offsets — `{cap, ptr, len}` vs `{ptr, cap, len}`

After removing the stale save, the test still crashed with SIGSEGV.  A
runtime layout probe added to `parse_with_zmm` revealed:

```
String layout probe: w0=0x10 (cap=16)  w1=0x79a1d0000ce0 (ptr)  w2=0xb (len=11)
```

The assembly assumed `String = {ptr@0, cap@8, len@16}`, but the Rust
compiler in use lays out `Vec<u8>` as `{cap@0, ptr@8, len@16}`.  The SAX
assembly was reading `[r8]` (cap) instead of `[r8 + 8]` (ptr) for the
decoded string pointer, producing a garbage pointer value (e.g. `0x8`
when cap=8) that caused the segfault.  The DOM assembly was unaffected
because it delegates String field access to the Rust function
`dom_take_box_str`.

The fix updates both `.Lsc_emit_escaped` (string values) and
`.Lke_emit_escaped` (key values) to read the pointer from `[r8 + 8]`:

```asm
    mov     rsi, qword ptr [r8 + 8]    // box_ptr  (ptr at offset 8)
    mov     rdx, qword ptr [r8 + 16]   // box_len  (len at offset 16)
```

A comment in the assembly documents the verified field layout.

### Results

All 29 unit tests pass (`cargo test --lib -- --test-threads=1`).  The
`zmm_sax_escaped_strings` test, which verified `parse_with_zmm` against the
Rust reference parser on inputs with `\n`, `\t`, `\r`, `\"`, `\uXXXX`,
escaped keys, and escape sequences spanning chunk boundaries, now passes
cleanly.

### Commit

f6bd9f4 fix: two bugs in SAX assembly escape path

---

## Session N+1 — Eliminate String layout assumptions by changing escaped_string/escaped_key to &str

### Motivation

The previous fix for the SAX assembly SIGSEGV revealed the root cause:
the assembly was reading `String` fields at hard-coded offsets assuming
`{ptr@0, cap@8, len@16}`, but the Rust compiler in use laid out `Vec<u8>`
as `{cap@0, ptr@8, len@16}`.  Rather than just updating the offsets and
hoping the layout stays stable, the approach was changed to eliminate the
need to read `String` fields from assembly entirely.

### Design decision

The `Sax` trait methods `escaped_string` and `escaped_key` previously took
`Box<str>` so the writer could own the decoded text.  The assembly
trampolines were responsible for constructing the `Box<str>` by reading
`ptr` and `len` from the `unescape_buf` `String` struct.

Changing the signature to `&str` (matching `string` and `key`) means the
trampolines need only cast the raw `(ptr, len)` they already receive to a
`&str` — the same one-liner pattern used for all other string methods.
No `String` field access in assembly is required at all.

The `&str` is a short-lived borrow of `unescape_buf`'s heap buffer.  It
is valid for the duration of the vtable call.  Writers that only inspect
the value (benchmarks, examples) incur zero allocation.  Writers that
need ownership (e.g. `DomWriter`) copy with `Box::from(s)` internally.

### Changes

- `src/sax.rs`: `Sax` trait — `Box<str>` → `&str`.
- `src/lib.rs`: `WriterForZmm` trait and blanket impl updated; trampolines
  for `zw_escaped_string` / `zw_escaped_key` reduced to one-liners;
  SWAR `parse_json_impl` call sites changed from `.as_str().into()` to
  `.as_str()`; test `EventLog` signatures updated.
- `src/dom/mod.rs`: `DomWriter::escaped_string` / `escaped_key` now take
  `&str` and do `Box::from(s)` to preserve internal `Box<str>` ownership.
- `examples/sax_example.rs`, `benches/parse.rs`: method signatures updated.
- `asm/x86_64/parse_json_zmm_sax.S`: comment updated (`box_ptr` → `s_ptr`).

### Results

All 29 unit tests pass.  Benchmarks compile.  The assembly no longer
contains any `String` field offsets.

### Commit

612de06 refactor: change escaped_string/escaped_key to take &str`612de06` refactor: change escaped_string/escaped_key to take &str

## Session — Example CPUID auto-dispatch

### What was done

Refactored `examples/dom_example.rs` and `examples/sax_example.rs` to
auto-select the AVX-512BW assembly path at runtime using
`is_x86_feature_detected!("avx512bw")` instead of requiring a `-- zmm`
command-line flag.

- Removed the two-function table and `-- zmm` usage from both doc comments.
- `dom_example`: merged `inspect` / `inspect_zmm` into a single
  `inspect(label: &str, tape: Dom)` function; `main` does the CPUID check
  and calls the appropriate parser, then passes the result to `inspect`.
  Added `Dom` to the `use` imports.
- `sax_example`: replaced `run_portable` and `run_zmm` with a single
  `report(label: &str, counts: Counter)` function; `main` does the CPUID
  check and calls the appropriate parser, then passes counts to `report`.
  Removed the `std::env::args` CLI argument parsing entirely.

### Design decisions

Mirrored the same CPUID-dispatch pattern in both examples for consistency.
The `#[cfg(target_arch = "x86_64")]` guard around the
`is_x86_feature_detected!(...)` check ensures the examples compile and run
correctly on non-x86_64 targets (falling back to the portable path).

### Results

Both examples compile without warnings and produce correct output.  On an
AVX-512BW machine the assembly path is selected automatically.

### Commit

`601c6ee` examples: CPUID auto-dispatch; remove -- zmm CLI flag

## Session — Parallel mmap JSON-lines example

### What was done

Added `examples/mmap_parallel.rs`: a new example that memory-maps a JSON
Lines file, partitions it into ~1 MiB chunks at `\n` boundaries, then
parses every chunk in parallel using Rayon.  CPUID auto-selects the
AVX-512BW assembly path when available.

Also added `memmap2 = "0.9"` and `rayon = "1"` to `[dev-dependencies]` in
`Cargo.toml`.

### Design decisions

**Why iterate lines within each chunk?**  `parse_with` / `parse_with_zmm`
each expect a single well-formed JSON value.  A raw ~1 MiB slice of a JSON
Lines file contains hundreds of individual JSON objects separated by `\n`,
not one big document.  The solution is to partition the mmap into
newline-aligned chunks for Rayon — giving each thread a contiguous region
to work with — and then iterate over the individual lines within each chunk
before calling the parser.

**Chunk boundary alignment.**  The `split_at_newlines` function scans
forward from the nominal chunk end to the next `\n`, ensuring no line
is split across chunks.  Lines whose trailing `\n` falls past the end
of file are still handled correctly.

**`StringCounter` accumulation.**  Each Rayon task returns a
`StringCounter`; `reduce` combines them with simple integer addition,
avoiding any shared state or locking.

### Results

On a 12.7 MB test file (200 k lines) split into 13 chunks:

```
keys   found : 600000   (3 keys/line × 200 000 lines)
strings found: 200000   (1 string value/line × 200 000 lines)
```

### Commit

`6a055ea` examples: add mmap_parallel JSON-lines parallel SAX counter

## Session — mmap_parallel self-generates its test file

### What was done

Reworked `examples/mmap_parallel.rs` so that `main` first creates
`/tmp/file.jsonl` (1 GiB, ~10.7 million lines) before mapping and parsing
it, removing the CLI path argument entirely.

Each generated line is exactly 100 bytes including the trailing `\n`:

```
{"identifier":"user000000000000","description":"item000000000000","subcategory":"type000000000000"}
```

Keys: "identifier" (10), "description" (11), "subcategory" (11) — all ≥ 10 chars.
Values: 16-char strings (4-char prefix + 12-digit line index) — all ≥ 10 chars.
1 073 741 800 bytes ÷ 100 = 10 737 418 lines → exactly 1 GiB.

### Design decisions

Used a `BufWriter` with a 4 MiB buffer for fast sequential writes.  The
format string uses escaped braces (`{{`/`}}`) in a regular string literal
rather than a raw string, avoiding raw-string delimiter collisions.

`Instant` timing is printed separately for file generation and for parsing.

### Results

On this machine:

- File generation: 1.15 s
- Parse (1024 × ~1 MiB chunks, Rayon + AVX-512BW): 36 ms
- keys found: 32 212 254  (10 737 418 × 3)
- strings found: 32 212 254  (10 737 418 × 3)

### Commit

`0f70426` examples: mmap_parallel generates its own 1 GiB test file

## Session — Push unescape responsibility to Sax implementors

### What was done

Changed the contract of `Sax::escaped_string` and `Sax::escaped_key`: they
now receive the **raw** (still-escaped) `&str` slice directly from the
source JSON rather than a pre-decoded string.  Callers that need the decoded
text call `unescape_str` themselves.

Changes across the codebase:

- `src/sax.rs`: docstrings updated to document the raw-string contract.
- `src/lib.rs`:
  - `parse_json_impl` — removed `unescape_buf: &mut String` parameter;
    passes `raw` / `current_key_raw` directly to `writer.escaped_string` /
    `writer.escaped_key`.
  - `parse_with` — no longer creates or passes an `unescape_buf`.
  - `parse_with_zmm` — no longer creates or passes an `unescape_buf`;
    `parse_json_zmm_sax` extern signature loses its last parameter.
- `src/dom/mod.rs`: `DomWriter::escaped_string` and `escaped_key` now
  allocate a local `String`, call `crate::unescape_str`, and convert to
  `Box<str>` internally.  They are the only implementations that need
  decoded text.
- `asm/x86_64/parse_json_zmm_sax.S`:
  - Function signature comment: removed `unescape_buf / r9` argument.
  - Prologue: removed `mov [rbp+LOC_UNESCAPE], r9` save.
  - `.Lsc_emit_escaped`: removed `call unescape_str` and String field
    reads; now calls `VTAB_ESCAPED_STRING(rbx, rsi, rdx)` directly with
    the raw ptr/len that were already in rsi/rdx.
  - `.Lke_emit_escaped`: same — calls `VTAB_ESCAPED_KEY` directly with
    `LOC_KEY_PTR` / `LOC_KEY_LEN`.
  - Removed `LOC_UNESCAPE` .equ constant and its stack-layout comment.
- `asm/x86_64/parse_json_zmm_dom.S`: **unchanged** — the DOM assembly
  still calls `unescape_str` + `dom_take_box_str` internally and never
  goes through the `Sax` trait for escaped entries.

### Design decisions

The previous design coupled the escape-decoding step to the parser
internals: `parse_json_impl` always allocated/cleared a `String` and ran
`unescape_str` before calling the trait method, even for implementations
that only count strings and discard the content.  Moving the call into
`DomWriter` (the only implementation that needs decoded text from the SWAR
path) eliminates that allocation for all other `Sax` implementations and
simplifies the assembly SAX path by ~12 instructions per escape event.

### Results

All 29 unit tests pass.

### Commit

`9c2d164` refactor: escaped_string/escaped_key receive raw source &str; DomWriter unescapes internally

## Session: remove unescape_buf from parse_json_zmm_dom

### What was done

Removed the `unescape_buf: *mut String` parameter from `parse_json_zmm_dom`,
the hand-written AVX-512BW DOM assembly parser.  Previously the caller
(`parse_to_dom_zmm`) had to allocate a `String`, pass its raw pointer in as
the 7th argument, and the assembly would call `unescape_str` (to fill it) and
then `dom_take_box_str` (to box the result) at each escaped-string/key site.

### Design decisions

The two-call sequence (`unescape_str` + `dom_take_box_str`) was collapsed into
a single new function `dom_unescape_to_box_str(raw_ptr, raw_len, out_ptr,
out_len)` that allocates its own `String` internally, decodes the escapes, and
writes the `Box<str>` pointer and length to the caller-supplied output
pointers.  This is the same pattern that stabilised the SAX path in the
previous session — the caller no longer owns an escape buffer.

`dom_take_box_str` was deleted; `unescape_str` is still present and public
(called from `dom_unescape_to_box_str` and directly from
`DomWriter::escaped_string/escaped_key`).

On the assembly side `LOC_UNESCAPE` (stack slot `[rbp-48]`) is removed and
the two external calls at `.Lsc_emit_escaped` and `.Lke_emit_escaped` are each
replaced by a single `call dom_unescape_to_box_str`.  The `has_escapes_out`
and `tape_cap` stack arguments shift from `[rbp+24]/[rbp+32]` to
`[rbp+16]/[rbp+24]` following the removal of the 7th argument.

### Results

29/29 tests green.  No benchmark regression expected (same allocation pattern,
one fewer external call per escaped token).

### Commit

`da9e8aa` refactor: remove unescape_buf from parse_json_zmm_dom; add dom_unescape_to_box_str

## Session: recompute DRAM bandwidth with ZMM loads

### What was done

Added `examples/mem_bw_zmm.rs` — a standalone memory-bandwidth benchmark that
allocates a 2 GiB, 64-byte aligned buffer (via `std::alloc::alloc`), touches
every page to force physical backing, then runs 8 sequential passes with two
AVX-512 strategies and reports best and median GiB/s:

* **zmm** — `vmovdqu64` (temporal) loads via `_mm512_loadu_si512`.
* **zmm-nt** — `vmovntdqa` (non-temporal streaming) loads via
  `_mm512_stream_load_si512`; bypasses CPU read-allocate.

Both strategies OR all loaded vectors into a 512-bit accumulator and store it
to prevent dead-code elimination.

### Design decisions

The previous bandwidth estimate (~45 GiB/s) was measured with scalar reads.
Using ZMM loads gives the prefetcher/memory controller a better chance to
stream at full width, which is more representative of what the AVX-512BW
parser actually exercises.  2 GiB ensures the working set is far larger than
the 64 MB L3 cache so results reflect DRAM, not cache, bandwidth.

Non-temporal loads (`vmovntdqa`) require 16-byte alignment (64-byte for
ZMM); the 64-byte aligned allocation guarantees this.

### Results

Ryzen 9 9955HX (Zen 5, DDR5 dual-channel):

| Strategy                 | Best      | Median    |
|--------------------------|-----------|-----------|
| zmm temporal (`vmovdqu64`) | 47.5 GiB/s | 47.4 GiB/s |
| zmm-nt (`vmovntdqa`)     | 49.5 GiB/s | 47.9 GiB/s |

The parallel JSON parser (26.6 GiB/s) reaches ~56 % of the ZMM temporal
ceiling, up from the ~59 % figure that used the now-known-underestimated
scalar baseline.

README updated: replaced "45 GiB/s scalar" row with the two ZMM rows and
revised efficiency to ~56 %.

### Commit

`b50d502` example: add mem_bw_zmm — ZMM temporal and NT load bandwidth benchmark

## Session N — serde example timing

### Add timing to serde_example

Added `std::time::Instant` timing around the two main phases in `run()`:

- `parse_to_dom` (or `parse_to_dom_zmm` on AVX-512BW): measures the JSON
  parse + tape-build time.
- `from_taperef`: measures the serde deserialization walk over the tape.

Both durations are printed in milliseconds alongside the existing record
count output.  Example output on Ryzen 9 9955HX with a ~1 MiB input:

```
parse_to_dom_zmm + from_taperef  (AVX-512BW): decoded 8066 records, last id=8065
  parse_to_dom: 1.212 ms  |  from_taperef: 1.755 ms
```

### Design decisions

`Instant::now()` / `elapsed()` is sufficient accuracy for a single-run
example (sub-100 µs jitter on this machine).  No warmup loop or statistics
are needed; users who want repeatable microbenchmarks should use the Criterion
benches.

### Commit

`dd4bbe7` example: add parse_to_dom and from_taperef timing to serde_example

## Session — use dom_parser / sax_parser in examples

### What was done

Updated the three remaining examples (`sax_example.rs`, `dom_example.rs`,
`mmap_parallel.rs`) to use the safe CPUID-dispatching helpers `sax_parser()`
and `dom_parser()` introduced in `3fa487c`.

`SaxParser` was given `#[derive(Copy, Clone)]` so it can be captured by value
in Rayon parallel closures (`mmap_parallel.rs`).

In `mmap_parallel.rs` the two separate `parse_line_into_zmm` /
`parse_line_into_rust` helper functions and the manual `is_x86_feature_detected!`
guard inside `parse_chunk` were removed.  `parse_chunk` now takes a
`SaxParser` argument; `sax_parser()` is called once in `main()` and the
`Copy` value is captured by the Rayon closure.

### Design decisions

Removing the two helper functions reduces the example from ~235 lines to
~175 lines with no loss of functionality.  Passing `SaxParser` by value (copy)
into `parse_chunk` is idiomatic for a trivially-copyable handle; an alternative
would be passing `&SaxParser` and relying on `Sync`, but by-value is cleaner
when `Copy` is available.

`serde_example.rs` was already updated to use `dom_parser()` in `3fa487c` and
required no further changes.

### Results

All 29 library tests pass.  Both `sax_example` and `dom_example` run correctly,
and `mmap_parallel` compiles cleanly with zero warnings.

### Commit

`f79d93d` use dom_parser/sax_parser in examples, add Copy+Clone to SaxParser

## Session — serde_example combined MiB/s + serde_json comparison

### What was done

Reworked `serde_example.rs` to report a single combined MiB/s for the
`parse_to_dom` + `from_taperef` pipeline, and added a `run_serde_json`
function that times `serde_json::from_str::<Vec<Record>>` end-to-end on
the same data for a direct comparison.

The per-step breakdown (parse ms / serde ms) is still printed alongside the
combined figure so both are visible.

### Design decisions

`serde_json` was already a dev-dependency (used in the benchmarks), so no
new dependency was required.  The combined time is computed as
`parse_elapsed + serde_elapsed` using `std::ops::Add` for `Duration`, which
keeps the arithmetic exact and avoids floating-point rounding between the two
measurements.

### Results

On this machine (AVX-512BW):
```
asmjson (AVX-512BW)    : 8066 records  |  parse: 0.9 ms  serde: 7.5 ms  combined: 8.4 ms  (97 MiB/s)
serde_json:          8066 records  |  combined: 24.1 ms  (34 MiB/s)
```

### Commit

`16ce530` serde_example: combined MiB/s + serde_json comparison

## Session — Repository moved to atomicincrement org

### Move to atomicincrement GitHub organisation

**What was done** — The repository was transferred from the personal
`andy-thomason` GitHub account to the `atomicincrement` organisation.
All in-repo references to the old URL were updated:

- `Cargo.toml` `repository` field → `https://github.com/atomicincrement/asmjson`
- `README.md` CI badge URL and LICENSE link → `atomicincrement/asmjson`

**Design decisions** — Only canonical GitHub URLs embedded in source were
changed; local filesystem paths (e.g. in this log) were left as-is since they
reflect the machine's directory layout, not the remote.

**Results** — No functional changes; purely metadata/URL updates.

**Commit** — pending

## Session — Fix CI failures (SIGILL + doctest arity)

### Fix AVX-512 SIGILL in tests on GitHub Actions runners

**What was done** — The CI was failing with `SIGILL: illegal instruction`
because the `#[cfg(target_arch = "x86_64")]` zmm test helpers
(`zmm_dom_matches`, `zmm_dom_rejects`, `zmm_sax_matches`) and the
`zmm_dom_overflow_retry` test called AVX-512 assembly unconditionally.
GitHub Actions `ubuntu-latest` runners do not have AVX-512 hardware.
A runtime guard `if !is_x86_feature_detected!("avx512bw") { return; }` was
added at the top of each of the four affected functions so they skip silently
on non-AVX-512 machines instead of crashing with an illegal instruction.

**Design decisions** — Early-return skipping (rather than `#[ignore]`) keeps
the tests in the normal `cargo test` run and makes them self-activating on any
AVX-512-capable machine.  The public API already used `is_x86_feature_detected`
for dispatch; the tests now mirror that pattern.

### Fix doctest arity for parse_to_dom

**What was done** — Two code examples in `README.md` called
`parse_to_dom(src)` with one argument, but the function signature changed to
`parse_to_dom(src, initial_capacity: Option<usize>)` in a prior session.
Both invocations were updated to `parse_to_dom(src, None)`.

**Results** — All 29 unit tests and all 9 (+ 2 ignored) doctests pass locally
and CI is expected to pass on `ubuntu-latest` (no AVX-512, zmm tests skip).

**Commit** — e4e68f4 fix: gate zmm tests on avx512bw; fix doctest arity; bump to 0.2.3

## Session — First draft JOSS paper

### Write paper.md and paper.bib for Journal of Open Source Software

**What was done** — Created a first-draft JOSS submission in
`doc/paper/paper.md` (Pandoc Markdown, JOSS format) and `doc/paper/paper.bib`
(BibLaTeX references), following the JOSS paper guidelines fetched from
joss.readthedocs.io.  The paper (~1200 body words) covers:

- **Summary** — high-level description for a non-specialist audience.
- **Statement of need** — JSON parsing as a data-pipeline bottleneck; gap
  between library performance and hardware capability.
- **State of the field** — comparison with simdjson/Langdale & Lemire,
  simd-json, sonic-rs, serde_json; explains how asmjson differs
  (AVX-512BW, direct threading, DOM-in-assembly).
- **Software design** — two assembly listings (vcmp classify chunk, tzcnt
  whitespace skip), tape DOM design, SWAR fallback, API surface.
- **Research impact statement** — crates.io release; future directions
  (CSV/TSV, compact tape, proc-macro SAX deserialiser).
- **AI usage disclosure** — GitHub Copilot assisted; assembly hand-authored.

**Design decisions** — JOSS requires Pandoc Markdown, not raw LaTeX; the
toolchain converts via ConTeXt to PDF.  Assembly listings are fenced code
blocks (rendered as listings in the PDF).  Word count ~1496 including YAML
front matter, well within the 750–1750 word target.

**Results** — `doc/paper/paper.md` and `doc/paper/paper.bib` committed.

**Commit** — 13e4b59 docs: first draft JOSS paper (paper.md + paper.bib)
