#![cfg(unix)]
#![allow(clippy::unwrap_used)]

use std::os::unix::fs::symlink;
use std::path::PathBuf;
use std::process::Command as ProcessCommand;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use lingrep_core::analysis::{
    AnalyzerDescriptor, AnalyzerFamily, ExternalComponent, ExternalProtocolLimits,
    ExternalRunConfig, ExtractionProfile, InputUnitId, NormalizationPolicy, SegmentationMode,
    TextUnit, TextUnitKind, external_component_closure_digest,
};
use lingrep_core::execution::{
    ExecutionBatch, ExecutionLimits, ExternalAdapterCommand, ExternalAdapterProcess,
    LiveExternalSource, SentenceBatchSource, VerifiedAdapterManifest,
};
use lingrep_core::fsm::{BoundCatalog, CompileLimits, PatternSource, compile_catalog};
use lingrep_core::token_match::{
    AttributeKey, AttributeKind, BoundProjection, BoundTokenSentence, SchemaAttribute,
    TokenAttributeIndex, TokenSchema,
};

const PORTABLE_CONTENT_WORDS: &str = include_str!("../../patterns/portable-content-words.lx");

fn portable_content_catalog(schema: &TokenSchema, projection: &BoundProjection) -> BoundCatalog {
    compile_catalog(
        PatternSource::named("patterns/portable-content-words.lx", PORTABLE_CONTENT_WORDS),
        CompileLimits::default(),
    )
    .unwrap()
    .matcher
    .bind_with_projection(schema, projection.keys().iter().cloned())
    .unwrap()
}

fn portable_content_realization(
    catalog: &BoundCatalog,
    words: &BoundTokenSentence,
) -> Vec<(String, String)> {
    let text = catalog.projection().ordinal_key("text").unwrap();
    catalog
        .find_matches(words)
        .unwrap()
        .iter()
        .map(|item| {
            (
                item.pattern.to_string(),
                words
                    .token(item.start)
                    .unwrap()
                    .get(text)
                    .unwrap()
                    .to_owned(),
            )
        })
        .collect()
}

fn fixture() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/external_echo_adapter.py")
}

fn config(manifest: &VerifiedAdapterManifest) -> ExternalRunConfig {
    config_for(manifest, SegmentationMode::Input)
}

fn config_for(
    manifest: &VerifiedAdapterManifest,
    segmentation: SegmentationMode,
) -> ExternalRunConfig {
    let schema = TokenSchema::new([
        SchemaAttribute::new("text", AttributeKind::Scalar).unwrap(),
        SchemaAttribute::new("lemma", AttributeKind::Scalar).unwrap(),
    ])
    .unwrap();
    let projection = schema
        .bind_projection([TokenAttributeIndex::Orth, TokenAttributeIndex::Lemma])
        .unwrap();
    let descriptor = AnalyzerDescriptor::new(
        AnalyzerFamily::External,
        ExtractionProfile::Morphology,
        segmentation,
        NormalizationPolicy::None,
        "ja",
        match segmentation {
            SegmentationMode::Input => "input-v1",
            SegmentationMode::Analyzer => "adapter-v1",
        },
        manifest.implementation_digest(),
        manifest.resource_manifest_digest(),
        [0; 32],
        [0; 32],
        external_component_closure_digest(manifest.components()),
        [0; 32],
        schema.digest(),
    )
    .unwrap();
    ExternalRunConfig::new(
        "live-1",
        descriptor,
        schema,
        projection,
        manifest.components().to_vec(),
        ExternalProtocolLimits::default(),
    )
    .unwrap()
}

fn command(mode: &str) -> ExternalAdapterCommand {
    ExternalAdapterCommand::interpreted(
        "python3",
        "external_echo_adapter.py",
        [PathBuf::from(mode)],
    )
    .unwrap()
}

fn manifest(command: &ExternalAdapterCommand) -> VerifiedAdapterManifest {
    let bytes = std::fs::read(fixture()).unwrap();
    VerifiedAdapterManifest::new(
        fixture().parent().unwrap(),
        [(
            PathBuf::from("external_echo_adapter.py"),
            *blake3::hash(&bytes).as_bytes(),
        )],
        std::iter::empty::<(PathBuf, [u8; 32])>(),
        command,
        vec![ExternalComponent::new("adapter", *blake3::hash(&bytes).as_bytes()).unwrap()],
    )
    .unwrap()
}

fn copy_manifest_file(
    source_root: &std::path::Path,
    target_root: &std::path::Path,
    relative: &str,
) -> (PathBuf, [u8; 32]) {
    let source = source_root.join(relative);
    let target = target_root.join(relative);
    std::fs::create_dir_all(target.parent().unwrap()).unwrap();
    std::fs::copy(&source, &target).unwrap();
    let bytes = std::fs::read(target).unwrap();
    (PathBuf::from(relative), *blake3::hash(&bytes).as_bytes())
}

fn resource_entries(
    root: &std::path::Path,
    directory: &std::path::Path,
    entries: &mut Vec<(PathBuf, [u8; 32])>,
) {
    for entry in std::fs::read_dir(directory).unwrap() {
        let entry = entry.unwrap();
        let path = entry.path();
        let file_type = entry.file_type().unwrap();
        if file_type.is_dir() {
            resource_entries(root, &path, entries);
        } else {
            assert!(file_type.is_file(), "model resources must be regular files");
            let relative = path.strip_prefix(root).unwrap().to_path_buf();
            let digest = *blake3::hash(&std::fs::read(path).unwrap()).as_bytes();
            entries.push((relative, digest));
        }
    }
}

fn copy_regular_tree(source: &std::path::Path, target: &std::path::Path) {
    std::fs::create_dir_all(target).unwrap();
    for entry in std::fs::read_dir(source).unwrap() {
        let entry = entry.unwrap();
        let source_path = entry.path();
        let target_path = target.join(entry.file_name());
        let file_type = entry.file_type().unwrap();
        if file_type.is_dir() {
            copy_regular_tree(&source_path, &target_path);
        } else {
            assert!(
                file_type.is_file(),
                "fixture trees must contain regular files"
            );
            std::fs::copy(source_path, target_path).unwrap();
        }
    }
}

fn decode_digest(value: &str) -> [u8; 32] {
    assert_eq!(value.len(), 64);
    let mut digest = [0_u8; 32];
    let (pairs, remainder) = value.as_bytes().as_chunks::<2>();
    assert!(remainder.is_empty());
    for (target, pair) in digest.iter_mut().zip(pairs) {
        *target = u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap();
    }
    digest
}

struct StanzaOwnerFixture {
    _isolated: tempfile::TempDir,
    command: ExternalAdapterCommand,
    manifest: VerifiedAdapterManifest,
    config: ExternalRunConfig,
    projection: lingrep_core::token_match::BoundProjection,
}

