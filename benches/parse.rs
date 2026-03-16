#[cfg(feature = "stats")]
use asmjson::choose_classifier;
#[cfg(feature = "stats")]
use asmjson::parse_to_tape as parse_json;
#[cfg(target_arch = "x86_64")]
use asmjson::parse_to_tape_zmm_dyn;
#[cfg(target_arch = "x86_64")]
use asmjson::parse_to_tape_zmm_tape;
#[cfg(feature = "stats")]
use asmjson::stats;
use asmjson::{TapeEntryKind, classify_u64, classify_zmm, parse_to_tape};
use criterion::{Criterion, Throughput, criterion_group, criterion_main};

// ---------------------------------------------------------------------------
// Post-parse traversal helpers
// ---------------------------------------------------------------------------

/// Sum the lengths of every String and Key entry in a tape.
#[inline]
fn tape_sum_lens(tape: &asmjson::Tape<'_>) -> usize {
    tape.entries
        .iter()
        .map(|e| match e.kind() {
            TapeEntryKind::String | TapeEntryKind::Key => e.as_string().map_or(0, |s| s.len()),
            _ => 0,
        })
        .sum()
}

/// Recursively sum the lengths of every string value and object key in a
/// serde_json Value tree.
fn serde_sum_lens(v: &serde_json::Value) -> usize {
    match v {
        serde_json::Value::String(s) => s.len(),
        serde_json::Value::Object(map) => map
            .iter()
            .map(|(k, val)| k.len() + serde_sum_lens(val))
            .sum(),
        serde_json::Value::Array(arr) => arr.iter().map(serde_sum_lens).sum(),
        _ => 0,
    }
}

/// Recursively sum the lengths of every string value and object key in a
/// sonic-rs Value tree.
fn sonic_sum_lens(v: &sonic_rs::Value) -> usize {
    use sonic_rs::{JsonContainerTrait, JsonValueTrait};
    if let Some(s) = v.as_str() {
        return s.len();
    }
    if let Some(obj) = v.as_object() {
        return obj
            .iter()
            .map(|(k, val): (&str, &sonic_rs::Value)| k.len() + sonic_sum_lens(val))
            .sum();
    }
    if let Some(arr) = v.as_array() {
        return arr
            .iter()
            .map(|val: &sonic_rs::Value| sonic_sum_lens(val))
            .sum();
    }
    0
}

// ---------------------------------------------------------------------------
// Stats helper — compiled in only with --features stats
// ---------------------------------------------------------------------------

