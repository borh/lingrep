use std::collections::BTreeMap;
use tree_sitter::{Node, Parser, Tree};
use tree_sitter_lingrep::LANGUAGE;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use super::expand::expand_indexed_declaration;
use super::lower::lower_authored_document;
use super::model::{
    LookDirection, LookPolarity, ParsedDefinitionBody, ParsedExpr, ParsedProgram, SourceSpan,
};
use super::source_text::parser_text;
use super::{CompileError, CompileLimits, ExpandError, PatternSource};
use crate::fsm::PositionAssertion;

const MAX_NODES: usize = 4096;
const MAX_DEPTH: usize = 128;
const MAX_CELLS: usize = 1 << 20;
const MAX_OUTPUT_BYTES: usize = 8 << 20;

#[derive(Debug, thiserror::Error)]
pub enum DiagramError {
    #[error(transparent)]
    Compile(#[from] CompileError),
    #[error(transparent)]
    Expansion(#[from] ExpandError),
    #[error("no definition named {name}")]
    MissingDefinition { name: String },
    #[error("{name} is a {kind}; diagrams support patterns and fragments")]
    UnsupportedDefinition { name: String, kind: String },
    #[error("diagram exceeds the {0} limit")]
    Budget(&'static str),
    #[error("diagram width must be between 32 and 240 columns")]
    Width,
    #[error("pattern diagram source and compiler expression differ")]
    SourceShape,
}

pub struct DiagramCatalog {
    source: String,
    tree: Tree,
    program: ParsedProgram,
    limits: CompileLimits,
}

impl DiagramCatalog {
    /// Checks source syntax and shape; callers requiring reference binding must compile the catalog first.
    pub fn parse(source: PatternSource<'_>, limits: CompileLimits) -> Result<Self, DiagramError> {
        let program = lower_authored_document(&source, &limits)?;
        let source = parser_text(source.text).into_owned();
        let mut parser = Parser::new();
        parser
            .set_language(&LANGUAGE.into())
            .map_err(|_| DiagramError::SourceShape)?;
        let tree = parser
            .parse(&source, None)
            .ok_or(DiagramError::SourceShape)?;
        Ok(Self {
            source,
            tree,
            program,
            limits,
        })
    }

    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.program.definitions.iter().filter_map(|definition| {
            matches!(definition.body, ParsedDefinitionBody::Pattern { .. })
                .then_some(definition.name.as_str())
        })
    }

    fn publication_note(&self) -> Option<&'static str> {
        self.program
            .policy
            .as_ref()
            .map(|_| "Catalog publication policy applies separately from recognition.")
    }

    pub fn render_publication_note(&self, width: usize) -> Result<String, DiagramError> {
        if !(32..=240).contains(&width) {
            return Err(DiagramError::Width);
        }
        Ok(self.publication_note().map_or_else(String::new, |note| {
            format!("{}\n", wrap(note, width).join("\n"))
        }))
    }

    pub fn diagram(&self, name: &str, expand: bool) -> Result<PatternDiagram, DiagramError> {
        let definition = self
            .program
            .definitions
            .iter()
            .find(|definition| definition.name == name);
        let Some(definition) = definition else {
            let kind = if self.program.sets.iter().any(|value| value.name == name) {
                Some("set")
            } else if self.program.tests.iter().any(|value| value.name == name) {
                Some("test")
            } else if self.program.groups.iter().any(|value| value.name == name) {
                Some("group")
            } else {
                None
            };
            if let Some(kind) = kind {
                return Err(DiagramError::UnsupportedDefinition {
                    name: name.to_owned(),
                    kind: kind.into(),
                });
            }
            return Err(DiagramError::MissingDefinition {
                name: name.to_owned(),
            });
        };
        let ParsedDefinitionBody::Pattern { expr, .. } = &definition.body else {
            return Err(DiagramError::UnsupportedDefinition {
                name: name.to_owned(),
                kind: "lexicon".into(),
            });
        };
        if expand {
            let expanded = expand_indexed_declaration(
                self.tree.root_node(),
                &self.source,
                self.limits,
                name,
                MAX_DEPTH,
            )?;
            let mut diagram = Self::parse(PatternSource::named(name, &expanded), self.limits)?
                .diagram(name, false)?;
            diagram.annotations = self.annotations(definition, &diagram.annotations);
            diagram.publication_note = self.publication_note();
            return Ok(diagram);
        }
        let range = definition.span.byte_range();
        let node = self
            .tree
            .root_node()
            .named_descendant_for_byte_range(range.start as usize, range.end as usize)
            .ok_or(DiagramError::SourceShape)?;
        let mut count = 0;
        let expression = flow(expr, &self.source, &mut count, 0)?;
        let annotations = node
            .child_by_field_name("where")
            .map(|n| raw(&self.source, n))
            .transpose()?
            .into_iter()
            .collect::<Vec<_>>();
        let annotations = self.annotations(definition, &annotations);
        Ok(PatternDiagram {
            name: safe(name),
            expression,
            annotations,
            publication_note: self.publication_note(),
        })
    }

    fn annotations(
        &self,
        definition: &super::model::ParsedDefinition,
        existing: &[String],
    ) -> Vec<String> {
        let mut result = existing.to_vec();
        if let Some(test) = &definition.skip_test {
            let label = format!("skip: @{test}");
            if !result.contains(&label) {
                result.push(label);
            }
        }
        for key in ["transform", "render"] {
            if let Some(entry) = definition.metadata.get(key) {
                let label = format!("publication {key}: {}", metadata_text(&entry.value));
                if !result.contains(&label) {
                    result.push(label);
                }
            }
        }
        result
    }
}

fn metadata_text(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::Array(values) => format!(
            "[{}]",
            values
                .iter()
                .map(metadata_text)
                .collect::<Vec<_>>()
                .join(", ")
        ),
        serde_json::Value::Object(entries) => format!(
            "{{{}}}",
            entries
                .iter()
                .map(|(key, value)| format!(
                    "{}: {}",
                    serde_json::Value::String(key.clone()),
                    metadata_text(value)
                ))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        _ => value.to_string(),
    }
}

pub struct PatternDiagram {
    name: String,
    expression: Flow,
    annotations: Vec<String>,
    publication_note: Option<&'static str>,
}

pub(super) fn render_embedded_definition_diagram(source: &str) -> Result<String, DiagramError> {
    let catalog = DiagramCatalog::parse(
        PatternSource::named("diagram.lx", source),
        CompileLimits::default(),
    )?;
    let names = catalog.names().collect::<Vec<_>>();
    if names.is_empty() {
        return Err(DiagramError::SourceShape);
    }
    let mut parts = Vec::new();
    let mut annotations = Vec::new();
    let mut nodes = usize::from(names.len() > 1);
    let mut bytes = 0;
    for name in &names {
        let diagram = catalog.diagram(name, false)?;
        charge_expression(
            &diagram.expression,
            &mut nodes,
            &mut bytes,
            usize::from(names.len() > 1),
        )?;
        for note in diagram.annotations {
            let note = if names.len() == 1 {
                note
            } else {
                format!("{name}: {note}")
            };
            bytes += note.len();
            if bytes > MAX_OUTPUT_BYTES {
                return Err(DiagramError::Budget("8 MiB diagram text"));
            }
            annotations.push(note);
        }
        parts.push(diagram.expression);
    }
    let expression = if parts.len() == 1 {
        parts.remove(0)
    } else {
        Flow::Sequence(parts)
    };
    PatternDiagram {
        name: safe(&names.join(" → ")),
        expression,
        annotations,
        publication_note: catalog.publication_note(),
    }
    .render_svg_with_style(100, SvgStyle::Embedded)
}

fn charge_expression(
    expression: &Flow,
    nodes: &mut usize,
    bytes: &mut usize,
    depth: usize,
) -> Result<(), DiagramError> {
    *nodes += 1;
    if *nodes > MAX_NODES {
        return Err(DiagramError::Budget("4096 nodes"));
    }
    if depth > MAX_DEPTH {
        return Err(DiagramError::Budget("128 levels"));
    }
    match expression {
        Flow::Leaf(label) => *bytes += label.len(),
        Flow::Sequence(parts)
        | Flow::Choice(parts)
        | Flow::Constraint {
            operands: parts, ..
        } => {
            for part in parts {
                charge_expression(part, nodes, bytes, depth + 1)?;
            }
        }
        Flow::Repeat { body, .. } => charge_expression(body, nodes, bytes, depth + 1)?,
        Flow::Frame { label, body } | Flow::Lookaround { label, body } => {
            *bytes += label.len();
            charge_expression(body, nodes, bytes, depth + 1)?;
        }
    }
    if *bytes > MAX_OUTPUT_BYTES {
        return Err(DiagramError::Budget("8 MiB diagram text"));
    }
    Ok(())
}

enum SvgStyle {
    Standalone,
    Embedded,
}

#[derive(Debug)]
enum Flow {
    Leaf(String),
    Sequence(Vec<Self>),
    Choice(Vec<Self>),
    Constraint {
        label: &'static str,
        operands: Vec<Self>,
    },
    Repeat {
        body: Box<Self>,
        min: usize,
        max: Option<usize>,
    },
    Frame {
        label: String,
        body: Box<Self>,
    },
    Lookaround {
        label: String,
        body: Box<Self>,
    },
}

fn raw(source: &str, node: Node<'_>) -> Result<String, DiagramError> {
    source
        .get(node.byte_range())
        .map(str::to_owned)
        .ok_or(DiagramError::SourceShape)
}

fn source_text(source: &str, span: SourceSpan) -> Result<String, DiagramError> {
    let range = span.byte_range();
    source
        .get(range.start as usize..range.end as usize)
        .map(str::to_owned)
        .ok_or(DiagramError::SourceShape)
}

fn flow(
    expr: &ParsedExpr,
    source: &str,
    count: &mut usize,
    depth: usize,
) -> Result<Flow, DiagramError> {
    *count += 1;
    if *count > MAX_NODES {
        return Err(DiagramError::Budget("4096 nodes"));
    }
    if depth > MAX_DEPTH {
        return Err(DiagramError::Budget("128 levels"));
    }
    let mut child = |expr| flow(expr, source, count, depth + 1);
    Ok(match expr {
        ParsedExpr::Token(_, span) | ParsedExpr::SameAs { span, .. } => {
            Flow::Leaf(source_text(source, *span)?)
        }
        ParsedExpr::Reference { name, .. } => Flow::Leaf(format!("@{name}")),
        ParsedExpr::Assertion(assertion) => Flow::Leaf(match assertion {
            PositionAssertion::SentenceStart => "^ (zero-width)".into(),
            PositionAssertion::SentenceEnd => "$ (zero-width)".into(),
            _ => return Err(DiagramError::SourceShape),
        }),
        ParsedExpr::Sequence(parts) => {
            Flow::Sequence(parts.iter().map(&mut child).collect::<Result<_, _>>()?)
        }
        ParsedExpr::Alternation(parts) => {
            Flow::Choice(parts.iter().map(&mut child).collect::<Result<_, _>>()?)
        }
        ParsedExpr::Intersection { operands, .. } => Flow::Constraint {
            label: "ALL: same span",
            operands: operands.iter().map(&mut child).collect::<Result<_, _>>()?,
        },
        ParsedExpr::AnyOrder { operands, .. } => Flow::Constraint {
            label: "UNORDERED; []* gaps between operands only",
            operands: operands.iter().map(&mut child).collect::<Result<_, _>>()?,
        },
        ParsedExpr::Repeat { expr, min, max } => Flow::Repeat {
            body: Box::new(child(expr)?),
            min: *min,
            max: *max,
        },
        ParsedExpr::Group(expr) => child(expr)?,
        ParsedExpr::NamedGroup { name, expr, .. } => Flow::Frame {
            label: format!("${name}"),
            body: Box::new(child(expr)?),
        },
        ParsedExpr::Lookaround(look) => {
            let direction = match look.direction {
                LookDirection::PrecededBy => "preceded_by",
                LookDirection::FollowedBy => "followed_by",
            };
            let polarity = if look.polarity == LookPolarity::Negative {
                "not_"
            } else {
                ""
            };
            Flow::Lookaround {
                label: format!("zero-width: {polarity}{direction}"),
                body: Box::new(child(&look.body)?),
            }
        }
    })
}

fn safe(value: &str) -> String {
    let mut output = String::with_capacity(value.len());
    for ch in value.chars() {
        if ch.is_control()
            || matches!(ch, '\u{061c}' | '\u{200e}'..='\u{200f}' | '\u{2028}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
        {
            output.extend(ch.escape_default());
        } else {
            output.push(ch);
        }
    }
    output
}

#[derive(Clone)]
enum Draw {
    Text {
        x: usize,
        y: usize,
        text: String,
    },
    Marker {
        x: usize,
        y: usize,
        text: &'static str,
    },
    Line {
        x1: usize,
        y1: usize,
        x2: usize,
        y2: usize,
    },
    Rect {
        x: usize,
        y: usize,
        width: usize,
        height: usize,
    },
}

struct Drawing {
    width: usize,
    height: usize,
    entry: usize,
    exit: usize,
    commands: Vec<Draw>,
}

impl Drawing {
    fn empty(width: usize, height: usize, entry: usize, exit: usize) -> Result<Self, DiagramError> {
        if width
            .checked_mul(height)
            .is_none_or(|cells| cells > MAX_CELLS)
        {
            return Err(DiagramError::Budget("layout cells"));
        }
        Ok(Self {
            width,
            height,
            entry,
            exit,
            commands: Vec::new(),
        })
    }
    fn text(&mut self, x: usize, y: usize, text: impl Into<String>) {
        self.commands.push(Draw::Text {
            x,
            y,
            text: text.into(),
        });
    }
    fn marker(&mut self, x: usize, y: usize, text: &'static str) {
        self.commands.push(Draw::Marker { x, y, text });
    }
    fn line(&mut self, x1: usize, y1: usize, x2: usize, y2: usize) {
        self.commands.push(Draw::Line { x1, y1, x2, y2 });
    }
    fn put(&mut self, child: Self, x: usize, y: usize) {
        self.commands
            .extend(child.commands.into_iter().map(|command| match command {
                Draw::Text { x: cx, y: cy, text } => Draw::Text {
                    x: x + cx,
                    y: y + cy,
                    text,
                },
                Draw::Marker { x: cx, y: cy, text } => Draw::Marker {
                    x: x + cx,
                    y: y + cy,
                    text,
                },
                Draw::Line { x1, y1, x2, y2 } => Draw::Line {
                    x1: x + x1,
                    y1: y + y1,
                    x2: x + x2,
                    y2: y + y2,
                },
                Draw::Rect {
                    x: cx,
                    y: cy,
                    width,
                    height,
                } => Draw::Rect {
                    x: x + cx,
                    y: y + cy,
                    width,
                    height,
                },
            }));
    }
}

#[derive(Default)]
struct Layout<'a> {
    pending: Vec<&'a Flow>,
    labels: Vec<String>,
}

impl<'a> Layout<'a> {
    fn label(&mut self, label: &str, width: usize) -> String {
        let label = safe(label);
        if UnicodeWidthStr::width(label.as_str()) <= width {
            return label;
        }
        self.labels.push(label);
        format!("[L{}]", self.labels.len())
    }
    fn leaf(&mut self, label: &str, width: usize) -> Result<Drawing, DiagramError> {
        let label = self.label(label, width.saturating_sub(4));
        let width = UnicodeWidthStr::width(label.as_str()) + 4;
        let mut result = Drawing::empty(width, 3, 1, 1)?;
        result.commands.push(Draw::Rect {
            x: 0,
            y: 0,
            width,
            height: 3,
        });
        result.text(2, 1, label);
        Ok(result)
    }
    fn render(&mut self, flow: &'a Flow, width: usize) -> Result<Drawing, DiagramError> {
        let minimum_width = if matches!(flow, Flow::Repeat { .. }) {
            20
        } else {
            16
        };
        if width < minimum_width && !matches!(flow, Flow::Leaf(_)) {
            self.pending.push(flow);
            return self.leaf(&format!("[D{}]", self.pending.len()), width);
        }
        match flow {
            Flow::Leaf(label) => self.leaf(label, width),
            Flow::Sequence(parts) => {
                let parts = parts
                    .iter()
                    .map(|part| self.render(part, width.saturating_sub(4)))
                    .collect::<Result<Vec<_>, _>>()?;
                sequence(parts, width)
            }
            Flow::Choice(parts) => {
                let parts = parts
                    .iter()
                    .map(|part| self.render(part, width.saturating_sub(4)))
                    .collect::<Result<Vec<_>, _>>()?;
                parallel(parts)
            }
            Flow::Constraint { label, operands } => {
                let mut references = Vec::new();
                for operand in operands {
                    self.pending.push(operand);
                    references.push(format!("[D{}]", self.pending.len()));
                }
                let inner = self.leaf(&references.join(", "), width.saturating_sub(4))?;
                let label = self.label(label, width.saturating_sub(4));
                frame(inner, &label)
            }
            Flow::Frame { label, body } => {
                let inner = self.render(body, width.saturating_sub(4))?;
                let label = self.label(label, width.saturating_sub(4));
                frame(inner, &label)
            }
            Flow::Lookaround { label, body } => {
                self.pending.push(body);
                self.leaf(&format!("{label} [D{}]", self.pending.len()), width)
            }
            Flow::Repeat { body, min, max } => {
                if *max == Some(0) {
                    return self.leaf("repeat 0 (skip operand)", width);
                }
                let inner = self.render(body, width.saturating_sub(8))?;
                let bounds = match max {
                    Some(max) if max == min => min.to_string(),
                    Some(max) => format!("{min}..{max}"),
                    None => format!("{min}..unbounded"),
                };
                let label = self.label(&format!("repeat {bounds}"), width.saturating_sub(8));
                let framed = frame(inner, &label)?;
                let optional = *min == 0;
                let repeating = max.is_none_or(|max| max > 1);
                let top = if optional { 2 } else { 0 };
                let bottom = if repeating { 2 } else { 0 };
                let mut repeated = Drawing::empty(
                    framed.width + 4,
                    framed.height + top + bottom,
                    framed.entry + top,
                    framed.exit + top,
                )?;
                repeated.line(0, repeated.entry, 2, repeated.entry);
                repeated.line(
                    repeated.width - 3,
                    repeated.exit,
                    repeated.width - 1,
                    repeated.exit,
                );
                if optional {
                    repeated.line(0, 0, 0, repeated.entry);
                    repeated.line(0, 0, repeated.width - 1, 0);
                    repeated.line(repeated.width - 1, 0, repeated.width - 1, repeated.exit);
                    repeated.marker(repeated.width / 2, 0, "▶");
                }
                if repeating {
                    let y = repeated.height - 1;
                    repeated.line(repeated.width - 1, repeated.exit, repeated.width - 1, y);
                    repeated.line(0, y, repeated.width - 1, y);
                    repeated.line(0, y, 0, repeated.entry);
                    repeated.marker(repeated.width / 2, y, "◀");
                }
                repeated.put(framed, 2, top);
                Ok(repeated)
            }
        }
    }
}

fn frame(inner: Drawing, label: &str) -> Result<Drawing, DiagramError> {
    let width = (inner.width + 4).max(UnicodeWidthStr::width(label) + 4);
    let mut result = Drawing::empty(width, inner.height + 3, inner.entry + 2, inner.exit + 2)?;
    result.commands.push(Draw::Rect {
        x: 0,
        y: 0,
        width,
        height: result.height,
    });
    result.text(2, 0, label);
    result.line(0, result.entry, 2, result.entry);
    result.line(inner.width + 1, result.exit, width - 1, result.exit);
    result.put(inner, 2, 2);
    Ok(result)
}

fn parallel(parts: Vec<Drawing>) -> Result<Drawing, DiagramError> {
    let width = parts.iter().map(|part| part.width).max().unwrap_or(1) + 4;
    let height = parts
        .iter()
        .map(|part| part.height + 1)
        .sum::<usize>()
        .max(1)
        - 1;
    let mut result = Drawing::empty(
        width,
        height,
        parts.first().map_or(0, |p| p.entry),
        parts.last().map_or(0, |p| height - p.height + p.exit),
    )?;
    let mut y = 0;
    let mut ports = Vec::new();
    for part in parts {
        let next_y = y + part.height + 1;
        let entry = y + part.entry;
        let exit = y + part.exit;
        result.line(0, entry, 2, entry);
        result.line(part.width + 1, exit, width - 1, exit);
        ports.push((entry, exit));
        result.put(part, 2, y);
        y = next_y;
    }
    result.exit = result.entry;
    if let (Some(first), Some(last)) = (ports.first(), ports.last()) {
        result.line(0, first.0, 0, last.0);
        result.line(
            width - 1,
            first.1.min(result.exit),
            width - 1,
            last.1.max(result.exit),
        );
    }
    Ok(result)
}

fn sequence(parts: Vec<Drawing>, width: usize) -> Result<Drawing, DiagramError> {
    if parts.is_empty() {
        return Drawing::empty(1, 1, 0, 0);
    }
    let mut rows: Vec<Vec<Drawing>> = Vec::new();
    let mut row = Vec::new();
    let mut row_width = 0;
    for part in parts {
        if !row.is_empty() && row_width + 3 + part.width > width.saturating_sub(4) {
            rows.push(std::mem::take(&mut row));
            row_width = 0;
        }
        row_width += part.width + if row.is_empty() { 0 } else { 3 };
        row.push(part);
    }
    rows.push(row);
    let mut stacked = Vec::new();
    for row in rows {
        let entry = row.iter().map(|p| p.entry).max().unwrap_or(0);
        let height = row
            .iter()
            .map(|p| p.height + entry - p.entry)
            .max()
            .unwrap_or(1);
        let width = row.iter().map(|p| p.width + 3).sum::<usize>() - 3;
        let mut drawing = Drawing::empty(width, height, entry, entry)?;
        let mut x = 0;
        let mut previous_exit = entry;
        for part in row {
            let part_exit = entry + part.exit - part.entry;
            if x > 0 {
                drawing.line(x - 4, previous_exit, x - 2, previous_exit);
                drawing.line(
                    x - 2,
                    previous_exit.min(entry),
                    x - 2,
                    previous_exit.max(entry),
                );
                drawing.line(x - 2, entry, x, entry);
                drawing.marker(x - 1, entry, "▶");
            }
            let next_x = x + part.width + 3;
            let y = entry - part.entry;
            drawing.put(part, x, y);
            previous_exit = part_exit;
            x = next_x;
        }
        drawing.exit = previous_exit;
        stacked.push(drawing);
    }
    if stacked.len() == 1 {
        return Ok(stacked.remove(0));
    }
    let width = stacked.iter().map(|p| p.width).max().unwrap_or(1) + 4;
    let height = stacked.iter().map(|p| p.height + 2).sum::<usize>() - 2;
    let entry = stacked.first().map_or(0, |p| p.entry);
    let exit = stacked.last().map_or(0, |p| height - p.height + p.exit);
    let mut result = Drawing::empty(width, height, entry, exit)?;
    let mut y = 0;
    let last = stacked.len() - 1;
    for (i, part) in stacked.into_iter().enumerate() {
        let next_y = y + part.height + 2;
        if i > 0 {
            result.line(0, y - 1, 0, y + part.entry);
            result.line(0, y + part.entry, 2, y + part.entry);
            result.marker(1, y + part.entry, "▶");
        } else {
            result.line(0, y + part.entry, 2, y + part.entry);
        }
        if i < last {
            result.line(part.width + 1, y + part.exit, width - 1, y + part.exit);
            result.line(width - 1, y + part.exit, width - 1, next_y - 1);
            result.line(0, next_y - 1, width - 1, next_y - 1);
            result.marker(width / 2, next_y - 1, "◀");
        } else {
            result.line(part.width + 1, y + part.exit, width - 1, y + part.exit);
        }
        result.put(part, 2, y);
        y = next_y;
    }
    Ok(result)
}

impl PatternDiagram {
    fn layout(&self, width: usize) -> Result<(Vec<Drawing>, Vec<String>), DiagramError> {
        if !(32..=240).contains(&width) {
            return Err(DiagramError::Width);
        }
        let mut layout = Layout::default();
        let first = endpoints(layout.render(&self.expression, width - 4)?)?;
        let mut cells = first.width * first.height;
        let mut drawings = vec![first];
        let mut index = 0;
        while index < layout.pending.len() {
            if index >= MAX_NODES {
                return Err(DiagramError::Budget("subdiagrams"));
            }
            let drawing = endpoints(layout.render(layout.pending[index], width - 4)?)?;
            cells += drawing.width * drawing.height;
            if cells > MAX_CELLS {
                return Err(DiagramError::Budget("layout cells"));
            }
            drawings.push(drawing);
            index += 1;
        }
        let mut notes = layout
            .labels
            .into_iter()
            .enumerate()
            .map(|(i, label)| format!("[L{}] {label}", i + 1))
            .collect::<Vec<_>>();
        notes.extend(self.annotations.iter().map(|note| safe(note)));
        Ok((drawings, notes))
    }

