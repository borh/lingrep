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
export function locationURL(base, params, evidence, view) {
    const url = new URL(base);
    const entries = Object.entries(params).map(([key, value]) => [key, String(value)]);
    const hash = new URLSearchParams(entries);
    if (evidence)
        hash.set('evidence', evidence);
    hash.set('view', JSON.stringify(view));
    url.hash = hash.toString();
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
        for (const key of ['source', 'corpus', 'analysis', 'raw', 'run', 'notice', 'lines', 'context'])
            if (seed[key] != null) {
                if (typeof seed[key] !== 'string' || key === 'run' && !['0', '1'].includes(seed[key]))
                    throw new Error('The initial question contains invalid settings.');
                params.set(key, seed[key]);
            }
    }
    const overrides = new URLSearchParams(hash.replace(/^#/, ''));
    if (overrides.has('evidence') || overrides.has('attempt'))
        for (const key of ['source', 'corpus', 'analysis', 'raw', 'run'])
            params.delete(key);
    for (const [key, value] of overrides)
        params.set(key, value);
    return params;
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
    if (!record(value) || typeof value.pattern !== 'string' || !value.pattern || !ordinal(value.sentence) || !Array.isArray(value.roles) || !value.roles.every(role) || typeof value.analysis !== 'string' || typeof value.fingerprint !== 'string')
        throw new Error('The frequency occurrence in this address is invalid. Open it again from the frequency view.');
    return { pattern: value.pattern, sentence: value.sentence, roles: value.roles.map(({ name, start, end }) => ({ name, start, end })), analysis: value.analysis, fingerprint: value.fingerprint };
}
/** The frequency view address to return to, or null unless `value` is a relative `frequencies` address. */
export const frequencyReturn = (value) => value !== null && /^frequencies(#|$)/.test(value) ? value : null;