fn stanza_owner_fixture() -> StanzaOwnerFixture {
    let python = PathBuf::from(std::env::var_os("LINGREP_STANZA_ADAPTER_PYTHON").unwrap());
    let source_model = PathBuf::from(std::env::var_os("LINGREP_STANZA_MODEL_ROOT").unwrap());
    let repository = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .to_path_buf();
    let isolated = tempfile::tempdir().unwrap();
    let root = isolated.path();
    let implementation = [
        "python/stanza-adapter/bin/lingrep-stanza-adapter",
        "python/stanza-adapter/pyproject.toml",
        "python/stanza-adapter/uv.lock",
        "python/stanza-adapter/src/lingrep_stanza_adapter/__init__.py",
        "python/stanza-adapter/src/lingrep_stanza_adapter/__main__.py",
        "python/stanza-adapter/src/lingrep_stanza_adapter/adapter.py",
        "python/stanza-adapter/src/lingrep_stanza_adapter/bundle.py",
    ]
    .map(|relative| copy_manifest_file(&repository, root, relative));
    let model = root.join("model");
    copy_regular_tree(&source_model, &model);
    let profile: serde_json::Value =
        serde_json::from_slice(&std::fs::read(model.join("lingrep-stanza-model.json")).unwrap())
            .unwrap();
    let selection = &profile["selections"]["morphology/input"];
    let profile_digest = decode_digest(
        profile["selection_digests"]["morphology/input"]
            .as_str()
            .unwrap(),
    );
    let schema = TokenSchema::new(selection["schema"].as_array().unwrap().iter().map(
        |attribute| {
            let raw_kind = attribute["kind"].as_str().unwrap();
            assert!(matches!(raw_kind, "scalar" | "string_set"));
            let kind = if raw_kind == "scalar" {
                AttributeKind::Scalar
            } else {
                AttributeKind::StringSet
            };
            SchemaAttribute::new(attribute["key"].as_str().unwrap(), kind).unwrap()
        },
    ))
    .unwrap();
    let projection = schema
        .bind_projection(
            ["text", "lemma", "upos", "xpos", "feat.Number"]
                .map(|key| AttributeKey::new(key).unwrap()),
        )
        .unwrap();
    let mut resources = Vec::new();
    resource_entries(root, &model, &mut resources);
    resources.sort_by(|left, right| left.0.cmp(&right.0));
    let entrypoint = "python/stanza-adapter/bin/lingrep-stanza-adapter";
    let command =
        ExternalAdapterCommand::stanza(python, entrypoint, "model", "fr", "cpu", 1).unwrap();
    let manifest = VerifiedAdapterManifest::new(
        root,
        implementation,
        resources,
        &command,
        vec![ExternalComponent::new("stanza-profile", profile_digest).unwrap()],
    )
    .unwrap();
    let descriptor = AnalyzerDescriptor::new(
        AnalyzerFamily::Stanza,
        ExtractionProfile::Morphology,
        SegmentationMode::Input,
        NormalizationPolicy::Nfc,
        "fr",
        "input-v1",
        manifest.implementation_digest(),
        manifest.resource_manifest_digest(),
        [0; 32],
        profile_digest,
        external_component_closure_digest(manifest.components()),
        profile_digest,
        schema.digest(),
    )
    .unwrap();
    let config = ExternalRunConfig::new(
        "stanza-fr-gsd-v1",
        descriptor,
        schema,
        projection.clone(),
        manifest.components().to_vec(),
        ExternalProtocolLimits::default(),
    )
    .unwrap();
    StanzaOwnerFixture {
        _isolated: isolated,
        command,
        manifest,
        config,
        projection,
    }
}

fn launch(
    mode: &str,
    diagnostic: lingrep_core::execution::DiagnosticCallback,
) -> ExternalAdapterProcess {
    let command = command(mode);
    let manifest = manifest(&command);
    let config = config(&manifest);
    ExternalAdapterProcess::launch(command, manifest, config, diagnostic).unwrap()
}

fn input() -> TextUnit {
    TextUnit::sentence(InputUnitId::new(7), "猫。", NormalizationPolicy::None).unwrap()
}

