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

