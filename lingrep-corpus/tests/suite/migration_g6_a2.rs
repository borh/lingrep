#[test]
fn retired_artifact_requires_rebuild() {
    crate::support::assert_retired_artifact_requires_rebuild(
        include_bytes!("../fixtures/retired/g6-a2.json.zst"),
        "g6-a2",
        6,
    );
}