fn group_exists(group: i32) -> bool {
    let result = unsafe { libc::kill(-group, 0) };
    result == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

#[test]
fn persistent_echo_process_returns_a_canonical_batch() {
    let mut process = launch("echo", Arc::new(|_| Ok(())));
    let batch = process
        .exchange_batch(vec![input()], &ExecutionLimits::default())
        .unwrap();
    assert_eq!(batch.analysis().inputs()[0].input().text(), "猫。");
    assert_eq!(batch.analysis().inputs()[0].sentences()[0].words().len(), 1);
    process.finish().unwrap();
}

#[test]
#[ignore = "requires LINGREP_SPACY_ADAPTER_PYTHON pointing at the locked adapter environment"]
fn locked_spacy_adapter_runs_through_the_verified_rust_process_owner() {
    let python = PathBuf::from(std::env::var_os("LINGREP_SPACY_ADAPTER_PYTHON").unwrap());
    let repository = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .to_path_buf();
    let isolated = tempfile::tempdir().unwrap();
    let root = isolated.path();
    let implementation = [
        "python/spacy-adapter/bin/lingrep-spacy-adapter",
        "python/spacy-adapter/pyproject.toml",
        "python/spacy-adapter/uv.lock",
        "python/spacy-adapter/src/lingrep_spacy_adapter/__init__.py",
        "python/spacy-adapter/src/lingrep_spacy_adapter/__main__.py",
        "python/spacy-adapter/src/lingrep_spacy_adapter/adapter.py",
        "python/spacy-adapter/src/lingrep_spacy_adapter/bundle.py",
    ]
    .map(|relative| copy_manifest_file(&repository, root, relative));
    let model = root.join("model");
    let status = ProcessCommand::new(&python)
        .args([
            "-c",
            "import spacy,sys; spacy.blank('en').to_disk(sys.argv[1])",
        ])
        .arg("model")
        .current_dir(root)
        .status()
        .unwrap();
    assert!(status.success());
    let entrypoint = "python/spacy-adapter/bin/lingrep-spacy-adapter";
    let status = ProcessCommand::new(&python)
        .arg(root.join(entrypoint))
        .args([
            "--model-root",
            "model",
            "--backend",
            "cpu",
            "--prepare",
            "--probe-text",
            "Cats run.",
        ])
        .current_dir(root)
        .status()
        .unwrap();
    assert!(status.success());
    let mut resources = Vec::new();
    resource_entries(root, &model, &mut resources);
    resources.sort_by(|left, right| left.0.cmp(&right.0));
    let model_resource = root.join(&resources[0].0);
    let model_resource_bytes = std::fs::read(&model_resource).unwrap();

    let command = ExternalAdapterCommand::spacy(python, entrypoint, "model", "cpu", 1).unwrap();
    let profile: serde_json::Value =
        serde_json::from_slice(&std::fs::read(model.join("lingrep-spacy-model.json")).unwrap())
            .unwrap();
    let profile_digest = decode_digest(
        profile["selection_digests"]["tokenize/input"]
            .as_str()
            .unwrap(),
    );
    let manifest = VerifiedAdapterManifest::new(
        root,
        implementation,
        resources,
        &command,
        vec![ExternalComponent::new("spacy-profile", profile_digest).unwrap()],
    )
    .unwrap();
    let schema =
        TokenSchema::new([SchemaAttribute::new("text", AttributeKind::Scalar).unwrap()]).unwrap();
    let projection = schema.bind_projection([TokenAttributeIndex::Orth]).unwrap();
    let descriptor = AnalyzerDescriptor::new(
        AnalyzerFamily::External,
        ExtractionProfile::Tokenize,
        SegmentationMode::Input,
        NormalizationPolicy::Nfc,
        "en",
        "input-v1",
        manifest.implementation_digest(),
        manifest.resource_manifest_digest(),
        [0; 32],
        [0; 32],
        external_component_closure_digest(manifest.components()),
        profile_digest,
        schema.digest(),
    )
    .unwrap();
    let expected_descriptor = descriptor.clone();
    let expected_schema = schema.clone();
    let config = ExternalRunConfig::new(
        "spacy-smoke-v1",
        descriptor,
        schema,
        projection.clone(),
        manifest.components().to_vec(),
        ExternalProtocolLimits::default(),
    )
    .unwrap();
    let model_mutation_command = command.clone();
    let model_mutation_manifest = manifest.clone();
    let model_mutation_config = config.clone();
    let adapter_mutation_command = command.clone();
    let adapter_mutation_manifest = manifest.clone();
    let adapter_mutation_config = config.clone();
    let mut process =
        ExternalAdapterProcess::launch(command, manifest, config, Arc::new(|_| Ok(()))).unwrap();
    assert_eq!(process.descriptor(), &expected_descriptor);
    assert_eq!(process.schema(), &expected_schema);
    assert_eq!(process.execution_provenance().backend.as_ref(), "cpu");
    assert!(process.execution_provenance().runtime.contains("platform-"));
    assert_ne!(process.execution_provenance().lock_digest, [0; 32]);
    let batch = process
        .exchange_batch(
            vec![
                TextUnit::sentence(InputUnitId::new(70), "Café.", NormalizationPolicy::Nfc)
                    .unwrap(),
            ],
            &ExecutionLimits::default(),
        )
        .unwrap();
    let sentence = &batch.analysis().inputs()[0].sentences()[0];
    let text = projection.ordinal_key("text").unwrap();
    assert_eq!(
        (0..sentence.words().len())
            .map(|index| sentence.words().token(index).unwrap().get(text).unwrap())
            .collect::<Vec<_>>(),
        ["Café", "."]
    );
    assert_eq!(sentence.source_range().as_usize(), 0..6);
    assert_eq!(sentence.surface_tokens()[0].source_range().as_usize(), 0..5);
    process.finish().unwrap();

    std::fs::write(&model_resource, b"changed model resource").unwrap();
    let model_error = ExternalAdapterProcess::launch(
        model_mutation_command,
        model_mutation_manifest,
        model_mutation_config,
        Arc::new(|_| Ok(())),
    )
    .unwrap_err();
    assert!(
        model_error
            .to_string()
            .contains("manifest metadata mismatch"),
        "unexpected model mutation error: {model_error}"
    );
    std::fs::write(&model_resource, model_resource_bytes).unwrap();

    let adapter = root.join("python/spacy-adapter/src/lingrep_spacy_adapter/adapter.py");
    std::fs::write(&adapter, b"changed adapter implementation").unwrap();
    let adapter_error = ExternalAdapterProcess::launch(
        adapter_mutation_command,
        adapter_mutation_manifest,
        adapter_mutation_config,
        Arc::new(|_| Ok(())),
    )
    .unwrap_err();
    assert!(
        adapter_error
            .to_string()
            .contains("manifest metadata mismatch"),
        "unexpected adapter mutation error: {adapter_error}"
    );
}

#[test]
#[ignore = "requires the locked adapter Python and LINGREP_SPACY_GINZA_MODEL"]
fn locked_ginza_model_runs_through_rust_with_validated_identity_and_provenance() {
    let python = PathBuf::from(std::env::var_os("LINGREP_SPACY_ADAPTER_PYTHON").unwrap());
    let ginza_model = std::env::var("LINGREP_SPACY_GINZA_MODEL").unwrap();
    let repository = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .to_path_buf();
    let isolated = tempfile::tempdir().unwrap();
    let root = isolated.path();
    let implementation = [
        "python/spacy-adapter/bin/lingrep-spacy-adapter",
        "python/spacy-adapter/pyproject.toml",
        "python/spacy-adapter/uv.lock",
        "python/spacy-adapter/src/lingrep_spacy_adapter/__init__.py",
        "python/spacy-adapter/src/lingrep_spacy_adapter/__main__.py",
        "python/spacy-adapter/src/lingrep_spacy_adapter/adapter.py",
        "python/spacy-adapter/src/lingrep_spacy_adapter/bundle.py",
    ]
    .map(|relative| copy_manifest_file(&repository, root, relative));
    let status = ProcessCommand::new(&python)
        .args([
            "-c",
            "import spacy,sys; spacy.load(sys.argv[1]).to_disk('model')",
            &ginza_model,
        ])
        .current_dir(root)
        .status()
        .unwrap();
    assert!(status.success());
    let entrypoint = "python/spacy-adapter/bin/lingrep-spacy-adapter";
    let status = ProcessCommand::new(&python)
        .arg(root.join(entrypoint))
        .args([
            "--model-root",
            "model",
            "--backend",
            "cpu",
            "--prepare",
            "--probe-text",
            "猫が走る。",
            "--morphology-key",
            "spacy.morph.inflection",
            "--morphology-key",
            "spacy.morph.reading",
        ])
        .current_dir(root)
        .status()
        .unwrap();
    assert!(status.success());
    let model = root.join("model");
    let profile: serde_json::Value =
        serde_json::from_slice(&std::fs::read(model.join("lingrep-spacy-model.json")).unwrap())
            .unwrap();
    let profile_digest = decode_digest(
        profile["selection_digests"]["morphology/input"]
            .as_str()
            .unwrap(),
    );
    let mut resources = Vec::new();
    resource_entries(root, &model, &mut resources);
    resources.sort_by(|left, right| left.0.cmp(&right.0));
    let command = ExternalAdapterCommand::spacy(python, entrypoint, "model", "cpu", 1).unwrap();
    let manifest = VerifiedAdapterManifest::new(
        root,
        implementation,
        resources,
        &command,
        vec![ExternalComponent::new("spacy-profile", profile_digest).unwrap()],
    )
    .unwrap();
    let schema = TokenSchema::new([
        SchemaAttribute::new("text", AttributeKind::Scalar).unwrap(),
        SchemaAttribute::new("lemma", AttributeKind::Scalar).unwrap(),
        SchemaAttribute::new("norm", AttributeKind::Scalar).unwrap(),
        SchemaAttribute::new("upos", AttributeKind::Scalar).unwrap(),
        SchemaAttribute::new("xpos", AttributeKind::Scalar).unwrap(),
        SchemaAttribute::new("spacy.morph.inflection", AttributeKind::StringSet).unwrap(),
        SchemaAttribute::new("spacy.morph.reading", AttributeKind::StringSet).unwrap(),
    ])
    .unwrap();
    let projection = schema
        .bind_projection(
            ["text", "lemma", "upos", "xpos"].map(|key| AttributeKey::new(key).unwrap()),
        )
        .unwrap();
    let descriptor = AnalyzerDescriptor::new(
        AnalyzerFamily::External,
        ExtractionProfile::Morphology,
        SegmentationMode::Input,
        NormalizationPolicy::None,
        "ja",
        "input-v1",
        manifest.implementation_digest(),
        manifest.resource_manifest_digest(),
        [0; 32],
        [0; 32],
        external_component_closure_digest(manifest.components()),
        profile_digest,
        schema.digest(),
    )
    .unwrap();
    let expected_identity = descriptor.semantic_identity();
    let config = ExternalRunConfig::new(
        "ginza-owner-v1",
        descriptor,
        schema.clone(),
        projection.clone(),
        manifest.components().to_vec(),
        ExternalProtocolLimits::default(),
    )
    .unwrap();
    let mut process =
        ExternalAdapterProcess::launch(command, manifest, config, Arc::new(|_| Ok(()))).unwrap();
    assert_eq!(process.descriptor().semantic_identity(), expected_identity);
    assert_eq!(process.schema(), &schema);
    assert_eq!(process.execution_provenance().backend.as_ref(), "cpu");
    assert!(process.execution_provenance().runtime.contains("ja-ginza-"));
    let batch = process
        .exchange_batch(
            vec![
                TextUnit::sentence(
                    InputUnitId::new(71),
                    "猫が走る。",
                    NormalizationPolicy::None,
                )
                .unwrap(),
            ],
            &ExecutionLimits::default(),
        )
        .unwrap();
    let sentence = &batch.analysis().inputs()[0].sentences()[0];
    let text = projection.ordinal_key("text").unwrap();
    let forms = (0..sentence.words().len())
        .map(|index| sentence.words().token(index).unwrap().get(text).unwrap())
        .collect::<String>();
    assert_eq!(forms, "猫が走る。");
    let catalog = portable_content_catalog(&schema, &projection);
    assert_eq!(
        portable_content_realization(&catalog, sentence.words()),
        [
            ("content_noun".to_owned(), "猫".to_owned()),
            ("lexical_predicate".to_owned(), "走る".to_owned())
        ]
    );
    process.finish().unwrap();
}

#[test]
#[ignore = "requires the locked Stanza CPU environment and fixed French GSD model"]
fn locked_stanza_adapter_runs_through_the_verified_rust_process_owner() {
    let fixture = stanza_owner_fixture();
    let mut process = ExternalAdapterProcess::launch(
        fixture.command,
        fixture.manifest,
        fixture.config,
        Arc::new(|_| Ok(())),
    )
    .unwrap();
    assert_eq!(process.descriptor().family(), AnalyzerFamily::Stanza);
    assert_eq!(
        process.descriptor().profile(),
        ExtractionProfile::Morphology
    );
    assert_eq!(process.descriptor().segmentation(), SegmentationMode::Input);
    assert_eq!(process.descriptor().language_policy(), "fr");
    assert_eq!(process.descriptor().boundary_policy(), "input-v1");
    assert!(
        process
            .schema()
            .attributes()
            .iter()
            .any(|attribute| attribute.key().as_str() == "feat.Number")
    );
    assert_eq!(process.execution_provenance().backend.as_ref(), "cpu");
    assert!(
        process
            .execution_provenance()
            .runtime
            .contains("stanza-1.14.0")
    );
    assert!(process.execution_provenance().runtime.contains("platform-"));

    let batch = process
        .exchange_batch(
            vec![
                TextUnit::sentence(
                    InputUnitId::new(80),
                    "Je mange du pain.",
                    NormalizationPolicy::Nfc,
                )
                .unwrap(),
            ],
            &ExecutionLimits::default(),
        )
        .unwrap();
    assert_eq!(
        batch.analysis().inputs()[0].input().text(),
        "Je mange du pain."
    );
    let sentence = &batch.analysis().inputs()[0].sentences()[0];
    let expanded = sentence
        .surface_tokens()
        .iter()
        .find(|surface| surface.source_range().as_usize() == (9..11))
        .unwrap();
    assert_eq!(expanded.word_range(), 2..4);
    let text = fixture.projection.ordinal_key("text").unwrap();
    assert_eq!(sentence.words().token(2).unwrap().get(text), Some("de"));
    assert_eq!(sentence.words().token(3).unwrap().get(text), Some("le"));
    let catalog = portable_content_catalog(process.schema(), &fixture.projection);
    assert_eq!(
        portable_content_realization(&catalog, sentence.words()),
        [
            ("lexical_predicate".to_owned(), "mange".to_owned()),
            ("content_noun".to_owned(), "pain".to_owned())
        ]
    );
    process.finish().unwrap();
}

#[test]
#[ignore = "requires the locked Stanza CPU environment and fixed French GSD model"]
fn locked_stanza_mwt_preserves_source_and_word_identity_through_rust() {
    let fixture = stanza_owner_fixture();
    let projection = fixture.projection.clone();
    let process = ExternalAdapterProcess::launch(
        fixture.command,
        fixture.manifest,
        fixture.config,
        Arc::new(|_| Ok(())),
    )
    .unwrap();
    let batches = std::iter::once(vec![
        TextUnit::sentence(
            InputUnitId::new(81),
            "Je mange du pain.",
            NormalizationPolicy::Nfc,
        )
        .unwrap(),
    ]);
    let mut source = LiveExternalSource::new(process, batches, "stanza-fr-gsd").unwrap();
    assert_eq!(source.trust_class(), "locally-executed");
    let ExecutionBatch::BoundTokenized { sentences, .. } = source
        .next_batch(&ExecutionLimits::default())
        .unwrap()
        .unwrap()
    else {
        panic!("Stanza morphology must yield schema-bound sentences");
    };
    let text = projection.ordinal_key("text").unwrap();
    let forms = (0..sentences[0].value.words().len())
        .map(|index| {
            sentences[0]
                .value
                .words()
                .token(index)
                .unwrap()
                .get(text)
                .unwrap()
        })
        .collect::<Vec<_>>();
    assert_eq!(forms, ["Je", "mange", "de", "le", "pain", "."]);
    assert!(
        source
            .next_batch(&ExecutionLimits::default())
            .unwrap()
            .is_none()
    );
}

#[test]
#[ignore = "requires the locked Stanza CPU environment and fixed French GSD model"]
fn stanza_input_newline_is_refused_before_model_invocation() {
    let fixture = stanza_owner_fixture();
    let mut process = ExternalAdapterProcess::launch(
        fixture.command,
        fixture.manifest,
        fixture.config,
        Arc::new(|_| Ok(())),
    )
    .unwrap();
    let error = process
        .exchange_batch(
            vec![
                TextUnit::sentence(
                    InputUnitId::new(82),
                    "Je mange\ndu pain.",
                    NormalizationPolicy::Nfc,
                )
                .unwrap(),
            ],
            &ExecutionLimits::default(),
        )
        .unwrap_err();
    assert!(
        matches!(
            &error,
            lingrep_core::execution::ExternalProcessError::Worker(_)
                | lingrep_core::execution::ExternalProcessError::Exit { .. }
        ),
        "unexpected Stanza newline error: {error}"
    );
}

#[test]
fn bounded_request_batch_does_not_add_a_poll_sleep_per_record() {
    let mut process = launch("echo", Arc::new(|_| Ok(())));
    let inputs = (0..64)
        .map(|ordinal| {
            TextUnit::sentence(
                InputUnitId::new(ordinal + 100),
                format!("固定文{ordinal}"),
                NormalizationPolicy::None,
            )
            .unwrap()
        })
        .collect();
    let started = Instant::now();
    process
        .exchange_batch(inputs, &ExecutionLimits::default())
        .unwrap();
    assert!(started.elapsed() < Duration::from_millis(500));
    process.finish().unwrap();
}

#[test]
fn manifest_digest_is_verified_before_spawn() {
    let command = command("echo");
    let error = VerifiedAdapterManifest::new(
        fixture().parent().unwrap(),
        [(PathBuf::from("external_echo_adapter.py"), [7; 32])],
        std::iter::empty::<(PathBuf, [u8; 32])>(),
        &command,
        vec![ExternalComponent::new("adapter", [7; 32]).unwrap()],
    )
    .unwrap_err();
    assert!(error.to_string().contains("manifest digest mismatch"));
}

#[test]
fn implementation_manifest_digest_pins_the_sealed_framing() {
    let command = command("echo");
    assert_eq!(
        manifest(&command).implementation_digest(),
        [
            238, 116, 103, 172, 72, 25, 236, 152, 175, 33, 227, 8, 151, 137, 98, 251, 35, 214, 11,
            19, 252, 219, 36, 83, 156, 91, 163, 79, 15, 81, 253, 61,
        ]
    );
}

#[test]
fn manifest_rejects_traversal_and_symlink_entries() {
    let command = command("echo");
    let traversal = VerifiedAdapterManifest::new(
        fixture().parent().unwrap(),
        [(PathBuf::from("../external_echo_adapter.py"), [0; 32])],
        std::iter::empty::<(PathBuf, [u8; 32])>(),
        &command,
        vec![ExternalComponent::new("adapter", [0; 32]).unwrap()],
    )
    .unwrap_err();
    assert!(traversal.to_string().contains("root-relative"));

    let directory = tempfile::tempdir().unwrap();
    let target = directory.path().join("target.py");
    std::fs::copy(fixture(), &target).unwrap();
    symlink("target.py", directory.path().join("adapter.py")).unwrap();
    let digest = *blake3::hash(&std::fs::read(&target).unwrap()).as_bytes();
    let linked = VerifiedAdapterManifest::new(
        directory.path(),
        [(PathBuf::from("adapter.py"), digest)],
        std::iter::empty::<(PathBuf, [u8; 32])>(),
        &ExternalAdapterCommand::interpreted("python3", "adapter.py", ["echo"]).unwrap(),
        vec![ExternalComponent::new("adapter", digest).unwrap()],
    )
    .unwrap_err();
    assert!(linked.to_string().contains("symlink"));
}

#[test]
fn spacy_manifest_requires_the_exact_relative_model_and_project_lock() {
    assert!(
        ExternalAdapterCommand::spacy("", "adapter/bin/lingrep-spacy-adapter", "model", "cpu", 1)
            .is_err()
    );
    assert!(
        ExternalAdapterCommand::spacy(
            "python3",
            "adapter/bin/lingrep-spacy-adapter",
            "model",
            "cpu",
            0,
        )
        .is_err()
    );
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    for (relative, bytes) in [
        (
            "adapter/bin/lingrep-spacy-adapter",
            b"entrypoint".as_slice(),
        ),
        ("adapter/pyproject.toml", b"project".as_slice()),
        ("adapter/uv.lock", b"lock".as_slice()),
        (
            "adapter/src/lingrep_spacy_adapter/__init__.py",
            b"package".as_slice(),
        ),
        (
            "adapter/src/lingrep_spacy_adapter/__main__.py",
            b"main".as_slice(),
        ),
        (
            "adapter/src/lingrep_spacy_adapter/adapter.py",
            b"adapter".as_slice(),
        ),
        (
            "adapter/src/lingrep_spacy_adapter/bundle.py",
            b"bundle".as_slice(),
        ),
        ("model/config.cfg", b"config".as_slice()),
        ("model/lingrep-spacy-model.json", b"{}".as_slice()),
    ] {
        let path = root.join(relative);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, bytes).unwrap();
    }
    let command = ExternalAdapterCommand::spacy(
        "python3",
        "adapter/bin/lingrep-spacy-adapter",
        "model",
        "cpu",
        1,
    )
    .unwrap();
    let entry = |relative: &str| {
        (
            PathBuf::from(relative),
            *blake3::hash(&std::fs::read(root.join(relative)).unwrap()).as_bytes(),
        )
    };
    let implementation = [
        entry("adapter/bin/lingrep-spacy-adapter"),
        entry("adapter/pyproject.toml"),
        entry("adapter/uv.lock"),
        entry("adapter/src/lingrep_spacy_adapter/__init__.py"),
        entry("adapter/src/lingrep_spacy_adapter/__main__.py"),
        entry("adapter/src/lingrep_spacy_adapter/adapter.py"),
        entry("adapter/src/lingrep_spacy_adapter/bundle.py"),
    ];
    let resources = [
        entry("model/config.cfg"),
        entry("model/lingrep-spacy-model.json"),
    ];
    let admitted = VerifiedAdapterManifest::new(
        root,
        implementation.clone(),
        resources.clone(),
        &command,
        vec![ExternalComponent::new("spacy-profile", [1; 32]).unwrap()],
    )
    .unwrap();
    let rocm_command = ExternalAdapterCommand::spacy(
        "python3",
        "adapter/bin/lingrep-spacy-adapter",
        "model",
        "rocm",
        32,
    )
    .unwrap();
    let rocm_manifest = VerifiedAdapterManifest::new(
        root,
        implementation.clone(),
        resources.clone(),
        &rocm_command,
        vec![ExternalComponent::new("spacy-profile", [1; 32]).unwrap()],
    )
    .unwrap();
    assert_ne!(
        command.configuration_digest(),
        rocm_command.configuration_digest()
    );
    assert_eq!(admitted.components(), rocm_manifest.components());

    let missing_module = VerifiedAdapterManifest::new(
        root,
        implementation[..implementation.len() - 1].iter().cloned(),
        resources.clone(),
        &command,
        vec![ExternalComponent::new("spacy-profile", [1; 32]).unwrap()],
    )
    .unwrap_err();
    assert!(
        missing_module
            .to_string()
            .contains("executable project closure")
    );

    let extra_module = root.join("adapter/src/lingrep_spacy_adapter/unsealed.py");
    std::fs::write(&extra_module, b"unsealed").unwrap();
    let extra_module_error = VerifiedAdapterManifest::new(
        root,
        implementation.clone(),
        resources.clone(),
        &command,
        vec![ExternalComponent::new("spacy-profile", [1; 32]).unwrap()],
    )
    .unwrap_err();
    assert!(
        extra_module_error
            .to_string()
            .contains("executable project closure")
    );
    std::fs::remove_file(extra_module).unwrap();

    let shadow_module = root.join("adapter/src/spacy.py");
    std::fs::write(&shadow_module, b"shadow import").unwrap();
    let shadow_module_error = VerifiedAdapterManifest::new(
        root,
        implementation.clone(),
        resources.clone(),
        &command,
        vec![ExternalComponent::new("spacy-profile", [1; 32]).unwrap()],
    )
    .unwrap_err();
    assert!(
        shadow_module_error
            .to_string()
            .contains("executable project closure")
    );
    std::fs::remove_file(shadow_module).unwrap();

    let missing = VerifiedAdapterManifest::new(
        root,
        implementation.clone(),
        [resources[1].clone()],
        &command,
        vec![ExternalComponent::new("spacy-profile", [1; 32]).unwrap()],
    )
    .unwrap_err();
    assert!(
        missing
            .to_string()
            .contains("exact model-root file closure")
    );

    let decoy = root.join("decoy");
    std::fs::create_dir(&decoy).unwrap();
    std::fs::write(decoy.join("resource"), b"decoy").unwrap();
    let decoy_error = VerifiedAdapterManifest::new(
        root,
        implementation.clone(),
        [entry("decoy/resource")],
        &command,
        vec![ExternalComponent::new("spacy-profile", [1; 32]).unwrap()],
    )
    .unwrap_err();
    assert!(decoy_error.to_string().contains("exact model-root closure"));

    assert!(
        ExternalAdapterCommand::spacy(
            "python3",
            "adapter/bin/lingrep-spacy-adapter",
            "/outside/model",
            "cpu",
            1,
        )
        .is_err()
    );

    let cache = root.join("model/__pycache__/model.pyc");
    std::fs::create_dir_all(cache.parent().unwrap()).unwrap();
    std::fs::write(&cache, b"cache").unwrap();
    let cache_error = VerifiedAdapterManifest::new(
        root,
        implementation,
        [
            resources[0].clone(),
            resources[1].clone(),
            entry("model/__pycache__/model.pyc"),
        ],
        &command,
        vec![ExternalComponent::new("spacy-profile", [1; 32]).unwrap()],
    )
    .unwrap_err();
    assert!(cache_error.to_string().contains("generated Python cache"));
}

