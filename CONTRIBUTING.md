# Contributing to asmjson

Thank you for your interest in contributing!  This is an experimental,
research-oriented crate; contributions that improve correctness, performance,
portability, or documentation are welcome.

## Reporting issues

Please [open a GitHub issue](https://github.com/atomicincrement/asmjson/issues/new)
and include:

- A minimal reproducer (a short JSON input and the call that triggers the bug).
- The output you expected and what you actually observed.
- Your CPU model (relevant for AVX-512BW vs SWAR path) and Rust toolchain
  version (`rustc --version`).

## Submitting patches

1. Fork the repository and create a feature branch.
2. Make your changes.  Keep commits focused; one logical change per commit.
3. Run the full test suite:

   ```sh
   cargo test
   ```

4. Format your Rust code before committing:

   ```sh
   cargo fmt
   ```

5. Open a pull request against `master` with a clear description of what
   the change does and why.

## Hand-written assembly

The files under `asm/x86_64/` are hand-written GNU assembler.  If you modify
them, please also update the corresponding comments in `src/lib.rs` and the
design notes in `doc/dev.md`.

## Code style

- Rust: standard `rustfmt` formatting (`cargo fmt`).
- Assembly: Intel syntax, one blank line between logically distinct blocks,
  descriptive inline comments for every non-obvious instruction.
- Ensure exactly one blank line between Rust function definitions (see
  [AGENTS.md](AGENTS.md) for the automated-agent policy).

## Seeking support

For questions about using the library, open a
[GitHub Discussion](https://github.com/atomicincrement/asmjson/discussions)
or a GitHub issue labelled `question`.
