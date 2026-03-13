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
