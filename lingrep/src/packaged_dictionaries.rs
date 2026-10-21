//! The UniDic dictionaries published for `ling dictionary install`, where they
//! are installed, and the default dictionary path.

use std::fs::{self, File};
use std::io::{self, BufWriter, IsTerminal, Read, Write};
use std::path::{Path, PathBuf};

use clap::ValueEnum;

use crate::error::CliError;

/// Base URL for `<file_name>.zst` release assets.
const RELEASE_URL: &str = "https://github.com/borh/lingrep/releases/download/unidic-202512";

/// A dictionary built by the flake's `vibrato-dict-*` package.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PackagedDictionary {
    pub name: &'static str,
    pub file_name: &'static str,
    /// BLAKE3 of the decompressed `.dic`, as pinned by
    /// `vibrato-dictionaries.tests.digests` in `flake.nix`.
    pub blake3: &'static str,
    pub license: &'static str,
}

const BSD: &str = "BSD-3-Clause (UniDic is also available under GPL-2.0 and LGPL-2.1)";
const CC_BY_NC_SA: &str = "CC BY-NC-SA 4.0: non-commercial use only, with attribution to NINJAL";

pub const DICTIONARIES: [PackagedDictionary; 7] = [
    PackagedDictionary {
        name: "cwj",
        file_name: "unidic-cwj-202512.dic",
        blake3: "80aeaf4225676386368932b1f632d71e05d81b4be185e0b786c360656944abbf",
        license: BSD,
    },
    PackagedDictionary {
        name: "csj",
        file_name: "unidic-csj-202512.dic",
        blake3: "80c6e211be770d4e8ee0e2443584f285818e508acea84f21ea03d5eb47afe37f",
        license: BSD,
    },
    PackagedDictionary {
        name: "novel",
        file_name: "unidic-novel-202512.dic",
        blake3: "20ff3deb8b988e345e78af3079a61a023ceaa974151988046931c94894d66348",
        license: CC_BY_NC_SA,
    },
    PackagedDictionary {
        name: "qkana",
        file_name: "unidic-qkana-202512.dic",
        blake3: "c15e76a03d56bceec589da2ac6b93c7491b5f01bf1be8b8b5c1ae1075490f955",
        license: CC_BY_NC_SA,
    },
    PackagedDictionary {
        name: "kindai-bungo",
        file_name: "unidic-kindai-bungo-202512.dic",
        blake3: "a12b36972086e170e428eef56b53d8eafe8b24d16628c9edf7370a87b804bb8b",
        license: CC_BY_NC_SA,
    },
    PackagedDictionary {
        name: "kinsei-edo",
        file_name: "unidic-kinsei-edo-202512.dic",
        blake3: "9aa4a90441d4209b2b59d9eee2609be79f3307df56083c5d618afd00d458604b",
        license: CC_BY_NC_SA,
    },
    PackagedDictionary {
        name: "kinsei-bungo",
        file_name: "unidic-kinsei-bungo-202512.dic",
        blake3: "8fbb017faaff3d2ab412a6d23195b51b5adf412c1742d14c8da039623bb52ea9",
        license: CC_BY_NC_SA,
    },
];

/// How to supply a Vibrato dictionary, for errors that report none.
pub const MISSING_DICTIONARY_HINT: &str =
    "run `ling dictionary install`, pass --dict-path, or set LINGREP_DICTIONARY_PATH";

/// The dictionary `ling` loads when neither `--dict-path` nor
/// `LINGREP_DICTIONARY_PATH` names one.
pub const DEFAULT: PackagedDictionary = DICTIONARIES[0];

/// A dictionary name accepted by `ling dictionary install`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DictionaryName(pub PackagedDictionary);

impl ValueEnum for DictionaryName {
    fn value_variants<'a>() -> &'a [Self] {
        const VARIANTS: [DictionaryName; 7] = [
            DictionaryName(DICTIONARIES[0]),
            DictionaryName(DICTIONARIES[1]),
            DictionaryName(DICTIONARIES[2]),
            DictionaryName(DICTIONARIES[3]),
            DictionaryName(DICTIONARIES[4]),
            DictionaryName(DICTIONARIES[5]),
            DictionaryName(DICTIONARIES[6]),
        ];
        &VARIANTS
    }

