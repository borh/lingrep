export const defaultView = () => ({ kind: 'concordance', scopeDocument: null, returnAfter: -1, pattern: '', document: null, after: -1, filter: '' });
const record = (value) => typeof value === 'object' && value !== null && !Array.isArray(value);
const ordinal = (value) => typeof value === 'number' && Number.isSafeInteger(value) && value >= 0;
function concordance(value) {
    return record(value) && value.kind === 'concordance' && typeof value.pattern === 'string' && (value.document === null || ordinal(value.document)) && typeof value.after === 'number' && Number.isSafeInteger(value.after) && value.after >= -1 && typeof value.filter === 'string' && (value.scopeDocument == null || ordinal(value.scopeDocument)) && (value.returnAfter == null || typeof value.returnAfter === 'number' && Number.isSafeInteger(value.returnAfter) && value.returnAfter >= -1);
}
export function parseView(text) {
    if (!text)
        return defaultView();
    const value = JSON.parse(text);
    if (concordance(value))
        return value;
    if (record(value) && (value.kind === 'example' || value.kind === 'reader') && typeof value.pattern === 'string' && ordinal(value.document) && (value.sequence === null || ordinal(value.sequence))) {
        if (concordance(value.returnTo))
            return value;
        if (value.kind === 'reader' && record(value.returnTo) && value.returnTo.kind === 'example' && concordance(value.returnTo.returnTo) && typeof value.returnTo.pattern === 'string' && ordinal(value.returnTo.document) && (value.returnTo.sequence === null || ordinal(value.returnTo.sequence)))
            return value;
    }
    throw new Error('The saved reading location is invalid. Open a new question.');
}
export function locationHash(params, evidence, view) {
    const hash = new URLSearchParams(Object.entries(params).map(([key, value]) => [key, String(value)]));
    if (evidence)
        hash.set('evidence', evidence);
    hash.set('view', JSON.stringify(view));
    return '#' + hash.toString();
}
export function locationURL(base, params, evidence, view) {
    const url = new URL(base);
    url.hash = locationHash(params, evidence, view);
    return url.href;
}
export function savedQuestion(value) {
    if (!record(value) || !['lingrep.question/v1', 'lingrep.question-probe/v1'].includes(String(value.format)) || !['question', 'source', 'corpus', 'analysis', 'where'].every(key => typeof value[key] === 'string'))
        throw new Error('Choose a saved lingrep question document.');
    if (!Object.values(value).every(field => typeof field === 'string' || typeof field === 'number' && Number.isFinite(field)))
        throw new Error('The question document contains invalid settings.');
    parseView(typeof value.view === 'string' ? value.view : null);
    return value;
}
export function seedParams(seed, hash) {
    const params = new URLSearchParams();
    if (seed != null) {
        if (!record(seed))
            throw new Error('The initial question could not be read.');
        for (const key of ['source', 'corpus', 'analysis', 'raw', 'run', 'notice', 'lines', 'context', 'pattern'])
            if (seed[key] != null) {
                if (typeof seed[key] !== 'string' || key === 'run' && !['0', '1'].includes(seed[key]))
                    throw new Error('The initial question contains invalid settings.');
                params.set(key, seed[key]);
            }
    }
    const overrides = new URLSearchParams(hash.replace(/^#/, ''));
    if (overrides.has('catalog'))
        params.delete('source');
    if (overrides.has('evidence') || overrides.has('attempt'))
        for (const key of ['source', 'corpus', 'analysis', 'raw', 'run', 'pattern'])
            params.delete(key);
    if (overrides.has('corpus') && !overrides.has('analysis') && overrides.get('corpus') !== params.get('corpus'))
        params.delete('analysis');
    for (const [key, value] of overrides)
        params.set(key, value);
    return params;
}
/** Replaces `source` with `catalog` only when the source matches the served catalog and its fingerprint is available. */
export function catalogParams(params, served) {
    if (served.fingerprint === undefined || params.source !== served.source)
        return params;
    const { source: _source, ...rest } = params;
    return { ...rest, catalog: served.fingerprint };
}
/** Removes `catalog`, preserving an explicit `source`; otherwise expands the served source and reports a fingerprint mismatch in `changed`. */
export function resolveCatalog(params, served) {
    const linked = params.get('catalog');
    params.delete('catalog');
    if (linked === null || params.has('source'))
        return { changed: null };
    params.set('source', served.source);
    return { changed: linked === served.fingerprint ? null : linked };
}
/** The analysis identity a search of `params`' corpus expects: the address's own, else `run`'s when `run` read that corpus. */
export function expectedAnalysis(params, run) {
    if (params.get('analysis'))
        return params.get('analysis');
    return run && run.dataset === params.get('corpus') ? run.contract_run?.analysis_identity ?? null : null;
}
export function parseOccurrence(text) {
    if (!text)
        return null;
    let value;
    try {
        value = JSON.parse(text);
    }
    catch {
        value = null;
    }
    const role = (item) => record(item) && typeof item.name === 'string' && ordinal(item.start) && ordinal(item.end) && item.end >= item.start;
    if (!record(value) || typeof value.pattern !== 'string' || !value.pattern || !ordinal(value.sentence) || !ordinal(value.document) || !Array.isArray(value.roles) || !value.roles.every(role) || typeof value.analysis !== 'string' || typeof value.fingerprint !== 'string')
        throw new Error('The frequency occurrence in this address is invalid. Open it again from the frequency view.');
    return { pattern: value.pattern, sentence: value.sentence, document: value.document, roles: value.roles.map(({ name, start, end }) => ({ name, start, end })), analysis: value.analysis, fingerprint: value.fingerprint };
}
export function parseEntryLines(text) {
    if (!text)
        return null;
    let value;
    try {
        value = JSON.parse(text);
    }
    catch {
        value = null;
    }
    const role = (item) => record(item) && typeof item.name === 'string' && ordinal(item.start) && ordinal(item.end) && item.end >= item.start;
    const span = (item) => item.start === undefined && item.end === undefined || ordinal(item.start) && ordinal(item.end) && item.end > item.start;
    const line = (item) => record(item) && ordinal(item.sentence) && ordinal(item.document) && span(item) && Array.isArray(item.roles) && item.roles.every(role);
    const names = (item) => item === undefined || record(item) && Object.values(item).every(name => typeof name === 'string');
    if (!record(value) || !Array.isArray(value.words) || !value.words.every(word => typeof word === 'string') || !Array.isArray(value.lines) || !value.lines.length || !value.lines.every(line) || !ordinal(value.total) || value.total < value.lines.length || value.works !== undefined && !ordinal(value.works) || !names(value.names) || !ordinal(value.at) || value.at >= value.lines.length)
        throw new Error('The frequency entry in this address is invalid. Open it again from the frequency view.');
    return { words: value.words, total: value.total, ...(value.works !== undefined ? { works: value.works } : {}), at: value.at, ...(value.names !== undefined ? { names: value.names } : {}), lines: value.lines.map(({ sentence, document, start, end, roles }) => ({ sentence, document, ...(start !== undefined ? { start, end } : {}), roles: roles.map(({ name, start, end }) => ({ name, start, end })) })) };
}
/** A listed line read as a concordance line from its sentence's token `texts`: up to `context` tokens on each side of its span, the span's text, and the role words outside the span by token index. A line without a span spans its role words. */
export function entryContext(texts, line, context) {
    const start = line.start ?? Math.min(...line.roles.map(role => role.start)), end = line.end ?? Math.max(...line.roles.map(role => role.end));
    const tokens = (from, to) => texts.slice(Math.max(0, from), to).map((text, offset) => ({ index: Math.max(0, from) + offset, attributes: { text } }));
    const bound = new Map();
    for (const role of line.roles)
        for (let index = role.start; index < role.end; index++)
            if (index < start || index >= end)
                bound.set(index, [...(bound.get(index) ?? []), role.name]);
    return { left: context ? tokens(start - context, start) : [], hit: texts.slice(start, end).join(''), right: tokens(end, end + context), bound };
}
/** The frequency view address to return to, or null unless `value` is a relative `frequencies` address. */
export const frequencyReturn = (value) => value !== null && /^frequencies(#|$)/.test(value) ? value : null;
