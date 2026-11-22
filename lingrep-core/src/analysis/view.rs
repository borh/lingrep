//! The analysis view: the folded text an analyzer reads, aligned to the exact
//! source by an [`OffsetMap`].
//!
//! Terms used throughout:
//!
//! - **source**: the exact admitted text of one unit. A token's **surface** is
//!   the source slice it covers.
//! - **fold**: [`ViewFold::fold`], the function from source to view.
//! - **view**: the text produced by applying the fold to the source.
//! - **group**: the smallest pair of source and view spans the map aligns.

use std::{cell::Cell, collections::BTreeSet, sync::LazyLock};

use aho_corasick::{AhoCorasick, AhoCorasickBuilder, MatchKind};
use rustc_hash::{FxHashMap, FxHashSet};
use unicode_normalization::{
    IsNormalized, UnicodeNormalization, char as unicode_char, is_nfc_quick,
};

/// Changes whenever [`ViewFold::fold`] produces different output for any input.
const FOLD_REVISION: &[u8] = b"lingrep.analysis-view-fold/3";

/// A half-open range of code points in a source text.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct SourceSpan {
    start: u32,
    end: u32,
}

impl SourceSpan {
    /// # Panics
    ///
    /// Panics if `start > end`.
    #[must_use]
    pub const fn new(start: u32, end: u32) -> Self {
        assert!(start <= end, "source span start exceeds its end");
        Self { start, end }
    }

    #[must_use]
    pub const fn start(self) -> u32 {
        self.start
    }

    #[must_use]
    pub const fn end(self) -> u32 {
        self.end
    }
}

/// A half-open range of code points in an analysis view.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ViewSpan {
    start: u32,
    end: u32,
}

impl ViewSpan {
    /// # Panics
    ///
    /// Panics if `start > end`.
    #[must_use]
    pub const fn new(start: u32, end: u32) -> Self {
        assert!(start <= end, "view span start exceeds its end");
        Self { start, end }
    }

    #[must_use]
    pub const fn start(self) -> u32 {
        self.start
    }

    #[must_use]
    pub const fn end(self) -> u32 {
        self.end
    }
}

/// One alignment group other than a single code point mapped to a single code
/// point. Groups are ordered and disjoint in both coordinates; outside them the
/// two texts advance together one code point at a time.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Group {
    source_start: u32,
    source_end: u32,
    view_start: u32,
    view_end: u32,
}

/// Code-point alignment between a source text and its analysis view.
///
/// Both directions are total over `0..=len` and monotonic. A span that cuts a
/// group widens to the whole group: the start moves to the group's start and the
/// end to the group's end. Every group covers at least one source and one view
/// code point, except when the view is empty and the source is not.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct OffsetMap {
    source_len: u32,
    view_len: u32,
    groups: Vec<Group>,
}

impl OffsetMap {
    #[must_use]
    pub const fn source_len(&self) -> u32 {
        self.source_len
    }

    #[must_use]
    pub const fn view_len(&self) -> u32 {
        self.view_len
    }

    /// Whether every code point maps to exactly one code point.
    #[must_use]
    pub fn is_one_to_one(&self) -> bool {
        self.groups.is_empty()
    }

    /// Returns the source span covering exactly the code points that produced
    /// `view`.
    ///
    /// # Panics
    ///
    /// Panics if `view` ends beyond [`Self::view_len`].
    #[must_use]
    pub fn to_source(&self, view: ViewSpan) -> SourceSpan {
        assert!(view.end <= self.view_len, "view span exceeds the view");
        if self.view_len == 0 {
            return SourceSpan::new(0, self.source_len);
        }
        SourceSpan::new(
            align(&self.groups, view.start, Side::Start, Direction::ToSource),
            align(&self.groups, view.end, Side::End, Direction::ToSource),
        )
    }

    /// Returns the view span produced by the groups that `source` touches.
    ///
    /// # Panics
    ///
    /// Panics if `source` ends beyond [`Self::source_len`].
    #[must_use]
    pub fn to_view(&self, source: SourceSpan) -> ViewSpan {
        assert!(
            source.end <= self.source_len,
            "source span exceeds the source"
        );
        if self.view_len == 0 {
            return ViewSpan::new(0, 0);
        }
        ViewSpan::new(
            align(&self.groups, source.start, Side::Start, Direction::ToView),
            align(&self.groups, source.end, Side::End, Direction::ToView),
        )
    }
}

#[derive(Clone, Copy)]
enum Side {
    Start,
    End,
}

#[derive(Clone, Copy)]
enum Direction {
    ToSource,
    ToView,
}

fn align(groups: &[Group], position: u32, side: Side, direction: Direction) -> u32 {
    type Coordinates = fn(&Group) -> (u32, u32);
    let (from, to): (Coordinates, Coordinates) = match direction {
        Direction::ToSource => (
            |group: &Group| (group.view_start, group.view_end),
            |group: &Group| (group.source_start, group.source_end),
        ),
        Direction::ToView => (
            |group: &Group| (group.source_start, group.source_end),
            |group: &Group| (group.view_start, group.view_end),
        ),
    };
    let index = groups.partition_point(|group| from(group).1 <= position);
    if let Some(group) = groups.get(index) {
        let (from_start, _) = from(group);
        let (to_start, to_end) = to(group);
        if from_start == position {
            return to_start;
        }
        if from_start < position {
            return match side {
                Side::Start => to_start,
                Side::End => to_end,
            };
        }
    }
    match index
        .checked_sub(1)
        .and_then(|previous| groups.get(previous))
    {
        Some(group) => position - from(group).1 + to(group).1,
        None => position,
    }
}

