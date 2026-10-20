use std::fs::{self, File};
use std::io::{self, Cursor};
use std::path::Path;
use vibrato_rkyv::{Dictionary, Tokenizer};
use zstd::stream::read::Decoder;

use crate::error::PipelineError;

/// Load raw dictionary bytes from a path.
///
/// Trust-boundary contract: every on-disk failure (missing file, unreadable,
/// corrupt zstd stream, no usable file in a directory, unknown extension) is
/// surfaced as `Err(PipelineError::Dictionary(..))` rather than a panic. Callers on
/// the trust boundary must propagate this with `?`.
pub fn load_dictionary_data(dict_path: &str) -> Result<Vec<u8>, PipelineError> {
    let mut reader = open_dictionary_stream(dict_path)?;
    let mut bytes = Vec::new();
    io::copy(&mut reader, &mut bytes).map_err(|error| {
        PipelineError::Dictionary(format!("Failed to read dictionary {dict_path:?}: {error}"))
    })?;
    Ok(bytes)
}

pub fn create_tokenizer(dict_data: &[u8]) -> Result<Tokenizer, PipelineError> {
    let dict = Dictionary::read(Cursor::new(dict_data)).map_err(|error| {
        PipelineError::Dictionary(format!("Failed to parse dictionary: {error}"))
    })?;
    Tokenizer::new(dict)
        .ignore_space(true)
        .map_err(|error| PipelineError::Dictionary(format!("Failed to create tokenizer: {error}")))
}

/// Construct a tokenizer directly from a dictionary path.
///
/// Dispatch is on the `.zst` suffix alone. No other extension is inspected and
/// no directory is searched for a dictionary inside it: anything not ending
/// `.zst` is memory-mapped as an uncompressed dictionary file, whatever it is
/// named, and is rejected on its contents rather than on its name.
///
/// Trust-boundary contract: every failure is surfaced as
/// `Err(PipelineError::Dictionary(..))` rather than a panic, and callers on the trust
/// boundary must propagate this with `?` — but unlike [`load_dictionary_data`],
/// both branches here also trade validation for speed against a cache keyed on
/// *file metadata*, never on content. The key is `dev`, `ino`, `size`, `mtime`,
/// and `mtime_nsec` (`vibrato_rkyv::dictionary::compute_metadata_hash`). The
/// two branches trade differently.
///
/// The `.zst` branch (`Dictionary::from_zstd` with `CacheStrategy::GlobalCache`)
/// keys a *decompressed copy* in the user cache directory
/// (`~/.cache/vibrato-rkyv/<metadata-hash>.dic` on Linux) on the metadata of the
/// compressed file. The first load decompresses, fsyncs, fully validates, and
/// persists roughly 1.31 GB there; later loads reuse it. A `.zst` replaced in
/// place with identical metadata therefore serves the *stale* decompressed copy.
///
/// The non-`.zst` branch (`Dictionary::from_path` with `LoadMode::TrustCache`)
/// maps exactly the path it was given, so nothing stale is served. Its exposure
/// is validation instead: a zero-length proof file named from that metadata
/// (`<metadata-hash>.sha256`, looked up first in a `.cache` sibling of the
/// dictionary and then in the user cache directory) makes the load take
/// `access_unchecked` rather than the checked `access` — skipping rkyv
/// validation of memory-mapped bytes. Only the magic number is still read from
/// the file itself. Upstream documents this as a time-of-check-to-time-of-use
/// exposure. Absent a proof file the load validates fully and then writes one.
///
/// Neither exposure occurs in this project's supported workflow, where the
/// dictionary is a read-only `/nix/store` path: it cannot be replaced in place
/// by the user running the pipeline, and concurrently live store paths hold
/// distinct inodes. The argument is narrower than it looks. Nix canonicalises
/// store mtimes to 1, so for such a path `mtime` and `mtime_nsec` contribute no
/// entropy and the key degenerates to `(dev, ino, size)`; a collected store path
/// whose inode is later reused at an identical size would collide. The
/// load-bearing property is that the store is read-only, not content addressing
/// — an ordinary input-addressed derivation output is not content-addressed.
pub fn create_tokenizer_from_path(dict_path: &str) -> Result<Tokenizer, PipelineError> {
    let dict = if dict_path.ends_with(".zst") {
        Dictionary::from_zstd(
            dict_path,
            vibrato_rkyv::dictionary::CacheStrategy::GlobalCache,
        )
        .map_err(|error| {
            PipelineError::Dictionary(format!(
                "Failed to read compressed dictionary {dict_path:?}: {error}"
            ))
        })?
    } else {
        Dictionary::from_path(dict_path, vibrato_rkyv::dictionary::LoadMode::TrustCache).map_err(
            |error| {
                PipelineError::Dictionary(format!(
                    "Failed to read dictionary {dict_path:?}: {error}"
                ))
            },
        )?
    };
    Tokenizer::new(dict)
        .ignore_space(true)
        .map_err(|error| PipelineError::Dictionary(format!("Failed to create tokenizer: {error}")))
}

