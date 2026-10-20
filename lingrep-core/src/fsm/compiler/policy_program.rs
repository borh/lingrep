//! The `policy` block's compile-time half: lowering its CST into the authored
//! [`Program`], and the static rules over the program that results.
//!
//! The IR and its binding live in [`crate::fsm::policy_program`]; this module
//! is the one that reads syntax and judges a catalog. The split matters
//! because the two answer different questions: a defect here is *what the
//! author wrote*, and a defect there is *what the catalog resolves to*.
//!
//! The rules divide by what they need, not by when they run.
//! [`lower_policy_block`] owns everything answerable from the statement alone;
//! [`check_policy_program`] owns everything needing the catalog, and runs on
//! the **bound** program because that is what "needing the catalog" means.
//!
//! **Argument domains are resolved here rather than in a later pass**, because
//! the domain decides how a node is read. `relation: after` arrives as a
//! `policy_enum` and `relation: "after"` as a `metadata_string`; a pass running
//! after lowering would have only the lowered value and would have to report
//! the second as an unknown enum — naming the wrong defect. The author's
//! mistake is the quoting, not the word.

use std::collections::BTreeMap;
use std::sync::Arc;

use compact_str::CompactString;
use tree_sitter::Node;

use super::PatternSource;
use super::diagnostic::{Diagnostic, DiagnosticCode, DiagnosticSeverity};
use super::lower::diagnostic_at;
use super::model::{
    BoundDefinition, BoundExpr, DefinitionVisibility, ParsedDefinition, SourceId, SourceRegistry,
    SourceSpan,
};
use super::policy::quoted_list;
use crate::fsm::policy::{Terminal, levenshtein_at_most_one};
use crate::fsm::policy_program::{
    BoundOp, BoundProgram, BoundSubject, Criterion, DefId, DefinitionDomain, Effect, Evidence,
    GroupIndex, Op, Operand, PolicySpan, Program, Relation, StatementLabel, Subject,
};

/// The arguments `resolve` takes.
///
/// A table per verb rather than a `match` per rule, so a verb that forgets an
/// argument is a missing table entry rather than a silently accepted key, and
/// so the suggester reads the same source the checker does.
const RESOLVE_KEYS: &[&str] = &["rank", "partition"];

/// The arguments `exclude`, `require` and `detach` take.
const ASSOCIATE_KEYS: &[&str] = &["with", "relation", "evidence"];

/// The arguments `admit` takes.
const ADMIT_KEYS: &[&str] = &["terminal"];

/// Every `relation:` the language admits, and its spelling.
///
/// A table rather than a `match` so the checker and any suggester read one
/// source: a spelling that reaches the enum without reaching the diagnostic is
/// exactly the drift a second list invites.
pub(super) const RELATIONS: &[(&str, Relation)] = &[
    ("at_start", Relation::AtStart),
    ("at_end", Relation::AtEnd),
    ("after", Relation::After),
    ("before", Relation::Before),
    ("within", Relation::Within),
    ("in_sentence", Relation::InSentence),
];

/// Every `evidence:` the language admits. There is no default.
pub(super) const EVIDENCES: &[(&str, Evidence)] =
    &[("raw", Evidence::Raw), ("current", Evidence::Current)];

/// Every `rank:` criterion the language admits.
pub(super) const CRITERIA: &[(&str, Criterion)] = &[
    ("longest", Criterion::Longest),
    ("shortest", Criterion::Shortest),
    ("priority_order", Criterion::PriorityOrder),
    ("start", Criterion::Start),
    ("end", Criterion::End),
    ("pattern", Criterion::Pattern),
];

/// Lowers one `policy_block` node into the authored program.
///
/// Diagnostics are collected **across statements** and **within** one, so a
/// compile reports every malformed statement and every bad value in each,
/// rather than one per edit. That matches `Program::bind`, which collects a
/// statement's subject and operand together; an author fixing one value should
/// not be told about the next on the following compile.
///
/// One short-circuit survives, and it is deliberate rather than residual: an
/// unrecognized argument key is reported alone, because it explains every other
/// complaint the statement would draw. See [`PolicyLowerer::check_argument_keys`].
///
/// # Errors
///
/// Every statement whose verb, subject, arguments or values the language does
/// not admit.
pub(super) fn lower_policy_block(
    block: Node<'_>,
    source: &PatternSource<'_>,
    source_label: &Arc<str>,
    source_id: SourceId,
) -> Result<Program, Vec<Diagnostic>> {
    let lowerer = PolicyLowerer {
        source,
        source_label,
        source_id,
    };
    lowerer.lower_block(block)
}