/// The analysis view of one source text and its alignment to that source.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct AnalysisView {
    text: String,
    map: OffsetMap,
}

impl AnalysisView {
    /// `source` as its own view, mapped one to one.
    ///
    /// # Panics
    ///
    /// Panics if `source` has more than `u32::MAX` code points.
    #[must_use]
    pub fn unchanged(source: &str) -> Self {
        let len = u32::try_from(source.chars().count()).expect("source exceeds u32 code points");
        Self {
            text: source.to_owned(),
            map: OffsetMap {
                source_len: len,
                view_len: len,
                groups: Vec::new(),
            },
        }
    }

    /// The NFC form of `source`, aligned to it. Unlike [`ViewFold::fold`], no
    /// other rule applies and every canonical singleton is replaced, so the
    /// text is exactly what NFC produces.
    ///
    /// # Panics
    ///
    /// Panics if `source` has more than `u32::MAX` code points.
    #[must_use]
    pub fn canonical_composition(source: &str) -> Self {
        if is_nfc_quick(source.chars()) == IsNormalized::Yes {
            return Self::unchanged(source);
        }
        let mut cells = Cells::default();
        let mut position = 0u32;
        for c in source.chars() {
            position = position
                .checked_add(1)
                .expect("source exceeds u32 code points");
            cells.push(&[c], position);
        }
        cells.source_len = position;
        normalize(&cells, Keep::None).into_view()
    }

    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    #[must_use]
    pub const fn map(&self) -> &OffsetMap {
        &self.map
    }

    #[must_use]
    pub fn into_parts(self) -> (String, OffsetMap) {
        (self.text, self.map)
    }
}

/// A configured fold: the Unicode rules of [`Self::fold`] plus the protected
/// surfaces of one dictionary.
#[derive(Clone, Debug)]
pub struct ViewFold {
    protected: Option<AhoCorasick>,
    identity: [u8; 32],
}

impl ViewFold {
    /// A fold with no protected surfaces: every canonical singleton is replaced.
    #[must_use]
    pub fn unprotected() -> Self {
        Self::from_protected(BTreeSet::new())
    }

    /// Derives the protected surfaces from every surface of a dictionary.
    ///
    /// A surface is protected when, after the fold with every canonical
    /// singleton kept, it contains a singleton, begins with a character that can
    /// start a normalization chunk, and either has at least two code points or
    /// is a single singleton whose canonical decomposition occurs in no
    /// surface. Dictionary order and duplicates do not affect the result.
    #[must_use]
    pub fn from_dictionary_surfaces(surfaces: impl IntoIterator<Item = impl AsRef<str>>) -> Self {
        let mut candidates = BTreeSet::new();
        let mut surface_chars = FxHashSet::default();
        for surface in surfaces {
            let surface = surface.as_ref();
            surface_chars.extend(surface.chars());
            if surface
                .chars()
                .any(|c| is_variation_selector(c) || singleton_target(c).is_some())
            {
                let form = fold_cells(surface, Keep::AllSingletons).text();
                if form.chars().any(|c| singleton_target(c).is_some())
                    && form.chars().next().is_some_and(starts_chunk)
                {
                    candidates.insert(form);
                }
            }
        }
        candidates.retain(|form: &String| {
            let mut chars = form.chars();
            match (chars.next(), chars.next()) {
                (Some(only), None) => {
                    singleton_target(only).is_some_and(|target| !surface_chars.contains(&target))
                }
                _ => true,
            }
        });
        Self::from_protected(candidates)
    }

    fn from_protected(protected: BTreeSet<String>) -> Self {
        let mut hasher = blake3::Hasher::new();
        hasher.update(FOLD_REVISION);
        let version = unicode_normalization::UNICODE_VERSION;
        hasher.update(&[version.0, version.1, version.2]);
        for surface in &protected {
            hasher.update(&(surface.len() as u64).to_le_bytes());
            hasher.update(surface.as_bytes());
        }
        let protected = (!protected.is_empty()).then(|| {
            AhoCorasickBuilder::new()
                .match_kind(MatchKind::Standard)
                .build(&protected)
                .expect("a few hundred short literals fit every automaton limit")
        });
        Self {
            protected,
            identity: *hasher.finalize().as_bytes(),
        }
    }

    /// Identity of the rules of [`Self::fold`] without the protected surfaces.
    /// It and a dictionary's surfaces determine the [`Self::identity`] of
    /// [`Self::from_dictionary_surfaces`] over those surfaces.
    #[must_use]
    pub fn rules_identity() -> [u8; 32] {
        let mut hasher = blake3::Hasher::new();
        hasher.update(FOLD_REVISION);
        let version = unicode_normalization::UNICODE_VERSION;
        hasher.update(&[version.0, version.1, version.2]);
        *hasher.finalize().as_bytes()
    }

    /// Identity of this fold's output function: equal identities produce equal
    /// views for every source.
    #[must_use]
    pub const fn identity(&self) -> [u8; 32] {
        self.identity
    }

