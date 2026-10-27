//! Typed command-local semantics and executable argv examples.

pub struct CommandExample {
    pub id: &'static str,
    pub purpose: &'static str,
    pub argv: &'static [&'static str],
    pub placeholders: &'static [ExamplePlaceholder],
}

pub struct ExamplePlaceholder {
    pub name: &'static str,
    pub description: &'static str,
}

#[derive(Clone, Copy)]
pub enum ExamplePublication {
    Published(&'static [CommandExample]),
    Omitted { reason: &'static str },
}

impl ExamplePublication {
    fn published(self) -> &'static [CommandExample] {
        match self {
            Self::Published(examples) => examples,
            Self::Omitted { .. } => &[],
        }
    }
}

pub struct CommandSemantics {
    pub command: &'static str,
    pub input_modes: &'static [&'static str],
    pub examples: ExamplePublication,
}

const PATTERNS: ExamplePlaceholder = ExamplePlaceholder {
    name: "patterns",
    description: "lingrep patterns catalog file",
};
const DICTIONARY: ExamplePlaceholder = ExamplePlaceholder {
    name: "dictionary",
    description: "Vibrato dictionary cache",
};
const ROOT: ExamplePlaceholder = ExamplePlaceholder {
    name: "root",
    description: "direct corpus root",
};
const ARTIFACT: ExamplePlaceholder = ExamplePlaceholder {
    name: "artifact",
    description: "prepared corpus artifact",
};
const ARTIFACT_OUTPUT: ExamplePlaceholder = ExamplePlaceholder {
    name: "artifact_output",
    description: "new artifact destination",
};
const MANPAGE_OUTPUT: ExamplePlaceholder = ExamplePlaceholder {
    name: "manpage_output",
    description: "new manual-page directory",
};

static ROOT_EXAMPLES: &[CommandExample] = &[CommandExample {
    id: "lingrep/check-inline",
    purpose: "Validate one inline pattern.",
    argv: &["check", "--pattern", "[]."],
    placeholders: &[],
}];
static CHECK_EXAMPLES: &[CommandExample] = &[CommandExample {
    id: "check/inline-pattern",
    purpose: "Validate one inline pattern.",
    argv: &["check", "--pattern", "[]."],
    placeholders: &[],
}];
static DOCTOR_EXAMPLES: &[CommandExample] = &[CommandExample {
    id: "doctor/explicit-inputs",
    purpose: "Probe an explicit dictionary, catalog, and prepared artifact.",
    argv: &[
        "doctor",
        "--dict-path",
        "{dictionary}",
        "--patterns",
        "{patterns}",
        "--corpus",
        "{artifact}",
    ],
    placeholders: &[DICTIONARY, PATTERNS, ARTIFACT],
}];
static MATCHES_EXAMPLES: &[CommandExample] = &[CommandExample {
    id: "matches/catalog-file-direct-root",
    purpose: "Search a direct corpus with a lingrep patterns catalog.",
    argv: &[
        "matches",
        "--patterns",
        "{patterns}",
        "--dict-path",
        "{dictionary}",
        "{root}",
    ],
    placeholders: &[PATTERNS, DICTIONARY, ROOT],
}];
static SUMMARIZE_EXAMPLES: &[CommandExample] = &[CommandExample {
    id: "summarize/catalog-file-direct-root",
    purpose: "Summarize matches over a direct corpus.",
    argv: &[
        "summarize",
        "--patterns",
        "{patterns}",
        "--dict-path",
        "{dictionary}",
        "{root}",
    ],
    placeholders: &[PATTERNS, DICTIONARY, ROOT],
}];
static TOKENIZE_EXAMPLES: &[CommandExample] = &[CommandExample {
    id: "tokenize/direct-root",
    purpose: "Tokenize a direct corpus.",
    argv: &["tokenize", "--dict-path", "{dictionary}", "{root}"],
    placeholders: &[DICTIONARY, ROOT],
}];
static ATTRIBUTES_EXAMPLES: &[CommandExample] = &[CommandExample {
    id: "coverage/direct-root",
    purpose: "Report per-attribute presence over a direct corpus.",
    argv: &["coverage", "--dict-path", "{dictionary}", "{root}"],
    placeholders: &[DICTIONARY, ROOT],
}];
static CORPUS_BUILD_EXAMPLES: &[CommandExample] = &[CommandExample {
    id: "corpus build/direct-root",
    purpose: "Build a prepared corpus artifact.",
    argv: &[
        "corpus",
        "build",
        "--dict-path",
        "{dictionary}",
        "--output",
        "{artifact_output}",
        "{root}",
    ],
    placeholders: &[DICTIONARY, ARTIFACT_OUTPUT, ROOT],
}];
static CORPUS_INSPECT_EXAMPLES: &[CommandExample] = &[CommandExample {
    id: "corpus inspect/prepared-artifact",
    purpose: "Inspect a prepared corpus artifact.",
    argv: &["corpus", "inspect", "{artifact}"],
    placeholders: &[ARTIFACT],
}];
static CORPUS_SOURCES_EXAMPLES: &[CommandExample] = &[CommandExample {
    id: "corpus inventory/direct-root",
    purpose: "List a direct corpus inventory.",
    argv: &["corpus", "inventory", "{root}"],
    placeholders: &[ROOT],
}];
static CORPUS_VARIANTS_EXAMPLES: &[CommandExample] = &[CommandExample {
    id: "corpus variants/source-or-artifact",
    purpose: "Report every prepared realization beside one source.",
    argv: &["corpus", "variants", "{artifact}"],
    placeholders: &[ARTIFACT],
}];
static DESCRIBE_EXAMPLES: &[CommandExample] = &[CommandExample {
    id: "describe/machine-capabilities",
    purpose: "Read one command capability as JSON.",
    argv: &["describe", "matches", "--json"],
    placeholders: &[],
}];
static COMPLETION_EXAMPLES: &[CommandExample] = &[CommandExample {
    id: "completions/bash",
    purpose: "Generate Bash completion.",
    argv: &["completions", "bash"],
    placeholders: &[],
}];
static MANPAGE_EXAMPLES: &[CommandExample] = &[CommandExample {
    id: "manpages/generate-all",
    purpose: "Generate every public manual page.",
    argv: &["manpages", "--output", "{manpage_output}"],
    placeholders: &[MANPAGE_OUTPUT],
}];
static SCHEMA_EXAMPLES: &[CommandExample] = &[CommandExample {
    id: "contracts/query-detail",
    purpose: "Read the complete query contract.",
    argv: &["contracts", "lingrep.query/v2", "--json"],
    placeholders: &[],
}];