    pub fn render_text(&self, width: usize, ascii: bool) -> Result<String, DiagramError> {
        let (drawings, notes) = self.layout(width)?;
        let mut output = String::new();
        for line in wrap(&self.name, width) {
            append(&mut output, &format!("{line}\n"))?;
        }
        for (i, drawing) in drawings.iter().enumerate() {
            if i > 0 {
                append(&mut output, &format!("\n[D{i}]\n"))?;
            }
            append(&mut output, &drawing_text(drawing, ascii)?)?;
        }
        for note in notes {
            append(&mut output, "\n")?;
            for line in wrap(&note, width) {
                append(&mut output, &format!("{line}\n"))?;
            }
        }
        Ok(output)
    }

    pub fn render_svg(&self, width: usize) -> Result<String, DiagramError> {
        self.render_svg_with_style(width, SvgStyle::Standalone)
    }

    fn render_svg_with_style(&self, width: usize, style: SvgStyle) -> Result<String, DiagramError> {
        let (drawings, mut notes) = self.layout(width)?;
        notes.extend(self.publication_note.map(str::to_owned));
        let note_lines = notes
            .iter()
            .flat_map(|note| wrap(note, width))
            .collect::<Vec<_>>();
        let heading = wrap(&self.name, width);
        let height = 4
            + heading.len()
            + drawings
                .iter()
                .map(|drawing| drawing.height + 2)
                .sum::<usize>()
            + note_lines.len();
        let stylesheet = match style {
            SvgStyle::Standalone => {
                "<style>svg{background:#fff;color:#17212b}text{fill:#17212b;font:14px ui-monospace,\"Noto Sans Mono\",\"DejaVu Sans Mono\",monospace}.fsm-edge{stroke:#435363;stroke-width:1.5;fill:none}.fsm-node{stroke:#435363;stroke-width:1.5;fill:#fff}.fsm-label-background{fill:#fff}</style>"
            }
            SvgStyle::Embedded => "",
        };
        let mut output = format!(
            "<svg xmlns=\"http://www.w3.org/2000/svg\" class=\"pattern-fsm-svg\" width=\"{}\" height=\"{}\" viewBox=\"0 0 {} {}\" role=\"img\" aria-label=\"{}: recognition flow\"><title>{}</title><desc>Recognition flow; publication policy is separate. Numbered labels and subdiagrams are shown below.</desc>{stylesheet}",
            (width + 2) * 9,
            height * 20,
            (width + 2) * 9,
            height * 20,
            xml(&self.name),
            xml(&self.name)
        );
        for (i, line) in heading.iter().enumerate() {
            append(&mut output, &svg_text(1, i + 1, line))?;
        }
        let mut offset_y = heading.len() + 2;
        for (i, drawing) in drawings.iter().enumerate() {
            if i > 0 {
                append(&mut output, &svg_text(1, offset_y, &format!("[D{i}]")))?;
                offset_y += 1;
            }
            for command in &drawing.commands {
                match command {
                    Draw::Text { .. } | Draw::Marker { .. } => {}
                    Draw::Line { x1, y1, x2, y2 } => append(
                        &mut output,
                        &format!(
                            "<path class=\"fsm-edge\" d=\"M {},{} L {},{}\"/>",
                            (x1 + 1) * 9 + 4,
                            (y1 + offset_y) * 20 + 10,
                            (x2 + 1) * 9 + 4,
                            (y2 + offset_y) * 20 + 10
                        ),
                    )?,
                    Draw::Rect {
                        x,
                        y,
                        width,
                        height,
                    } => append(
                        &mut output,
                        &format!(
                            "<rect class=\"fsm-node\" x=\"{}\" y=\"{}\" width=\"{}\" height=\"{}\" rx=\"5\"/>",
                            (x + 1) * 9 + 4,
                            (y + offset_y) * 20 + 10,
                            (width - 1) * 9,
                            (height - 1) * 20
                        ),
                    )?,
                }
            }
            for command in &drawing.commands {
                match command {
                    Draw::Text { x, y, text } => {
                        append(&mut output, &svg_text(x + 1, y + offset_y, text))?
                    }
                    Draw::Marker { x, y, text } => {
                        append(&mut output, &svg_text(x + 1, y + offset_y, text))?
                    }
                    _ => {}
                }
            }
            offset_y += drawing.height + 1;
        }
        for line in note_lines {
            append(&mut output, &svg_text(1, offset_y, &line))?;
            offset_y += 1;
        }
        append(&mut output, "</svg>\n")?;
        Ok(output)
    }
}

fn endpoints(inner: Drawing) -> Result<Drawing, DiagramError> {
    let mut result = Drawing::empty(inner.width + 4, inner.height, inner.entry, inner.exit)?;
    result.line(0, inner.entry, 2, inner.entry);
    result.line(inner.width + 1, inner.exit, inner.width + 3, inner.exit);
    result.marker(0, inner.entry, "●");
    result.marker(inner.width + 3, inner.exit, "◎");
    result.put(inner, 2, 0);
    Ok(result)
}

fn append(output: &mut String, value: &str) -> Result<(), DiagramError> {
    if output.len() + value.len() > MAX_OUTPUT_BYTES {
        return Err(DiagramError::Budget("8 MiB output"));
    }
    output.push_str(value);
    Ok(())
}

fn wrap(text: &str, width: usize) -> Vec<String> {
    let mut result = Vec::new();
    let mut line = String::new();
    let mut used = 0;
    let mut last_space = None;
    for grapheme in text.graphemes(true) {
        let cells = UnicodeWidthStr::width(grapheme);
        while used + cells > width && !line.is_empty() {
            let remainder = last_space.map_or_else(String::new, |index| line.split_off(index));
            result.push(std::mem::take(&mut line));
            line = remainder;
            used = UnicodeWidthStr::width(line.as_str());
            last_space = None;
        }
        line.push_str(grapheme);
        used += cells;
        if grapheme.chars().all(char::is_whitespace) {
            last_space = Some(line.len());
        }
    }
    if !line.is_empty() {
        result.push(line);
    }
    result
}

fn drawing_text(drawing: &Drawing, ascii: bool) -> Result<String, DiagramError> {
    let mut edges = vec![0_u8; drawing.width * drawing.height];
    let mut text = BTreeMap::new();
    let mut line = |x1: usize, y1: usize, x2: usize, y2: usize| -> Result<(), DiagramError> {
        if x1 >= drawing.width
            || x2 >= drawing.width
            || y1 >= drawing.height
            || y2 >= drawing.height
        {
            return Err(DiagramError::Budget("layout coordinates"));
        }
        if y1 == y2 {
            for x in x1.min(x2)..=x1.max(x2) {
                let mask = (u8::from(x > x1.min(x2)) * 8) | (u8::from(x < x1.max(x2)) * 2);
                edges[y1 * drawing.width + x] |= mask;
            }
        } else {
            for y in y1.min(y2)..=y1.max(y2) {
                let mask = u8::from(y > y1.min(y2)) | (u8::from(y < y1.max(y2)) * 4);
                edges[y * drawing.width + x1] |= mask;
            }
        }
        Ok(())
    };
    for command in &drawing.commands {
        match command {
            Draw::Line { x1, y1, x2, y2 } => line(*x1, *y1, *x2, *y2)?,
            Draw::Rect {
                x,
                y,
                width,
                height,
            } => {
                line(*x, *y, x + width - 1, *y)?;
                line(*x, y + height - 1, x + width - 1, y + height - 1)?;
                line(*x, *y, *x, y + height - 1)?;
                line(x + width - 1, *y, x + width - 1, y + height - 1)?;
            }
            Draw::Text { x, y, text: label } => {
                text.insert((*y, *x), label.clone());
            }
            Draw::Marker { x, y, text: marker } => {
                let marker = if ascii {
                    match *marker {
                        "▶" => ">",
                        "◀" => "<",
                        "●" => "o",
                        "◎" => "O",
                        other => other,
                    }
                } else {
                    marker
                };
                text.insert((*y, *x), marker.to_string());
            }
        }
    }
    let mut output = String::new();
    for y in 0..drawing.height {
        let mut row = String::new();
        let mut x = 0;
        while x < drawing.width {
            if let Some(label) = text.get(&(y, x)) {
                row.push_str(label);
                x += UnicodeWidthStr::width(label.as_str()).max(1);
            } else {
                let edge = edges[y * drawing.width + x];
                let glyph = if ascii {
                    match edge {
                        0 => ' ',
                        1 | 4 | 5 => '|',
                        2 | 8 | 10 => '-',
                        _ => '+',
                    }
                } else {
                    match edge {
                        0 => ' ',
                        1 | 4 | 5 => '│',
                        2 | 8 | 10 => '─',
                        6 => '┌',
                        12 => '┐',
                        3 => '└',
                        9 => '┘',
                        7 => '├',
                        13 => '┤',
                        14 => '┬',
                        11 => '┴',
                        _ => '┼',
                    }
                };
                row.push(glyph);
                x += 1;
            }
        }
        append(&mut output, row.trim_end())?;
        append(&mut output, "\n")?;
    }
    Ok(output)
}

fn svg_text(x: usize, y: usize, text: &str) -> String {
    format!(
        "<rect class=\"fsm-label-background\" x=\"{}\" y=\"{}\" width=\"{}\" height=\"18\"/><text x=\"{}\" y=\"{}\" xml:space=\"preserve\">{}</text>",
        x * 9 - 2,
        y * 20,
        UnicodeWidthStr::width(text) * 9 + 4,
        x * 9,
        y * 20 + 15,
        xml(text)
    )
}

fn xml(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    fn catalog(source: &str) -> DiagramCatalog {
        DiagramCatalog::parse(
            PatternSource::named("test.lx", source),
            CompileLimits::default(),
        )
        .unwrap()
    }

    #[test]
    fn publication_transform_wraps_between_json_fields() {
        let catalog = catalog(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../patterns/npv-collocation.lx"
        )));
        let diagram = catalog.diagram("npv", false).unwrap();
        for width in 32..=160 {
            let text = diagram.render_text(width, false).unwrap();
            assert_eq!(
                text.matches("\"scope\":").count(),
                4,
                "width {width}: {text}"
            );
            for line in text.lines() {
                assert!(UnicodeWidthStr::width(line) <= width, "{width}: {line}");
            }
        }
        let text = diagram.render_text(160, false).unwrap();
        assert!(text.contains("\"op\": \"join\""), "{text}");
    }

    #[test]
    fn embedded_svg_uses_theme_classes_and_escapes_source_labels() {
        let svg = render_embedded_definition_diagram("p := [text~/<script>/].").unwrap();
        let document = roxmltree::Document::parse(&svg).unwrap();
        assert_eq!(
            document.root_element().attribute("class"),
            Some("pattern-fsm-svg")
        );
        assert!(
            !document
                .descendants()
                .any(|node| node.has_tag_name("script")
                    || node.has_tag_name("style")
                    || node.attribute("id").is_some())
        );
        assert!(svg.contains("&lt;script&gt;"));
        for class in ["fsm-edge", "fsm-node", "fsm-label-background"] {
            assert!(svg.contains(class), "{svg}");
        }
    }

    #[test]
    fn compound_diagram_node_limit_applies_across_components() {
        let source = format!(
            "a := {}. b := {}.",
            "[] ".repeat(MAX_NODES / 2),
            "[] ".repeat(MAX_NODES / 2)
        );
        assert!(matches!(
            render_embedded_definition_diagram(&source),
            Err(DiagramError::Budget("4096 nodes"))
        ));
    }

    #[test]
    fn repeat_routes_are_outside_the_operand_frame() {
        for (quantifier, optional, repeating) in
            [("?", true, false), ("*", true, true), ("+", false, true)]
        {
            let diagram = catalog(&format!("fragment p := [text=a]{quantifier}."))
                .diagram("p", false)
                .unwrap();
            let (drawings, _) = diagram.layout(80).unwrap();
            let drawing = &drawings[0];
            let (x, y, width, height) = drawing
                .commands
                .iter()
                .find_map(|command| match command {
                    Draw::Rect {
                        x,
                        y,
                        width,
                        height,
                    } => Some((*x, *y, *width, *height)),
                    _ => None,
                })
                .unwrap();
            let has_outer_track = |above| {
                drawing.commands.iter().any(|command| match command {
                    Draw::Line { x1, y1, x2, y2 } => {
                        y1 == y2
                            && *x1 < x
                            && *x2 >= x + width
                            && if above { *y1 < y } else { *y1 >= y + height }
                    }
                    _ => false,
                })
            };
            assert_eq!(has_outer_track(true), optional, "{quantifier}: bypass");
            assert_eq!(has_outer_track(false), repeating, "{quantifier}: loop-back");
            if !repeating {
                assert_eq!(drawing.height, y + height, "no unused rows below ?");
            }
        }
    }

    #[test]
    fn railroad_layout_snapshots() {
        for (name, source, width) in [
            ("optional", "p := [text=a]? [text=b].", 80),
            ("star", "p := [text=a]* [text=b].", 80),
            ("plus", "p := [text=a]+ [text=b].", 80),
            ("choice", "p := ([text=a] | [text=b]) [text=c].", 80),
            ("wrapped_sequence", "p := [text=a] [text=b] [text=c].", 32),
            ("zero_repeat", "p := [text=a]{0} [text=b].", 80),
        ] {
            let diagram = catalog(source).diagram("p", false).unwrap();
            insta::assert_snapshot!(name, diagram.render_text(width, false).unwrap());
        }
    }

    #[test]
    fn selection_includes_fragments_in_source_order_and_rejects_values() {
        let catalog = catalog("set S := {a}. fragment f := [text in @S]. p := @f+.");
        assert_eq!(catalog.names().collect::<Vec<_>>(), ["f", "p"]);
        assert!(matches!(
            catalog.diagram("S", false),
            Err(DiagramError::UnsupportedDefinition { .. })
        ));
        assert!(matches!(
            catalog.diagram("missing", false),
            Err(DiagramError::MissingDefinition { .. })
        ));
        let authored = catalog.diagram("p", false).unwrap();
        let expanded = catalog.diagram("p", true).unwrap();
        assert!(authored.render_text(80, false).unwrap().contains("@f"));
        let expanded = expanded.render_text(80, false).unwrap();
        assert!(expanded.contains("[text in {a}]"), "{expanded}");
        assert!(!expanded.contains("@f"));
    }

    #[test]
    fn constrained_operators_remain_distinct_from_alternation() {
        let catalog = catalog(
            "p := ([text=a] | [text=b]) ([text=c] & [text=c]) any_order((?<x> [text=d]), (?<y> [text=e])).",
        );
        let diagram = catalog.diagram("p", false).unwrap();
        assert!(matches!(diagram.expression, Flow::Sequence(_)));
        let text = diagram.render_text(80, false).unwrap();
        for label in ["ALL: same span", "UNORDERED", "[]* gaps", "$x", "$y"] {
            assert!(text.contains(label), "missing {label}: {text}");
        }
    }

    #[test]
    fn narrow_layout_keeps_full_labels_and_has_no_terminal_controls() {
        let catalog = catalog(
            "p := ^ (?<x> [text=日本語e\u{301}🙂長い表記の条件]) [lemma=走る]{2,4} followed_by([text=。]) $. ",
        );
        let diagram = catalog.diagram("p", false).unwrap();
        for width in [32, 80, 120] {
            let text = diagram.render_text(width, false).unwrap();
            assert!(
                text.replace('\n', "")
                    .contains("日本語e\u{301}🙂長い表記の条件"),
                "{text}"
            );
            assert!(text.contains("2..4"));
            assert!(text.contains("zero-width"));
            assert!(!text.contains('\u{1b}'));
            for line in text.lines() {
                assert!(UnicodeWidthStr::width(line) <= width, "{width}: {line}");
            }
        }
        let ascii = diagram.render_text(80, true).unwrap();
        assert!(!ascii.contains('─'));
        assert!(ascii.contains("--"));
    }

    #[test]
    fn standalone_svg_has_visible_constraints_and_escaped_labels() {
        let catalog = catalog("p := (?<a> [text~/<&/]) (?<v> [upos=VERB]) where $v -[obj]-> $a.");
        let svg = catalog.diagram("p", false).unwrap().render_svg(80).unwrap();
        let document = roxmltree::Document::parse(&svg).unwrap();
        let root = document.root_element();
        assert_eq!(
            root.tag_name().namespace(),
            Some("http://www.w3.org/2000/svg")
        );
        let visible = root
            .descendants()
            .filter(roxmltree::Node::is_text)
            .filter_map(|n| n.text())
            .collect::<String>();
        assert!(visible.contains("where $v -[obj]-> $a"));
        assert!(visible.contains("<&"));
        assert!(!svg.contains("var(--"));
        assert!(!svg.contains("<script"));
    }

    #[test]
    fn diagram_limits_fail_before_rendering_oversized_layouts() {
        let source = format!("p := {}.", "[] ".repeat(MAX_NODES + 1));
        assert!(matches!(
            catalog(&source).diagram("p", false),
            Err(DiagramError::Budget("4096 nodes"))
        ));
        let diagram = PatternDiagram {
            name: "wide".into(),
            expression: Flow::Choice(
                (0..MAX_NODES)
                    .map(|_| Flow::Leaf("x".repeat(220)))
                    .collect(),
            ),
            annotations: Vec::new(),
            publication_note: None,
        };
        assert!(matches!(
            diagram.render_text(240, false),
            Err(DiagramError::Budget("layout cells"))
        ));
        assert!(matches!(diagram.render_svg(31), Err(DiagramError::Width)));
        let mut chain = "fragment f0 := [].\n".to_owned();
        for index in 1..=MAX_DEPTH {
            chain.push_str(&format!("fragment f{index} := @f{}.\n", index - 1));
        }
        chain.push_str(&format!("p := @f{MAX_DEPTH}."));
        assert!(matches!(
            catalog(&chain).diagram("p", true),
            Err(DiagramError::Expansion(
                ExpandError::ExpansionTooDeep { .. }
            ))
        ));
    }

    #[test]
    fn control_characters_are_visible_in_labels_and_legends() {
        let diagram = PatternDiagram {
            name: "controls".into(),
            expression: Flow::Leaf("[text=\u{1b}[31m\u{202e}]".into()),
            annotations: vec!["note\n\t\u{009b}".into()],
            publication_note: None,
        };
        let text = diagram.render_text(32, false).unwrap();
        assert!(!text.contains('\u{1b}'));
        assert!(!text.contains('\u{202e}'));
        assert!(!text.contains('\u{009b}'));
        assert!(text.contains("\\u{202e}"));
    }

    #[test]
    fn ascii_connectors_preserve_literal_arrows_in_predicates() {
        let diagram = catalog("p := [text=▶] [text=◀] [text=●] [text=◎].")
            .diagram("p", false)
            .unwrap();
        let text = diagram.render_text(80, true).unwrap();
        for literal in ["[text=▶]", "[text=◀]", "[text=●]", "[text=◎]"] {
            assert!(text.contains(literal), "{text}");
        }
    }

    #[test]
    fn representative_catalogs_fit_narrow_and_expanded_layouts() {
        for (file, source) in [
            (
                "double-wo",
                include_str!(concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/../patterns/double-wo.lx"
                )),
            ),
            (
                "sentence-final",
                include_str!(concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/../patterns/sentence-final.lx"
                )),
            ),
            (
                "ud-dependency",
                include_str!(concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/../patterns/ud-dependency.lx"
                )),
            ),
        ] {
            let catalog = catalog(source);
            for name in catalog.names() {
                for expanded in [false, true] {
                    let diagram = catalog
                        .diagram(name, expanded)
                        .unwrap_or_else(|e| panic!("{file}/{name}, expanded={expanded}: {e}"));
                    for width in [32, 80, 120] {
                        let text = diagram.render_text(width, false).unwrap_or_else(|e| {
                            panic!("{file}/{name}, expanded={expanded}, width={width}: {e}")
                        });
                        for line in text.lines() {
                            assert!(
                                UnicodeWidthStr::width(line) <= width,
                                "{file}/{name}, width={width}: {line}"
                            );
                        }
                    }
                }
            }
        }
    }
}