/// A dictionary's identity digest: blake3 over its *decompressed* bytes.
///
/// This is the `resource_manifest_digest` a Vibrato [`AnalyzerDescriptor`]
/// carries, so it reaches every prepared artifact's analysis identity. Its
/// definition is therefore fixed: blake3 of exactly the bytes
/// [`load_dictionary_data`] returns, whatever it costs to obtain them.
///
/// [`AnalyzerDescriptor`]: crate::analysis::AnalyzerDescriptor
///
/// What is *not* fixed is how often that cost is paid. For a **`.zst`**
/// dictionary the answer is memoized in the user cache directory
/// (`~/.cache/lingrep/dictionary-identity/<metadata-key>`) under the same
/// `(dev, ino, size, mtime, mtime_nsec)` key
/// [`create_tokenizer_from_path`] already relies on for the decompressed
/// dictionary itself. A hit costs one 64-byte read; a miss streams the file
/// through the hasher without materializing 1.31 GB and then records the
/// answer.
///
/// The `.zst` restriction is the whole soundness argument, not an
/// optimization detail. Memoizing an identity input is safe only where a stale
/// memo cannot *disagree* with the dictionary the run loads, and that holds
/// exactly once: a `.zst` replaced in place with identical metadata serves a
/// stale memoized digest **and** a stale decompressed dictionary, because both
/// caches key on the same metadata and live in the same directory, so the
/// digest still names the bytes the run actually tokenized with. The staleness
/// exposure is then the one [`create_tokenizer_from_path`] already documents,
/// and no wider.
///
/// An **uncompressed** dictionary is memory-mapped directly, so it is always
/// fresh. A memo there would pair fresh tokenization with a stale identity, and
/// every artifact built afterwards would be addressed under a dictionary the
/// run did not use — the identity collision the `a1` scheme exists to prevent,
/// reintroduced through a cache. That branch is therefore not memoized and
/// pays the hash, which is memory-speed on a mapped file.
///
/// A memo that cannot be read or written — no `HOME`, a read-only cache
/// directory, a truncated file — is not an error; the digest is recomputed and
/// the run proceeds.
///
/// # Errors
///
/// Returns `Err(PipelineError::Dictionary(..))` when the dictionary itself cannot be
/// read or decompressed, exactly as [`load_dictionary_data`] does.
pub fn dictionary_identity(dict_path: &str) -> Result<[u8; 32], PipelineError> {
    let memo = identity_memo_path(Path::new(dict_path));
    if let Some(memo) = memo.as_deref()
        && let Some(digest) = read_identity_memo(memo)
    {
        return Ok(digest);
    }
    // An uncompressed dictionary is answered by mapping the file itself. A
    // `.zst` is not, so it is streamed through the hasher — one decompression,
    // on a memo miss only.
    let digest = digest_from_dictionary_file(dict_path)
        .map_or_else(|| hash_dictionary_stream(dict_path), Ok)?;
    if let Some(memo) = memo.as_deref() {
        write_identity_memo(memo, &digest);
    }
    Ok(digest)
}

