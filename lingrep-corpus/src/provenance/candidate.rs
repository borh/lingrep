use crate::CorpusError;
#[cfg(test)]
use lingrep_core::analysis::ValidatedAnalysisBatch;
use lingrep_core::analysis::{AnalyzedInput, AnalyzedSentence};

const COMPONENT: &str = "candidate V1 provenance";
const MAGIC: &[u8; 8] = b"LINGPRV1";
const HEADER_LEN: usize = 25;
const LAYOUT_TAG: u8 = 0;
const MAX_RAW_BYTES: usize = 512 * 1024 * 1024;
const MAX_ITEMS: usize = 10_000_000;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct CandidateSurface {
    pub source_start: u32,
    pub source_end: u32,
    pub word_start: u32,
    pub word_end: u32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct CandidateSentence {
    pub text: String,
    pub surfaces: Vec<CandidateSurface>,
}

pub(super) fn from_bound(
    text: &str,
    words: &lingrep_core::token_match::BoundTokenSentence,
) -> Result<Vec<CandidateSurface>, CorpusError> {
    use lingrep_core::token_match::TokenSequence;
    let boundaries = text
        .char_indices()
        .map(|(byte, _)| byte)
        .chain(std::iter::once(text.len()))
        .collect::<Vec<_>>();
    let mut surfaces: Vec<CandidateSurface> = Vec::new();
    for word in 0..words.len() {
        let Some(span) = words.char_span(word) else {
            return Ok(Vec::new());
        };
        let start = *boundaries
            .get(span.start as usize)
            .ok_or_else(|| invalid("character start exceeds sentence"))? as u32;
        let end = *boundaries
            .get(span.end as usize)
            .ok_or_else(|| invalid("character end exceeds sentence"))? as u32;
        if let Some(previous) = surfaces.last_mut()
            && previous.source_start == start
            && previous.source_end == end
        {
            previous.word_end += 1;
        } else {
            if start >= end
                || surfaces
                    .last()
                    .is_some_and(|previous| previous.source_end > start)
            {
                return Err(invalid("character offsets are not ordered surface ranges"));
            }
            surfaces.push(CandidateSurface {
                source_start: start,
                source_end: end,
                word_start: word as u32,
                word_end: word as u32 + 1,
            });
        }
    }
    Ok(surfaces)
}

#[cfg(test)]
pub(super) fn from_analysis_batch(
    batch: &ValidatedAnalysisBatch,
) -> Result<Vec<CandidateSentence>, CorpusError> {
    let sentence_count = batch
        .inputs()
        .iter()
        .try_fold(0_usize, |count, input| {
            count.checked_add(input.sentences().len())
        })
        .ok_or_else(|| invalid("candidate sentence count overflow"))?;
    if sentence_count > MAX_ITEMS {
        return Err(invalid("too many candidate sentences"));
    }
    let mut candidates = Vec::with_capacity(sentence_count);
    for analyzed_input in batch.inputs() {
        for sentence in analyzed_input.sentences() {
            let sentence_start = sentence.source_range().start();
            let text = analyzed_input
                .input()
                .slice(sentence.source_range())
                .map_err(|_| invalid("canonical sentence range is invalid"))?
                .to_owned();
            let surfaces = sentence
                .surface_tokens()
                .iter()
                .map(|surface| {
                    let source_start =
                        surface
                            .source_range()
                            .start()
                            .checked_sub(sentence_start)
                            .ok_or_else(|| invalid("surface begins before its sentence"))?;
                    let source_end = surface
                        .source_range()
                        .end()
                        .checked_sub(sentence_start)
                        .ok_or_else(|| invalid("surface ends before its sentence"))?;
                    let word_range = surface.word_range();
                    Ok(CandidateSurface {
                        source_start,
                        source_end,
                        word_start: word_range.start,
                        word_end: word_range.end,
                    })
                })
                .collect::<Result<Vec<_>, CorpusError>>()?;
            candidates.push(CandidateSentence { text, surfaces });
        }
    }
    validate(&candidates)?;
    Ok(candidates)
}

pub(super) fn from_analyzed_sentence(
    analyzed: &AnalyzedInput,
    sentence: &AnalyzedSentence,
) -> Result<CandidateSentence, CorpusError> {
    let sentence_start = sentence.source_range().start();
    let text = analyzed
        .input()
        .slice(sentence.source_range())
        .map_err(|_| invalid("canonical sentence range is invalid"))?
        .to_owned();
    let surfaces = sentence
        .surface_tokens()
        .iter()
        .map(|surface| {
            let source_start = surface
                .source_range()
                .start()
                .checked_sub(sentence_start)
                .ok_or_else(|| invalid("surface begins before its sentence"))?;
            let source_end = surface
                .source_range()
                .end()
                .checked_sub(sentence_start)
                .ok_or_else(|| invalid("surface ends before its sentence"))?;
            let word_range = surface.word_range();
            Ok(CandidateSurface {
                source_start,
                source_end,
                word_start: word_range.start,
                word_end: word_range.end,
            })
        })
        .collect::<Result<Vec<_>, CorpusError>>()?;
    let candidate = CandidateSentence { text, surfaces };
    validate(std::slice::from_ref(&candidate))?;
    Ok(candidate)
}

pub(super) fn encode(sentences: &[CandidateSentence]) -> Result<Vec<u8>, CorpusError> {
    validate(sentences)?;
    let mut raw = Vec::new();
    put_varint(&mut raw, sentences.len() as u64);
    for sentence in sentences {
        put_bytes(&mut raw, sentence.text.as_bytes());
        put_varint(&mut raw, sentence.surfaces.len() as u64);
        let mut prior_source_end = 0_u32;
        let mut prior_word_end = 0_u32;
        for surface in &sentence.surfaces {
            put_varint(&mut raw, u64::from(surface.source_start - prior_source_end));
            put_varint(
                &mut raw,
                u64::from(surface.source_end - surface.source_start),
            );
            put_varint(&mut raw, u64::from(surface.word_start - prior_word_end));
            put_varint(&mut raw, u64::from(surface.word_end - surface.word_start));
            prior_source_end = surface.source_end;
            prior_word_end = surface.word_end;
        }
    }
    if raw.len() > MAX_RAW_BYTES {
        return Err(invalid("raw payload exceeds its fixed bound"));
    }
    let stored = zstd::bulk::compress(&raw, 3)
        .map_err(|error| invalid(format!("compression failed: {error}")))?;
    let mut artifact = Vec::with_capacity(HEADER_LEN + stored.len());
    artifact.extend_from_slice(MAGIC);
    artifact.push(LAYOUT_TAG);
    artifact.extend_from_slice(&(raw.len() as u64).to_le_bytes());
    artifact.extend_from_slice(&(stored.len() as u64).to_le_bytes());
    artifact.extend_from_slice(&stored);
    Ok(artifact)
}

pub(super) fn decode(artifact: &[u8]) -> Result<Vec<CandidateSentence>, CorpusError> {
    if artifact.len() < HEADER_LEN || &artifact[..8] != MAGIC || artifact[8] != LAYOUT_TAG {
        return Err(invalid("invalid magic, version, or layout"));
    }
    let raw_len = usize_from_u64(u64::from_le_bytes(
        artifact[9..17]
            .try_into()
            .map_err(|_| invalid("invalid raw length"))?,
    ))?;
    let stored_len = usize_from_u64(u64::from_le_bytes(
        artifact[17..25]
            .try_into()
            .map_err(|_| invalid("invalid stored length"))?,
    ))?;
    if raw_len > MAX_RAW_BYTES || HEADER_LEN.checked_add(stored_len) != Some(artifact.len()) {
        return Err(invalid("payload length is invalid"));
    }
    let raw = zstd::bulk::decompress(&artifact[HEADER_LEN..], raw_len)
        .map_err(|error| invalid(format!("decompression failed: {error}")))?;
    if raw.len() != raw_len {
        return Err(invalid("decompressed length differs from its header"));
    }
    let mut reader = Reader::new(&raw);
    let sentence_count = reader.count("sentence count")?;
    if sentence_count > reader.bytes.len().saturating_sub(reader.offset) / 2 {
        return Err(invalid("sentence count exceeds payload"));
    }
    let mut sentences = Vec::with_capacity(sentence_count);
    for _ in 0..sentence_count {
        let text = std::str::from_utf8(reader.bytes("sentence text")?)
            .map_err(|_| invalid("sentence text is not UTF-8"))?
            .to_owned();
        let surface_count = reader.count("surface count")?;
        if surface_count > reader.bytes.len().saturating_sub(reader.offset) / 4 {
            return Err(invalid("surface count exceeds payload"));
        }
        let mut surfaces = Vec::with_capacity(surface_count);
        let mut prior_source_end = 0_u32;
        let mut prior_word_end = 0_u32;
        for _ in 0..surface_count {
            let source_start = prior_source_end
                .checked_add(reader.u32("source gap")?)
                .ok_or_else(|| invalid("source start overflow"))?;
            let source_end = source_start
                .checked_add(reader.u32("source length")?)
                .ok_or_else(|| invalid("source end overflow"))?;
            let word_start = prior_word_end
                .checked_add(reader.u32("word gap")?)
                .ok_or_else(|| invalid("word start overflow"))?;
            let word_end = word_start
                .checked_add(reader.u32("word length")?)
                .ok_or_else(|| invalid("word end overflow"))?;
            surfaces.push(CandidateSurface {
                source_start,
                source_end,
                word_start,
                word_end,
            });
            prior_source_end = source_end;
            prior_word_end = word_end;
        }
        sentences.push(CandidateSentence { text, surfaces });
    }
    reader.finish()?;
    validate(&sentences)?;
    Ok(sentences)
}

fn validate(sentences: &[CandidateSentence]) -> Result<(), CorpusError> {
    if sentences.len() > MAX_ITEMS {
        return Err(invalid("too many sentences"));
    }
    for sentence in sentences {
        if sentence.surfaces.len() > MAX_ITEMS {
            return Err(invalid("too many surfaces"));
        }
        let mut prior_source_end = 0_u32;
        let mut prior_word_end = 0_u32;
        for surface in &sentence.surfaces {
            let start = surface.source_start as usize;
            let end = surface.source_end as usize;
            if surface.source_start < prior_source_end
                || start >= end
                || end > sentence.text.len()
                || !sentence.text.is_char_boundary(start)
                || !sentence.text.is_char_boundary(end)
                || surface.word_start < prior_word_end
                || surface.word_start >= surface.word_end
            {
                return Err(invalid("invalid source range or word alignment"));
            }
            prior_source_end = surface.source_end;
            prior_word_end = surface.word_end;
        }
    }
    Ok(())
}

fn put_bytes(output: &mut Vec<u8>, bytes: &[u8]) {
    put_varint(output, bytes.len() as u64);
    output.extend_from_slice(bytes);
}

fn put_varint(output: &mut Vec<u8>, mut value: u64) {
    while value >= 0x80 {
        output.push((value as u8) | 0x80);
        value >>= 7;
    }
    output.push(value as u8);
}

fn usize_from_u64(value: u64) -> Result<usize, CorpusError> {
    usize::try_from(value).map_err(|_| invalid("stored length exceeds usize"))
}

struct Reader<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl<'a> Reader<'a> {
    const fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, offset: 0 }
    }

    fn varint(&mut self, label: &str) -> Result<u64, CorpusError> {
        let mut value = 0_u64;
        for shift in (0..=63).step_by(7) {
            let byte = *self
                .bytes
                .get(self.offset)
                .ok_or_else(|| invalid(format!("truncated {label}")))?;
            self.offset += 1;
            if shift == 63 && byte > 1 {
                return Err(invalid(format!("overflowing {label}")));
            }
            value |= u64::from(byte & 0x7f) << shift;
            if byte & 0x80 == 0 {
                if shift > 0 && byte == 0 {
                    return Err(invalid(format!("non-canonical {label}")));
                }
                return Ok(value);
            }
        }
        Err(invalid(format!("overflowing {label}")))
    }

    fn count(&mut self, label: &str) -> Result<usize, CorpusError> {
        let count = usize_from_u64(self.varint(label)?)?;
        if count > MAX_ITEMS {
            return Err(invalid(format!("{label} exceeds limit")));
        }
        Ok(count)
    }

    fn u32(&mut self, label: &str) -> Result<u32, CorpusError> {
        u32::try_from(self.varint(label)?).map_err(|_| invalid(format!("{label} exceeds u32")))
    }

    fn bytes(&mut self, label: &str) -> Result<&'a [u8], CorpusError> {
        let len = usize_from_u64(self.varint(&format!("{label} length"))?)?;
        let end = self
            .offset
            .checked_add(len)
            .ok_or_else(|| invalid(format!("{label} length overflow")))?;
        let value = self
            .bytes
            .get(self.offset..end)
            .ok_or_else(|| invalid(format!("truncated {label}")))?;
        self.offset = end;
        Ok(value)
    }

    fn finish(self) -> Result<(), CorpusError> {
        if self.offset != self.bytes.len() {
            return Err(invalid("trailing raw payload bytes"));
        }
        Ok(())
    }
}

fn invalid(message: impl Into<String>) -> CorpusError {
    CorpusError::InvalidFormat {
        component: COMPONENT,
        message: message.into(),
    }
}
