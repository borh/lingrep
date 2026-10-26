//! Self-contained shareable-permalink codec.

use std::io::{Read, Write};

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use flate2::{Compression, read::DeflateDecoder, write::DeflateEncoder};

/// The lingrep permalink format, with a mandatory corpus selector.
pub const WIRE_VERSION: &str = "1";

/// Maximum decompressed lingrep patterns source size.
pub const MAX_PATTERN_BYTES: usize = 1024 * 1024;

/// Supported maximum for the complete `/?v=1&q=…&k=…[&c=…]` request target.
///
/// Sized so a full research catalog can ride the URL: the seeded
/// `sf-experimental.lx` encodes to ~12KB, and corpus switching carries the
/// whole editor buffer, so the 8KB value in force before 2026-08-19 refused
/// every switch made with such a catalog open. 32KB keeps real catalogs with
/// headroom while staying far below browser request-line limits (~2MB) and
/// the front proxy's (Caddy) header budget.
pub const MAX_REQUEST_TARGET_LEN: usize = 32_000;

#[derive(Debug, PartialEq, Eq)]
pub enum PermalinkError {
    TooLarge,
    Malformed,
}

/// Encode a shareable target.
///
/// `corpus` is the selector (`k`) and is always emitted. `digest` is an
/// *assertion* about which content produced the results and is emitted only when
/// the caller actually knows one — a lazily built corpus has no identity yet, so
/// the selector must be expressible without it. An absent `c` means "no
/// assertion", not "no corpus".
///
/// `retained` is the per-pattern retention the sender was reading (`r`),
/// emitted when the sender's page had one. The recipient maps it onto its
/// own offers; see [`crate::state::link_retention`].
///
/// `corpus` must already be a validated [`crate::corpus_set::CorpusName`]
/// (`[A-Za-z0-9_-]`), which is what makes the concatenation below safe without
/// URL escaping. Both it and the digest are charged against
/// [`MAX_REQUEST_TARGET_LEN`], so no name length can smuggle a target past the
/// transport limit.
pub fn encode(
    pattern: &str,
    corpus: &str,
    digest: Option<&str>,
    retained: Option<usize>,
) -> Result<String, PermalinkError> {
    if pattern.len() > MAX_PATTERN_BYTES {
        return Err(PermalinkError::TooLarge);
    }
    let mut encoder = DeflateEncoder::new(Vec::new(), Compression::best());
    encoder
        .write_all(pattern.as_bytes())
        .map_err(|_| PermalinkError::Malformed)?;
    let compressed = encoder.finish().map_err(|_| PermalinkError::Malformed)?;
    let query = URL_SAFE_NO_PAD.encode(compressed);
    let mut target = format!("/?v={WIRE_VERSION}&q={query}&k={corpus}");
    if let Some(digest) = digest {
        target.push_str("&c=");
        target.push_str(digest);
    }
    if let Some(retained) = retained {
        target.push_str("&r=");
        target.push_str(&retained.to_string());
    }
    if target.len() > MAX_REQUEST_TARGET_LEN {
        return Err(PermalinkError::TooLarge);
    }
    Ok(target)
}

/// Encode a shareable target as the query-only reference the HTTP surface answers
/// with.
///
/// A browser resolves a query-only reference against the current document, so
/// this form cannot leave the mount a reverse proxy put the page under — which
/// is why `POST /api/permalink` answers with it and `app.js` accepts no other
/// shape. [`encode`] keeps returning the root-absolute `/?…` form for callers
/// that need a request target, and the two differ by exactly that leading
/// slash; this function exists so neither the endpoint nor a test has to
/// re-derive the relation by hand.
pub fn encode_query_only(
    pattern: &str,
    corpus: &str,
    digest: Option<&str>,
    retained: Option<usize>,
) -> Result<String, PermalinkError> {
    // Deriving from `encode` charges the query against the same
    // `MAX_REQUEST_TARGET_LEN` budget as the target it resolves back into.
    let target = encode(pattern, corpus, digest, retained)?;
    Ok(target.trim_start_matches('/').to_owned())
}

