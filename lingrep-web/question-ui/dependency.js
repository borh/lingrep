import { attributeMembers, basicAttributeKeys, surfaceOf } from './model.js';
const scalar = (token, key) => { const value = token.attributes[key]; return typeof value === 'string' ? value : undefined; };
/** The 0-based head index, or null at the root. */
export function headIndex(token) {
    const head = Number(scalar(token, 'dep.head'));
    return head > 0 ? head - 1 : null;
}
export function hasDependencies(tokens) {
    return tokens.length > 0 && tokens.every(token => { const head = scalar(token, 'dep.head'); return head !== undefined && /^\d+$/.test(head) && Number(head) <= tokens.length; });
}
export function evidenceWords(row, length) {
    const roles = new Map();
    const add = (index, role) => { if (!Number.isSafeInteger(index) || index < 0 || index >= length)
        return; const set = roles.get(index) ?? new Set(); if (role)
        set.add(role); roles.set(index, set); };
    for (let index = row.span.start; index < row.span.end; index++)
        add(index);
    for (const capture of row.captures ?? [])
        for (let index = capture.span.start; index < capture.span.end; index++)
            add(index, capture.name);
    const bind = (node, role) => { if (node.node_kind === 'word' && node.token_index !== undefined)
        add(node.token_index, role); };
    for (const binding of row.graph_bindings ?? []) {
        if (binding.binding_kind === 'node')
            bind(binding.node, binding.name);
        else if (binding.binding_kind === 'edge') {
            bind(binding.head, `${binding.name}.head`);
            bind(binding.dependent, `${binding.name}.dependent`);
        }
    }
    return [...roles].sort(([a], [b]) => a - b).map(([index, set]) => ({ index, roles: [...set].sort(), inSpan: index >= row.span.start && index < row.span.end }));
}
/** Which words begin a 文節, from GiNZA's labels; null unless every word carries one. */
export function bunsetsuStarts(tokens) {
    const starts = tokens.map(token => {
        const label = scalar(token, 'spacy.ginza.bunsetu') ?? attributeMembers(token.attributes['conllu.misc']).find(entry => entry.startsWith('BunsetuBILabel='))?.slice(15);
        return label === 'B' ? true : label === 'I' ? false : null;
    });
    return starts.length && starts.every(start => start !== null) ? starts : null;
}
/** One unit per word, or per 文節 when `starts` is given; a 文節 attaches through its first word whose head lies outside it. */
export function dependencyUnits(tokens, starts) {
    const spans = [];
    tokens.forEach((_, index) => { if (!spans.length || starts?.[index] !== false)
        spans.push([index, index]);
    else
        spans[spans.length - 1][1] = index; });
    const unitOf = new Map();
    spans.forEach(([first, last], unit) => { for (let index = first; index <= last; index++)
        unitOf.set(index, unit); });
    return spans.map(([first, last], unit) => {
        let headWord = first;
        for (let index = first; index <= last; index++) {
            const head = headIndex(tokens[index]);
            if (head === null || unitOf.get(head) !== unit) {
                headWord = index;
                break;
            }
        }
        const head = headIndex(tokens[headWord]);
        return { first, last, head: head === null ? null : unitOf.get(head) ?? null, rel: scalar(tokens[headWord], 'dep.rel') ?? '' };
    });
}
export function annotatedBunsetsu(spans, tokens) {
    const chunks = spans.map((span, id) => ({ span, id })).filter(({ span }) => span.kind === 'bunsetsu')
        .sort((a, b) => a.span.start - b.span.start);
    const unitById = new Map(chunks.map(({ id }, unit) => [id, unit]));
    return chunks.map(({ span }) => {
        const word = span.head == null ? undefined : tokens[span.head];
        const target = word ? headIndex(word) : null;
        const governor = span.governor == null ? undefined : spans[span.governor];
        const sameEdge = target !== null && governor && governor.start <= target && target < governor.end;
        return { first: span.start, last: span.end - 1, head: span.governor == null ? null : unitById.get(span.governor) ?? null,
            rel: word && sameEdge ? scalar(word, 'dep.rel') ?? '' : '' };
    });
}
/** Arc levels by nesting: an arc rises one level above the highest arc it encloses. Input arcs must be sorted shortest first. */
function nest(arcs) {
    const placed = [];
    for (const arc of arcs) {
        const lo = Math.min(arc.from, arc.to), hi = Math.max(arc.from, arc.to);
        placed.push({ ...arc, level: 1 + Math.max(0, ...placed.filter(inner => Math.min(inner.from, inner.to) >= lo && Math.max(inner.from, inner.to) <= hi).map(inner => inner.level)) });
    }
    return placed;
}
const byExtent = (a, b) => Math.abs(a.from - a.to) - Math.abs(b.from - b.to) || Math.min(a.from, a.to) - Math.min(b.from, b.to);
/** Units holding evidence words, with the units between them folded into gaps, and the arcs among the kept units. */
export function focusLayout(units, words) {
    const kept = new Set(), bound = new Set();
    for (const word of words) {
        const unit = units.findIndex(unit => word.index >= unit.first && word.index <= unit.last);
        if (unit < 0)
            continue;
        kept.add(unit);
        if (word.roles.length)
            bound.add(unit);
    }
    const items = [], position = new Map();
    units.forEach((_, unit) => {
        if (kept.has(unit)) {
            position.set(unit, items.length);
            items.push({ kind: 'unit', unit });
            return;
        }
        const last = items.at(-1);
        if (last?.kind === 'gap')
            items[items.length - 1] = { kind: 'gap', count: last.count + 1 };
        else
            items.push({ kind: 'gap', count: 1 });
    });
    const arcs = [...kept].flatMap(unit => { const head = units[unit].head; return head !== null && head !== unit && kept.has(head) ? [{ from: position.get(head), to: position.get(unit), rel: units[unit].rel, bound: bound.has(unit) && bound.has(head) }] : []; });
    return { items, arcs: nest(arcs.sort(byExtent)).map(({ from, to, rel, level, bound }) => ({ from, to, rel, level, bound })) };
}
export function verticalArcs(tokens, words) {
    const roles = new Set(words.filter(word => word.roles.length).map(word => word.index));
    const arcs = tokens.flatMap((token, to) => { const from = headIndex(token); return from === null ? [] : [{ from, to, rel: scalar(token, 'dep.rel') ?? '', bound: roles.has(from) && roles.has(to) }]; }).sort(byExtent);
    const placed = [];
    for (const arc of arcs) {
        const lo = Math.min(arc.from, arc.to), hi = Math.max(arc.from, arc.to);
        let column = 1;
        while (placed.some(other => other.column === column && !(Math.max(other.from, other.to) < lo || hi < Math.min(other.from, other.to))))
            column++;
        placed.push({ ...arc, column });
    }
    return placed;
}
/** The context words a match binds outside its span, by sentence token index, with their roles. */
export function boundOutside(row, length) {
    return new Map(evidenceWords(row, length).filter(word => !word.inSpan && word.roles.length).map(word => [word.index, word.roles]));
}
/** The reader's choice of view, else 文節 when every word is labelled with one, else the whole sentence on a narrow screen; a 文節 choice without labels shows words. */
export function dependencyView(choice, bunsetsu, compact) {
    if (choice)
        return choice === 'bunsetsu' && !bunsetsu ? 'words' : choice;
    return bunsetsu ? 'bunsetsu' : compact ? 'vertical' : 'words';
}
/** The word a token attaches to and its 1-based number, as `抜かし·17`; null at the root. */
export function headLabel(tokens, index) {
    const head = headIndex(tokens[index]);
    return head === null || !tokens[head] ? null : `${surfaceOf(tokens[head])}·${head + 1}`;
}
const LABEL_SETS = ['ud-v2', 'spacy', 'clearnlp', 'penn', 'unidic'];
/** `value` of `attribute` in plain words, from the first label set that names it, or its base relation's name for a `dep.rel` subtype. Uses English if Japanese is unavailable and returns null for missing or identical glosses. */
export function valueName(names, attribute, value, locale) {
    const entry = (value) => LABEL_SETS.map(set => names?.[set]?.[attribute]?.[value]).find(Boolean);
    const found = entry(value) ?? (attribute === 'dep.rel' && value.includes(':') ? entry(value.slice(0, value.indexOf(':'))) : undefined);
    const gloss = found?.[locale] ?? found?.en ?? null;
    return gloss === value ? null : gloss;
}
const KEY_NAMES = {
    'text': { ja: 'テキスト', en: 'Text' },
    'surface': { ja: '表層形', en: 'Surface' },
    'lemma': { ja: '辞書形', en: 'Dictionary form' },
    'norm': { ja: '正規化形', en: 'Normalized form' },
    'upos': { ja: '品詞', en: 'Part of speech' },
    'xpos': { ja: '解析器の品詞', en: 'Analyzer POS tag' },
    'dep.head': { ja: '係り先', en: 'Attaches to' },
    'dep.rel': { ja: '関係', en: 'Relation' },
    'dep.enhanced': { ja: '拡張係り受け', en: 'Enhanced dependencies' },
    'unidic.pos1': { ja: '品詞大分類', en: 'POS level 1' },
    'unidic.pos2': { ja: '品詞中分類', en: 'POS level 2' },
    'unidic.pos3': { ja: '品詞小分類', en: 'POS level 3' },
    'unidic.pos4': { ja: '品詞細分類', en: 'POS level 4' },
    'unidic.ctype': { ja: '活用型', en: 'Conjugation type' },
    'unidic.cform': { ja: '活用形', en: 'Conjugation form' },
    'unidic.lform': { ja: '語彙素読み', en: 'Lexeme reading' },
    'unidic.orthbase': { ja: '書字形基本形', en: 'Base written form' },
    'unidic.form': { ja: '語形出現形', en: 'Inflected word form' },
    'unidic.formbase': { ja: '語形基本形', en: 'Base word form' },
    'unidic.kana': { ja: '仮名形出現形', en: 'Kana form' },
    'unidic.kanabase': { ja: '仮名形基本形', en: 'Base kana form' },
    'unidic.pron': { ja: '発音形出現形', en: 'Pronunciation' },
    'unidic.pronbase': { ja: '発音形基本形', en: 'Base pronunciation' },
    'unidic.goshu': { ja: '語種', en: 'Word origin' },
    'unidic.type': { ja: '語彙素類', en: 'Lexeme class' },
    'unidic.lextype': { ja: '辞書種別', en: 'Dictionary source' },
    'unidic.itype': { ja: '語頭変化型', en: 'Initial variation type' },
    'unidic.iform': { ja: '語頭変化形', en: 'Initial variation form' },
    'unidic.icontype': { ja: '語頭変化結合型', en: 'Initial variation connection type' },
    'unidic.ftype': { ja: '語末変化型', en: 'Final variation type' },
    'unidic.fform': { ja: '語末変化形', en: 'Final variation form' },
    'unidic.fcontype': { ja: '語末変化結合型', en: 'Final variation connection type' },
    'unidic.atype': { ja: 'アクセント型', en: 'Accent type' },
    'unidic.acontype': { ja: 'アクセント結合型', en: 'Accent connection type' },
    'unidic.amodtype': { ja: 'アクセント修飾型', en: 'Accent modification type' },
};
/** Token `index`'s details as the evidence table words them: `dep.head` as the head's word and number (`root` at the root) and other values by their names in `names`. */
export function tokenDetails(tokens, index, row, names, locale, root) {
    const attributes = tokens[index].attributes, word = evidenceWords(row, tokens.length).find(word => word.index === index);
    const detail = (key) => ({ key, name: KEY_NAMES[key]?.[locale] ?? null, values: attributeMembers(attributes[key]).map(value => ({ value, gloss: key === 'dep.head' ? (value === '0' ? root : headLabel(tokens, index)) : valueName(names, key, value, locale) })) });
    const basics = basicAttributeKeys(attributes).map(detail), keys = Object.keys(attributes);
    return { place: word?.inSpan ? 'span' : word?.roles.length ? 'bound' : 'outside', roles: word?.roles ?? [], basics, all: keys.length > basics.length ? keys.map(detail) : null };
}
