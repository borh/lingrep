use std::collections::HashMap;
use std::sync::LazyLock;

pub static TAG_MAP: LazyLock<HashMap<&'static str, &'static str>> = LazyLock::new(|| {
    let mut m = HashMap::new();
    m.insert("記号-一般", "NOUN");
    m.insert("記号-文字", "NOUN");
    m.insert("感動詞-フィラー", "INTJ");
    m.insert("感動詞-一般", "INTJ");
    m.insert("空白", "SPACE");
    m.insert("形状詞-一般", "ADJ");
    m.insert("形状詞-タリ", "ADJ");
    m.insert("形状詞-助動詞語幹", "AUX");
    m.insert("形容詞-一般", "ADJ");
    m.insert("形容詞-非自立可能", "ADJ");
    m.insert("助詞-格助詞", "ADP");
    m.insert("助詞-係助詞", "ADP");
    m.insert("助詞-終助詞", "PART");
    m.insert("助詞-準体助詞", "SCONJ");
    m.insert("助詞-接続助詞", "SCONJ");
    m.insert("助詞-副助詞", "ADP");
    m.insert("助動詞", "AUX");
    m.insert("接続詞", "CCONJ");
    m.insert("接頭辞", "NOUN");
    m.insert("接尾辞-形状詞的", "PART");
    m.insert("接尾辞-形容詞的", "AUX");
    m.insert("接尾辞-動詞的", "PART");
    m.insert("接尾辞-名詞的-サ変可能", "NOUN");
    m.insert("接尾辞-名詞的-一般", "NOUN");
    m.insert("接尾辞-名詞的-助数詞", "NOUN");
    m.insert("接尾辞-名詞的-副詞可能", "NOUN");
    m.insert("代名詞", "PRON");
    m.insert("動詞-一般", "VERB");
    m.insert("動詞-非自立可能", "AUX");
    m.insert("副詞", "ADV");
    m.insert("補助記号-ＡＡ-一般", "SYM");
    m.insert("補助記号-ＡＡ-顔文字", "PUNCT");
    m.insert("補助記号-一般", "SYM");
    m.insert("補助記号-括弧開", "PUNCT");
    m.insert("補助記号-括弧閉", "PUNCT");
    m.insert("補助記号-句点", "PUNCT");
    m.insert("補助記号-読点", "PUNCT");
    m.insert("名詞-固有名詞-一般", "PROPN");
    m.insert("名詞-固有名詞-人名-一般", "PROPN");
    m.insert("名詞-固有名詞-人名-姓", "PROPN");
    m.insert("名詞-固有名詞-人名-名", "PROPN");
    m.insert("名詞-固有名詞-地名-一般", "PROPN");
    m.insert("名詞-固有名詞-地名-国", "PROPN");
    m.insert("名詞-助動詞語幹", "AUX");
    m.insert("名詞-数詞", "NUM");
    m.insert("名詞-普通名詞-サ変可能", "NOUN");
    m.insert("名詞-普通名詞-サ変形状詞可能", "NOUN");
    m.insert("名詞-普通名詞-一般", "NOUN");
    m.insert("名詞-普通名詞-形状詞可能", "NOUN");
    m.insert("名詞-普通名詞-助数詞可能", "NOUN");
    m.insert("名詞-普通名詞-副詞可能", "NOUN");
    m.insert("連体詞", "DET");
    m.insert("外国語", "PROPN");
    m.insert("絵文字・記号等", "SYM");
    m
});
pub static TAG_ORTH_MAP: LazyLock<HashMap<&'static str, HashMap<&'static str, &'static str>>> =
    LazyLock::new(|| {
        let mut m = HashMap::new();
        m.insert("空白", {
            let mut sub_m = HashMap::new();
            sub_m.insert(" ", "SPACE");
            sub_m.insert("　", "X");
            sub_m
        });
        m.insert("助詞-副助詞", {
            let mut sub_m = HashMap::new();
            sub_m.insert("たり", "PART");
            sub_m
        });
        m.insert("連体詞", {
            let mut sub_m = HashMap::new();
            sub_m.insert("あの", "DET");
            sub_m.insert("かの", "DET");
            sub_m.insert("この", "DET");
            sub_m.insert("その", "DET");
            sub_m.insert("どの", "DET");
            sub_m.insert("彼の", "DET");
            sub_m.insert("此の", "DET");
            sub_m.insert("其の", "DET");
            sub_m.insert("ある", "PRON");
            sub_m.insert("こんな", "PRON");
            sub_m.insert("そんな", "PRON");
            sub_m.insert("どんな", "PRON");
            sub_m.insert("あらゆる", "PRON");
            sub_m
        });
        m
    });
pub static TAG_BIGRAM_MAP: LazyLock<
    HashMap<(&'static str, &'static str), (Option<&'static str>, Option<&'static str>)>,
> = LazyLock::new(|| {
    let mut m = HashMap::new();
    m.insert(("形容詞-非自立可能", "助詞-終助詞"), (Some("AUX"), None));
    m.insert(("名詞-普通名詞-形状詞可能", "助動詞"), (Some("ADJ"), None));
    m.insert(
        ("名詞-普通名詞-サ変可能", "動詞-非自立可能"),
        (Some("VERB"), Some("AUX")),
    );
    m.insert(
        ("名詞-普通名詞-サ変形状詞可能", "動詞-非自立可能"),
        (Some("VERB"), Some("AUX")),
    );
    m.insert(
        ("名詞-普通名詞-副詞可能", "動詞-非自立可能"),
        (None, Some("VERB")),
    );
    m.insert(("副詞", "動詞-非自立可能"), (None, Some("VERB")));
    m.insert(("形容詞-一般", "動詞-非自立可能"), (None, Some("VERB")));
    m.insert(
        ("形容詞-非自立可能", "動詞-非自立可能"),
        (None, Some("VERB")),
    );
    m.insert(("接頭辞", "動詞-非自立可能"), (None, Some("VERB")));
    m.insert(("助詞-係助詞", "動詞-非自立可能"), (None, Some("VERB")));
    m.insert(("助詞-副助詞", "動詞-非自立可能"), (None, Some("VERB")));
    m.insert(("助詞-格助詞", "動詞-非自立可能"), (None, Some("VERB")));
    m.insert(("補助記号-読点", "動詞-非自立可能"), (None, Some("VERB")));
    m.insert(("形容詞-一般", "接尾辞-名詞的-一般"), (None, Some("PART")));
    m.insert(("助詞-格助詞", "形状詞-助動詞語幹"), (None, Some("NOUN")));
    m.insert(("連体詞", "形状詞-助動詞語幹"), (None, Some("NOUN")));
    m.insert(("動詞-一般", "助詞-副助詞"), (None, Some("PART")));
    m.insert(("動詞-非自立可能", "助詞-副助詞"), (None, Some("PART")));
    m.insert(("助動詞", "助詞-副助詞"), (None, Some("PART")));
    m
});