pub fn decode(query: &str) -> Result<String, PermalinkError> {
    if query.len() > MAX_REQUEST_TARGET_LEN {
        return Err(PermalinkError::Malformed);
    }
    let compressed = URL_SAFE_NO_PAD
        .decode(query)
        .map_err(|_| PermalinkError::Malformed)?;
    let mut output = Vec::new();
    DeflateDecoder::new(compressed.as_slice())
        .take((MAX_PATTERN_BYTES + 1) as u64)
        .read_to_end(&mut output)
        .map_err(|_| PermalinkError::Malformed)?;
    if output.len() > MAX_PATTERN_BYTES {
        return Err(PermalinkError::Malformed);
    }
    String::from_utf8(output).map_err(|_| PermalinkError::Malformed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn q_of(target: &str) -> &str {
        target
            .strip_prefix("/?")
            .unwrap()
            .split('&')
            .find_map(|pair| pair.strip_prefix("q="))
            .unwrap()
    }

    #[test]
    fn roundtrip_ascii() {
        let pattern = "verb := [upos=VERB].";
        let target = encode(pattern, "default", Some("abcdef012345"), None).unwrap();
        assert!(target.starts_with("/?v=1&"));
        assert_eq!(decode(q_of(&target)).unwrap(), pattern);
    }

    #[test]
    fn roundtrip_japanese_multibyte() {
        let pattern = "result := [text=そう] followed_by([lemma=だ] $).";
        let target = encode(pattern, "default", Some("abcdef012345"), None).unwrap();
        assert_eq!(decode(q_of(&target)).unwrap(), pattern);
    }

    #[test]
    fn query_only_is_the_encoded_target_without_its_root_slash() {
        let pattern = "verb := [upos=VERB].";
        let target = encode(pattern, "sci", Some("abcdef012345"), None).unwrap();
        let query_only = encode_query_only(pattern, "sci", Some("abcdef012345"), None).unwrap();

        assert!(query_only.starts_with("?v=1&"));
        assert_eq!(target, format!("/{query_only}"));
        assert_eq!(decode(q_of(&target)).unwrap(), pattern);
    }

    #[test]
    fn query_only_is_charged_against_the_same_target_limit() {
        assert_eq!(
            encode_query_only(&noisy(40_000), "default", Some("abcdef012345"), None),
            Err(PermalinkError::TooLarge)
        );
    }

    #[test]
    fn encode_refuses_when_target_too_large() {
        assert_eq!(
            encode(&noisy(40_000), "default", Some("abcdef012345"), None),
            Err(PermalinkError::TooLarge)
        );
    }

    #[test]
    fn encode_rejects_pattern_over_decompressed_cap_even_when_compressible() {
        let compressible = "a".repeat(MAX_PATTERN_BYTES + 1);
        assert_eq!(
            encode(&compressible, "default", Some("abcdef012345"), None),
            Err(PermalinkError::TooLarge)
        );
    }

    #[test]
    fn decode_rejects_malformed_base64() {
        assert_eq!(decode("not*valid*base64"), Err(PermalinkError::Malformed));
    }

    #[test]
    fn decode_rejects_malformed_deflate() {
        let blob = URL_SAFE_NO_PAD.encode([0xff, 0x00, 0x13, 0x37]);
        assert_eq!(decode(&blob), Err(PermalinkError::Malformed));
    }

    #[test]
    fn decode_rejects_non_utf8() {
        let mut encoder = DeflateEncoder::new(Vec::new(), Compression::best());
        encoder.write_all(&[0xff, 0xfe, 0xfd]).unwrap();
        let blob = URL_SAFE_NO_PAD.encode(encoder.finish().unwrap());
        assert_eq!(decode(&blob), Err(PermalinkError::Malformed));
    }

    #[test]
    fn decode_rejects_decompression_bomb() {
        let mut encoder = DeflateEncoder::new(Vec::new(), Compression::best());
        encoder
            .write_all(&vec![0_u8; MAX_PATTERN_BYTES + 1024])
            .unwrap();
        let blob = URL_SAFE_NO_PAD.encode(encoder.finish().unwrap());
        assert!(
            blob.len() < 4_000,
            "bomb blob should be small: {}",
            blob.len()
        );
        assert_eq!(decode(&blob), Err(PermalinkError::Malformed));
    }

    #[test]
    fn decode_rejects_over_length_query() {
        let big = "A".repeat(MAX_REQUEST_TARGET_LEN + 1);
        assert_eq!(decode(&big), Err(PermalinkError::Malformed));
    }

    #[test]
    fn v1_carries_a_selector_without_a_digest() {
        let target = encode("p := [].", "sci", None, None).unwrap();
        assert!(target.starts_with("/?v=1&"));
        assert!(target.contains("&k=sci"));
        assert!(!target.contains("&c="));
        assert_eq!(decode(q_of(&target)).unwrap(), "p := [].");
    }

    #[test]
    fn v1_carries_the_retention_after_the_digest() {
        let target = encode("p := [].", "sci", Some("abcdef012345"), Some(50)).unwrap();
        assert!(target.ends_with("&k=sci&c=abcdef012345&r=50"), "{target}");
        let target = encode("p := [].", "sci", None, Some(50)).unwrap();
        assert!(target.ends_with("&k=sci&r=50"), "{target}");
        assert_eq!(decode(q_of(&target)).unwrap(), "p := [].");
    }

    #[test]
    fn v1_carries_both_when_a_digest_is_known() {
        let target = encode("p := [].", "sci", Some("abcdef012345"), None).unwrap();
        assert!(target.contains("&k=sci"));
        assert!(target.contains("&c=abcdef012345"));
    }

    /// Incompressible ASCII, so a prefix's encoded length grows with the prefix
    /// and every byte index is a `char` boundary.
    fn noisy(len: usize) -> String {
        let mut state = 0x9e37_79b9_u32;
        (0..len)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 17;
                state ^= state << 5;
                char::from(32 + (state % 95) as u8)
            })
            .collect()
    }

    /// The name is charged against the budget, so a long name cannot smuggle a
    /// target past the transport limit.
    ///
    /// The pattern is sized **by search** to the largest prefix a one-character
    /// name still admits, so the extra bytes of a maximal name have to push it
    /// over. A fixed oversized pattern would make both sides `Err` and the
    /// assertion vacuous — it would hold even if `encode` appended the name
    /// after the length check, which is precisely the defect this guards.
    /// Searching also keeps the test honest across a change to
    /// [`MAX_REQUEST_TARGET_LEN`] or to the compression settings.
    #[test]
    fn the_corpus_name_counts_against_the_target_budget() {
        // The longest key `CorpusName::parse` admits.
        let long_name = "n".repeat(64);
        // Sized against the limit (the original 12,000 was 1.5x the 8KB value):
        // at 1.5x the cap, even Huffman-level shrinkage of the printable noise
        // leaves the encoded target oversized, so the upper bound holds. Taken
        // as `char`s rather than sliced as a `str`, so no prefix can ever land
        // inside a UTF-8 sequence.
        let noisy: Vec<char> = noisy(MAX_REQUEST_TARGET_LEN * 3 / 2).chars().collect();
        let prefix = |n: usize| noisy[..n].iter().collect::<String>();
        assert!(
            encode(&prefix(noisy.len()), "s", None, None).is_err(),
            "the search needs an upper bound that already fails"
        );

        // Largest prefix the one-character name still admits.
        let (mut fits, mut over) = (0_usize, noisy.len());
        while over - fits > 1 {
            let mid = fits + (over - fits) / 2;
            if encode(&prefix(mid), "s", None, None).is_ok() {
                fits = mid;
            } else {
                over = mid;
            }
        }
        let boundary = prefix(fits);

        // The precondition. A future change that makes this `Err` fails loudly
        // instead of passing vacuously.
        let short =
            encode(&boundary, "s", None, None).expect("the one-character name must still fit");
        assert!(
            short.len() + (long_name.len() - 1) > MAX_REQUEST_TARGET_LEN,
            "the search must land near the limit for the name to be able to push \
             it over; short target is {} bytes against a {MAX_REQUEST_TARGET_LEN}-byte limit",
            short.len()
        );
        assert_eq!(
            encode(&boundary, &long_name, None, None),
            Err(PermalinkError::TooLarge)
        );
    }
}