/// Every static rule that needs the catalog, not just the statement.
///
/// Separate from lowering because these are questions the syntax cannot
/// answer: whether an operand is a fragment needs the definition list, and
/// expanding a `%group` operand needs the index. It runs on the **bound**
/// program for exactly that reason.
///
/// Diagnostics are collected rather than returned one at a time, so one
/// compile reports every violated rule. They carry their own severity: a
/// statement the engine cannot execute is an error, and one it executes over an
/// empty set is a warning, exactly as [`extract_policy`] splits its own.
///
/// [`extract_policy`]: super::policy::extract_policy
pub(super) fn check_policy_program(
    program: &BoundProgram,
    definitions: &[ParsedDefinition],
    groups: &GroupIndex,
    domain: &DefinitionDomain,
    sources: &SourceRegistry,
) -> Vec<Diagnostic> {
    let mut diagnostics = Vec::new();
    let fragment_name = |definition: DefId| {
        definitions
            .get(definition.index())
            .filter(|held| held.visibility() == Some(DefinitionVisibility::Fragment))
            .map(|held| &held.name)
    };
    let diagnostic = |span: PolicySpan, message: String| {
        super::policy::diagnostic_at(
            sources,
            span.0,
            DiagnosticCode::InvalidPolicyValue,
            message,
            DiagnosticSeverity::Error,
        )
    };
    let warning = |span: PolicySpan, message: String| {
        super::policy::diagnostic_at(
            sources,
            span.0,
            DiagnosticCode::IneffectivePolicy,
            message,
            DiagnosticSeverity::Warning,
        )
    };

    // A subject that selects nothing. The statement is well-formed and the
    // engine runs it; it simply runs it over an empty set, on every sentence,
    // forever. Inert and honored look identical from the catalog, which is the
    // same reason an unreferenced anchor warns.
    //
    // Stated over the **subject domain** rather than over group membership,
    // which is what makes one rule cover every spelling. A `%group` is the
    // interesting case — its members are all fragments, so it holds definitions
    // and still selects none — but `@fragment` and an `all` in a catalog with no
    // publishable pattern are the same defect, and a rule that named only the
    // group would leave them silent.
    for op in &program.ops {
        let subject = op.subject();
        if !groups
            .expand_subject(subject, domain.eligible_for(subject))
            .is_empty()
        {
            continue;
        }
        let cause = match subject {
            BoundSubject::All => "this catalog declares no publishable definition, so `all` selects nothing"
                .to_owned(),
            BoundSubject::Patterns => "this catalog declares no publishable ordinary pattern, so `patterns` selects nothing".to_owned(),
            BoundSubject::Def(id) => format!(
                "`{}` never emits a record, so no record can carry its name",
                domain.name(id)
            ),
            BoundSubject::Group(group) => format!(
                "no member of `%{}` emits a record: a group holding only fragments and                  synthesized evidence emitters selects nothing",
                groups.name(group)
            ),
        };
        diagnostics.push(warning(
            op.span(),
            format!(
                "this statement can never affect a record — {cause}; name a pattern, or delete                  the statement"
            ),
        ));
    }

    for op in &program.ops {
        let BoundOp::Associate {
            effect,
            operand,
            relation,
            evidence,
            span,
            ..
        } = op
        else {
            continue;
        };

        // A fragment never emits, so it is never in the survivor set. Drawing
        // operands from that set therefore finds nothing, and the association
        // silently holds for nothing rather than failing. `raw` is the set the
        // fragment's synthesized evidence emitter contributes to.
        if *evidence == Evidence::Current {
            for definition in groups.expand_operand(*operand) {
                if let Some(name) = fragment_name(definition) {
                    diagnostics.push(diagnostic(
                        *span,
                        format!(
                            "this operand reaches the fragment `{name}`, and `evidence: current` \
                             draws from the surviving record set; a fragment never emits, so it \
                             has no survivor to be found there — declare `evidence: raw`, which \
                             is what its synthesized evidence emitter contributes to"
                        ),
                    ));
                }
            }
        }

        // `detach` removes a region *at an edge* of the published span. The
        // other four relations locate an operand without naming a region to
        // remove, so there is nothing for the verb to do with them.
        if *effect == Effect::Detach && !matches!(relation, Relation::AtStart | Relation::AtEnd) {
            let spelling = RELATIONS
                .iter()
                .find(|(_, held)| held == relation)
                .map_or("that relation", |(spelling, _)| spelling);
            diagnostics.push(diagnostic(
                *span,
                format!(
                    "`detach` removes a region at an edge of the published span, and \
                     `{spelling}` names no such region; `detach` takes `at_start` or `at_end`"
                ),
            ));
        }
    }

    // A `within` window so wide it admits nearly every record. Executable
    // exactly as written, so this is a warning: what is wrong is the magnitude,
    // and refusing it would be the compiler overruling a declaration it can
    // honor.
    for op in &program.ops {
        let BoundOp::Admit {
            terminal: Terminal::Within(within),
            span,
            ..
        } = op
        else {
            continue;
        };
        if *within > IMPLAUSIBLE_TERMINAL_WINDOW {
            diagnostics.push(super::policy::diagnostic_at(
                sources,
                span.0,
                DiagnosticCode::ImplausiblePolicyValue,
                format!(
                    "this `admit` accepts a record ending up to {within} tokens before its \
                     anchor, which is nearly every record; a sentence-final window is normally \
                     at most {IMPLAUSIBLE_TERMINAL_WINDOW} tokens"
                ),
                DiagnosticSeverity::Warning,
            ));
        }
    }

    diagnostics
}

/// The widest `within` window that reads as a sentence-final claim.
///
/// Not a limit: a wider one compiles and runs. It is the threshold past which
/// the declaration stops distinguishing records, which is a statement about
/// what the author probably meant rather than about what the engine can do.
const IMPLAUSIBLE_TERMINAL_WINDOW: u32 = 64;

/// Whether a rank orders every pair of distinct records.
///
/// Total exactly when it ends with `Start`, `End`, `Pattern` — and that tail is
/// total **only because the emitter dedups on `(pattern, start, end)`**
/// (`matcher/emit.rs`'s `MatchKey`), so no two distinct raw records share all
/// three. The premise is recorded here because a change to that dedup key
/// silently invalidates this rule rather than breaking it.
///
/// Positional, not membership: the tail must be the *last* three, because a
/// criterion after `Pattern` would be unreachable and one before it leaves
/// pairs undecided.
///
/// The tail is required rather than appended. Appending it would make a rank
/// mean something the author did not write, and a declaration that does not say
/// what it does is the failure this whole tier exists to close.
fn rank_is_total(rank: &[Criterion]) -> bool {
    rank.len() >= TOTAL_TAIL.len() && rank[rank.len() - TOTAL_TAIL.len()..] == TOTAL_TAIL
}

/// The criteria that, together and in this order, decide every pair of
/// distinct records. See [`rank_is_total`] for why they suffice.
const TOTAL_TAIL: [Criterion; 3] = [Criterion::Start, Criterion::End, Criterion::Pattern];