/// Builds a tokenizer and its identity digest in one pass.
///
/// The pairing avoids retaining a 1.31 GB decompressed buffer solely to hash
/// it after constructing the tokenizer. An uncompressed dictionary is mapped
/// once for the tokenizer and hashed out of the same page cache; a `.zst` is
/// decompressed for the tokenizer and streamed once more for the digest. See
/// [`create_tokenizer_from_path`] and [`dictionary_identity`] for what each
/// half trades.
///
/// A *memoized* digest is one 64-byte read and short-circuits all of it.
///
/// # Errors
///
/// Returns `Err(PipelineError::Dictionary(..))` when the dictionary cannot be mapped
/// or read.
pub fn create_tokenizer_with_identity(
    dict_path: &str,
) -> Result<(Tokenizer, [u8; 32]), PipelineError> {
    let memo = identity_memo_path(Path::new(dict_path));
    if let Some(memo) = memo.as_deref()
        && let Some(identity) = read_identity_memo(memo)
    {
        return Ok((create_tokenizer_from_path(dict_path)?, identity));
    }

    let tokenizer = create_tokenizer_from_path(dict_path)?;
    let digest = digest_from_dictionary_file(dict_path)
        .map_or_else(|| hash_dictionary_stream(dict_path), Ok)?;
    if let Some(memo) = memo.as_deref() {
        write_identity_memo(memo, &digest);
    }
    Ok((tokenizer, digest))
}

/// The digest of the dictionary file itself, when that file already holds the
/// dictionary's decompressed bytes.
///
/// An **uncompressed** dictionary is that file: `load_dictionary_data` reads
/// the `.dic` and returns its bytes unchanged, so mapping the file and mapping
/// what a stream would have produced are the same bytes and therefore the same
/// digest.
///
/// A `.zst` archive is not, so it answers `None` and the caller streams it
/// through the hasher. `None` means "no answer from here", never "no digest":
/// a file that is unreadable or cannot be mapped is a miss rather than an
/// error, as is a path that is not a `.dic`. Every `None` sends the caller to
/// [`hash_dictionary_stream`], which always works and merely costs more.
///
/// A compressed input used to be answered from the decompressed copy the loader
/// leaves in the vibrato cache, reached through a fork-only
/// `Dictionary::cached_decompressed_path` accessor. Every packaged dictionary
/// ships uncompressed, so that route served only a user-supplied `.zst`, and
/// holding it meant pinning `vibrato-rkyv` to a private fork that no public
/// checkout can fetch. The cost of dropping it is one extra decompression per
/// `.zst`, which the identity memo already pays at most once per file.
fn digest_from_dictionary_file(dict_path: &str) -> Option<[u8; 32]> {
    // Restricted to the extension `load_dictionary_data` reads as raw bytes, so
    // this maps exactly what the stream would have hashed and never guesses at
    // a path the loader would refuse.
    if dict_path.ends_with(".dic") {
        return hash_mapped_file(Path::new(dict_path));
    }
    None
}

/// BLAKE3 over a mapped file, in parallel, or `None` if it cannot be read.
///
/// BLAKE3's tree structure lets many threads hash one file while producing the
/// same digest as a serial pass. See
/// `docs/results/2026-08-24-parallel-dictionary-decompression/`.
fn hash_mapped_file(path: &Path) -> Option<[u8; 32]> {
    let mut hasher = blake3::Hasher::new();
    hasher.update_mmap_rayon(path).ok()?;
    Some(*hasher.finalize().as_bytes())
}