    /// Folds `source` into its analysis view.
    ///
    /// The source is never modified; tokens take their surface from it through
    /// the returned map. The view applies these rules, in this order:
    ///
    /// 1. **Variation selectors are removed.** U+FE00..U+FE0F and
    ///    U+E0100..U+E01EF select a glyph, not a different character (Unicode
    ///    §23.4), and the `NFKC_Casefold` property maps all of them to nothing
    ///    (UAX #44). UniDic has no surface containing one and analyses a
    ///    selector as a separate symbol, splitting 葛 U+E0100 城市 as
    ///    葛 / VS / 城市. A removed selector joins the group of the preceding code
    ///    point, so a token's surface keeps its ideographic variation sequence;
    ///    a selector at the start of the text joins the following group. A base
    ///    and selector that `StandardizedVariants.txt` lists as the standardized
    ///    variant of a CJK compatibility ideograph (U+585A U+FE00 for U+FA10)
    ///    become that ideograph, which rule 4 then keeps or replaces.
    /// 2. **Half-width katakana (U+FF61..U+FF9F) become full width** by their
    ///    compatibility decomposition (UAX #15). A voiced or semi-voiced sound
    ///    mark U+FF9E or U+FF9F after a half-width kana composes with it when
    ///    Unicode defines the composite (ｶﾞ → ガ, one group). Otherwise it
    ///    becomes the spacing U+309B or U+309C, never the combining U+3099 or
    ///    U+309A, which would attach to whatever precedes it.
    /// 3. **U+FF5E FULLWIDTH TILDE becomes U+301C WAVE DASH.** JIS X 0208 row 1
    ///    cell 33 is WAVE DASH; Microsoft's CP932 table (`CP932.TXT`) decodes
    ///    the same byte pair 0x8160 to U+FF5E, so the two code points denote
    ///    one character depending on the decoder. UniDic spells 長〜い and ね〜
    ///    with U+301C.
    /// 4. **Canonical composition (NFC, UAX #15)**, except for canonical
    ///    singletons inside a protected surface. A singleton is a character
    ///    whose canonical decomposition is one other character: the CJK
    ///    compatibility ideographs (U+F900..U+FAFF and U+2F800..U+2FA1F, minus
    ///    the twelve unified ideographs in that block), Greek oxia vowels
    ///    (U+1F71 ά becomes U+03AC), U+212B ANGSTROM SIGN and others. NFC
    ///    replaces every singleton. The fold keeps one only where it lies within
    ///    an occurrence of a protected surface of the bound dictionary (see
    ///    [`Self::from_dictionary_surfaces`]), because the dictionary lists that
    ///    spelling and its unified form is a different or missing entry: UniDic
    ///    has 臭氣 only with U+FA5C, and NFC splits it into two tokens.
    ///    Everywhere else the singleton is replaced, so 神 U+FA19 in an ordinary
    ///    compound reaches the unified entry. A one-character surface is
    ///    protected only when its unified form occurs in no surface: UniDic's
    ///    U+FA15 alone is the name ヒロシ, but U+51DE spells 康凞 and 忠凞, so
    ///    U+FA15 is replaced and those entries are reached.
    ///
    /// Each rule maps the text to a fixed point, so folding a view returns it
    /// unchanged.
    ///
    /// # Panics
    ///
    /// Panics if `source` has more than `u32::MAX` code points.
    #[must_use]
    pub fn fold(&self, source: &str) -> AnalysisView {
        if !needs_fold(source) {
            return AnalysisView::unchanged(source);
        }
        let cells = match &self.protected {
            None => fold_cells(source, Keep::None),
            Some(automaton) => {
                let kept_all = fold_cells(source, Keep::AllSingletons);
                match protected_cells(&kept_all, automaton) {
                    Some(protected) => normalize(&kept_all, Keep::Cells(&protected)),
                    None => kept_all,
                }
            }
        };
        cells.into_view()
    }
}

/// Changes whenever [`text_key`] produces different output for any input.
pub(crate) const TEXT_KEY_REVISION: &[u8] = b"lingrep.text-key/1";

pub(crate) static UNPROTECTED: LazyLock<ViewFold> = LazyLock::new(ViewFold::unprotected);

/// The dictionary-independent match key of a token surface.
///
/// The key is [`ViewFold::unprotected`] applied to `surface`, with every
/// full-width ASCII variant U+FF01..U+FF5D then replaced by U+0021..U+007D, the
/// character its `<wide>` decomposition names (UnicodeData.txt). U+FF5E is not
/// replaced because the fold has already made it U+301C WAVE DASH, and U+3000
/// IDEOGRAPHIC SPACE is not a full-width ASCII variant.
#[must_use]
pub fn text_key(surface: &str) -> String {
    let (folded, _) = UNPROTECTED.fold(surface).into_parts();
    if !folded.chars().any(is_full_width_ascii) {
        return folded;
    }
    folded
        .chars()
        .map(|c| {
            if is_full_width_ascii(c) {
                char::from_u32(u32::from(c) - 0xFEE0).expect("U+0021..U+007D are characters")
            } else {
                c
            }
        })
        .collect()
}