    fn to_possible_value(&self) -> Option<clap::builder::PossibleValue> {
        Some(clap::builder::PossibleValue::new(self.0.name).help(self.0.license))
    }
}

/// `$XDG_DATA_HOME/lingrep/dictionaries`, or `~/.local/share/lingrep/dictionaries`
/// when `XDG_DATA_HOME` is unset or empty, matching `install.sh`.
#[must_use]
pub fn install_directory() -> Option<PathBuf> {
    let non_empty = |name| std::env::var_os(name).filter(|value| !value.is_empty());
    let data_home = non_empty("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| non_empty("HOME").map(|home| PathBuf::from(home).join(".local/share")))?;
    Some(data_home.join("lingrep").join("dictionaries"))
}

/// `LINGREP_DICTIONARY_PATH` when set and non-empty, otherwise the installed
/// default dictionary when that file exists.
#[must_use]
pub fn fallback_dictionary_path() -> Option<PathBuf> {
    std::env::var_os("LINGREP_DICTIONARY_PATH")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            install_directory()
                .map(|directory| directory.join(DEFAULT.file_name))
                .filter(|path| path.is_file())
        })
}

/// Result of [`install`].
#[derive(Debug, PartialEq, Eq)]
pub struct Installed {
    pub path: PathBuf,
    pub bytes: u64,
    /// The file was already present with the expected digest.
    pub already_present: bool,
}

/// Downloads `dictionary` into `directory` and checks its digest, unless the
/// file is already there with that digest.
pub fn install(dictionary: PackagedDictionary, directory: &Path) -> Result<Installed, CliError> {
    let path = directory.join(dictionary.file_name);
    if path.is_file() {
        let (bytes, digest) = digest_file(&path).map_err(|error| io_error(&path, &error))?;
        if digest == dictionary.blake3 {
            return Ok(Installed {
                path,
                bytes,
                already_present: true,
            });
        }
    }
    let url = format!("{RELEASE_URL}/{}.zst", dictionary.file_name);
    let response = reqwest::blocking::Client::builder()
        .timeout(None)
        .build()
        .and_then(|client| client.get(&url).send())
        .and_then(reqwest::blocking::Response::error_for_status)
        .map_err(|error| CliError::Dictionary(format!("cannot download {url}: {error}")))?;
    let total = response.content_length();
    let progress = Progress::new(dictionary.file_name, total);
    let bytes = unpack_verified(progress.wrap(response), dictionary.blake3, &path)?;
    progress.finish();
    Ok(Installed {
        path,
        bytes,
        already_present: false,
    })
}

/// Decompresses the zstd stream `compressed` to `path`, replacing it only when
/// the decompressed bytes have BLAKE3 `expected`. Returns their length.
fn unpack_verified(compressed: impl Read, expected: &str, path: &Path) -> Result<u64, CliError> {
    let directory = path.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(directory).map_err(|error| io_error(directory, &error))?;
    let partial = tempfile::Builder::new()
        .prefix(".partial-")
        .tempfile_in(directory)
        .map_err(|error| io_error(directory, &error))?;
    let mut decoder = zstd::stream::read::Decoder::new(compressed)
        .map_err(|error| CliError::Dictionary(format!("cannot read the download: {error}")))?;
    let mut writer = HashingWriter::new(BufWriter::new(partial.as_file()));
    io::copy(&mut decoder, &mut writer)
        .map_err(|error| CliError::Dictionary(format!("download interrupted: {error}")))?;
    let (bytes, digest) = writer
        .finish()
        .map_err(|error| io_error(partial.path(), &error))?;
    if digest != expected {
        return Err(CliError::Dictionary(format!(
            "downloaded dictionary has BLAKE3 {digest}, expected {expected}; nothing was installed"
        )));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        partial
            .as_file()
            .set_permissions(fs::Permissions::from_mode(0o644))
            .map_err(|error| io_error(partial.path(), &error))?;
    }
    partial
        .as_file()
        .sync_all()
        .map_err(|error| io_error(partial.path(), &error))?;
    partial
        .persist(path)
        .map_err(|error| io_error(path, &error.error))?;
    Ok(bytes)
}

fn digest_file(path: &Path) -> io::Result<(u64, String)> {
    let mut writer = HashingWriter::new(io::sink());
    io::copy(&mut File::open(path)?, &mut writer)?;
    writer.finish()
}

