//! SAX-mode parsing example.
//!
//! Demonstrates implementing the [`Sax`] trait to process JSON events
//! in a single streaming pass without building an intermediate DOM.
//!
//! The example [`Counter`] writer counts every scalar kind in the input.
//!
//! Two entry points are shown:
//!
//! | Function | Description |
//! |----------|-------------|
//! | [`parse_with`] | Portable SWAR classifier — runs on any architecture. |
//! | [`parse_with_zmm`] | Hand-written AVX-512BW x86-64 assembly — `unsafe`, x86_64 only. |
//!
//! Run the portable version:
//!
//! ```sh
//! cargo run --example sax_example
//! ```
//!
//! Run the AVX-512BW assembly version (requires a Skylake-X or later CPU):
//!
//! ```sh
//! cargo run --example sax_example -- zmm
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

fn run_portable(src: &str) {
    println!("=== parse_with  (portable SWAR) ===");
    let counts = parse_with(src, Counter::default()).expect("parse failed");
    println!("{counts:#?}");
    println!();
}

#[cfg(target_arch = "x86_64")]
fn run_zmm(src: &str) {
    println!("=== parse_with_zmm  (AVX-512BW assembly) ===");
    // SAFETY: caller must ensure the CPU supports AVX-512BW.
    let counts = unsafe { parse_with_zmm(src, Counter::default()) }.expect("parse failed");
    println!("{counts:#?}");
    println!();
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let use_zmm = args.get(1).map(|s| s == "zmm").unwrap_or(false);

    if use_zmm {
        #[cfg(target_arch = "x86_64")]
        run_zmm(SRC);

        #[cfg(not(target_arch = "x86_64"))]
        {
            eprintln!("parse_with_zmm is only available on x86_64");
            std::process::exit(1);
        }
    } else {
        run_portable(SRC);
    }
}
