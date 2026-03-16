//! Tight-loop driver for `parse_to_tape_zmm_tape` — run under perf.
//!
//!   cargo build --release --example perf_zmm_tape
//!   perf record -g target/release/examples/perf_zmm_tape
//!   perf report

#[cfg(target_arch = "x86_64")]
fn main() {
    use asmjson::parse_to_tape_zmm_tape;

    // ~10 MiB of mixed JSON (same generator as the criterion bench).
    let data = gen_mixed(10 * 1024 * 1024);

    // Enough iterations for perf to gather useful samples (~5 s on modern hw).
    let iters = 400u64;
    let mut total_entries = 0usize;
    for _ in 0..iters {
        let tape = parse_to_tape_zmm_tape(&data, None).expect("parse failed");
        // Touch the tape so the compiler can't eliminate the work.
        total_entries += tape.entries.len();
    }
    eprintln!("total_entries={total_entries} (iters={iters})");
}

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
            format!("{:.2}", i as f64 * 0.5)
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

#[cfg(not(target_arch = "x86_64"))]
fn main() {
    eprintln!("perf_zmm_tape only runs on x86_64");
}
