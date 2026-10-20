use std::path::{Path, PathBuf};

use tempfile::TempDir;
use vibrato_rkyv::SystemDictionaryBuilder;

/// Small serialized Vibrato dictionary for tests that do not own UniDic semantics.
///
/// The guard owns the temporary artifact. Keep it alive while a runtime or child
/// process can read Self::path.
pub struct MiniDictionary {
    _directory: TempDir,
    path: PathBuf,
    compressed_path: PathBuf,
}

impl MiniDictionary {
    /// Build the fixed, exactly characterized miniature dictionary.
    #[must_use]
    pub fn new() -> Self {
        let directory = tempfile::tempdir().expect("create miniature dictionary directory");
        let path = directory.path().join("mini.dic");
        let compressed_path = directory.path().join("mini.dic.zst");
        let lexicon = concat!(
            "猫,0,0,1,名詞,普通名詞,一般,*,*,*,ネコ,猫,猫,ネコ,猫,ネコ,和,*,*\n",
            "犬,0,0,1,名詞,普通名詞,一般,*,*,*,イヌ,犬\n",
            "鳥,0,0,1,名詞,普通名詞,一般,*,*,*,トリ,鳥\n",
            "一,0,0,1,名詞,数詞,*,*,*,*,イチ,一\n",
            "二,0,0,1,名詞,数詞,*,*,*,*,ニ,二\n",
            "三,0,0,1,名詞,数詞,*,*,*,*,サン,三\n",
            "文,0,0,1,名詞,普通名詞,一般,*,*,*,ブン,文\n",
            "研究,0,0,1,名詞,普通名詞,サ変可能,*,*,*,ケンキュウ,研究\n",
            "実験,0,0,1,名詞,普通名詞,サ変可能,*,*,*,ジッケン,実験,実験,ジッケン,実験,ジッケン,漢,*,*\n",
            "調査,0,0,1,名詞,普通名詞,サ変可能,*,*,*,チョウサ,調査,調査,チョーサ,調査,チョーサ,漢,*,*\n",
            "私,0,0,1,代名詞,*,*,*,*,*,ワタシ,私\n",
            "彼,0,0,1,代名詞,*,*,*,*,*,カレ,彼\n",
            "と,0,0,1,助詞,格助詞,*,*,*,*,ト,と\n",
            "を,0,0,1,助詞,格助詞,*,*,*,*,ヲ,を,を,オ,を,オ,和,*,*\n",
            "が,0,0,1,助詞,格助詞,*,*,*,*,ガ,が,が,ガ,が,ガ,和,*,*\n",
            "は,0,0,1,助詞,係助詞,*,*,*,*,ハ,は,は,ワ,は,ワ,和,*,*\n",
            "も,0,0,1,助詞,係助詞,*,*,*,*,モ,も\n",
            "鳴く,0,0,1,動詞,一般,*,*,五段-カ行,終止形-一般,ナク,鳴く\n",
            "走る,0,0,1,動詞,一般,*,*,五段-ラ行,終止形-一般,ハシル,走る,走る,ハシル,走る,ハシル,和,ハ濁,基本形\n",
            "眠る,0,0,1,動詞,一般,*,*,五段-ラ行,終止形-一般,ネムル,眠る\n",
            "いる,0,0,1,動詞,非自立可能,*,*,上一段-ア行,終止形-一般,イル,居る,いる,イル,いる,イル,和,*,*\n",
            "行っ,0,0,1,動詞,一般,*,*,五段-ワア行,連用形-促音便,オコナウ,行う,行っ,オコナッ,行う,オコナウ,和,*,*\n",
            "行う,0,0,1,動詞,一般,*,*,五段-ワア行,終止形-一般,オコナウ,行う,行う,オコナウ,行う,オコナウ,和,*,*\n",
            "た,0,0,1,助動詞,*,*,*,助動詞-タ,終止形-一般,タ,た,た,タ,た,タ,和,*,*\n",
            "進む,0,0,1,動詞,一般,*,*,五段-マ行,終止形-一般,ススム,進む\n",
            "食べる,0,0,1,動詞,一般,*,*,下一段-バ行,終止形-一般,タベル,食べる,食べる,タベル,食べる\n",
            "。,0,0,1,補助記号,句点,*,*,*,*,*,。,。,*,。,*,記号,*,*\n",
        );
        let dictionary = SystemDictionaryBuilder::from_readers(
            lexicon.as_bytes(),
            "1 1\n0 0 0".as_bytes(),
            "DEFAULT 0 1 0\nSPACE 0 1 0\n0x0020 SPACE".as_bytes(),
            "DEFAULT,0,0,100,名詞,普通名詞,一般,*,*,*".as_bytes(),
        )
        .expect("build miniature dictionary");
        let mut bytes = Vec::new();
        dictionary
            .write(&mut bytes)
            .expect("serialize miniature dictionary");
        std::fs::write(&path, &bytes).expect("write miniature dictionary");
        let compressed =
            zstd::stream::encode_all(bytes.as_slice(), 0).expect("compress miniature dictionary");
        std::fs::write(&compressed_path, compressed).expect("write compressed dictionary");

        for artifact in [&path, &compressed_path] {
            let mut permissions = std::fs::metadata(artifact)
                .expect("read miniature dictionary metadata")
                .permissions();
            permissions.set_readonly(true);
            std::fs::set_permissions(artifact, permissions)
                .expect("mark miniature dictionary read-only");
        }

        Self {
            _directory: directory,
            path,
            compressed_path,
        }
    }

    /// Borrow the serialized dictionary path for a runtime or child process.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Borrow the compressed view of the same serialized dictionary bytes.
    #[must_use]
    pub fn compressed_path(&self) -> &Path {
        &self.compressed_path
    }
}

impl Default for MiniDictionary {
    fn default() -> Self {
        Self::new()
    }
}
