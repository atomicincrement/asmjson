//! SAX-mode parsing example.
//!
//! Demonstrates implementing the [`Sax`] trait to process JSON events
//! in a single streaming pass without building an intermediate DOM.
//!
//! The example [`Counter`] writer counts every scalar kind in the input.
//!
//! At runtime the example checks CPUID.  If the CPU supports AVX-512BW the
//! fast hand-written assembly path ([`parse_with_zmm`]) is used;
//! otherwise it falls back to the portable SWAR path ([`parse_with`]).
//!
//! ```sh
//! cargo run --example sax_example
//! ```

#[cfg(target_arch = "x86_64")]
use asmjson::parse_with_zmm;
use asmjson::{Sax, parse_with};

// ---------------------------------------------------------------------------
// Custom SAX writer
// ---------------------------------------------------------------------------

/// Counts each kind of JSON event produced by the parser.
#[derive(Default, Debug)]
struct Counter {
    nulls: usize,
    bools: usize,
    numbers: usize,
    strings: usize,
    keys: usize,
    objects: usize,
    arrays: usize,
}

impl<'src> Sax<'src> for Counter {
    type Output = Self;

    fn null(&mut self) {
        self.nulls += 1;
    }

    fn bool_val(&mut self, _v: bool) {
        self.bools += 1;
    }

    fn number(&mut self, _s: &'src str) {
        self.numbers += 1;
    }

    // Unescaped string value — `s` borrows directly from the source JSON.
    fn string(&mut self, _s: &'src str) {
        self.strings += 1;
    }

    // Escaped string value — decoded text, valid only for this call.
    fn escaped_string(&mut self, _s: &str) {
        self.strings += 1;
    }

    // Unescaped object key.
    fn key(&mut self, _s: &'src str) {
        self.keys += 1;
    }

    // Escaped object key — decoded text, valid only for this call.
    fn escaped_key(&mut self, _s: &str) {
        self.keys += 1;
    }

    fn start_object(&mut self) {
        self.objects += 1;
    }

    // end_object / end_array are only needed when tracking nesting.
    fn end_object(&mut self) {}

    fn start_array(&mut self) {
        self.arrays += 1;
    }

    fn end_array(&mut self) {}

    fn finish(self) -> Option<Self::Output> {
        Some(self)
    }
}

// ---------------------------------------------------------------------------
// Sample data
// ---------------------------------------------------------------------------

const SRC: &str = r#"[
    {"id":1,"name":"Alice","active":true,"score":9.5,"tags":["rust","json"]},
    {"id":2,"name":"Bob","active":false,"score":null,"tags":["simd","avx512"]},
    {"id":3,"name":"Carol","active":true,"score":7.0,"tags":[]}
]"#;

fn report(label: &str, counts: Counter) {
    println!("=== {label} ===");
    println!("{counts:#?}");
    println!();
}

fn main() {
    #[cfg(target_arch = "x86_64")]
    if is_x86_feature_detected!("avx512bw") {
        // SAFETY: CPUID confirmed AVX-512BW is available.
        let counts = unsafe { parse_with_zmm(SRC, Counter::default()) }.expect("parse failed");
        report("parse_with_zmm  (AVX-512BW assembly)", counts);
        return;
    }

    let counts = parse_with(SRC, Counter::default()).expect("parse failed");
    report("parse_with  (portable SWAR)", counts);
}
