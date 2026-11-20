/** The token's exact source text where the corpus carries `surface`, else its `text`. */
export const surfaceOf = (token) => { const surface = token.attributes.surface; return typeof surface === 'string' ? surface : token.attributes.text; };
export const attributeMembers = (value) => value === undefined ? [] : typeof value === 'string' ? [value] : value;
/** A run's failure text, translated when the server reports a known failure code. */
export const runError = (run, t) => run.error_code === 'analysis_changed' ? t('analysisChanged') : run.error_code === 'result_limit' ? t('resultLimit') : run.error;
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
        error: v => v === null || typeof v === 'string', search_complete: v => typeof v === 'boolean', exportable: v => typeof v === 'boolean', error_code: v => v === null || v === 'analysis_changed' || v === 'result_limit', definitions: v => Array.isArray(v) && v.every(x => typeof x === 'string'),
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
const BASIC_ATTRIBUTES = ['surface', 'text', 'lemma', 'norm', 'upos', 'xpos', 'dep.head', 'dep.rel', 'unidic.cform', 'unidic.ctype', 'unidic.kana', 'unidic.pron', 'unidic.orthbase', 'unidic.lform', 'unidic.goshu', 'unidic.pos1', 'unidic.pos2', 'unidic.pos3', 'unidic.pos4', 'unidic.pronbase', 'unidic.kanabase'];
/** The token-detail rows' keys, at most 18, in reading order: the portable keys and the dependency, then UniDic's, then the token's others except raw CoNLL-U fields. A narrow panel shows the first rows only. */
export function basicAttributeKeys(attributes) {
    return [...BASIC_ATTRIBUTES.filter(key => key in attributes), ...Object.keys(attributes).filter(key => !BASIC_ATTRIBUTES.includes(key) && !key.startsWith('conllu.'))].slice(0, 18);
}
/** The offset of the line defining `name` in a catalog source (`name :=`, after an optional definition keyword), or -1. */
export function definitionOffset(source, name) {
    const escaped = name.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
    return source.search(new RegExp(`^[ \\t]*(?:(?:pattern|fragment|lexicon|set)[ \\t]+)?${escaped}[ \\t]*:=`, 'm'));
}
export function parseStoredSentence(value) {
    if (!objectValue(value) || typeof value.analysis_identity !== 'string' || !Number.isSafeInteger(value.sentence) || Number(value.sentence) < 0 || !Array.isArray(value.tokens) || !value.tokens.every(tokenValue))
        throw new Error('The stored token analysis could not be read.');
    return value;
}
/** Returns definitions with counts at least half the largest positive count. */
export function overflowingDefinitions(counts) {
    const largest = Math.max(0, ...Object.values(counts));
    return largest > 0 ? Object.keys(counts).filter(name => counts[name] * 2 >= largest) : [];
}
const shellWord = (value) => "'" + value.replaceAll("'", "'\\''") + "'";
/** Builds a command that applies the scope clause and filters JSONL records by definition and document. */
export function cliCommand(where, limit) {
    let command = 'ling --json matches --corpus CORPUS.lingcorpus --patterns query.lx --context 24';
    if (where)
        command += ' --where ' + shellWord(where);
    const tests = [
        ...(limit?.pattern ? [`.data.pattern == ${JSON.stringify(limit.pattern)}`] : []),
        ...(limit?.document != null ? [`.data.document_ordinal == ${limit.document}`] : []),
    ];
    if (tests.length)
        command += ' | jq -c ' + shellWord(`select(.type == "match" and ${tests.join(' and ')})`);
    return command;
}
const diagnosticMessages = {
    'invalid lingrep patterns syntax': {
        en: 'The pattern is not written correctly. Check that every bracket is closed and every definition ends with a period.',
        ja: 'パターンの書き方に誤りがあります。括弧が閉じているか、各定義がピリオドで終わっているかを確認してください。',
    },
};
/** Japanese for the compiler's full-width-character notes, matched on the note text the compiler writes. */
const noteMessages = [
    [/^`(.+)` \(U\+[0-9A-F]{4}\) is full-width; write the ASCII `(.)` instead$/u, found => `「${found[1]}」は全角文字です。半角の「${found[2]}」を入力してください。`],
    [/^`.+` \(U\+[0-9A-F]{4}\) is a full-width space; write an ASCII space instead$/u, () => '全角スペースではなく半角スペースで区切ってください。'],
];
function noteText(message, locale) {
    if (locale === 'ja')
        for (const [pattern, text] of noteMessages) {
            const found = message.match(pattern);
            if (found)
                return text(found);
        }
    return `${message}.`;
}
/** A catalog diagnostic as one located sentence in `locale`, followed by its notes; messages without a translation stay as the compiler wrote them. */
export function diagnosticText(diagnostic, locale) {
    const { line, column } = diagnostic.data.location ?? {};
    const message = [diagnosticMessages[diagnostic.data.message]?.[locale] ?? diagnostic.data.message, ...(diagnostic.data.related_locations ?? []).map(note => noteText(note.message, locale))].join(' ');
    if (!line)
        return message;
    const place = locale === 'ja' ? `${line} 行${column ? ` ${column} 文字目` : ''}` : `Line ${line}${column ? `, column ${column}` : ''}`;
    return `${place}: ${message}`;
}
export const LOCALE_KEY = 'lingrep-web:locale:v1';
/** The saved page language, shared with the other lingrep-web pages: that key, else this page's former `ling-questions-locale` key, else the browser language. */
export function storedLocale(storage, language) {
    for (const key of [LOCALE_KEY, 'ling-questions-locale']) {
        try {
            const stored = storage?.getItem(key);
            if (stored === 'ja' || stored === 'en')
                return stored;
        }
        catch { /* Fall through to the browser language. */ }
    }
    return language.toLowerCase().startsWith('ja') ? 'ja' : 'en';
}
/** The name a match's document is shown by: its metadata title, else its source name without a corpus file extension. */
export function documentTitle(row) {
    return row.source_metadata?.document?.title ?? row.source_name.replace(/\.(conllu|jsonl|txt|md|xml)$/i, '');
}
