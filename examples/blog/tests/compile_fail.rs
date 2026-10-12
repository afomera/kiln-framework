//! The M0 exit criterion: mistakes that cause N+1 queries, broken links or wrong keys are
//! compile errors. Each file in tests/ui must fail to build with the recorded message.

#[test]
fn compile_errors() {
    trybuild::TestCases::new().compile_fail("tests/ui/*.rs");
}
