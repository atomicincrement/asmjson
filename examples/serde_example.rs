//! Serde deserialization example.
//!
//! Generates the same "mixed" JSON format used in the benchmarks, then
//! deserialises the array into a `Vec<Record>` using asmjson's serde
//! integration.
//!
//! CPUID auto-selects the AVX-512BW assembly path when available.
//!
//! ```sh
//! cargo run --features serde --example serde_example
//! ```

use asmjson::de::from_taperef;
use asmjson::parse_to_dom;
#[cfg(target_arch = "x86_64")]
use asmjson::parse_to_dom_zmm;
use serde::Deserialize;

// ---------------------------------------------------------------------------
// Data model matching the "mixed" generator
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct Meta {
    x: u64,
    y: u64,
}

#[derive(Debug, Deserialize)]
struct Record {
    id: u64,
    name: String,
    active: bool,
    /// `null` when `id % 3 == 0`.
    score: Option<f64>,
    tags: Vec<String>,
    meta: Meta,
}

// ---------------------------------------------------------------------------
// Data generator (same as benches/parse.rs gen_mixed)
// ---------------------------------------------------------------------------

fn gen_mixed(target_bytes: usize) -> String {
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
            r#"{{"id":{i},"name":"item{i}","active":{active},"score":{score},"tags":["alpha","beta"],"meta":{{"x":{x},"y":{y}}}}}"#,
            x = i % 1000,
            y = (i * 7) % 1000,
        ));
    }
    out.push(']');
    out
}

// ---------------------------------------------------------------------------
// Parse + deserialise
// ---------------------------------------------------------------------------

fn run(label: &str, data: &str) {
    // Parse to DOM.
    let t0 = std::time::Instant::now();
    #[cfg(target_arch = "x86_64")]
    let tape = if is_x86_feature_detected!("avx512bw") {
        // SAFETY: CPUID confirmed AVX-512BW.
        unsafe { parse_to_dom_zmm(data, None) }.expect("parse failed")
    } else {
        parse_to_dom(data).expect("parse failed")
    };
    #[cfg(not(target_arch = "x86_64"))]
    let tape = parse_to_dom(data).expect("parse failed");
    let parse_elapsed = t0.elapsed();

    // Deserialise the root array.
    let t1 = std::time::Instant::now();
    let root = tape.root().expect("empty tape");
    let records: Vec<Record> = from_taperef(root).expect("deserialise failed");
    let serde_elapsed = t1.elapsed();

    // Spot-check a few records.
    assert_eq!(records[0].id, 0);
    assert_eq!(records[0].name, "item0");
    assert!(records[0].active);
    assert_eq!(records[0].score, None); // id % 3 == 0 → null
    assert_eq!(records[0].tags, ["alpha", "beta"]);
    assert_eq!(records[0].meta.x, 0);
    assert_eq!(records[0].meta.y, 0);

    assert_eq!(records[1].id, 1);
    assert!(!records[1].active);
    assert_eq!(records[1].score, Some(0.0)); // i/2 = 0

    let bytes = data.len() as f64;
    let gib = (1u64 << 30) as f64;
    let parse_gibs = bytes / (parse_elapsed.as_secs_f64() * gib);
    let serde_gibs = bytes / (serde_elapsed.as_secs_f64() * gib);
    println!(
        "{label}: decoded {} records, last id={}\n  parse_to_dom: {:.3} ms  ({:.2} GiB/s)  |  from_taperef: {:.3} ms  ({:.2} GiB/s)",
        records.len(),
        records.last().unwrap().id,
        parse_elapsed.as_secs_f64() * 1000.0,
        parse_gibs,
        serde_elapsed.as_secs_f64() * 1000.0,
        serde_gibs,
    );
}

fn main() {
    // ~1 MiB so the example runs instantly.
    let data = gen_mixed(1024 * 1024);

    #[cfg(target_arch = "x86_64")]
    let label = if is_x86_feature_detected!("avx512bw") {
        "parse_to_dom_zmm + from_taperef  (AVX-512BW)"
    } else {
        "parse_to_dom + from_taperef  (portable SWAR)"
    };
    #[cfg(not(target_arch = "x86_64"))]
    let label = "parse_to_dom + from_taperef  (portable SWAR)";

    run(label, &data);
}