fn io_error(path: &Path, error: &io::Error) -> CliError {
    CliError::Dictionary(format!("{}: {error}", path.display()))
}

struct HashingWriter<W> {
    inner: W,
    hasher: blake3::Hasher,
    bytes: u64,
}

impl<W: Write> HashingWriter<W> {
    fn new(inner: W) -> Self {
        Self {
            inner,
            hasher: blake3::Hasher::new(),
            bytes: 0,
        }
    }

    fn finish(mut self) -> io::Result<(u64, String)> {
        self.inner.flush()?;
        Ok((self.bytes, self.hasher.finalize().to_hex().to_string()))
    }
}

impl<W: Write> Write for HashingWriter<W> {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        let written = self.inner.write(buffer)?;
        self.hasher.update(&buffer[..written]);
        self.bytes += written as u64;
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

/// Download progress on stderr, written only when stderr is a terminal.
struct Progress {
    enabled: bool,
    label: &'static str,
    total: Option<u64>,
}

impl Progress {
    fn new(label: &'static str, total: Option<u64>) -> Self {
        Self {
            enabled: io::stderr().is_terminal(),
            label,
            total,
        }
    }

    fn wrap<R: Read>(&self, inner: R) -> ProgressReader<'_, R> {
        ProgressReader {
            inner,
            progress: self,
            read: 0,
            shown_mb: u64::MAX,
        }
    }

    fn show(&self, read: u64) {
        let mb = |bytes: u64| bytes / 1_000_000;
        let line = match self.total {
            Some(total) => format!("downloading {}: {}/{} MB", self.label, mb(read), mb(total)),
            None => format!("downloading {}: {} MB", self.label, mb(read)),
        };
        let _ = write!(io::stderr(), "\r{line}");
    }

    fn finish(&self) {
        if self.enabled {
            let _ = writeln!(io::stderr());
        }
    }
}

struct ProgressReader<'a, R> {
    inner: R,
    progress: &'a Progress,
    read: u64,
    shown_mb: u64,
}

impl<R: Read> Read for ProgressReader<'_, R> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let read = self.inner.read(buffer)?;
        self.read += read as u64;
        if self.progress.enabled && self.read / 1_000_000 != self.shown_mb {
            self.shown_mb = self.read / 1_000_000;
            self.progress.show(self.read);
        }
        Ok(read)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn compressed(bytes: &[u8]) -> Vec<u8> {
        zstd::stream::encode_all(bytes, 3).unwrap()
    }

    #[test]
    fn a_download_with_the_expected_digest_is_installed() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("nested/unidic-test.dic");
        let content = b"dictionary bytes".repeat(1000);
        let expected = blake3::hash(&content).to_hex().to_string();

        let bytes = unpack_verified(&compressed(&content)[..], &expected, &path).unwrap();

        assert_eq!(bytes, content.len() as u64);
        assert_eq!(fs::read(&path).unwrap(), content);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o644
            );
        }
        assert_eq!(fs::read_dir(path.parent().unwrap()).unwrap().count(), 1);
    }

    #[test]
    fn a_download_with_another_digest_leaves_the_installed_file_and_no_partial() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("unidic-test.dic");
        fs::write(&path, b"previous").unwrap();
        let wrong = blake3::hash(b"other").to_hex().to_string();

        let error = unpack_verified(&compressed(b"new")[..], &wrong, &path).unwrap_err();

        assert!(
            error.to_string().contains("nothing was installed"),
            "{error}"
        );
        assert_eq!(fs::read(&path).unwrap(), b"previous");
        assert_eq!(fs::read_dir(directory.path()).unwrap().count(), 1);
    }

    #[test]
    fn a_truncated_download_leaves_no_partial() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("unidic-test.dic");
        let stream = compressed(&b"dictionary bytes".repeat(1000));

        let error = unpack_verified(&stream[..stream.len() / 2], "", &path).unwrap_err();

        assert!(error.to_string().contains("interrupted"), "{error}");
        assert_eq!(fs::read_dir(directory.path()).unwrap().count(), 0);
    }

    #[test]
    fn every_digest_is_the_one_the_flake_pins() {
        let flake = include_str!("../../flake.nix");
        for dictionary in DICTIONARIES {
            let line = format!("{}  {}", dictionary.blake3, dictionary.file_name);
            assert!(flake.contains(&line), "flake.nix does not pin `{line}`");
        }
    }
}