fn stanza_fixture() -> (
    tempfile::TempDir,
    Vec<(PathBuf, [u8; 32])>,
    Vec<(PathBuf, [u8; 32])>,
) {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    for (relative, bytes) in [
        (
            "adapter/bin/lingrep-stanza-adapter",
            b"entrypoint".as_slice(),
        ),
        ("adapter/pyproject.toml", b"project".as_slice()),
        ("adapter/uv.lock", b"lock".as_slice()),
        (
            "adapter/src/lingrep_stanza_adapter/__init__.py",
            b"package".as_slice(),
        ),
        (
            "adapter/src/lingrep_stanza_adapter/__main__.py",
            b"main".as_slice(),
        ),
        (
            "adapter/src/lingrep_stanza_adapter/adapter.py",
            b"adapter".as_slice(),
        ),
        (
            "adapter/src/lingrep_stanza_adapter/bundle.py",
            b"bundle".as_slice(),
        ),
        ("model/resources.json", b"resources".as_slice()),
        ("model/tokenize/gsd.pt", b"tokenizer".as_slice()),
        ("model/lingrep-stanza-model.json", b"{}".as_slice()),
    ] {
        let path = root.join(relative);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, bytes).unwrap();
    }
    let entry = |relative: &str| {
        (
            PathBuf::from(relative),
            *blake3::hash(&std::fs::read(root.join(relative)).unwrap()).as_bytes(),
        )
    };
    let implementation = [
        "adapter/bin/lingrep-stanza-adapter",
        "adapter/pyproject.toml",
        "adapter/uv.lock",
        "adapter/src/lingrep_stanza_adapter/__init__.py",
        "adapter/src/lingrep_stanza_adapter/__main__.py",
        "adapter/src/lingrep_stanza_adapter/adapter.py",
        "adapter/src/lingrep_stanza_adapter/bundle.py",
    ]
    .map(entry)
    .to_vec();
    let resources = [
        "model/resources.json",
        "model/tokenize/gsd.pt",
        "model/lingrep-stanza-model.json",
    ]
    .map(entry)
    .to_vec();
    (directory, implementation, resources)
}

