use crate::fsm::compiler::{
    CompileError, CompileLimits, CompileOutput, DefinitionAccumulator, PatternSource,
};

/// Mutable pattern-definition phase for a compiled NFA matcher.
pub struct FsmBuilder {
    definitions: DefinitionAccumulator,
    limits: CompileLimits,
}

impl Default for FsmBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl FsmBuilder {
    pub fn new() -> Self {
        Self {
            definitions: DefinitionAccumulator::new(),
            limits: CompileLimits::default(),
        }
    }

    /// Creates a builder with caller-tightened compile limits.
    pub fn with_limits(limits: CompileLimits) -> Self {
        Self {
            definitions: DefinitionAccumulator::new(),
            limits,
        }
    }

    /// Adds one typed lingrep patterns definition without resolving its references.
    ///
    /// References are bound only when [`Self::build`] consumes the complete
    /// definition set, so later registrations may satisfy forward references.
    pub fn add_definition(&mut self, source: PatternSource<'_>) -> Result<(), CompileError> {
        self.definitions.add(source, &self.limits)
    }

    /// Adds every definition from one complete lingrep patterns document without binding
    /// references until [`Self::build`] consumes the accumulated catalog.
    pub fn add_document(&mut self, source: PatternSource<'_>) -> Result<(), CompileError> {
        self.definitions.add_document(source, &self.limits)
    }

    /// Atomically binds all definitions and builds the immutable matcher.
    pub fn build(self) -> Result<CompileOutput, CompileError> {
        self.definitions.build(&self.limits)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::token_match::{TokenAttributeIndex, TokenInfo};

    fn token(orth: &str, pos1: &str) -> TokenInfo {
        let mut token = TokenInfo::default();
        token.set(TokenAttributeIndex::Orth, orth.into());
        token.set(TokenAttributeIndex::Pos1, pos1.into());
        token
    }

    #[test]
    fn compiled_matcher_handles_plain_nfa_definitions() {
        let mut builder = FsmBuilder::new();
        builder
            .add_definition(PatternSource::named("p.lx", "p := [text=a]."))
            .unwrap();
        let matcher = builder.build().unwrap().matcher;

        assert_eq!(matcher.find_matches(&[token("a", "X")]).len(), 1);
    }

    #[test]
    fn duplicate_names_fail_atomically_at_build() {
        let mut builder = FsmBuilder::new();
        builder
            .add_definition(PatternSource::named("first.lx", "p := [text=a]."))
            .unwrap();
        builder
            .add_definition(PatternSource::named("second.lx", "p := [text=b]."))
            .unwrap();

        assert!(builder.build().is_err());
    }

    #[test]
    fn builder_does_not_invent_metadata_for_a_pattern_name() {
        let mut builder = FsmBuilder::new();
        builder
            .add_definition(PatternSource::named(
                "test-pattern.lx",
                "test_pattern := [text=a].",
            ))
            .unwrap();

        let matcher = builder.build().unwrap().matcher;
        assert!(matcher.patterns()["test_pattern"].metadata.is_empty());
    }

    #[test]
    fn builder_preserves_declared_metadata() {
        let mut builder = FsmBuilder::new();
        builder
            .add_definition(PatternSource::named(
                "metadata.lx",
                "category: \"observed\"\np := [text=a].",
            ))
            .unwrap();

        let matcher = builder.build().unwrap().matcher;
        assert_eq!(matcher.patterns()["p"].metadata["category"], "observed");
    }
}
