//! DOM-mode parsing example.
//!
//! Demonstrates building a [`Dom`] tape and navigating it with the
//! [`JsonRef`] cursor API.
//!
//! At runtime the example checks CPUID.  If the CPU supports AVX-512BW the
//! fast hand-written assembly path ([`parse_to_dom_zmm`]) is used;
//! otherwise it falls back to the portable SWAR path ([`parse_to_dom`]).
//!
//! ```sh
//! cargo run --example dom_example
//! ```

#[cfg(target_arch = "x86_64")]
use asmjson::parse_to_dom_zmm;
use asmjson::{Dom, JsonRef, parse_to_dom};

const SRC: &str = r#"
{
    "name": "Alice",
    "age": 30,
    "active": true,
    "score": 9.5,
    "tags": ["rust", "json", "simd"],
    "address": {
        "city": "Springfield",
        "zip": "12345"
    },
    "notes": null
}
"#;

fn inspect(label: &str, tape: Dom) {
    println!("=== {label} ===");

    let root = tape.root().expect("empty tape");

    // Scalar fields
    println!("name   : {:?}", root.get("name").as_str());
    println!("age    : {:?}", root.get("age").as_i64());
    println!("active : {:?}", root.get("active").as_bool());
    println!("score  : {:?}", root.get("score").as_f64());
    println!("notes  : is_null={}", root.get("notes").is_null());

    // Array — iterate with index_at
    let tags = root.get("tags").expect("tags missing");
    let tag_count = tags.len().unwrap_or(0);
    print!("tags   :");
    for i in 0..tag_count {
        print!(" {:?}", tags.index_at(i).as_str());
    }
    println!();

    // Nested object
    println!("city   : {:?}", root.get("address").get("city").as_str());
    println!("zip    : {:?}", root.get("address").get("zip").as_str());
    println!();
}

fn main() {
    #[cfg(target_arch = "x86_64")]
    if is_x86_feature_detected!("avx512bw") {
        // SAFETY: CPUID confirmed AVX-512BW is available.
        let tape = unsafe { parse_to_dom_zmm(SRC, None) }.expect("parse failed");
        inspect("parse_to_dom_zmm  (AVX-512BW assembly)", tape);
        return;
    }

    let tape = parse_to_dom(SRC).expect("parse failed");
    inspect("parse_to_dom  (portable SWAR)", tape);
}
