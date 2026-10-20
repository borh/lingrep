//! Snapshot tests for the lingrep-web CLI surface (help text etc.).

#[test]
fn cli_snapshots() {
    trycmd::TestCases::new().case("tests/cli/*.trycmd");
}
