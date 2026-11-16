import { attributeMembers } from './model.js';
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
        else {
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