/// A criterion's authored spelling, for a diagnostic.
fn spelling_of<T: PartialEq>(value: &T, domain: &[(&'static str, T)]) -> &'static str {
    domain
        .iter()
        .find(|(_, held)| held == value)
        .map_or("that criterion", |(spelling, _)| *spelling)
}

/// [`quoted_list`] over a domain table's spellings.
///
/// A projection onto the one renderer rather than a second one: a catalog
/// diagnostic and a policy diagnostic listing a closed set should not be able
/// to disagree about how a set is punctuated.
fn quoted_list_of<T>(domain: &[(&str, T)]) -> String {
    let spellings: Vec<&str> = domain.iter().map(|(spelling, _)| *spelling).collect();
    quoted_list(&spellings)
}

struct PolicyLowerer<'a, 'src> {
    source: &'a PatternSource<'src>,
    source_label: &'a Arc<str>,
    source_id: SourceId,
}

/// One `policy_argument`, unread.
///
/// The value is kept as a node rather than a lowered value because the node's
/// *kind* is the domain evidence — see this module's header.
struct Argument<'tree> {
    key: CompactString,
    key_node: Node<'tree>,
    value: Node<'tree>,
}

impl<'src> PolicyLowerer<'_, 'src> {
    fn lower_block(&self, block: Node<'_>) -> Result<Program, Vec<Diagnostic>> {
        let mut ops = Vec::new();
        let mut labels = Vec::new();
        let mut diagnostics = Vec::new();
        let mut cursor = block.walk();
        for child in block.named_children(&mut cursor) {
            match child.kind() {
                "comment" => {}
                // This is the only stage holding both the statement node and
                // the text it was cut from, so the label is built here rather
                // than reconstructed from the bound IR downstream.
                "policy_statement" => match self.lower_statement(child) {
                    Ok(op) => {
                        ops.push(op);
                        labels.push(self.label_for(child));
                    }
                    Err(reported) => diagnostics.extend(reported),
                },
                other => diagnostics.push(self.error(
                    child,
                    DiagnosticCode::UnknownNode,
                    format!("unknown named node `{other}` in a `policy` block"),
                )),
            }
        }

        if diagnostics.is_empty() {
            Ok(Program { ops, labels })
        } else {
            Err(diagnostics)
        }
    }

    /// How this statement is named to a human.
    ///
    /// The node's own source text and its 1-based opening line. Tree-sitter
    /// counts rows from zero and every other diagnostic in this compiler adds
    /// the one, so this does too — a label that disagreed with the diagnostic
    /// pointing at the same statement would be worse than no label.
    fn label_for(&self, node: Node<'_>) -> StatementLabel {
        let text = node
            .utf8_text(self.source.text.as_bytes())
            .unwrap_or_default();
        let line = u32::try_from(node.start_position().row.saturating_add(1)).unwrap_or(u32::MAX);
        StatementLabel::new(text, line)
    }

    fn lower_statement(&self, node: Node<'_>) -> Result<Op, Vec<Diagnostic>> {
        let span = self.span_of(node)?;
        let verb_node = self.field(node, "verb")?;
        let verb = self.text(verb_node)?;
        let subject = self.lower_subject(self.field(node, "subject")?)?;
        let mut arguments = self.collect_arguments(node)?;

        // Checked before any argument is read. An unknown key is the mistake
        // that explains every other complaint the statement would draw — told
        // "this statement declares no `with:`" first, an author who typed
        // `wth:` has to find the typo unaided.
        let table = match verb {
            "resolve" => RESOLVE_KEYS,
            "exclude" | "require" | "detach" => ASSOCIATE_KEYS,
            "admit" => ADMIT_KEYS,
            other => {
                return Err(vec![self.error(
                    verb_node,
                    DiagnosticCode::UnknownNode,
                    format!("unknown policy verb `{other}`"),
                )]);
            }
        };
        self.check_argument_keys(&arguments, verb, table)?;

        // Values are collected, not short-circuited: a statement declaring two
        // malformed values reports both. `take_required` is collected the same
        // way, so a statement missing two keys names both of them.
        //
        // The key check above is the one deliberate short-circuit, and its
        // reason is stated there: an unknown key explains every complaint the
        // statement would otherwise draw, and reporting the consequences beside
        // the cause is what makes an author hunt for a typo.
        let mut reported = Vec::new();
        let op = match verb {
            "resolve" => {
                let partition = match take_optional(&mut arguments, "partition") {
                    Some(argument) => self
                        .partition_key(argument.value)
                        .map_err(|errors| reported.extend(errors))
                        .ok(),
                    None => None,
                };
                let rank = self
                    .take_required(&mut arguments, "rank", node)
                    .and_then(|rank| self.lower_rank(rank.value))
                    .map_err(|errors| reported.extend(errors))
                    .ok();
                // Guarded on `reported` as well as on `rank`: a malformed
                // `partition:` must not lower just because the rank is fine.
                match rank {
                    Some(rank) if reported.is_empty() => Some(Op::Resolve {
                        subject,
                        rank,
                        partition,
                        span,
                    }),
                    _ => None,
                }
            }
            "exclude" | "require" | "detach" => {
                let effect = match verb {
                    "exclude" => Effect::Exclude,
                    "require" => Effect::Require,
                    _ => Effect::Detach,
                };
                let operand = self
                    .take_required(&mut arguments, "with", node)
                    .and_then(|with| self.lower_operand(with.value))
                    .map_err(|errors| reported.extend(errors))
                    .ok();
                let relation = self
                    .take_required(&mut arguments, "relation", node)
                    .and_then(|held| self.lower_enum(held.value, "relation", RELATIONS))
                    .map_err(|errors| reported.extend(errors))
                    .ok();
                let evidence = self
                    .take_required(&mut arguments, "evidence", node)
                    .and_then(|held| self.lower_enum(held.value, "evidence", EVIDENCES))
                    .map_err(|errors| reported.extend(errors))
                    .ok();
                match (operand, relation, evidence) {
                    (Some(operand), Some(relation), Some(evidence)) => Some(Op::Associate {
                        effect,
                        subject,
                        operand,
                        relation,
                        evidence,
                        span,
                    }),
                    _ => None,
                }
            }
            "admit" => self
                .take_required(&mut arguments, "terminal", node)
                .and_then(|terminal| self.lower_terminal(terminal.value))
                .map(|(distance, anchor)| Op::Admit {
                    subject,
                    terminal: distance,
                    anchor,
                    span,
                })
                .map_err(|errors| reported.extend(errors))
                .ok(),
            // Unreachable: the key table above already rejected every other
            // spelling, and it is the same `match`.
            other => {
                return Err(vec![self.error(
                    verb_node,
                    DiagnosticCode::UnknownNode,
                    format!("unknown policy verb `{other}`"),
                )]);
            }
        };

        // Guarded on success, because a statement missing a required key leaves
        // its other arguments unconsumed and that is the reported failure rather
        // than a broken invariant.
        let Some(op) = op else {
            debug_assert!(
                !reported.is_empty(),
                "a statement that did not lower says why"
            );
            return Err(reported);
        };
        debug_assert!(
            arguments.is_empty(),
            "every key admitted by the verb's table is consumed by its arm"
        );
        debug_assert!(reported.is_empty(), "a lowered statement reports nothing");
        Ok(op)
    }

    /// Rejects an argument the verb does not have, suggesting the nearest one.
    ///
    /// The suggestion reuses the policy-key one-edit measure and reads the
    /// verb's own table, so a key that reaches the checker
    /// without reaching the suggester cannot exist.
    fn check_argument_keys(
        &self,
        arguments: &[Argument<'_>],
        verb: &str,
        table: &[&str],
    ) -> Result<(), Vec<Diagnostic>> {
        let mut diagnostics = Vec::new();
        for argument in arguments {
            if table.contains(&argument.key.as_str()) {
                continue;
            }
            // `partition:` on the wrong verb is its own defect, not a typo: the
            // key subdivides a contest, and only `resolve` holds one. The
            // generic message below would send the author hunting for a
            // spelling mistake that is not there.
            if argument.key == "partition" {
                diagnostics.push(self.error(
                    argument.key_node,
                    DiagnosticCode::PartitionOnNonResolve,
                    format!(
                        "`partition:` subdivides a resolve contest into per-class contests, and \
                         `{verb}` holds no contest; only `resolve` takes it"
                    ),
                ));
                continue;
            }
            let suggestion = table
                .iter()
                .find(|candidate| levenshtein_at_most_one(&argument.key, candidate))
                .map_or_else(
                    || format!("its arguments are {}", quoted_list(table)),
                    |candidate| format!("did you mean `{candidate}:`?"),
                );
            diagnostics.push(self.error(
                argument.key_node,
                DiagnosticCode::Syntax,
                format!("`{verb}` has no argument `{}`; {suggestion}", argument.key),
            ));
        }

        if diagnostics.is_empty() {
            Ok(())
        } else {
            Err(diagnostics)
        }
    }

    fn lower_subject(&self, node: Node<'_>) -> Result<Subject, Vec<Diagnostic>> {
        let Some(reference) = node.named_child(0) else {
            return Ok(if self.text(node)?.trim() == "patterns" {
                Subject::Patterns
            } else {
                Subject::All
            });
        };
        match reference.kind() {
            "pattern_reference" => Ok(Subject::Def(self.reference_name(reference)?)),
            "group_ref" => Ok(Subject::Group(self.reference_name(reference)?)),
            other => Err(vec![self.error(
                reference,
                DiagnosticCode::UnknownNode,
                format!("unknown subject node `{other}`"),
            )]),
        }
    }

    fn lower_operand(&self, node: Node<'_>) -> Result<Operand, Vec<Diagnostic>> {
        match node.kind() {
            "pattern_reference" => Ok(Operand::Def(self.reference_name(node)?)),
            "group_ref" => Ok(Operand::Group(self.reference_name(node)?)),
            _ => Err(vec![self.error(
                node,
                DiagnosticCode::Syntax,
                "`with:` takes a definition (`@name`) or a group (`%name`)",
            )]),
        }
    }

    /// The identifier a `@name` or `%name` reference carries.
    fn reference_name(&self, node: Node<'_>) -> Result<CompactString, Vec<Diagnostic>> {
        let identifier = node.named_child(0).ok_or_else(|| {
            vec![self.error(node, DiagnosticCode::Syntax, "reference has no name")]
        })?;
        Ok(CompactString::from(self.text(identifier)?))
    }

    fn lower_rank(&self, node: Node<'_>) -> Result<Vec<Criterion>, Vec<Diagnostic>> {
        if node.kind() != "policy_list" {
            return Err(vec![self.error(
                node,
                DiagnosticCode::Syntax,
                "`rank:` takes a list of criteria",
            )]);
        }
        let mut criteria = Vec::new();
        let mut diagnostics = Vec::new();
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            if child.kind() == "comment" {
                continue;
            }
            match self.lower_enum(child, "rank", CRITERIA) {
                Ok(criterion) => criteria.push(criterion),
                Err(reported) => diagnostics.extend(reported),
            }
        }
        if !diagnostics.is_empty() {
            return Err(diagnostics);
        }
        self.check_rank(&criteria, node)?;
        Ok(criteria)
    }

    /// The three rules a `rank:` must satisfy, reported at the list.
    ///
    /// Here rather than in [`check_policy_program`] because all three are
    /// answerable from the statement alone — the module header's division.
    fn check_rank(&self, rank: &[Criterion], node: Node<'_>) -> Result<(), Vec<Diagnostic>> {
        let mut diagnostics = Vec::new();

        // A criterion appearing twice is dead the second time: the first
        // occurrence already decided every pair it can decide, so the repeat
        // orders nothing. It is an authoring mistake, not a no-op to absorb.
        for (index, criterion) in rank.iter().enumerate() {
            if rank[..index].contains(criterion) {
                diagnostics.push(self.error(
                    node,
                    DiagnosticCode::Syntax,
                    format!(
                        "`rank:` names `{}` twice; the second decides nothing the first has not \
                         already decided",
                        spelling_of(criterion, CRITERIA)
                    ),
                ));
            }
        }

        // `longest` and `shortest` are opposite orders on one dimension.
        // Whichever comes first decides every pair of unequal width, so the
        // other is unreachable — and a rank declaring both reads as though it
        // expressed a preference it cannot express.
        if rank.contains(&Criterion::Longest) && rank.contains(&Criterion::Shortest) {
            diagnostics.push(self.error(
                node,
                DiagnosticCode::Syntax,
                "`rank:` names both `longest` and `shortest`, which are opposite orders on one \
                 dimension; whichever comes first decides every pair of unequal width, so the \
                 other can never be reached",
            ));
        }

        // Exactly one of these fires. A rank carrying the tail somewhere other
        // than the end *is* total — the tail decides every pair the moment it
        // is reached — so calling it non-total would name the wrong defect.
        // What is wrong is that nothing after `pattern` can ever run.
        if !rank_is_total(rank) {
            if rank
                .windows(TOTAL_TAIL.len())
                .any(|window| window == TOTAL_TAIL)
            {
                diagnostics.push(self.error(
                    node,
                    DiagnosticCode::Syntax,
                    "`rank:` names criteria after `pattern`, which can never be reached: \
                     `start, end, pattern` already orders every pair of distinct records, so \
                     nothing follows it",
                ));
            } else {
                diagnostics.push(self.error(
                    node,
                    DiagnosticCode::Syntax,
                    "a `rank:` must be total — it must order every pair of distinct records, or \
                     a resolve contest has no determinate winner; end it with \
                     `start, end, pattern`, which the language will not append for you",
                ));
            }
        }

        if diagnostics.is_empty() {
            Ok(())
        } else {
            Err(diagnostics)
        }
    }

    /// The `terminal:` mapping, which carries the distance **and** the anchor.
    ///
    /// One argument rather than two because they decide one thing together: a
    /// distance is measured *from* somewhere, and splitting them would admit a
    /// statement declaring a distance against nothing. The metadata key of the
    /// same name spells its anchor as a quoted string; here it is a bare enum,
    /// which is the whole point of the enum domain — an anchor is a reference,
    /// and a reference the compiler resolves should not look like free text.
    fn lower_terminal(&self, node: Node<'_>) -> Result<(Terminal, CompactString), Vec<Diagnostic>> {
        if node.kind() != "policy_map" {
            return Err(vec![self.error(
                node,
                DiagnosticCode::Syntax,
                "`terminal:` takes a mapping naming a distance and an anchor, as in \
                 `{exact: true, anchor: propositional_end}`",
            )]);
        }

        let mut exact = None;
        let mut within = None;
        let mut anchor = None;
        let mut diagnostics = Vec::new();
        let mut cursor = node.walk();
        for entry in node.named_children(&mut cursor) {
            if entry.kind() == "comment" {
                continue;
            }
            let key_node = match self.field(entry, "key") {
                Ok(found) => found,
                Err(reported) => {
                    diagnostics.extend(reported);
                    continue;
                }
            };
            let value = match self.field(entry, "value") {
                Ok(found) => found,
                Err(reported) => {
                    diagnostics.extend(reported);
                    continue;
                }
            };
            let key = match self.text(key_node) {
                Ok(found) => found,
                Err(reported) => {
                    diagnostics.extend(reported);
                    continue;
                }
            };
            match key {
                "exact" => match self.boolean(value) {
                    Ok(flag) => exact = Some((flag, value)),
                    Err(reported) => diagnostics.extend(reported),
                },
                "within" => match self.token_count(value) {
                    Ok(count) => within = Some(count),
                    Err(reported) => diagnostics.extend(reported),
                },
                "anchor" => match self.anchor_name(value) {
                    Ok(name) => anchor = Some(name),
                    Err(reported) => diagnostics.extend(reported),
                },
                other => diagnostics.push(self.error(
                    key_node,
                    DiagnosticCode::Syntax,
                    format!(
                        "a `terminal:` mapping has no field `{other}`; its fields are `exact`, \
                         `within` and `anchor`"
                    ),
                )),
            }
        }

        if !diagnostics.is_empty() {
            return Err(diagnostics);
        }

        let distance = match (exact, within) {
            (Some((true, _)), None) => Terminal::Exact,
            (None, Some(count)) => Terminal::Within(count),
            // `exact: false` is refused rather than read as "no constraint",
            // in the same words the metadata key uses: a disabled constraint
            // should be deleted, because an inert declaration that still looks
            // executable is the failure this tier exists to close.
            (Some((false, node)), _) => {
                return Err(vec![self.error(
                    node,
                    DiagnosticCode::InvalidPolicyValue,
                    "`exact: false` switches nothing off; delete the whole `admit` statement",
                )]);
            }
            (Some(_), Some(_)) => {
                return Err(vec![self.error(
                    node,
                    DiagnosticCode::Syntax,
                    "a `terminal:` mapping declares `exact` or `within`, not both",
                )]);
            }
            (None, None) => {
                return Err(vec![self.error(
                    node,
                    DiagnosticCode::Syntax,
                    "a `terminal:` mapping declares a distance: `exact: true` or `within: <n>`",
                )]);
            }
        };

        let anchor = anchor.ok_or_else(|| {
            vec![self.error(
                node,
                DiagnosticCode::Syntax,
                "a `terminal:` mapping names the anchor it measures against, as in \
                 `anchor: propositional_end`",
            )]
        })?;

        Ok((distance, anchor))
    }

    /// Resolves a bare identifier against a closed, compiler-owned domain.
    ///
    /// A quoted string reaching here is the author spelling an enum as free
    /// text. It gets its own sentence rather than "unknown value", because
    /// what is wrong is the quoting and the word is right.
    fn lower_enum<T: Copy>(
        &self,
        node: Node<'_>,
        key: &str,
        domain: &[(&str, T)],
    ) -> Result<T, Vec<Diagnostic>> {
        if node.kind() != "policy_enum" {
            return Err(vec![self.quoting_error(node, key, domain)]);
        }
        let text = self.text(node)?;
        domain
            .iter()
            .find(|(spelling, _)| *spelling == text)
            .map(|(_, value)| *value)
            .ok_or_else(|| {
                // Use the same one-edit measure on the value domain. `aftr` is
                // one edit from `after`, and being told only
                // that the value is unknown leaves the author reading a list
                // to find what they already nearly typed.
                let suggestion = domain
                    .iter()
                    .find(|(spelling, _)| levenshtein_at_most_one(text, spelling))
                    .map_or_else(
                        || format!("its values are {}", quoted_list_of(domain)),
                        |(spelling, _)| format!("did you mean `{spelling}`?"),
                    );
                vec![self.error(
                    node,
                    DiagnosticCode::Syntax,
                    format!("`{key}:` has no value `{text}`; {suggestion}"),
                )]
            })
    }

    /// A value in an enum position that is not a bare identifier.
    ///
    /// A quoted string gets its own sentence naming the **bare spelling**,
    /// because the word is right and the quoting is the mistake. Reporting it
    /// as an unknown enum would name the wrong defect, and a later pass could
    /// not tell the two apart — it would hold a decoded value with no memory
    /// of how it was written.
    fn quoting_error<T>(&self, node: Node<'_>, key: &str, domain: &[(&str, T)]) -> Diagnostic {
        if node.kind() == "metadata_string"
            && let Ok(text) = self.text(node)
            && let Ok(decoded) = serde_json::from_str::<String>(text)
            && domain.iter().any(|(spelling, _)| *spelling == decoded)
        {
            return self.error(
                node,
                DiagnosticCode::Syntax,
                format!("`{key}:` takes the bare spelling; write `{key}: {decoded}`"),
            );
        }
        self.error(
            node,
            DiagnosticCode::Syntax,
            format!(
                "`{key}:` takes one of its bare spellings, not {}; they are {}",
                self.shape(node),
                quoted_list_of(domain)
            ),
        )
    }

    /// The metadata key a `partition:` names.
    ///
    /// A bare identifier, like `anchor:` and for the same reason: the key is a
    /// reference the compiler resolves against the catalog's metadata, and a
    /// reference the compiler resolves should not look like free text.
    fn partition_key(&self, node: Node<'_>) -> Result<CompactString, Vec<Diagnostic>> {
        if node.kind() != "policy_enum" {
            return Err(vec![self.error(
                node,
                DiagnosticCode::Syntax,
                format!(
                    "`partition:` names a metadata key without quoting it, not {}",
                    self.shape(node)
                ),
            )]);
        }
        Ok(CompactString::from(self.text(node)?))
    }

    fn anchor_name(&self, node: Node<'_>) -> Result<CompactString, Vec<Diagnostic>> {
        if node.kind() != "policy_enum" {
            return Err(vec![self.error(
                node,
                DiagnosticCode::Syntax,
                format!(
                    "`anchor:` names a declared anchor without quoting it, not {}",
                    self.shape(node)
                ),
            )]);
        }
        Ok(CompactString::from(self.text(node)?))
    }

    fn boolean(&self, node: Node<'_>) -> Result<bool, Vec<Diagnostic>> {
        if node.kind() != "metadata_boolean" {
            return Err(vec![self.error(
                node,
                DiagnosticCode::Syntax,
                format!("expected `true` or `false`, not {}", self.shape(node)),
            )]);
        }
        Ok(self.text(node)? == "true")
    }

    fn token_count(&self, node: Node<'_>) -> Result<u32, Vec<Diagnostic>> {
        if node.kind() != "metadata_number" {
            return Err(vec![self.error(
                node,
                DiagnosticCode::Syntax,
                format!("expected a token count, not {}", self.shape(node)),
            )]);
        }
        self.text(node)?.parse::<u32>().map_err(|_| {
            vec![self.error(
                node,
                DiagnosticCode::Syntax,
                format!("expected a token count in 0..={}", u32::MAX),
            )]
        })
    }

    /// How a value node reads in a diagnostic, so a message can name what
    /// arrived rather than only what was wanted.
    fn shape(&self, node: Node<'_>) -> &'static str {
        match node.kind() {
            "metadata_string" => "a quoted string",
            "metadata_number" => "a number",
            "metadata_boolean" => "a boolean",
            "policy_list" => "a list",
            "policy_map" => "a mapping",
            "pattern_reference" => "a definition reference",
            "group_ref" => "a group reference",
            "policy_enum" => "a bare identifier",
            _ => "that",
        }
    }

    fn collect_arguments<'tree>(
        &self,
        statement: Node<'tree>,
    ) -> Result<Vec<Argument<'tree>>, Vec<Diagnostic>> {
        let mut arguments: Vec<Argument<'tree>> = Vec::new();
        let mut diagnostics = Vec::new();
        let mut cursor = statement.walk();
        for node in statement.children_by_field_name("argument", &mut cursor) {
            let key_node = match self.field(node, "key") {
                Ok(found) => found,
                Err(reported) => {
                    diagnostics.extend(reported);
                    continue;
                }
            };
            let value = match self.field(node, "value") {
                Ok(found) => found,
                Err(reported) => {
                    diagnostics.extend(reported);
                    continue;
                }
            };
            let key = match self.text(key_node) {
                Ok(found) => CompactString::from(found),
                Err(reported) => {
                    diagnostics.extend(reported);
                    continue;
                }
            };
            if arguments.iter().any(|held| held.key == key) {
                // Reported at the later of the two, which is the one the
                // author added — the same convention `metadata_entry` uses.
                diagnostics.push(self.error(
                    key_node,
                    DiagnosticCode::Syntax,
                    format!("duplicate argument `{key}`"),
                ));
                continue;
            }
            arguments.push(Argument {
                key,
                key_node,
                value,
            });
        }

        if diagnostics.is_empty() {
            Ok(arguments)
        } else {
            Err(diagnostics)
        }
    }

    fn take_required<'tree>(
        &self,
        arguments: &mut Vec<Argument<'tree>>,
        key: &str,
        statement: Node<'_>,
    ) -> Result<Argument<'tree>, Vec<Diagnostic>> {
        arguments
            .iter()
            .position(|held| held.key == key)
            .map(|index| arguments.remove(index))
            .ok_or_else(|| {
                vec![self.error(
                    statement,
                    DiagnosticCode::Syntax,
                    format!("this statement declares no `{key}:`"),
                )]
            })
    }

    fn field<'tree>(&self, node: Node<'tree>, name: &str) -> Result<Node<'tree>, Vec<Diagnostic>> {
        node.child_by_field_name(name).ok_or_else(|| {
            vec![self.error(
                node,
                DiagnosticCode::Syntax,
                format!("a `{}` node has no `{name}`", node.kind()),
            )]
        })
    }

    /// The node's source text, borrowed from the **document**, not the tree.
    fn text(&self, node: Node<'_>) -> Result<&'src str, Vec<Diagnostic>> {
        node.utf8_text(self.source.text.as_bytes()).map_err(|_| {
            vec![self.error(
                node,
                DiagnosticCode::Syntax,
                "policy syntax is not valid UTF-8",
            )]
        })
    }

    fn span_of(&self, node: Node<'_>) -> Result<PolicySpan, Vec<Diagnostic>> {
        SourceSpan::new(self.source_id, node.byte_range())
            .map(PolicySpan)
            .map_err(|_| {
                vec![self.error(
                    node,
                    DiagnosticCode::LimitExceeded,
                    "source span exceeds the supported offset range",
                )]
            })
    }

    fn error(
        &self,
        node: Node<'_>,
        code: DiagnosticCode,
        message: impl Into<String>,
    ) -> Diagnostic {
        diagnostic_at(
            self.source,
            Arc::clone(self.source_label),
            node,
            code,
            message,
        )
    }
}