/// Converts token boundaries of an analysis view, as view byte offsets, to
/// byte offsets of its source. A boundary inside an alignment group moves to
/// the end of the group, so the token that starts the group covers all of its
/// source and the next token's source range is empty.
pub(crate) struct SourceBoundaries<'a> {
    map: &'a OffsetMap,
    view: CodePointCursor<'a>,
    source: CodePointCursor<'a>,
}

impl<'a> SourceBoundaries<'a> {
    pub(crate) fn new(source: &'a str, view: &'a AnalysisView) -> Self {
        Self {
            map: view.map(),
            view: CodePointCursor::new(view.text()),
            source: CodePointCursor::new(source),
        }
    }

    /// # Panics
    ///
    /// Panics if `view_byte` is not a character boundary of the view.
    pub(crate) fn source_byte(&mut self, view_byte: usize) -> usize {
        let position = self.view.position_at(view_byte);
        let source = self.map.to_source(ViewSpan::new(position, position)).end();
        self.source.byte_at(source)
    }
}

/// Converts between byte offsets and code-point positions of one text. Each
/// call scans forward from the previous position. A lookup before that
/// position restarts the scan at the start of the text.
pub(crate) struct CodePointCursor<'a> {
    text: &'a str,
    byte: usize,
    position: u32,
}

impl<'a> CodePointCursor<'a> {
    pub(crate) const fn new(text: &'a str) -> Self {
        Self {
            text,
            byte: 0,
            position: 0,
        }
    }

    /// # Panics
    ///
    /// Panics if `position` exceeds the text's code-point count.
    pub(crate) fn byte_at(&mut self, position: u32) -> usize {
        if position < self.position {
            *self = Self::new(self.text);
        }
        let mut chars = self
            .text
            .get(self.byte..)
            .expect("the cursor stays on a character boundary")
            .chars();
        while self.position < position {
            let c = chars.next().expect("position lies within the text");
            self.byte += c.len_utf8();
            self.position += 1;
        }
        self.byte
    }

    /// # Panics
    ///
    /// Panics if `byte` is not a character boundary of the text.
    pub(crate) fn position_at(&mut self, byte: usize) -> u32 {
        if byte < self.byte {
            *self = Self::new(self.text);
        }
        let count = self
            .text
            .get(self.byte..byte)
            .expect("byte lies on a character boundary of the text")
            .chars()
            .count();
        self.byte = byte;
        self.position += u32::try_from(count).expect("a text fits u32 code points");
        self.position
    }
}

fn is_full_width_ascii(c: char) -> bool {
    matches!(c, '\u{FF01}'..='\u{FF5D}')
}

fn needs_fold(source: &str) -> bool {
    let rewritten = Cell::new(false);
    let quick = is_nfc_quick(source.chars().inspect(|&c| {
        if is_char_rewrite(c) {
            rewritten.set(true);
        }
    }));
    rewritten.get() || quick != IsNormalized::Yes
}

fn is_char_rewrite(c: char) -> bool {
    matches!(c, '\u{FE00}'..='\u{FE0F}' | '\u{FF5E}' | '\u{FF61}'..='\u{FF9F}' | '\u{E0100}'..='\u{E01EF}')
}

fn is_variation_selector(c: char) -> bool {
    matches!(c, '\u{FE00}'..='\u{FE0F}' | '\u{E0100}'..='\u{E01EF}')
}

/// The NFC form of a starter singleton, the only singletons the fold can keep.
/// A singleton's canonical decomposition mapping is one other character
/// (UAX #15 §5.1); NFC maps it, alone, to one different character.
fn singleton_target(c: char) -> Option<char> {
    if c < '\u{0340}'
        || is_nfc_quick(std::iter::once(c)) != IsNormalized::No
        || unicode_char::canonical_combining_class(c) != 0
    {
        return None;
    }
    let mut normalized = std::iter::once(c).nfc();
    match (normalized.next(), normalized.next()) {
        (Some(target), None) if target != c => Some(target),
        _ => None,
    }
}

/// Whether NFC of a text splits before `c`: the first code point of its
/// canonical decomposition is a starter that cannot compose with a preceding
/// character (`NFC_Quick_Check` is not Maybe).
fn starts_chunk(c: char) -> bool {
    if c < '\u{0300}' {
        return true;
    }
    let mut first = None;
    unicode_char::decompose_canonical(c, |d| {
        first.get_or_insert(d);
    });
    let first = first.unwrap_or(c);
    unicode_char::canonical_combining_class(first) == 0
        && is_nfc_quick(std::iter::once(first)) != IsNormalized::Maybe
}

/// Maps a standardized variation sequence to its CJK compatibility ideograph.
static COMPATIBILITY_VARIANTS: LazyLock<FxHashMap<(char, char), char>> = LazyLock::new(|| {
    let mut variants = FxHashMap::default();
    for c in ('\u{F900}'..='\u{FAFF}').chain('\u{2F800}'..='\u{2FA1F}') {
        let mut sequence = Vec::with_capacity(2);
        unicode_char::decompose_cjk_compat_variants(c, |d| sequence.push(d));
        if let [base, selector] = sequence[..] {
            variants.insert((base, selector), c);
        }
    }
    variants
});

