//! Compile-fail UI tests for the `#[bot]` / `#[on(...)]` macros, driven by
//! trybuild.  Each case in `tests/macro_ui/` must fail to compile with the
//! diagnostic captured in its companion `.stderr` file.
//!
//! Regenerate the expected output after intentional changes with:
//!   TRYBUILD=overwrite cargo test --test macro_ui
//!
//! Run with:
//!   cargo test --test macro_ui

#[test]
fn ui() {
    let t = trybuild::TestCases::new();
    t.compile_fail("tests/macro_ui/*.rs");
}
