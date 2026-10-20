use std::collections::BTreeMap;
use std::fs;
use std::ops::Range;
use std::path::PathBuf;

use blake3::Hasher;
use lingrep_core::fsm::{
    COMPILER_SCHEMA_IDENTITY, CompileLimits, CompileOutput, CompiledCatalog, GRAMMAR_REVISION,
    POLICY_SEMANTICS_IDENTITY, PatternSource, compile_catalog, normalize_inline_pattern,
};
use tree_sitter::{Node, Parser};
use tree_sitter_lingrep::LANGUAGE;

use crate::error::CliError;

pub(crate) enum CatalogInput {
    File(PathBuf),
    Inline(String),
}

pub(crate) struct CatalogSource {
    pub(crate) name: String,
    pub(crate) text: String,
}

pub(crate) struct LoadedCatalog {
    pub(crate) source: CatalogSource,
    pub(crate) output: CompileOutput,
    pub(crate) fingerprint: String,
}

pub(crate) struct AuthoredDefinition {
    pub(crate) name: String,
    pub(crate) catalog_ordinal: u64,
    pub(crate) body: AuthoredDefinitionBody,
    pub(crate) metadata: serde_json::Value,
    pub(crate) source_file: String,
    pub(crate) source_span: Range<u64>,
}

pub(crate) enum AuthoredDefinitionBody {
    Pattern { expression: String },
    Lexicon { by: Vec<String>, member_count: u64 },
}

pub(crate) fn load_and_compile(input: &CatalogInput) -> Result<LoadedCatalog, CliError> {
    let source = load(input)?;
    let output = compile_catalog(
        PatternSource::named(&source.name, &source.text),
        CompileLimits::default(),
    )
    .map_err(|error| CliError::CatalogCompile {
        error,
        source_text: source.text.clone(),
    })?;
    let fingerprint = catalog_fingerprint(&source.text);
    Ok(LoadedCatalog {
        source,
        output,
        fingerprint,
    })
}

fn load(input: &CatalogInput) -> Result<CatalogSource, CliError> {
    match input {
        CatalogInput::File(path) => fs::read_to_string(path)
            .map(|text| CatalogSource {
                name: path.display().to_string(),
                text,
            })
            .map_err(|error| {
                CliError::PatternParse(format!(
                    "failed to read lingrep patterns catalog {}: {error}",
                    path.display()
                ))
            }),
        CatalogInput::Inline(pattern) => {
            let text = normalize_inline_pattern(pattern);
            validate_inline_catalog_shape(&text)?;
            Ok(CatalogSource {
                name: "inline.lx".into(),
                text,
            })
        }
    }
}

fn validate_inline_catalog_shape(source: &str) -> Result<(), CliError> {
    let mut parser = Parser::new();
    parser
        .set_language(&LANGUAGE.into())
        .map_err(|error| CliError::InlineCatalogShape(error.to_string()))?;
    let parser_text = source.replace('\r', "\n");
    let tree = parser.parse(&parser_text, None).ok_or_else(|| {
        CliError::InlineCatalogShape("failed to parse inline lingrep patterns".into())
    })?;
    if tree.root_node().has_error() {
        return Ok(());
    }

    let mut definitions = 0usize;
    let mut public_definitions = 0usize;
    let mut stack = vec![tree.root_node()];
    while let Some(node) = stack.pop() {
        if node.kind() == "pattern_definition" {
            definitions += 1;
            let is_fragment = node
                .child_by_field_name("kind")
                .and_then(|kind| parser_text.get(kind.byte_range()))
                == Some("fragment");
            public_definitions += usize::from(!is_fragment);
            continue;
        }
        let mut cursor = node.walk();
        let children = node.named_children(&mut cursor).collect::<Vec<_>>();
        stack.extend(children.into_iter().rev());
    }

    if definitions == 1 && public_definitions == 1 {
        Ok(())
    } else {
        Err(CliError::InlineCatalogShape(format!(
            "--pattern must contain exactly one public definition and no fragments; found \
             {public_definitions} public definition(s) among {definitions} declaration(s)"
        )))
    }
}