/// Folded code points grouped by the source code points that produced them.
/// Cell `i` covers source `source_ends[i - 1]..source_ends[i]` and view
/// `view_ends[i - 1]..view_ends[i]`, with both starting at zero.
#[derive(Default)]
struct Cells {
    chars: Vec<char>,
    source_ends: Vec<u32>,
    view_ends: Vec<u32>,
    source_len: u32,
}

impl Cells {
    fn len(&self) -> usize {
        self.view_ends.len()
    }

    fn chars_of(&self, cell: usize) -> &[char] {
        let start = cell
            .checked_sub(1)
            .map_or(0, |previous| self.view_ends[previous]);
        &self.chars[start as usize..self.view_ends[cell] as usize]
    }

    fn push(&mut self, chars: &[char], source_end: u32) {
        self.chars.extend_from_slice(chars);
        self.view_ends.push(self.chars.len() as u32);
        self.source_ends.push(source_end);
    }

    fn text(&self) -> String {
        self.chars.iter().collect()
    }

    fn into_view(self) -> AnalysisView {
        let text = self.text();
        let view_len = self.chars.len() as u32;
        let mut groups = Vec::new();
        if self.view_ends.is_empty() && self.source_len > 0 {
            groups.push(Group {
                source_start: 0,
                source_end: self.source_len,
                view_start: 0,
                view_end: 0,
            });
        }
        let (mut source_start, mut view_start) = (0, 0);
        for (&source_end, &view_end) in self.source_ends.iter().zip(&self.view_ends) {
            if source_end - source_start != 1 || view_end - view_start != 1 {
                groups.push(Group {
                    source_start,
                    source_end,
                    view_start,
                    view_end,
                });
            }
            (source_start, view_start) = (source_end, view_end);
        }
        AnalysisView {
            text,
            map: OffsetMap {
                source_len: self.source_len,
                view_len,
                groups,
            },
        }
    }
}

#[derive(Clone, Copy)]
enum Keep<'a> {
    None,
    AllSingletons,
    Cells(&'a [bool]),
}

impl Keep<'_> {
    fn keeps(self, cells: &Cells, cell: usize) -> bool {
        match self {
            Self::None => false,
            Self::AllSingletons => {
                matches!(cells.chars_of(cell), &[c] if singleton_target(c).is_some())
            }
            Self::Cells(kept) => kept[cell],
        }
    }
}

/// Rules 1 to 3, then rule 4 with the singletons `keep` selects.
fn fold_cells(source: &str, keep: Keep<'_>) -> Cells {
    let mut cells = Cells::default();
    // The last source code point when it alone produced the last cell.
    let mut plain_previous: Option<char> = None;
    let mut half_width_previous = false;
    let mut position = 0u32;
    for c in source.chars() {
        position = position
            .checked_add(1)
            .expect("source exceeds u32 code points");
        let half_width = std::mem::take(&mut half_width_previous);
        let plain = plain_previous.take();
        if is_variation_selector(c) {
            if let Some(last) = cells.source_ends.last_mut() {
                *last = position;
                if let Some(&compatibility) =
                    plain.and_then(|base| COMPATIBILITY_VARIANTS.get(&(base, c)))
                {
                    *cells.chars.last_mut().expect("a cell holds a code point") = compatibility;
                }
            }
            continue;
        }
        match c {
            '\u{FF9E}' | '\u{FF9F}' => {
                let mark = if c == '\u{FF9E}' {
                    '\u{3099}'
                } else {
                    '\u{309A}'
                };
                let composite = half_width
                    .then(|| cells.chars.last().copied())
                    .flatten()
                    .and_then(|base| unicode_char::compose(base, mark));
                if let Some(composite) = composite {
                    *cells.chars.last_mut().expect("a cell holds a code point") = composite;
                    *cells.source_ends.last_mut().expect("a cell exists") = position;
                } else {
                    let spacing = if c == '\u{FF9E}' {
                        '\u{309B}'
                    } else {
                        '\u{309C}'
                    };
                    cells.push(&[spacing], position);
                }
            }
            '\u{FF61}'..='\u{FF9D}' => {
                let mut full_width = c;
                unicode_char::decompose_compatible(c, |d| full_width = d);
                cells.push(&[full_width], position);
                half_width_previous = true;
            }
            '\u{FF5E}' => cells.push(&['\u{301C}'], position),
            _ => {
                cells.push(&[c], position);
                plain_previous = Some(c);
            }
        }
    }
    cells.source_len = position;
    normalize(&cells, keep)
}