#[test]
fn stanza_command_requires_normalized_resources_language_backend_and_batch_size() {
    let entrypoint = "adapter/bin/lingrep-stanza-adapter";
    assert!(ExternalAdapterCommand::stanza("", entrypoint, "model", "fr", "cpu", 1).is_err());
    assert!(
        ExternalAdapterCommand::stanza("python3", entrypoint, "/model", "fr", "cpu", 1).is_err()
    );
    assert!(ExternalAdapterCommand::stanza("python3", entrypoint, "model", "", "cpu", 1).is_err());
    assert!(
        ExternalAdapterCommand::stanza("python3", entrypoint, "model", "fr/gsd", "cpu", 1).is_err()
    );
    assert!(
        ExternalAdapterCommand::stanza("python3", entrypoint, "model", "fr", "tpu", 1).is_err()
    );
    assert!(
        ExternalAdapterCommand::stanza("python3", entrypoint, "model", "fr", "cpu", 0).is_err()
    );
    assert!(
        ExternalAdapterCommand::stanza("python3", entrypoint, "model", "fr", "cpu", 2).is_err()
    );
}

#[test]
fn stanza_manifest_requires_the_exact_project_and_resource_closures() {
    let (directory, implementation, resources) = stanza_fixture();
    let command = ExternalAdapterCommand::stanza(
        "python3",
        "adapter/bin/lingrep-stanza-adapter",
        "model",
        "fr",
        "cpu",
        1,
    )
    .unwrap();
    VerifiedAdapterManifest::new(
        directory.path(),
        implementation.clone(),
        resources.clone(),
        &command,
        vec![ExternalComponent::new("stanza-profile", [1; 32]).unwrap()],
    )
    .unwrap();

    let missing_implementation = VerifiedAdapterManifest::new(
        directory.path(),
        implementation[..implementation.len() - 1].iter().cloned(),
        resources.clone(),
        &command,
        vec![ExternalComponent::new("stanza-profile", [1; 32]).unwrap()],
    )
    .unwrap_err();
    assert!(
        missing_implementation
            .to_string()
            .contains("executable project closure")
    );

    let missing_resource = VerifiedAdapterManifest::new(
        directory.path(),
        implementation,
        resources[..resources.len() - 1].iter().cloned(),
        &command,
        vec![ExternalComponent::new("stanza-profile", [1; 32]).unwrap()],
    )
    .unwrap_err();
    assert!(
        missing_resource
            .to_string()
            .contains("model profile is absent")
    );
}