pub(crate) fn catalog_fingerprint(source: &str) -> String {
    let normalized = source.trim();
    let mut digest = Hasher::new();
    digest.update(b"lingrep/catalog/v1");
    digest.update(b"tree-sitter-lingrep/");
    digest.update(GRAMMAR_REVISION.as_bytes());
    digest.update(COMPILER_SCHEMA_IDENTITY.as_bytes());
    digest.update(POLICY_SEMANTICS_IDENTITY.as_bytes());
    digest.update(&(normalized.len() as u64).to_le_bytes());
    digest.update(normalized.as_bytes());
    digest.finalize().to_hex().to_string()
}

impl LoadedCatalog {
    pub(crate) fn authored_definitions(&self) -> Result<Vec<AuthoredDefinition>, CliError> {
        let matcher = &self.output.matcher;
        let mut parser = Parser::new();
        parser
            .set_language(&LANGUAGE.into())
            .map_err(|error| CliError::PatternParse(error.to_string()))?;
        let parser_text = self.source.text.replace('\r', "\n");
        let tree = parser.parse(&parser_text, None).ok_or_else(|| {
            CliError::PatternParse("failed to parse lingrep patterns catalog".into())
        })?;
        let mut definitions =
            Vec::with_capacity(matcher.patterns().len() + matcher.lexicons().len());
        let mut stack = vec![tree.root_node()];
        while let Some(node) = stack.pop() {
            if matches!(node.kind(), "pattern_definition" | "lexicon_definition") {
                if let Some(descriptor) =
                    descriptor_from_definition(&self.source, matcher, node, definitions.len())?
                {
                    definitions.push(descriptor);
                }
                continue;
            }
            let mut cursor = node.walk();
            let children = node.named_children(&mut cursor).collect::<Vec<_>>();
            stack.extend(children.into_iter().rev());
        }
        if definitions.len() != matcher.patterns().len() + matcher.lexicons().len() {
            return Err(CliError::PatternParse(
                "compiled and authored catalog definitions differ".into(),
            ));
        }
        Ok(definitions)
    }
}

fn descriptor_from_definition(
    source: &CatalogSource,
    matcher: &CompiledCatalog,
    node: Node<'_>,
    ordinal: usize,
) -> Result<Option<AuthoredDefinition>, CliError> {
    let name_node = node
        .child_by_field_name("name")
        .ok_or_else(|| CliError::PatternParse("definition is missing its name".into()))?;
    let name = source_slice(&source.text, name_node)?.to_owned();
    let (body, metadata) = if node.kind() == "lexicon_definition" {
        let Some(lexicon) = matcher.lexicons().iter().find(|entry| entry.name() == name) else {
            return Ok(None);
        };
        (
            AuthoredDefinitionBody::Lexicon {
                by: lexicon
                    .by()
                    .iter()
                    .map(|attribute| attribute.pattern_key().to_owned())
                    .collect(),
                member_count: lexicon.member_count() as u64,
            },
            lexicon.metadata(),
        )
    } else {
        let pattern_node = node
            .child_by_field_name("pattern")
            .ok_or_else(|| CliError::PatternParse("definition is missing its expression".into()))?;
        let Some(pattern) = matcher.patterns().get(name.as_str()) else {
            return Ok(None);
        };
        (
            AuthoredDefinitionBody::Pattern {
                expression: source_slice(&source.text, pattern_node)?.to_owned(),
            },
            &pattern.metadata,
        )
    };
    let metadata = metadata
        .iter()
        .map(|(key, value)| (key.to_string(), value.clone()))
        .collect::<BTreeMap<_, _>>();
    Ok(Some(AuthoredDefinition {
        name,
        catalog_ordinal: ordinal as u64,
        body,
        metadata: serde_json::to_value(metadata)
            .expect("lingrep patterns metadata values are JSON-compatible"),
        source_file: source.name.clone(),
        source_span: node.start_byte() as u64..node.end_byte() as u64,
    }))
}

fn source_slice<'a>(source: &'a str, node: Node<'_>) -> Result<&'a str, CliError> {
    source.get(node.byte_range()).ok_or_else(|| {
        CliError::PatternParse("lingrep patterns source span is not valid UTF-8".into())
    })
}