/// Applies NFC chunk by chunk, retaining the source boundaries of unchanged
/// cells after a composed prefix. A kept singleton forms its own chunk because
/// canonical reordering stops at a starter.
fn normalize(input: &Cells, keep: Keep<'_>) -> Cells {
    let mut output = Cells {
        chars: Vec::with_capacity(input.chars.len()),
        source_ends: Vec::with_capacity(input.len()),
        view_ends: Vec::with_capacity(input.len()),
        source_len: input.source_len,
    };
    let mut chunk = Vec::new();
    let mut start = 0;
    while start < input.len() {
        let mut end = start + 1;
        let kept = keep.keeps(input, start);
        if !kept {
            while end < input.len()
                && !keep.keeps(input, end)
                && !input
                    .chars_of(end)
                    .first()
                    .copied()
                    .is_some_and(starts_chunk)
            {
                end += 1;
            }
        }
        let single = input.chars_of(start);
        let unchanged = kept
            || (end == start + 1
                && single.len() == 1
                && is_nfc_quick(single.iter().copied()) == IsNormalized::Yes);
        if unchanged {
            output.push(single, input.source_ends[start]);
        } else {
            chunk.clear();
            for cell in start..end {
                chunk.extend_from_slice(input.chars_of(cell));
            }
            let normalized: Vec<char> = chunk.iter().copied().nfc().collect();
            if normalized == chunk {
                for cell in start..end {
                    output.push(input.chars_of(cell), input.source_ends[cell]);
                }
            } else {
                let mut prefix_end = end;
                let mut prefix_len = normalized.len();
                while prefix_end > start + 1 {
                    let tail = input.chars_of(prefix_end - 1);
                    if tail.len() >= prefix_len || !normalized[..prefix_len].ends_with(tail) {
                        break;
                    }
                    prefix_end -= 1;
                    prefix_len -= tail.len();
                }
                let source_prefix_len = chunk.len() - (normalized.len() - prefix_len);
                if prefix_end < end
                    && chunk[..source_prefix_len]
                        .iter()
                        .copied()
                        .nfc()
                        .eq(normalized[..prefix_len].iter().copied())
                {
                    output.push(&normalized[..prefix_len], input.source_ends[prefix_end - 1]);
                    for cell in prefix_end..end {
                        output.push(input.chars_of(cell), input.source_ends[cell]);
                    }
                } else {
                    output.push(&normalized, input.source_ends[end - 1]);
                }
            }
        }
        start = end;
    }
    output
}