#[test]
fn stanza_manifest_rejects_undeclared_sibling_imports_symlinks_and_decoy_resources() {
    let (directory, implementation, resources) = stanza_fixture();
    let root = directory.path();
    let command = ExternalAdapterCommand::stanza(
        "python3",
        "adapter/bin/lingrep-stanza-adapter",
        "model",
        "fr",
        "cpu",
        1,
    )
    .unwrap();
    std::fs::write(root.join("adapter/src/stanza.py"), b"shadow import").unwrap();
    let shadow = VerifiedAdapterManifest::new(
        root,
        implementation.clone(),
        resources.clone(),
        &command,
        vec![ExternalComponent::new("stanza-profile", [1; 32]).unwrap()],
    )
    .unwrap_err();
    assert!(shadow.to_string().contains("executable project closure"));
    std::fs::remove_file(root.join("adapter/src/stanza.py")).unwrap();

    let linked = root.join("model/linked.pt");
    symlink("tokenize/gsd.pt", &linked).unwrap();
    let linked_digest = *blake3::hash(&std::fs::read(&linked).unwrap()).as_bytes();
    let symlink_error = VerifiedAdapterManifest::new(
        root,
        implementation.clone(),
        resources
            .iter()
            .cloned()
            .chain([(PathBuf::from("model/linked.pt"), linked_digest)]),
        &command,
        vec![ExternalComponent::new("stanza-profile", [1; 32]).unwrap()],
    )
    .unwrap_err();
    assert!(symlink_error.to_string().contains("symlink"));
    std::fs::remove_file(linked).unwrap();

    std::fs::create_dir(root.join("decoy")).unwrap();
    for name in ["resources.json", "lingrep-stanza-model.json"] {
        std::fs::write(root.join("decoy").join(name), b"decoy").unwrap();
    }
    let decoy = ["decoy/resources.json", "decoy/lingrep-stanza-model.json"].map(|relative| {
        (
            PathBuf::from(relative),
            *blake3::hash(&std::fs::read(root.join(relative)).unwrap()).as_bytes(),
        )
    });
    let decoy_error = VerifiedAdapterManifest::new(
        root,
        implementation,
        decoy,
        &command,
        vec![ExternalComponent::new("stanza-profile", [1; 32]).unwrap()],
    )
    .unwrap_err();
    assert!(
        decoy_error
            .to_string()
            .contains("exact resource-root closure")
    );
}