/// Removes and returns an argument the verb admits but does not require.
///
/// A free function beside [`PolicyLowerer::take_required`] rather than a
/// method only because it touches no lowerer state: absence is a legal answer,
/// so there is no diagnostic to build.
fn take_optional<'tree>(
    arguments: &mut Vec<Argument<'tree>>,
    key: &str,
) -> Option<Argument<'tree>> {
    arguments
        .iter()
        .position(|held| held.key == key)
        .map(|index| arguments.remove(index))
}

/// The cumulative non-emptiness obligation, narrowed to opposite-edge cases.
///
/// A single-edge `detach` cannot empty a record: `K`'s strict bound excludes a
/// whole-span operand, so the region removed is always a proper part. Two
/// detaches on *opposite* edges can still cross, and that case keeps a width
/// proof.
///
/// **The proof takes the maximum per edge, not the sum over all statements.**
/// A public span narrows monotonically — `public_start` only moves right and
/// `public_end` only moves left — so two `at_start` detaches remove the wider
/// of their regions, never both. Summing them would refuse a catalog that
/// cannot empty, and the same holds inside one statement whose `%group`
/// operand has several members: exactly one region is removed per edge.
///
/// Accumulated per [`DefId`] because `all`, `%group` and `@definition` are
/// three spellings that can select one definition. `eligible` is the subject
/// domain: a fragment can never be a subject, so it can never carry an
/// obligation.
///
/// Runs after binding, unlike [`check_policy_program`]: a definition's width is
/// a property of its resolved body, and the reference inside it is a name until
/// then.
pub(super) fn check_detach_non_emptiness(
    program: &BoundProgram,
    groups: &GroupIndex,
    domain: &DefinitionDomain,
    widths: &[(usize, Option<usize>)],
    sources: &SourceRegistry,
    names: &dyn Fn(DefId) -> CompactString,
) -> Vec<Diagnostic> {
    /// The widest region either edge can lose, and where it was declared.
    #[derive(Default)]
    struct Edge {
        widest: Option<usize>,
        unbounded: bool,
        span: Option<PolicySpan>,
    }

    let mut edges: BTreeMap<DefId, (Edge, Edge)> = BTreeMap::new();
    for op in &program.ops {
        let BoundOp::Associate {
            effect: Effect::Detach,
            subject,
            operand,
            relation,
            span,
            ..
        } = op
        else {
            continue;
        };
        // A relation naming no detachable region is already refused; reaching
        // here would mean this analysis had a different idea of which
        // statements detach than the rule that admits them.
        let at_start = match relation {
            Relation::AtStart => true,
            Relation::AtEnd => false,
            Relation::After | Relation::Before | Relation::Within | Relation::InSentence => {
                continue;
            }
        };

        // The widest member, because a `%group` operand removes one region per
        // edge and this must bound whichever member fires.
        let mut widest = Some(0usize);
        for member in groups.expand_operand(*operand) {
            match widths.get(member.index()).and_then(|(_, max)| *max) {
                Some(max) => widest = widest.map(|held| held.max(max)),
                None => widest = None,
            }
        }

        for definition in groups.expand_subject(*subject, domain.eligible_for(*subject)) {
            let entry = edges.entry(definition).or_default();
            let edge = if at_start { &mut entry.0 } else { &mut entry.1 };
            match widest {
                Some(max) => edge.widest = Some(edge.widest.unwrap_or(0).max(max)),
                None => edge.unbounded = true,
            }
            edge.span = Some(*span);
        }
    }

    let mut diagnostics = Vec::new();
    for (definition, (start, end)) in &edges {
        // Both edges, or there is nothing to prove: a lone edge cannot empty a
        // record whatever its operand's width. Reported at the `at_end`
        // statement, which is the later of the two to be admitted.
        let (Some(_), Some(span)) = (start.span, end.span) else {
            continue;
        };
        let subject_min = widths.get(definition.index()).map_or(0, |(min, _)| *min);
        let unbounded = start.unbounded || end.unbounded;
        let removed = start.widest.unwrap_or(0) + end.widest.unwrap_or(0);
        if !unbounded && removed < subject_min {
            continue;
        }

        let bound = if unbounded {
            "an operand of unbounded width".to_string()
        } else {
            format!("operands of up to {removed} tokens together")
        };
        diagnostics.push(super::policy::diagnostic_at(
            sources,
            span.0,
            DiagnosticCode::InvalidPolicyValue,
            format!(
                "`{}` is detached at both edges, and {bound} can meet inside a record of as \
                 few as {subject_min}; that would publish an empty span, which the language \
                 does not define — detach one edge, or narrow an operand",
                names(*definition)
            ),
            DiagnosticSeverity::Error,
        ));
    }
    diagnostics
}