/// Hashes the decompressed dictionary without materializing it.
fn hash_dictionary_stream(dict_path: &str) -> Result<[u8; 32], PipelineError> {
    struct HashingSink(blake3::Hasher);
    impl io::Write for HashingSink {
        fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
            self.0.update(buffer);
            Ok(buffer.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    let mut reader = open_dictionary_stream(dict_path)?;
    let mut sink = HashingSink(blake3::Hasher::new());
    io::copy(&mut reader, &mut sink).map_err(|error| {
        PipelineError::Dictionary(format!("Failed to read dictionary {dict_path:?}: {error}"))
    })?;
    Ok(*sink.0.finalize().as_bytes())
}

/// Opens the decompressed byte stream [`load_dictionary_data`] would collect.
///
/// Resolves the same four cases — `.zst` file, `.dic` file, a directory
/// holding either — so the two functions cannot disagree about which bytes a
/// path names.
fn open_dictionary_stream(dict_path: &str) -> Result<Box<dyn io::Read>, PipelineError> {
    let path = Path::new(dict_path);
    let resolved = if dict_path.ends_with(".zst") || dict_path.ends_with(".dic") {
        path.to_path_buf()
    } else if path.is_dir() {
        let compressed = path.join("system.dic.zst");
        let plain = path.join("system.dic");
        if compressed.exists() {
            compressed
        } else if plain.exists() {
            plain
        } else {
            return Err(PipelineError::Dictionary(format!(
                "No valid dictionary file found in directory {dict_path:?}"
            )));
        }
    } else {
        return Err(PipelineError::Dictionary(format!(
            "Invalid dictionary path: {dict_path:?}"
        )));
    };
    let file = File::open(&resolved).map_err(|error| {
        PipelineError::Dictionary(format!(
            "Failed to open dictionary file {resolved:?}: {error}"
        ))
    })?;
    if resolved.as_os_str().as_encoded_bytes().ends_with(b".zst") {
        let decoder = Decoder::new(file).map_err(|error| {
            PipelineError::Dictionary(format!(
                "Failed to create zstd decoder for {resolved:?}: {error}"
            ))
        })?;
        Ok(Box::new(decoder))
    } else {
        Ok(Box::new(io::BufReader::with_capacity(1 << 20, file)))
    }
}

/// Where this dictionary file's identity memo lives, when one can exist.
///
/// `None` when the file has no readable metadata or no user cache directory
/// can be named; both mean "recompute", never "fail".
fn identity_memo_path(dict_path: &Path) -> Option<std::path::PathBuf> {
    // Both dictionary encodings, and nothing else. A directory is not the path
    // either loader caches on, so it keys no memo.
    //
    // The two encodings reach the same guarantee by different routes. For a
    // `.zst`, `create_tokenizer_from_path` caches its decompressed copy on this
    // same metadata key, so a stale memo and a stale cache go stale together
    // and the run remains self-consistent.
    //
    // For a `.dic` the file is mapped fresh, so consistency rests on the key
    // itself: a stale memo requires the contents to change while `dev`, `ino`,
    // `size`, `mtime` and `mtime_nsec` all stay identical. That is the same
    // tuple `vibrato-rkyv` already trusts to decide a `.dic` needs no rkyv
    // validation at all — its `TrustCache` marker is named by a hash of it — so
    // memoizing the identity on it extends no trust the loader has not already
    // extended, and a dictionary that could defeat this would defeat that first
    // and be loaded unvalidated.
    //
    // The residual hazard is a `.dic` rewritten in place at an identical size
    // with its timestamps restored, which would pair fresh tokenization with a
    // stale identity. Packaged dictionaries do not do this: a `/nix/store` path
    // is read-only, and a rebuilt dictionary is a different path with a
    // different inode.
    let memoizable = dict_path
        .extension()
        .is_some_and(|suffix| suffix == "zst" || suffix == "dic");
    if !memoizable {
        return None;
    }
    let key = metadata_key(dict_path)?;
    let base = std::env::var_os("XDG_CACHE_HOME")
        .filter(|value| !value.is_empty())
        .map(std::path::PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME")
                .filter(|value| !value.is_empty())
                .map(|home| std::path::PathBuf::from(home).join(".cache"))
        })?;
    Some(base.join("lingrep").join("dictionary-identity").join(key))
}

/// The identity a dictionary file's metadata gives it, as a hex key.
fn metadata_key(dict_path: &Path) -> Option<String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let metadata = fs::metadata(dict_path).ok()?;
        let mut hasher = blake3::Hasher::new();
        hasher.update(b"lingrep.dictionary-identity/v1");
        hasher.update(&metadata.dev().to_le_bytes());
        hasher.update(&metadata.ino().to_le_bytes());
        hasher.update(&metadata.size().to_le_bytes());
        hasher.update(&metadata.mtime().to_le_bytes());
        hasher.update(&metadata.mtime_nsec().to_le_bytes());
        Some(hasher.finalize().to_hex().to_string())
    }
    #[cfg(not(unix))]
    {
        let _ = dict_path;
        None
    }
}