static SEMANTICS: &[CommandSemantics] = &[
    CommandSemantics {
        command: "ling",
        input_modes: &[],
        examples: ExamplePublication::Published(ROOT_EXAMPLES),
    },
    CommandSemantics {
        command: "doctor",
        input_modes: &[],
        examples: ExamplePublication::Published(DOCTOR_EXAMPLES),
    },
    CommandSemantics {
        command: "check",
        input_modes: &[],
        examples: ExamplePublication::Published(CHECK_EXAMPLES),
    },
    CommandSemantics {
        command: "matches",
        input_modes: &["direct", "stdin", "prepared"],
        examples: ExamplePublication::Published(MATCHES_EXAMPLES),
    },
    CommandSemantics {
        command: "summarize",
        input_modes: &["direct", "stdin", "prepared"],
        examples: ExamplePublication::Published(SUMMARIZE_EXAMPLES),
    },
    CommandSemantics {
        command: "corpus",
        input_modes: &[],
        examples: ExamplePublication::Omitted {
            reason: "examples belong to the corpus leaf commands",
        },
    },
    CommandSemantics {
        command: "corpus build",
        input_modes: &["direct"],
        examples: ExamplePublication::Published(CORPUS_BUILD_EXAMPLES),
    },
    CommandSemantics {
        command: "corpus inspect",
        input_modes: &["prepared"],
        examples: ExamplePublication::Published(CORPUS_INSPECT_EXAMPLES),
    },
    CommandSemantics {
        command: "corpus inventory",
        input_modes: &["direct"],
        examples: ExamplePublication::Published(CORPUS_SOURCES_EXAMPLES),
    },
    CommandSemantics {
        command: "corpus variants",
        input_modes: &["prepared"],
        examples: ExamplePublication::Published(CORPUS_VARIANTS_EXAMPLES),
    },
    CommandSemantics {
        command: "tokenize",
        input_modes: &["direct", "stdin", "prepared"],
        examples: ExamplePublication::Published(TOKENIZE_EXAMPLES),
    },
    CommandSemantics {
        command: "coverage",
        input_modes: &["direct", "stdin", "prepared"],
        examples: ExamplePublication::Published(ATTRIBUTES_EXAMPLES),
    },
    CommandSemantics {
        command: "explore",
        input_modes: &["direct", "prepared"],
        examples: ExamplePublication::Omitted {
            reason: "interactive TUI startup is not a deterministic executable example",
        },
    },
    CommandSemantics {
        command: "analyzer",
        input_modes: &[],
        examples: ExamplePublication::Omitted {
            reason: "examples belong to analyzer prepare",
        },
    },
    CommandSemantics {
        command: "analyzer prepare",
        input_modes: &[],
        examples: ExamplePublication::Omitted {
            reason: "preparation downloads Python, dependencies and models",
        },
    },
    CommandSemantics {
        command: "dictionary",
        input_modes: &[],
        examples: ExamplePublication::Omitted {
            reason: "examples belong to the dictionary leaf commands",
        },
    },
    CommandSemantics {
        command: "dictionary install",
        input_modes: &[],
        examples: ExamplePublication::Omitted {
            reason: "installing downloads a dictionary of 400 MB or more",
        },
    },
    CommandSemantics {
        command: "describe",
        input_modes: &[],
        examples: ExamplePublication::Published(DESCRIBE_EXAMPLES),
    },
    CommandSemantics {
        command: "completions",
        input_modes: &[],
        examples: ExamplePublication::Published(COMPLETION_EXAMPLES),
    },
    CommandSemantics {
        command: "manpages",
        input_modes: &[],
        examples: ExamplePublication::Published(MANPAGE_EXAMPLES),
    },
    CommandSemantics {
        command: "contracts",
        input_modes: &[],
        examples: ExamplePublication::Published(SCHEMA_EXAMPLES),
    },
];

