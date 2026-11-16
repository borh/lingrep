export const attributeMembers = (value) => value === undefined ? [] : typeof value === 'string' ? [value] : value;
export const errorMessage = (error) => error instanceof Error ? error.message : String(error);
export function element(selector, root = document) {
    const node = root.querySelector(selector);
    if (!node)
        throw new Error(`Missing interface element: ${selector}`);
    return node;
}
export function runningSnapshot(id, question) {
    return { ...question, id, phase: 'running', counts: {}, rows: {}, definitions: [], bodies: {}, elapsed: 0, received: 0, version: 0, started: 0, error: null, counters: null, contract_run: null };
}
const objectValue = (value) => typeof value === 'object' && value !== null && !Array.isArray(value);
const finiteNumber = (value) => typeof value === 'number' && Number.isFinite(value);
const stringMap = (value) => objectValue(value) && Object.values(value).every(value => typeof value === 'string');
const tokenValue = (value) => objectValue(value) && Number.isSafeInteger(value.index) && Number(value.index) >= 0 && objectValue(value.attributes) && typeof value.attributes.text === 'string' && Object.values(value.attributes).every(v => typeof v === 'string' || Array.isArray(v) && v.every(member => typeof member === 'string'));
function diagnosticValue(value) {
    if (!objectValue(value) || value.schema !== 'lingrep.diagnostic/v1' || value.type !== 'diagnostic' || !objectValue(value.data))
        return false;
    const data = value.data;
    if (!['stage', 'severity', 'message'].every(key => typeof data[key] === 'string'))
        return false;
    if (data.location == null)
        return true;
    if (!objectValue(data.location))
        return false;
    const location = data.location;
    if (!['line', 'column'].every(key => location[key] == null || Number.isSafeInteger(location[key]) && Number(location[key]) > 0))
        return false;
    const span = location.source_span;
    return span == null || objectValue(span) && Number.isSafeInteger(span.start) && Number.isSafeInteger(span.end) && Number(span.start) >= 0 && Number(span.end) >= Number(span.start);
}
function matchValue(value) {
    if (!objectValue(value) || !objectValue(value.span))
        return false;
    if (!['sequence', 'corpus_sentence_ordinal', 'document_ordinal'].every(key => finiteNumber(value[key])))
        return false;
    if (!['pattern', 'source_name', 'surface'].every(key => typeof value[key] === 'string'))
        return false;
    if (!finiteNumber(value.span.start) || !finiteNumber(value.span.end) || value.span.end < value.span.start)
        return false;
    return ['tokens', 'left_context', 'right_context'].every(key => value[key] === undefined && key !== 'tokens' || Array.isArray(value[key]) && value[key].every(tokenValue));
}
export function parseUpdate(text) {
    const value = JSON.parse(text);
    if (!objectValue(value) || typeof value.id !== 'string' || typeof value.reset !== 'boolean' || !objectValue(value.patch) || !objectValue(value.append))
        throw new Error('Invalid search update envelope.');
    if (value.patch.id !== undefined && value.patch.id !== value.id)
        throw new Error('Search update identity mismatch.');
    const checks = {
        id: v => typeof v === 'string', source: v => typeof v === 'string', dataset: v => typeof v === 'string', where: v => typeof v === 'string',
        phase: v => typeof v === 'string' && ['running', 'complete', 'failed', 'cancelled'].includes(v),
        elapsed: finiteNumber, received: finiteNumber, version: finiteNumber, started: finiteNumber,
        diagnostics: v => Array.isArray(v) && v.every(diagnosticValue),
        notices: v => Array.isArray(v) && v.every(value => typeof value === 'string'),
        error: v => v === null || typeof v === 'string', definitions: v => Array.isArray(v) && v.every(x => typeof x === 'string'),
        bodies: stringMap, counts: v => objectValue(v) && Object.values(v).every(finiteNumber),
        contract_run: v => v === null || objectValue(v) && typeof v.analysis_identity === 'string',
        counters: v => v === null || objectValue(v) && objectValue(v.counters) && finiteNumber(v.counters.documents_admitted),
    };
    for (const [key, field] of Object.entries(value.patch))
        if (checks[key] && !checks[key](field))
            throw new Error(`Invalid search update field: ${key}.`);
    for (const rows of Object.values(value.append))
        if (!Array.isArray(rows) || !rows.every(matchValue))
            throw new Error('Invalid example records in search update.');
    if (Object.values(value.append).some(rows => rows.length) && !finiteNumber(value.patch.version))
        throw new Error('Example update has no version.');
    return value;
}
export function parseSnapshot(value) {
    if (!objectValue(value) || typeof value.id !== 'string' || !['source', 'dataset', 'where', 'phase'].every(key => typeof value[key] === 'string') || !objectValue(value.rows) || !objectValue(value.counts) || !Array.isArray(value.definitions))
        throw new Error('The saved search could not be read. Run the question again.');
    parseUpdate(JSON.stringify({ id: value.id, reset: true, patch: Object.fromEntries(Object.entries(value).filter(([key]) => key !== 'rows')), append: value.rows }));
    return value;
}
export function sourceSelection(source, span) {
    const bytes = new TextEncoder().encode(source);
    if (!Number.isSafeInteger(span.start) || !Number.isSafeInteger(span.end) || span.start < 0 || span.end < span.start || span.end > bytes.length)
        return null;
    const decoder = new TextDecoder('utf-8', { fatal: true });
    try {
        return [decoder.decode(bytes.subarray(0, span.start)).length, decoder.decode(bytes.subarray(0, span.end)).length];
    }
    catch {
        return null;
    }
}
export function parseStoredSentence(value) {
    if (!objectValue(value) || typeof value.analysis_identity !== 'string' || !Number.isSafeInteger(value.sentence) || Number(value.sentence) < 0 || !Array.isArray(value.tokens) || !value.tokens.every(tokenValue))
        throw new Error('The stored token analysis could not be read.');
    return value;
}
