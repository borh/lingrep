#[test]
fn retired_artifact_requires_rebuild() {
    crate::support::assert_retired_artifact_requires_rebuild(
        include_bytes!("../fixtures/retired/g7-a2.json.zst"),
        "g7-a2",
        7,
    );
}
