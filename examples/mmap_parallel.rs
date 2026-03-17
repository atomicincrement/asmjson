//! Parallel JSON-lines parsing via memory-mapped I/O.
//!
//! Memory-maps a JSON Lines file (one JSON value per line), partitions it
//! into ~1 MiB chunks each ending on a `\n` boundary, then parses every
//! chunk concurrently with a SAX string counter using Rayon.
//!
//! CPUID auto-selects the AVX-512BW assembly path when available.
//!
//! On startup the example generates `/tmp/file.jsonl` (~1 GiB, 10 million
//! lines, each a JSON object with keys and values at least 10 characters
//! long), then immediately maps and parses it.
//!
//! ## Usage
//!
//! ```sh
//! cargo run --release --example mmap_parallel
//! ```

#[cfg(target_arch = "x86_64")]
use asmjson::parse_with_zmm;
use asmjson::{Sax, parse_with};
use memmap2::Mmap;
use rayon::prelude::*;
use std::{
    fs,
    io::{BufWriter, Write},
    path::Path,
    time::Instant,
};

// ---------------------------------------------------------------------------
// SAX writer — counts string values and keys
// ---------------------------------------------------------------------------

#[derive(Default, Debug)]
struct StringCounter {
    strings: usize,
    keys: usize,
}

impl<'src> Sax<'src> for StringCounter {
    type Output = Self;

    fn null(&mut self) {}

    fn bool_val(&mut self, _v: bool) {}

    fn number(&mut self, _s: &'src str) {}

    fn string(&mut self, _s: &'src str) {
        self.strings += 1;
    }

    fn escaped_string(&mut self, _s: &str) {
        self.strings += 1;
    }

    fn key(&mut self, _s: &'src str) {
        self.keys += 1;
    }

    fn escaped_key(&mut self, _s: &str) {
        self.keys += 1;
    }

    fn start_object(&mut self) {}

    fn end_object(&mut self) {}

    fn start_array(&mut self) {}

    fn end_array(&mut self) {}

    fn finish(self) -> Option<Self::Output> {
        Some(self)
    }
}

// ---------------------------------------------------------------------------
// Parsing
// ---------------------------------------------------------------------------

const CHUNK_SIZE: usize = 1 << 20; // 1 MiB

/// Parse one line using the AVX-512BW assembly path.
/// The caller must have already verified `avx512bw` support via CPUID.
#[cfg(target_arch = "x86_64")]
fn parse_line_into_zmm(line: &str, out: &mut StringCounter) {
    // SAFETY: caller confirmed AVX-512BW is available.
    if let Some(c) = unsafe { parse_with_zmm(line, StringCounter::default()) } {
        out.strings += c.strings;
        out.keys += c.keys;
    }
}

/// Parse one line using the portable SWAR path.
fn parse_line_into_rust(line: &str, out: &mut StringCounter) {
    if let Some(c) = parse_with(line, StringCounter::default()) {
        out.strings += c.strings;
        out.keys += c.keys;
    }
}

/// Parse every non-empty line in a chunk, returning total counts.
/// The parser variant is chosen once at the top of the function — avoiding a
/// redundant CPUID check on every line — by using two separate loops.
fn parse_chunk(chunk: &str) -> StringCounter {
    let mut out = StringCounter::default();
    #[cfg(target_arch = "x86_64")]
    if is_x86_feature_detected!("avx512bw") {
        for line in chunk.lines() {
            let line = line.trim();
            if !line.is_empty() {
                parse_line_into_zmm(line, &mut out);
            }
        }
        return out;
    }
    for line in chunk.lines() {
        let line = line.trim();
        if !line.is_empty() {
            parse_line_into_rust(line, &mut out);
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Chunking
// ---------------------------------------------------------------------------

/// Split `data` into chunks of at most `chunk_size` bytes, each ending at
/// (and including) a `\n` boundary so that every chunk contains only whole
/// JSON Lines.
fn split_at_newlines(data: &[u8], chunk_size: usize) -> Vec<&[u8]> {
    let mut chunks = Vec::new();
    let mut start = 0;

    while start < data.len() {
        let nominal_end = (start + chunk_size).min(data.len());

        // If we've reached the end of the file there is no need to search.
        let end = if nominal_end == data.len() {
            nominal_end
        } else {
            // Advance to the byte after the next '\n', or end of file.
            data[nominal_end..]
                .iter()
                .position(|&b| b == b'\n')
                .map(|i| nominal_end + i + 1)
                .unwrap_or(data.len())
        };

        chunks.push(&data[start..end]);
        start = end;
    }

    chunks
}

// ---------------------------------------------------------------------------
// Test-file generation
// ---------------------------------------------------------------------------

// Each generated line is exactly 100 bytes including the trailing '\n':
//   {"identifier":"user{i:012}","description":"item{i:012}","subcategory":"type{i:012}"}
// keys  : "identifier"(10), "description"(11), "subcategory"(11)  — all ≥ 10 chars
// values: 16 chars each  (4-char prefix + 12-digit index)          — all ≥ 10 chars
const LINE_BYTES: u64 = 100;
const TARGET_BYTES: u64 = 1 << 30; // 1 GiB
const FILE_PATH: &str = "/tmp/file.jsonl";

fn create_test_file(path: &Path) {
    let n_lines = TARGET_BYTES / LINE_BYTES;
    println!(
        "generating {path} ({n_lines} lines, {} MiB) …",
        TARGET_BYTES >> 20,
        path = path.display(),
    );
    let t = Instant::now();
    let file = fs::File::create(path).expect("cannot create file");
    let mut w = BufWriter::with_capacity(4 << 20, file);
    for i in 0..n_lines {
        writeln!(
            w,
            "{{\"identifier\":\"user{i:012}\",\"description\":\"item{i:012}\",\"subcategory\":\"type{i:012}\"}}"
        )
        .expect("write failed");
    }
    w.flush().expect("flush failed");
    println!("  done in {:.2?}", t.elapsed());
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

fn main() {
    let path = Path::new(FILE_PATH);
    create_test_file(path);

    let file = fs::File::open(path).expect("cannot open file");
    // SAFETY: we created the file ourselves; the mapping is read-only and
    // the file is not modified or truncated while the mapping is live.
    let mmap = unsafe { Mmap::map(&file) }.expect("mmap failed");

    let chunks = split_at_newlines(&mmap, CHUNK_SIZE);
    println!(
        "parsing {} MiB  →  {} chunk(s) of ~{} KiB …",
        mmap.len() >> 20,
        chunks.len(),
        CHUNK_SIZE / 1024,
    );

    let bytes = mmap.len();
    let t = Instant::now();
    let totals: StringCounter = chunks
        .par_iter()
        .map(|chunk| {
            let s = std::str::from_utf8(chunk).expect("non-UTF-8 data in chunk");
            parse_chunk(s)
        })
        .reduce(StringCounter::default, |mut a, b| {
            a.strings += b.strings;
            a.keys += b.keys;
            a
        });
    let elapsed = t.elapsed();
    let gib_per_sec = bytes as f64 / elapsed.as_secs_f64() / (1u64 << 30) as f64;
    println!("  done in {:.2?}  ({:.2} GiB/s)", elapsed, gib_per_sec);

    println!("keys   found : {}", totals.keys);
    println!("strings found: {}", totals.strings);
}