#[test]
fn stanza_manifest_and_command_must_name_the_same_resource_root() {
    let (directory, implementation, resources) = stanza_fixture();
    let command = ExternalAdapterCommand::stanza(
        "python3",
        "adapter/bin/lingrep-stanza-adapter",
        "other-model",
        "fr",
        "cpu",
        1,
    )
    .unwrap();
    let error = VerifiedAdapterManifest::new(
        directory.path(),
        implementation,
        resources,
        &command,
        vec![ExternalComponent::new("stanza-profile", [1; 32]).unwrap()],
    )
    .unwrap_err();
    assert!(error.to_string().contains("exact resource-root closure"));
}

#[test]
fn manifest_identity_and_component_closure_must_match_exactly() {
    let command = command("echo");
    let manifest = manifest(&command);
    let schema = TokenSchema::new([
        SchemaAttribute::new("text", AttributeKind::Scalar).unwrap(),
        SchemaAttribute::new("lemma", AttributeKind::Scalar).unwrap(),
    ])
    .unwrap();
    let projection = schema
        .bind_projection([TokenAttributeIndex::Orth, TokenAttributeIndex::Lemma])
        .unwrap();
    let descriptor = AnalyzerDescriptor::new(
        AnalyzerFamily::External,
        ExtractionProfile::Morphology,
        SegmentationMode::Input,
        NormalizationPolicy::None,
        "ja",
        "input-v1",
        manifest.implementation_digest(),
        [0; 32],
        [0; 32],
        [0; 32],
        external_component_closure_digest(manifest.components()),
        [0; 32],
        schema.digest(),
    )
    .unwrap();
    let config = ExternalRunConfig::new(
        "live-1",
        descriptor,
        schema,
        projection,
        manifest.components().to_vec(),
        ExternalProtocolLimits::default(),
    )
    .unwrap();
    let error = ExternalAdapterProcess::launch(command, manifest, config, Arc::new(|_| Ok(())))
        .unwrap_err();
    assert!(error.to_string().contains("do not exactly match"));
}

#[test]
fn verified_manifest_rejects_a_different_command_configuration() {
    let admitted = command("echo");
    let manifest = manifest(&admitted);
    let config = config(&manifest);
    let error =
        ExternalAdapterProcess::launch(command("stderr"), manifest, config, Arc::new(|_| Ok(())))
            .unwrap_err();
    assert!(error.to_string().contains("command does not match"));
}

#[test]
fn malformed_reordered_and_nonzero_adapters_fail_terminally_and_are_reaped() {
    for mode in ["malformed", "wrong_order", "nonzero"] {
        let mut process = launch(mode, Arc::new(|_| Ok(())));
        let group = process.process_group_id();
        let error = process
            .exchange_batch(vec![input()], &ExecutionLimits::default())
            .unwrap_err();
        assert!(
            matches!(
                error,
                lingrep_core::execution::ExternalProcessError::Protocol(_)
                    | lingrep_core::execution::ExternalProcessError::Exit { .. }
                    | lingrep_core::execution::ExternalProcessError::Worker(_)
            ),
            "unexpected {mode} error: {error}"
        );
        assert!(
            !group_exists(group),
            "{mode} process group survived failure"
        );
    }
}

#[test]
fn stderr_is_drained_bounded_and_callback_failure_waits_for_cleanup() {
    let observed = Arc::new(AtomicUsize::new(0));
    let callback_observed = Arc::clone(&observed);
    let mut process = launch(
        "stderr",
        Arc::new(move |bytes| {
            callback_observed.fetch_add(bytes.len(), Ordering::Relaxed);
            Ok(())
        }),
    );
    process
        .exchange_batch(vec![input()], &ExecutionLimits::default())
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(1);
    while observed.load(Ordering::Relaxed) < 200_000 && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(observed.load(Ordering::Relaxed) >= 200_000);
    assert_eq!(
        process.retained_stderr_bytes(),
        ExternalProtocolLimits::default().max_stderr_tail_bytes
    );
    process.finish().unwrap();

    let mut process = launch("stderr", Arc::new(|_| Err("sink closed".to_owned())));
    process
        .exchange_batch(vec![input()], &ExecutionLimits::default())
        .unwrap();
    let error = process.finish().unwrap_err();
    assert!(matches!(
        error,
        lingrep_core::execution::ExternalProcessError::Diagnostic(_)
    ));
}

#[test]
fn drop_terminates_the_owned_process_group_including_a_descendant() {
    let mut process = launch("descendant", Arc::new(|_| Ok(())));
    process
        .exchange_batch(vec![input()], &ExecutionLimits::default())
        .unwrap();
    let group = process.process_group_id();
    assert!(group_exists(group));
    drop(process);
    assert!(!group_exists(group));
}

#[test]
fn ignored_term_reaches_the_fixed_grace_then_kills_the_group() {
    let mut process = launch("ignore_term", Arc::new(|_| Ok(())));
    process
        .exchange_batch(vec![input()], &ExecutionLimits::default())
        .unwrap();
    let group = process.process_group_id();
    let started = Instant::now();
    drop(process);
    assert!(started.elapsed() >= Duration::from_millis(4_900));
    assert!(!group_exists(group));
}

#[test]
fn live_source_pulls_exactly_one_input_batch_at_a_time() {
    let pulled = Arc::new(AtomicUsize::new(0));
    let observed = Arc::clone(&pulled);
    let batches = (0_u64..2).map(move |ordinal| {
        observed.fetch_add(1, Ordering::Relaxed);
        vec![
            TextUnit::sentence(
                InputUnitId::new(ordinal + 10),
                format!("文{ordinal}"),
                NormalizationPolicy::None,
            )
            .unwrap(),
        ]
    });
    let process = launch("echo", Arc::new(|_| Ok(())));
    let mut source = LiveExternalSource::new(process, batches, "local-echo").unwrap();
    assert_eq!(source.trust_class(), "locally-executed");
    assert!(!source.wants_lookahead());
    assert_eq!(pulled.load(Ordering::Relaxed), 0);

    let first = source
        .next_batch(&ExecutionLimits::default())
        .unwrap()
        .unwrap();
    assert_eq!(pulled.load(Ordering::Relaxed), 1);
    let ExecutionBatch::BoundTokenized { sentences, .. } = first else {
        panic!("live morphology must yield schema-bound sentences");
    };
    assert_eq!(sentences[0].location.source_name.as_ref(), "local-echo");

    assert!(
        source
            .next_batch(&ExecutionLimits::default())
            .unwrap()
            .is_some()
    );
    assert_eq!(pulled.load(Ordering::Relaxed), 2);
    assert!(
        source
            .next_batch(&ExecutionLimits::default())
            .unwrap()
            .is_none()
    );
}