/// Each definition's `(minimum, maximum)` token width, by [`DefId`].
///
/// `None` is unbounded. Indexed by the definition's position in the bound
/// program, which is the same index a [`DefId`] carries — `bind` numbers a
/// [`DefinitionId`] from the position in the complete definition list this is
/// built from.
///
/// Memoized rather than walked in dependency order, because the topological
/// order `bind` computes is private to it and a second one would be a second
/// thing to keep in step. A definition already in progress answers `(0, None)`:
/// binding rejects reference cycles, so that is unreachable rather than a
/// silent approximation, and it keeps this total if that ever changes.
///
/// [`DefinitionId`]: super::model::DefinitionId
pub(super) fn definition_widths(definitions: &[BoundDefinition]) -> Vec<(usize, Option<usize>)> {
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Slot {
        Unvisited,
        InProgress,
        Done((usize, Option<usize>)),
    }

    fn width(
        index: usize,
        definitions: &[BoundDefinition],
        table: &mut [Slot],
    ) -> (usize, Option<usize>) {
        match table[index] {
            Slot::Done(width) => return width,
            Slot::InProgress => return (0, None),
            Slot::Unvisited => {}
        }
        table[index] = Slot::InProgress;
        let computed = definitions[index]
            .expr()
            .map_or((0, Some(0)), |expr| expr_width(expr, definitions, table));
        table[index] = Slot::Done(computed);
        computed
    }

    fn expr_width(
        expr: &BoundExpr,
        definitions: &[BoundDefinition],
        table: &mut [Slot],
    ) -> (usize, Option<usize>) {
        match expr {
            BoundExpr::Token(_) => (1, Some(1)),
            // Zero-width by construction: an assertion consumes nothing, a
            // lookaround consumes nothing outside itself, and `same_as`
            // constrains a span some other element already consumed.
            BoundExpr::Assertion(_) | BoundExpr::Lookaround(_) | BoundExpr::SameAs { .. } => {
                (0, Some(0))
            }
            BoundExpr::Sequence(parts) => parts.iter().fold((0, Some(0)), |total, part| {
                let (min, max) = expr_width(part, definitions, table);
                (
                    total.0 + min,
                    total.1.zip(max).map(|(held, add)| held + add),
                )
            }),
            BoundExpr::Alternation(parts) => parts
                .iter()
                .map(|part| expr_width(part, definitions, table))
                .reduce(|held, next| {
                    (
                        held.0.min(next.0),
                        held.1.zip(next.1).map(|(left, right)| left.max(right)),
                    )
                })
                // An alternation with no branches matches nothing, so it
                // consumes nothing.
                .unwrap_or((0, Some(0))),
            // Every operand consumes its exact-width node, while implicit
            // gaps make the complete span unbounded above.
            BoundExpr::AnyOrder { operands, .. } => (
                operands.iter().fold(0usize, |total, operand| {
                    total.saturating_add(expr_width(operand, definitions, table).0)
                }),
                None,
            ),
            // The tightest interval both operands admit: `max` of the minima,
            // `min` of the maxima. An unbounded operand constrains nothing
            // above, so it contributes no maximum rather than erasing the
            // other's.
            BoundExpr::Intersection { operands, .. } => operands
                .iter()
                .map(|operand| expr_width(operand, definitions, table))
                .reduce(|held, next| {
                    (
                        held.0.max(next.0),
                        match (held.1, next.1) {
                            (Some(left), Some(right)) => Some(left.min(right)),
                            (Some(only), None) | (None, Some(only)) => Some(only),
                            (None, None) => None,
                        },
                    )
                })
                .unwrap_or((0, Some(0))),
            BoundExpr::Repeat { expr, min, max } => {
                let (inner_min, inner_max) = expr_width(expr, definitions, table);
                (
                    inner_min * min,
                    max.zip(inner_max)
                        .and_then(|(times, each)| each.checked_mul(times)),
                )
            }
            BoundExpr::Capture { expr, .. } => expr_width(expr, definitions, table),
            BoundExpr::Definition { id, .. } => width(id.index(), definitions, table),
        }
    }

    let mut table = vec![Slot::Unvisited; definitions.len()];
    (0..definitions.len())
        .map(|index| width(index, definitions, &mut table))
        .collect()
}