#[must_use]
pub fn semantics_for(command: &str) -> Option<&'static CommandSemantics> {
    SEMANTICS.iter().find(|entry| entry.command == command)
}

#[must_use]
pub fn examples_for(command: &str) -> &'static [CommandExample] {
    semantics_for(command).map_or(&[], |entry| entry.examples.published())
}

pub fn all_examples() -> impl Iterator<Item = &'static CommandExample> {
    SEMANTICS
        .iter()
        .flat_map(|entry| entry.examples.published())
}

#[must_use]
pub fn render_posix(example: &CommandExample) -> String {
    example
        .argv
        .iter()
        .map(|token| quote_posix(token))
        .collect::<Vec<_>>()
        .join(" ")
}

fn quote_posix(token: &str) -> String {
    if !token.is_empty()
        && token
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"_@%+=:,./-".contains(&byte))
    {
        token.into()
    } else {
        format!("'{}'", token.replace('\'', "'\\''"))
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use clap::CommandFactory;

    use super::*;

    fn collect_public_paths(command: &clap::Command, prefix: &str, paths: &mut BTreeSet<String>) {
        for child in command
            .get_subcommands()
            .filter(|child| !child.is_hide_set() && child.get_name() != "help")
        {
            let path = if prefix.is_empty() {
                child.get_name().to_owned()
            } else {
                format!("{prefix} {}", child.get_name())
            };
            assert!(paths.insert(path.clone()), "duplicate Clap path {path}");
            collect_public_paths(child, &path, paths);
        }
    }

    #[test]
    fn semantic_paths_exactly_cover_the_public_clap_tree() {
        let mut command = crate::Cli::command();
        command.build();
        let mut parser_paths = BTreeSet::from(["ling".to_owned()]);
        collect_public_paths(&command, "", &mut parser_paths);

        let semantic_paths = SEMANTICS
            .iter()
            .map(|entry| entry.command.to_owned())
            .collect::<BTreeSet<_>>();
        assert_eq!(
            SEMANTICS.len(),
            semantic_paths.len(),
            "duplicate semantic path"
        );
        let missing_semantics = parser_paths.difference(&semantic_paths).collect::<Vec<_>>();
        let orphan_semantics = semantic_paths.difference(&parser_paths).collect::<Vec<_>>();
        assert!(
            missing_semantics.is_empty() && orphan_semantics.is_empty(),
            "missing semantics: {missing_semantics:?}; orphan semantics: {orphan_semantics:?}"
        );
    }

    #[test]
    fn examples_are_unique_and_every_omission_has_a_reason() {
        let mut ids = BTreeSet::new();
        for semantics in SEMANTICS {
            match semantics.examples {
                ExamplePublication::Published(examples) => {
                    assert!(
                        !examples.is_empty(),
                        "{} publishes no examples",
                        semantics.command
                    );
                    for example in examples {
                        assert!(
                            ids.insert(example.id),
                            "duplicate example id {}",
                            example.id
                        );
                    }
                }
                ExamplePublication::Omitted { reason } => {
                    assert!(
                        !reason.trim().is_empty(),
                        "{} has no omission reason",
                        semantics.command
                    );
                }
            }
        }
    }

    #[test]
    fn posix_rendering_quotes_only_unsafe_tokens() {
        let example = CommandExample {
            id: "test",
            purpose: "test",
            argv: &["check", "--pattern", "[].", "a'b"],
            placeholders: &[],
        };
        assert_eq!(render_posix(&example), "check --pattern '[].' 'a'\\''b'");
    }

    #[test]
    fn every_placeholder_is_declared_once_and_used() {
        for example in all_examples() {
            let used = example
                .argv
                .iter()
                .filter_map(|token| token.strip_prefix('{')?.strip_suffix('}'))
                .collect::<Vec<_>>();
            assert_eq!(used.len(), example.placeholders.len(), "{}", example.id);
            for placeholder in example.placeholders {
                assert_eq!(
                    used.iter()
                        .filter(|name| **name == placeholder.name)
                        .count(),
                    1,
                    "{}:{}",
                    example.id,
                    placeholder.name
                );
            }
        }
    }
}