#[test]
fn invalid_execution_limits_do_not_pull_or_execute_a_live_batch() {
    let pulled = Arc::new(AtomicUsize::new(0));
    let observed = Arc::clone(&pulled);
    let batches = std::iter::once_with(move || {
        observed.fetch_add(1, Ordering::Relaxed);
        vec![input()]
    });
    let process = launch("echo", Arc::new(|_| Ok(())));
    let mut source = LiveExternalSource::new(process, batches, "bounded-live").unwrap();
    let limits = ExecutionLimits {
        max_stage_sentences: 0,
        ..ExecutionLimits::default()
    };
    assert!(source.next_batch(&limits).is_err());
    assert_eq!(pulled.load(Ordering::Relaxed), 0);
}

#[test]
fn bounded_outbound_document_error_retains_input_identity() {
    let mut process = launch("echo", Arc::new(|_| Ok(())));
    let limits = ExecutionLimits {
        max_sentence_bytes: 16,
        max_input_record_bytes: 64,
        ..ExecutionLimits::default()
    };
    let error = process.exchange_batch(vec![input()], &limits).unwrap_err();
    let lingrep_core::execution::ExternalProcessError::Protocol(error) = error else {
        panic!("expected protocol limit error");
    };
    assert_eq!(error.category(), "limit");
    assert_eq!(error.input_id(), Some(InputUnitId::new(7)));
}

#[test]
fn live_preflight_enforces_segmentation_kind_without_treating_a_segment_as_a_sentence() {
    let segment_command = command("segment_two");
    let segment_manifest = manifest(&segment_command);
    let config = config_for(&segment_manifest, SegmentationMode::Analyzer);
    let mut process = ExternalAdapterProcess::launch(
        segment_command,
        segment_manifest,
        config,
        Arc::new(|_| Ok(())),
    )
    .unwrap();
    let limits = ExecutionLimits {
        max_sentence_bytes: 4,
        ..ExecutionLimits::default()
    };
    let segment = TextUnit::admit(
        InputUnitId::new(7),
        TextUnitKind::Segment,
        "abcdefgh",
        NormalizationPolicy::None,
    )
    .unwrap();
    let batch = process.exchange_batch(vec![segment], &limits).unwrap();
    assert_eq!(batch.analysis().inputs()[0].sentences().len(), 2);
    process.finish().unwrap();

    let wrong_command = command("echo");
    let wrong_manifest = manifest(&wrong_command);
    let config = config_for(&wrong_manifest, SegmentationMode::Analyzer);
    let mut process =
        ExternalAdapterProcess::launch(wrong_command, wrong_manifest, config, Arc::new(|_| Ok(())))
            .unwrap();
    let error = process
        .exchange_batch(vec![input()], &ExecutionLimits::default())
        .unwrap_err();
    assert!(error.to_string().contains("kind differs"));
}

#[test]
fn cancellation_interrupts_a_blocked_exchange_and_cleanup_reaps_it() {
    let mut process = launch("delay", Arc::new(|_| Ok(())));
    let group = process.process_group_id();
    let cancellation = process.cancellation_handle();
    let canceller = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(50));
        cancellation.cancel();
    });
    let started = Instant::now();
    assert!(
        process
            .exchange_batch(vec![input()], &ExecutionLimits::default())
            .is_err()
    );
    canceller.join().unwrap();
    assert!(started.elapsed() < Duration::from_secs(1));
    assert!(!group_exists(group));
}

#[test]
fn cancellation_escalates_against_an_adapter_that_ignores_term() {
    let mut process = launch("ignore_term_silent", Arc::new(|_| Ok(())));
    let group = process.process_group_id();
    let cancellation = process.cancellation_handle();
    let canceller = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(50));
        cancellation.cancel();
    });
    let started = Instant::now();
    let error = process
        .exchange_batch(vec![input()], &ExecutionLimits::default())
        .unwrap_err();
    canceller.join().unwrap();
    assert!(matches!(
        error,
        lingrep_core::execution::ExternalProcessError::Cancelled
    ));
    assert!(started.elapsed() >= Duration::from_millis(4_900));
    assert!(!group_exists(group));
}

#[test]
fn clean_finish_terminates_descendants_and_reaps_the_direct_child() {
    let mut process = launch("descendant", Arc::new(|_| Ok(())));
    process
        .exchange_batch(vec![input()], &ExecutionLimits::default())
        .unwrap();
    let pid = process.process_group_id();
    process.finish().unwrap();
    assert!(!group_exists(pid));
    let mut status = 0;
    assert_eq!(
        unsafe { libc::waitpid(pid, &raw mut status, libc::WNOHANG) },
        -1
    );
    assert_eq!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(libc::ECHILD)
    );
}

#[test]
fn wrong_live_tag_fails_immediately_and_trailing_records_fail_finish() {
    let mut wrong = launch("wrong_tag", Arc::new(|_| Ok(())));
    let started = Instant::now();
    assert!(
        wrong
            .exchange_batch(vec![input()], &ExecutionLimits::default())
            .is_err()
    );
    assert!(started.elapsed() < Duration::from_secs(1));

    let mut wrong_identity = launch("wrong_identity_silent", Arc::new(|_| Ok(())));
    let started = Instant::now();
    assert!(
        wrong_identity
            .exchange_batch(vec![input()], &ExecutionLimits::default())
            .is_err()
    );
    assert!(started.elapsed() < Duration::from_secs(1));

    let mut trailing = launch("extra_after_complete", Arc::new(|_| Ok(())));
    trailing
        .exchange_batch(vec![input()], &ExecutionLimits::default())
        .unwrap();
    let error = trailing.finish().unwrap_err();
    assert!(error.to_string().contains("after batch completion"));
}

#[test]
fn decreasing_documents_and_sentence_overproduction_fail_before_silence() {
    let mut reordered = launch("decreasing_document_silent", Arc::new(|_| Ok(())));
    let inputs = vec![
        input(),
        TextUnit::sentence(InputUnitId::new(8), "犬。", NormalizationPolicy::None).unwrap(),
    ];
    let started = Instant::now();
    assert!(
        reordered
            .exchange_batch(inputs, &ExecutionLimits::default())
            .is_err()
    );
    assert!(started.elapsed() < Duration::from_secs(1));

    let mut overproducing = launch("overproduce_silent", Arc::new(|_| Ok(())));
    let limits = ExecutionLimits {
        max_stage_sentences: 1,
        ..ExecutionLimits::default()
    };
    let started = Instant::now();
    assert!(
        overproducing
            .exchange_batch(vec![input()], &limits)
            .is_err()
    );
    assert!(started.elapsed() < Duration::from_secs(1));
}

#[test]
fn cancellation_interrupts_clean_finish_before_termination_grace() {
    let mut process = launch("finish_delay", Arc::new(|_| Ok(())));
    process
        .exchange_batch(vec![input()], &ExecutionLimits::default())
        .unwrap();
    let cancellation = process.cancellation_handle();
    let canceller = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(50));
        cancellation.cancel();
    });
    let started = Instant::now();
    let error = process.finish().unwrap_err();
    canceller.join().unwrap();
    assert!(matches!(
        error,
        lingrep_core::execution::ExternalProcessError::Cancelled
    ));
    assert!(started.elapsed() < Duration::from_secs(1));
}

#[test]
fn stale_cancellation_handle_cannot_signal_a_reused_process_group() {
    let process = launch("echo", Arc::new(|_| Ok(())));
    let cancellation = process.cancellation_handle();
    process.finish().unwrap();
    cancellation.cancel();

    let mut replacement = launch("echo", Arc::new(|_| Ok(())));
    replacement
        .exchange_batch(vec![input()], &ExecutionLimits::default())
        .unwrap();
    replacement.finish().unwrap();
}