/// Marks the singleton cells inside an occurrence of a protected surface, or
/// returns `None` when the text has no singleton.
fn protected_cells(cells: &Cells, automaton: &AhoCorasick) -> Option<Vec<bool>> {
    let mut text = String::with_capacity(cells.chars.len() * 3);
    let mut singletons = Vec::new();
    for cell in 0..cells.len() {
        let chars = cells.chars_of(cell);
        if let &[c] = chars
            && singleton_target(c).is_some()
        {
            singletons.push((text.len(), cell));
        }
        text.extend(chars);
    }
    if singletons.is_empty() {
        return None;
    }
    let mut kept = vec![false; cells.len()];
    for found in automaton.find_overlapping_iter(&text) {
        let first = singletons.partition_point(|&(byte, _)| byte < found.start());
        for &(byte, cell) in singletons.get(first..).unwrap_or_default() {
            if byte >= found.end() {
                break;
            }
            kept[cell] = true;
        }
    }
    Some(kept)
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    /// Surfaces standing in for a dictionary: compatibility-ideograph
    /// spellings with and without a unified entry, UniDic's U+212B surfaces,
    /// and UniDic's U+FA15 name entry whose unified form U+51DE occurs only
    /// inside compounds.
    const DICTIONARY: &[&str] = &[
        "\u{FA5C}氣",
        "\u{FA10}",
        "\u{FA15}",
        "康\u{51DE}",
        "神",
        "\u{FA19}",
        "\u{FA19}經",
        "\u{212B}",
        "（・\u{212B}・）",
        "葛城",
        "市",
    ];

    fn dictionary() -> ViewFold {
        ViewFold::from_dictionary_surfaces(DICTIONARY.iter().copied())
    }

    fn slice(text: &str, start: u32, end: u32) -> String {
        text.chars()
            .skip(start as usize)
            .take((end - start) as usize)
            .collect()
    }

    fn view_slice(view: &AnalysisView, span: ViewSpan) -> String {
        slice(view.text(), span.start(), span.end())
    }

    /// Source text, view text, and the view span each view code point's
    /// source maps back to.
    #[yare::parameterized(
        ideographic_variation_sequence = { "葛\u{E0100}城市", "葛城市", &[(0, 2), (2, 3), (3, 4)] },
        ivs_before_kana = { "赧\u{E0101}らめた", "赧らめた", &[(0, 2), (2, 3), (3, 4), (4, 5)] },
        half_width_with_voiced_mark = { "ｶﾞﾗｽﾉｸﾂ", "ガラスノクツ", &[(0, 2), (2, 3), (3, 4), (4, 5), (5, 6), (6, 7)] },
        voiced_mark_without_composite = { "ｱﾞ", "ア\u{309B}", &[(0, 1), (1, 2)] },
        voiced_mark_after_full_width = { "カﾞ", "カ\u{309B}", &[(0, 1), (1, 2)] },
        combining_voiced_mark = { "か\u{3099}", "が", &[(0, 2)] },
        composition_with_trailing_mark = { "U\u{0304}\u{0304}", "Ū\u{0304}", &[(0, 2), (2, 3)] },
        fullwidth_tilde = { "長～い", "長〜い", &[(0, 1), (1, 2), (2, 3)] },
        greek_oxia = { "\u{1F71}", "\u{03AC}", &[(0, 1)] },
        leading_selector = { "\u{FE00}葛", "葛", &[(0, 2)] },
        half_width_punctuation = { "｢ｳﾞｪﾙ｣｡", "「ヴェル」。", &[(0, 1), (1, 3), (3, 4), (4, 5), (5, 6), (6, 7)] },
    )]
    fn unprotected_fold(source: &str, expected: &str, sources: &[(u32, u32)]) {
        let view = ViewFold::unprotected().fold(source);
        assert_eq!(view.text(), expected);
        let mapped: Vec<(u32, u32)> = (0..view.map().view_len())
            .map(|start| {
                let span = view.map().to_source(ViewSpan::new(start, start + 1));
                (span.start(), span.end())
            })
            .collect();
        assert_eq!(mapped, sources);
    }

    #[yare::parameterized(
        compatibility_ideograph_with_entry = { "塚\u{FA10}", "塚\u{FA10}" },
        compatibility_surface = { "\u{FA5C}氣", "\u{FA5C}氣" },
        compatibility_char_outside_its_surface = { "\u{FA5C}い", "\u{81ED}い" },
        single_compatibility_entry_with_unified_entry = { "\u{FA19}樣", "神樣" },
        compatibility_char_inside_compound_entry = { "\u{FA19}經", "\u{FA19}經" },
        standardized_variant_becomes_its_compatibility_ideograph = { "塚\u{FE00}", "\u{FA10}" },
        emoticon_keeps_angstrom_sign = { "（・\u{212B}・）", "（・\u{212B}・）" },
        single_angstrom_sign_without_unified_form_in_any_surface = { "\u{212B}", "\u{212B}" },
        single_compatibility_entry_whose_unified_form_is_in_a_compound = { "康\u{FA15}", "康\u{51DE}" },
        single_compatibility_entry_alone_whose_unified_form_is_in_a_compound = { "\u{FA15}", "\u{51DE}" },
        greek_oxia_outside_dictionary = { "\u{1F71}", "\u{03AC}" },
    )]
    fn dictionary_fold(source: &str, expected: &str) {
        assert_eq!(dictionary().fold(source).text(), expected);
    }

    #[yare::parameterized(
        compatibility_ideograph = { "塚\u{FA10}", "塚塚" },
        compatibility_surface = { "\u{FA5C}氣", "\u{81ED}氣" },
        standardized_variant = { "塚\u{FE00}", "塚" },
        angstrom_sign = { "\u{212B}", "\u{00C5}" },
    )]
    fn unprotected_fold_replaces_every_singleton(source: &str, expected: &str) {
        assert_eq!(ViewFold::unprotected().fold(source).text(), expected);
    }

    #[yare::parameterized(
        first_full_width_ascii = { "\u{FF01}", "!" },
        last_full_width_ascii = { "\u{FF5D}", "}" },
        full_width_letters_and_digits = { "ＮＨＫ１２３", "NHK123" },
        fullwidth_tilde_is_wave_dash = { "長\u{FF5E}い", "長\u{301C}い" },
        ideographic_space = { "\u{3000}", "\u{3000}" },
        white_parenthesis_after_the_range = { "\u{FF5F}", "\u{FF5F}" },
        unprotected_fold_first = { "ｶﾞﾗｽ葛\u{E0100}\u{FA5C}氣", "ガラス葛\u{81ED}氣" },
    )]
    fn text_key_folds_full_width_ascii_after_the_unprotected_fold(surface: &str, expected: &str) {
        assert_eq!(text_key(surface), expected);
    }

    #[test]
    fn source_of_only_selectors_has_an_empty_view_covering_it() {
        let view = ViewFold::unprotected().fold("\u{FE00}\u{E0100}");
        assert_eq!(view.text(), "");
        assert_eq!(
            view.map().to_source(ViewSpan::new(0, 0)),
            SourceSpan::new(0, 2)
        );
        assert_eq!(
            view.map().to_view(SourceSpan::new(1, 2)),
            ViewSpan::new(0, 0)
        );
    }

    #[test]
    fn identity_depends_on_protected_surfaces_only() {
        let reordered: Vec<&str> = DICTIONARY
            .iter()
            .rev()
            .copied()
            .chain(["犬", "猫"])
            .collect();
        assert_eq!(
            ViewFold::from_dictionary_surfaces(reordered).identity(),
            dictionary().identity()
        );
        assert_eq!(
            ViewFold::from_dictionary_surfaces(["犬", "神"]).identity(),
            ViewFold::unprotected().identity()
        );
        assert_ne!(
            ViewFold::from_dictionary_surfaces(["\u{FA10}"]).identity(),
            ViewFold::unprotected().identity()
        );
    }

    const ATOMS: &[&str] = &[
        "a",
        "Z",
        " ",
        "か",
        "は",
        "が",
        "ア",
        "ー",
        "ｶ",
        "ｱ",
        "ｳ",
        "ﾜ",
        "ｰ",
        "ﾞ",
        "ﾟ",
        "｡",
        "｢",
        "葛",
        "城",
        "市",
        "塚",
        "臭",
        "氣",
        "神",
        "經",
        "樣",
        "赧",
        "\u{FA10}",
        "\u{FA5C}",
        "\u{FA19}",
        "\u{F91D}",
        "\u{FA1F}",
        "\u{2F81A}",
        "\u{FE00}",
        "\u{FE0F}",
        "\u{E0100}",
        "\u{E0101}",
        "\u{3099}",
        "\u{309A}",
        "\u{0301}",
        "\u{0316}",
        "\u{0345}",
        "\u{030A}",
        "\u{1F71}",
        "\u{03B1}",
        "\u{212B}",
        "A",
        "\u{00C5}",
        "\u{1100}",
        "\u{1161}",
        "\u{11A8}",
        "～",
        "〜",
        "\u{309B}",
        "（",
        "・",
        "）",
        "\u{FA5C}氣",
        "\u{FA19}經",
        "（・\u{212B}・）",
        "塚\u{FE00}",
        "\u{E0100}\u{FE00}",
    ];

    fn sources() -> impl Strategy<Value = String> {
        proptest::collection::vec(proptest::sample::select(ATOMS), 0..24)
            .prop_map(|atoms| atoms.concat())
    }

    fn folds() -> impl Strategy<Value = ViewFold> {
        prop_oneof![Just(ViewFold::unprotected()), Just(dictionary())]
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(512))]

        #[test]
        fn map_is_total_and_monotonic(source in sources(), fold in folds()) {
            let view = fold.fold(&source);
            let map = view.map();
            prop_assert_eq!(map.source_len() as usize, source.chars().count());
            prop_assert_eq!(map.view_len() as usize, view.text().chars().count());
            prop_assert_eq!(map.to_source(ViewSpan::new(0, map.view_len())), SourceSpan::new(0, map.source_len()));
            prop_assert_eq!(map.to_view(SourceSpan::new(0, map.source_len())), ViewSpan::new(0, map.view_len()));
            let mut previous = SourceSpan::default();
            for position in 0..=map.view_len() {
                let point = map.to_source(ViewSpan::new(position, position));
                prop_assert!(point.start() >= previous.start() && point.end() >= previous.end());
                previous = point;
            }
            let mut previous = ViewSpan::default();
            for position in 0..=map.source_len() {
                let point = map.to_view(SourceSpan::new(position, position));
                prop_assert!(point.start() >= previous.start() && point.end() >= previous.end());
                previous = point;
            }
        }

        #[test]
        fn folding_the_source_of_a_view_span_reproduces_it(
            source in sources(),
            fold in folds(),
            cut in (0u32..32, 0u32..32),
        ) {
            let view = fold.fold(&source);
            let len = view.map().view_len();
            let (start, end) = (cut.0.min(cut.1).min(len), cut.0.max(cut.1).min(len));
            let source_span = view.map().to_source(ViewSpan::new(start, end));
            let covering = view.map().to_view(source_span);
            prop_assert!(covering.start() <= start && covering.end() >= end);
            let refolded = fold.fold(&slice(&source, source_span.start(), source_span.end()));
            let expected = view_slice(&view, covering);
            // A slice can drop the context that protects a singleton, so compare
            // with every singleton replaced.
            let unprotected = ViewFold::unprotected();
            let (refolded_key, expected_key) = (unprotected.fold(refolded.text()), unprotected.fold(&expected));
            prop_assert_eq!(refolded_key.text(), expected_key.text());
            if fold.identity() == unprotected.identity() {
                prop_assert_eq!(refolded.text(), expected);
            }
        }

        #[test]
        fn canonical_composition_is_nfc_and_its_spans_compose_to_their_slices(
            source in sources(),
            cut in (0u32..32, 0u32..32),
        ) {
            let view = AnalysisView::canonical_composition(&source);
            prop_assert_eq!(view.text(), source.nfc().collect::<String>());
            let map = view.map();
            prop_assert_eq!(map.source_len() as usize, source.chars().count());
            let len = map.view_len();
            let (start, end) = (cut.0.min(cut.1).min(len), cut.0.max(cut.1).min(len));
            let source_span = map.to_source(ViewSpan::new(start, end));
            let covering = map.to_view(source_span);
            prop_assert!(covering.start() <= start && covering.end() >= end);
            prop_assert_eq!(
                slice(&source, source_span.start(), source_span.end()).nfc().collect::<String>(),
                view_slice(&view, covering)
            );
        }

        #[test]
        fn folding_is_idempotent(source in sources(), fold in folds()) {
            let view = fold.fold(&source);
            let again = fold.fold(view.text());
            prop_assert_eq!(again.text(), view.text());
            prop_assert!(again.map().is_one_to_one());
        }

        #[test]
        fn no_view_boundary_separates_a_selector_or_sound_mark_from_its_base(
            source in sources(),
            fold in folds(),
        ) {
            let view = fold.fold(&source);
            let chars: Vec<char> = source.chars().collect();
            for (index, pair) in chars.windows(2).enumerate() {
                let preceded = chars[..=index].iter().any(|&c| !is_variation_selector(c));
                let selector = is_variation_selector(pair[1]) && preceded;
                let composed_mark = matches!(pair[0], '\u{FF61}'..='\u{FF9D}')
                    && matches!(pair[1], '\u{FF9E}' | '\u{FF9F}')
                    && {
                        let mut base = pair[0];
                        unicode_char::decompose_compatible(pair[0], |d| base = d);
                        let mark = if pair[1] == '\u{FF9E}' { '\u{3099}' } else { '\u{309A}' };
                        unicode_char::compose(base, mark).is_some()
                    };
                if selector || composed_mark {
                    let boundary = index as u32 + 1;
                    let image = view.map().to_view(SourceSpan::new(boundary, boundary));
                    prop_assert!(image.start() < image.end(), "boundary {} of {:?} is a view boundary", boundary, source);
                }
            }
        }
    }
}