/// Reads a memoized digest, treating every defect as a miss.
fn read_identity_memo(memo: &Path) -> Option<[u8; 32]> {
    let recorded = fs::read(memo).ok()?;
    if recorded.len() != 32 {
        return None;
    }
    let mut digest = [0_u8; 32];
    digest.copy_from_slice(&recorded);
    Some(digest)
}

/// Records a digest, discarding every failure.
///
/// Writes a uniquely named temporary beside the memo and renames it into
/// place, so a concurrent reader observes either no memo or a complete one.
fn write_identity_memo(memo: &Path, digest: &[u8; 32]) {
    let Some(directory) = memo.parent() else {
        return;
    };
    if fs::create_dir_all(directory).is_err() {
        return;
    }
    let temporary = directory.join(format!(
        "{}.{}.tmp",
        memo.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or(""),
        std::process::id()
    ));
    if fs::write(&temporary, digest).is_err() {
        let _ = fs::remove_file(&temporary);
        return;
    }
    if fs::rename(&temporary, memo).is_err() {
        let _ = fs::remove_file(&temporary);
    }
}

#[cfg(test)]
mod identity_tests {
    use super::*;

    #[test]
    fn buffered_and_streamed_reads_preserve_bytes_and_directory_precedence() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let payload: Vec<u8> = (0..(3 * 1024 * 1024_u32))
            .map(|index| (index % 251) as u8)
            .collect();
        let plain = directory.path().join("system.dic");
        fs::write(&plain, &payload).expect("write plain dictionary");
        let directory_path = directory.path().to_str().expect("utf-8 path");
        for path in [plain.to_str().unwrap(), directory_path] {
            assert_eq!(load_dictionary_data(path).unwrap(), payload);
            assert_eq!(
                hash_dictionary_stream(path).unwrap(),
                *blake3::hash(&payload).as_bytes()
            );
        }

        let compressed_payload = b"compressed wins over the plain dictionary";
        let compressed = directory.path().join("system.dic.zst");
        fs::write(
            &compressed,
            zstd::encode_all(&compressed_payload[..], 3).expect("compress"),
        )
        .expect("write compressed dictionary");
        for path in [compressed.to_str().unwrap(), directory_path] {
            assert_eq!(load_dictionary_data(path).unwrap(), compressed_payload);
            assert_eq!(
                hash_dictionary_stream(path).unwrap(),
                *blake3::hash(compressed_payload).as_bytes()
            );
        }