#[cfg(feature = "stats")]
fn print_stats(label: &str, data: &str) {
    stats::reset();
    let _ = parse_json(data, choose_classifier());
    let s = stats::get();
    let total = s.value_whitespace
        + s.string_chars
        + s.string_escape
        + s.key_chars
        + s.key_escape
        + s.key_end
        + s.after_colon
        + s.atom_chars
        + s.object_start
        + s.array_start
        + s.after_value;
    eprintln!(
        "\n=== State entries: {label} ({} bytes, {total} total dispatches) ===",
        data.len()
    );
    let mut rows = vec![
        ("ValueWhitespace", s.value_whitespace),
        ("StringChars", s.string_chars),
        ("StringEscape", s.string_escape),
        ("KeyChars", s.key_chars),
        ("KeyEscape", s.key_escape),
        ("KeyEnd", s.key_end),
        ("AfterColon", s.after_colon),
        ("AtomChars", s.atom_chars),
        ("ObjectStart", s.object_start),
        ("ArrayStart", s.array_start),
        ("AfterValue", s.after_value),
    ];
    rows.sort_by(|a, b| b.1.cmp(&a.1));
    for (name, count) in rows {
        if count > 0 {
            eprintln!(
                "  {name:<20} {count:>12}  ({:.1}%)",
                count as f64 / total as f64 * 100.0
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Data generators
// ---------------------------------------------------------------------------

/// ~10 MiB JSON array of strings.
///
/// Each element is a 95-character ASCII string (printable, no quotes/backslashes).
/// Enough elements are emitted to reach the target size.
fn gen_string_array(target_bytes: usize) -> String {
    // Element template: "abcdefghij..." (95 visible ASCII chars), plus `,"` overhead.
    let value: String = (b'!'..=b'~') // 94 printable ASCII chars
        .filter(|&b| b != b'"' && b != b'\\')
        .map(|b| b as char)
        .cycle()
        .take(95)
        .collect();
    let element = format!(r#""{}""#, value); // ~97 bytes each
    let elements_needed = target_bytes / (element.len() + 1) + 1;

    let mut out = String::with_capacity(target_bytes + 64);
    out.push('[');
    for i in 0..elements_needed {
        if i > 0 {
            out.push(',');
        }
        out.push_str(&element);
    }
    out.push(']');
    out
}

/// ~10 MiB JSON object with string keys and string values.
///
/// Keys are "key00000" … "key99999"; values are 85-char ASCII strings.
fn gen_string_object(target_bytes: usize) -> String {
    let value: String = (b'!'..=b'~')
        .filter(|&b| b != b'"' && b != b'\\')
        .map(|b| b as char)
        .cycle()
        .take(85)
        .collect();
    // "keyNNNNN":"<85 chars>"  ≈ 102 bytes per member
    let members_needed = target_bytes / 102 + 1;

    let mut out = String::with_capacity(target_bytes + 64);
    out.push('{');
    for i in 0..members_needed {
        if i > 0 {
            out.push(',');
        }
        out.push_str(&format!(r#""key{:05}":"{}""#, i % 100_000, value));
    }
    out.push('}');
    out
}

/// ~10 MiB nested mixed JSON.
///
/// Top-level array of objects; each object contains:
///   "id": <number>, "name": <string>, "active": <bool>,
///   "score": <null|number>, "tags": [<string>, <string>],
///   "meta": { "x": <number>, "y": <number> }
fn gen_mixed(target_bytes: usize) -> String {
    let tag_a = "alpha";
    let tag_b = "beta";
    // One record template (printed below) is ~130 bytes.
    let record_size = 130usize;
    let records_needed = target_bytes / record_size + 1;

    let mut out = String::with_capacity(target_bytes + 64);
    out.push('[');
    for i in 0..records_needed {
        if i > 0 {
            out.push(',');
        }
        let active = if i % 2 == 0 { "true" } else { "false" };
        let score = if i % 3 == 0 {
            "null".to_string()
        } else {
            format!("{}", i / 2)
        };
        out.push_str(&format!(
            r#"{{"id":{i},"name":"item{i}","active":{active},"score":{score},"tags":["{tag_a}","{tag_b}"],"meta":{{"x":{x},"y":{y}}}}}"#,
            i = i,
            active = active,
            score = score,
            tag_a = tag_a,
            tag_b = tag_b,
            x = i % 1000,
            y = (i * 7) % 1000,
        ));
    }
    out.push(']');
    out
}

// ---------------------------------------------------------------------------
// Benchmarks
// ---------------------------------------------------------------------------

const TARGET: usize = 10 * 1024 * 1024; // 10 MiB

fn bench_string_array(c: &mut Criterion) {
    let data = gen_string_array(TARGET);
    #[cfg(feature = "stats")]
    print_stats("string_array", &data);
    let mut group = c.benchmark_group("string_array");
    group.throughput(Throughput::Bytes(data.len() as u64));
    group.bench_function("asmjson/zmm", |b| {
        b.iter(|| {
            let tape = parse_to_tape(&data, classify_zmm).unwrap();
            std::hint::black_box(tape_sum_lens(&tape))
        });
    });
    #[cfg(target_arch = "x86_64")]
    group.bench_function("asmjson/zmm_dyn", |b| {
        b.iter(|| {
            let tape = parse_to_tape_zmm_dyn(&data).unwrap();
            std::hint::black_box(tape_sum_lens(&tape))
        });
    });
    #[cfg(target_arch = "x86_64")]
    group.bench_function("asmjson/zmm_tape", |b| {
        b.iter(|| {
            let tape = parse_to_tape_zmm_tape(&data).unwrap();
            std::hint::black_box(tape_sum_lens(&tape))
        });
    });
    group.bench_function("asmjson/u64", |b| {
        b.iter(|| {
            let tape = parse_to_tape(&data, classify_u64).unwrap();
            std::hint::black_box(tape_sum_lens(&tape))
        });
    });
    group.bench_function("sonic-rs", |b| {
        b.iter(|| {
            let v = sonic_rs::from_str::<sonic_rs::Value>(&data).unwrap();
            std::hint::black_box(sonic_sum_lens(&v))
        });
    });
    group.bench_function("serde_json", |b| {
        b.iter(|| {
            let v = serde_json::from_str::<serde_json::Value>(&data).unwrap();
            std::hint::black_box(serde_sum_lens(&v))
        });
    });
    group.finish();
}

fn bench_string_object(c: &mut Criterion) {
    let data = gen_string_object(TARGET);
    #[cfg(feature = "stats")]
    print_stats("string_object", &data);
    let mut group = c.benchmark_group("string_object");
    group.throughput(Throughput::Bytes(data.len() as u64));
    group.bench_function("asmjson/zmm", |b| {
        b.iter(|| {
            let tape = parse_to_tape(&data, classify_zmm).unwrap();
            std::hint::black_box(tape_sum_lens(&tape))
        });
    });
    #[cfg(target_arch = "x86_64")]
    group.bench_function("asmjson/zmm_dyn", |b| {
        b.iter(|| {
            let tape = parse_to_tape_zmm_dyn(&data).unwrap();
            std::hint::black_box(tape_sum_lens(&tape))
        });
    });
    #[cfg(target_arch = "x86_64")]
    group.bench_function("asmjson/zmm_tape", |b| {
        b.iter(|| {
            let tape = parse_to_tape_zmm_tape(&data).unwrap();
            std::hint::black_box(tape_sum_lens(&tape))
        });
    });
    group.bench_function("asmjson/u64", |b| {
        b.iter(|| {
            let tape = parse_to_tape(&data, classify_u64).unwrap();
            std::hint::black_box(tape_sum_lens(&tape))
        });
    });
    group.bench_function("sonic-rs", |b| {
        b.iter(|| {
            let v = sonic_rs::from_str::<sonic_rs::Value>(&data).unwrap();
            std::hint::black_box(sonic_sum_lens(&v))
        });
    });
    group.bench_function("serde_json", |b| {
        b.iter(|| {
            let v = serde_json::from_str::<serde_json::Value>(&data).unwrap();
            std::hint::black_box(serde_sum_lens(&v))
        });
    });
    group.finish();
}

fn bench_mixed(c: &mut Criterion) {
    let data = gen_mixed(TARGET);
    #[cfg(feature = "stats")]
    print_stats("mixed", &data);
    let mut group = c.benchmark_group("mixed");
    group.throughput(Throughput::Bytes(data.len() as u64));
    group.bench_function("asmjson/zmm", |b| {
        b.iter(|| {
            let tape = parse_to_tape(&data, classify_zmm).unwrap();
            std::hint::black_box(tape_sum_lens(&tape))
        });
    });
    #[cfg(target_arch = "x86_64")]
    group.bench_function("asmjson/zmm_dyn", |b| {
        b.iter(|| {
            let tape = parse_to_tape_zmm_dyn(&data).unwrap();
            std::hint::black_box(tape_sum_lens(&tape))
        });
    });
    #[cfg(target_arch = "x86_64")]
    group.bench_function("asmjson/zmm_tape", |b| {
        b.iter(|| {
            let tape = parse_to_tape_zmm_tape(&data).unwrap();
            std::hint::black_box(tape_sum_lens(&tape))
        });
    });
    group.bench_function("asmjson/u64", |b| {
        b.iter(|| {
            let tape = parse_to_tape(&data, classify_u64).unwrap();
            std::hint::black_box(tape_sum_lens(&tape))
        });
    });
    group.bench_function("sonic-rs", |b| {
        b.iter(|| {
            let v = sonic_rs::from_str::<sonic_rs::Value>(&data).unwrap();
            std::hint::black_box(sonic_sum_lens(&v))
        });
    });
    group.bench_function("serde_json", |b| {
        b.iter(|| {
            let v = serde_json::from_str::<serde_json::Value>(&data).unwrap();
            std::hint::black_box(serde_sum_lens(&v))
        });
    });
    group.finish();
}

criterion_group!(
    benches,
    bench_string_array,
    bench_string_object,
    bench_mixed
);
criterion_main!(benches);
