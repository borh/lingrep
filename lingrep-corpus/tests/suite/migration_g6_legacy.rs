#[test]
fn retired_artifact_requires_rebuild() {
    crate::support::assert_retired_artifact_requires_rebuild(
        include_bytes!("../fixtures/retired/g6-legacy.json.zst"),
        "g6-legacy",
        6,
    );
}