        fs::write(&compressed, b"corrupt zstd").unwrap();
        for path in [compressed.to_str().unwrap(), directory_path] {
            assert!(matches!(
                load_dictionary_data(path),
                Err(PipelineError::Dictionary(_))
            ));
            assert!(matches!(
                hash_dictionary_stream(path),
                Err(PipelineError::Dictionary(_))
            ));
        }
        fs::remove_file(&compressed).unwrap();
        fs::remove_file(&plain).unwrap();
        let unsupported = directory.path().join("system.bin");
        fs::write(&unsupported, b"unsupported").unwrap();
        for path in [
            plain.to_str().unwrap(),
            unsupported.to_str().unwrap(),
            directory_path,
        ] {
            assert!(matches!(
                load_dictionary_data(path),
                Err(PipelineError::Dictionary(_))
            ));
            assert!(matches!(
                hash_dictionary_stream(path),
                Err(PipelineError::Dictionary(_))
            ));
        }
    }

    #[test]
    fn a_dot_zst_filename_uses_the_compressed_encoding() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join(".zst");
        let payload = b"compressed dictionary bytes";
        fs::write(&path, zstd::encode_all(&payload[..], 3).unwrap()).unwrap();
        let path = path.to_str().unwrap();
        assert_eq!(load_dictionary_data(path).unwrap(), payload);
        assert_eq!(
            hash_dictionary_stream(path).unwrap(),
            *blake3::hash(payload).as_bytes()
        );
    }

    /// The memo is offered only where a stale answer stays *consistent*.
    ///
    /// The soundness argument for memoizing an identity input is that a `.zst`
    /// replaced in place with identical metadata serves a stale memoized digest
    /// **and** a stale decompressed dictionary — same key, same cache
    /// directory, so they go stale together and the digest still names the
    /// bytes the run tokenized with.
    ///
    /// That argument does not survive the other branch.
    /// [`create_tokenizer_from_path`] maps an uncompressed dictionary directly,
    /// so it serves *fresh* bytes; a memo there would pair fresh tokenization
    /// with a stale identity, and every artifact built from it would be
    /// addressed under a dictionary the run did not use. That is the collision
    /// the `a1` identity scheme exists to prevent, reintroduced through a
    /// cache. So the uncompressed branch gets no memo and pays the hash.
    #[test]
    fn both_dictionary_encodings_are_memoized_and_nothing_else_is() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let compressed = directory.path().join("system.dic.zst");
        let plain = directory.path().join("system.dic");
        let stray = directory.path().join("system.bin");
        fs::write(
            &compressed,
            zstd::encode_all(&b"bytes"[..], 3).expect("compress"),
        )
        .expect("write compressed");
        fs::write(&plain, b"bytes").expect("write plain");
        fs::write(&stray, b"bytes").expect("write stray");

        assert!(
            identity_memo_path(&compressed).is_some(),
            "a compressed dictionary is memoized: its decompressed copy is cached on the same key"
        );
        assert!(
            identity_memo_path(&plain).is_some(),
            "an uncompressed dictionary is memoized on the same metadata key the loader \
             already trusts to skip validation"
        );
        assert_ne!(
            identity_memo_path(&compressed),
            identity_memo_path(&plain),
            "two different files must key two different memos"
        );
        assert_eq!(
            identity_memo_path(&stray),
            None,
            "an extension neither loader reads keys no memo"
        );
        assert_eq!(
            identity_memo_path(directory.path()),
            None,
            "a directory is not the path either loader caches on"
        );
    }

    /// A miss computes and records; a hit returns the recorded bytes without
    /// reading the dictionary at all — which is what makes the memo worth
    /// having, and also what makes a corrupted memo a miss rather than a
    /// wrong answer.
    #[test]
    fn the_memo_round_trips_and_treats_every_defect_as_a_miss() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let memo = directory.path().join("nested").join("key");
        let digest = *blake3::hash(b"dictionary bytes").as_bytes();

        assert_eq!(read_identity_memo(&memo), None, "absent memo is a miss");
        write_identity_memo(&memo, &digest);
        assert_eq!(read_identity_memo(&memo), Some(digest));

        fs::write(&memo, b"short").expect("truncate memo");
        assert_eq!(
            read_identity_memo(&memo),
            None,
            "a memo of the wrong length is a miss, not a digest"
        );
        assert!(
            fs::read_dir(memo.parent().expect("parent"))
                .expect("read directory")
                .filter_map(Result::ok)
                .all(|entry| !entry.file_name().to_string_lossy().ends_with(".tmp")),
            "no temporary survives a completed write"
        );
    }

    /// A `.zst` identity must equal a serial stream of the archive itself.
    ///
    /// Compressed input is no longer answered from the loader's decompressed
    /// cache copy — that accessor belonged to a fork of `vibrato-rkyv` a public
    /// build cannot fetch — so [`create_tokenizer_with_identity`] loads the
    /// tokenizer and then streams the archive through the hasher. The digest
    /// reaches every prepared artifact's analysis identity, so this pins it
    /// against its definition rather than against the route taken to it.
    ///
    /// The reference is [`hash_dictionary_stream`] rather than
    /// [`dictionary_identity`], because the latter would record a memo and
    /// short-circuit the branch under test.
    #[test]
    fn compressed_identity_equals_the_streamed_digest() {
        let mini = crate::testing::MiniDictionary::new();
        let compressed = mini
            .compressed_path()
            .to_str()
            .expect("miniature dictionary path is UTF-8");

        let reference = hash_dictionary_stream(compressed).expect("serial digest");

        // A dictionary in a fresh temporary directory has no memo, so this
        // call takes the load-then-stream branch.
        let (_tokenizer, cold) = create_tokenizer_with_identity(compressed).expect("cold load");
        assert_eq!(
            cold, reference,
            "a compressed dictionary must hash to its own streamed digest"
        );

        // That call recorded the memo, so this one takes the inline branch.
        let (_tokenizer, memoized) = create_tokenizer_with_identity(compressed).expect("warm load");
        assert_eq!(
            memoized, reference,
            "the memoized load must return the same digest as the cold one"
        );
    }

    /// Only an uncompressed dictionary is answered from a mapped file.
    ///
    /// This branch decides whether an identity costs a decompression. A `.dic`
    /// maps the file it already has; a `.zst` must decline so the caller
    /// streams it, because the loader's decompressed copy is no longer
    /// reachable from a public `vibrato-rkyv`.
    #[test]
    fn only_an_uncompressed_dictionary_is_answered_from_a_mapped_file() {
        let mini = crate::testing::MiniDictionary::new();
        let compressed = mini
            .compressed_path()
            .to_str()
            .expect("miniature dictionary path is UTF-8");
        let plain = mini
            .path()
            .to_str()
            .expect("miniature dictionary path is UTF-8");

        // Populate the loader's cache, which is what the accessor reads.
        let _ = create_tokenizer_from_path(compressed).expect("load the compressed dictionary");

        assert_eq!(
            digest_from_dictionary_file(compressed),
            None,
            "a `.zst` does not hold decompressed bytes and must decline"
        );
        assert_eq!(
            digest_from_dictionary_file(plain),
            Some(hash_dictionary_stream(plain).expect("serial digest")),
            "an uncompressed dictionary is its own decompressed copy and maps directly"
        );

        // Both encodings carry the same bytes: this is what makes the encoding
        // a packaging choice rather than an identity-bearing one. The two
        // encodings are now reached by different routes, so the equality is the
        // whole point rather than a by-product.
        assert_eq!(
            dictionary_identity(compressed).expect("compressed identity"),
            dictionary_identity(plain).expect("uncompressed identity"),
            "the encoding must not change the identity"
        );

        // A path the loader would refuse must not be answered with a digest of
        // whatever bytes happen to be there.
        let directory = tempfile::tempdir().expect("temporary directory");
        let stray = directory.path().join("dictionary.bin");
        fs::write(&stray, b"not a dictionary").expect("write");
        assert_eq!(
            digest_from_dictionary_file(stray.to_str().expect("utf-8 path")),
            None,
            "an extension `load_dictionary_data` does not read must decline"
        );
    }

    /// Distinct dictionary files key distinct memos, and an unreadable path
    /// keys none — the "recompute, never fail" half of the contract.
    #[test]
    fn the_memo_key_follows_the_file_and_is_absent_for_an_unreadable_path() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let first = directory.path().join("first.dic.zst");
        let second = directory.path().join("second.dic.zst");
        fs::write(&first, b"one").expect("write");
        fs::write(&second, b"two longer").expect("write");

        let first_memo = identity_memo_path(&first).expect("a memo path exists");
        let second_memo = identity_memo_path(&second).expect("a memo path exists");
        assert_ne!(first_memo, second_memo);
        assert_eq!(identity_memo_path(&first).as_ref(), Some(&first_memo));
        assert_eq!(
            identity_memo_path(&directory.path().join("absent.dic.zst")),
            None
        );
    }
}
