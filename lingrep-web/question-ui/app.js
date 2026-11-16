import { serviceURL, ServiceError, expiredSearch } from './service.js';
import { parseSections, restoreSections } from './sections.js';
import { addCondition, patternSource } from './composer.js';
import { defaultView, parseView, locationURL, savedQuestion, seedParams } from './location.js';
import { begin, receive, receiveUpdate, visible } from './state.mjs';
import { initQuestion, questionSettings } from './document.js';
import { element as $, errorMessage, runningSnapshot, parseUpdate, parseSnapshot, parseStoredSentence, attributeMembers, sourceSelection } from './model.js';
const escape = (value) => String(value ?? '').replace(/[&<>"']/g, c => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' }[c] ?? c));
const words = (tokens) => (tokens ?? []).map(token => token.attributes.text ?? '').join('');
const count = (value) => Number(value ?? 0).toLocaleString(locale === 'ja' ? 'ja-JP' : 'en-US');
let locale = localStorage.getItem('ling-questions-locale') === 'ja' ? 'ja' : 'en';
const en = Object.fromEntries([...document.querySelectorAll('[data-t]')].map(element => [element.dataset.t, element.textContent]));
const ja = {
    readingOptions: '表示の設定', questionActions: '問いの操作', questionNav: '問い', remix: '問いを複製', saveQuestion: '問いを保存', openQuestion: '開く', searchFor: '', among: 'の', update: '検索する', done: '完了', exclude: '選んだ値を除外', previousValues: '前の値', nextValues: '次の値', clearConditions: '条件を消去', textClause: '条件式を直接編集', read: '表示する用例は', examplesWith: '件、前後の文脈は', contextTokens: 'トークンずつ', closeAnalysis: '用例を閉じる', tokenHelp: 'トークンを選ぶと、属性を確認できます。', remixTest: '条件を追加', showLatest: '新しい結果を表示', prototypeOptions: '検索の設定', searchNav: '検索', incumbentNav: '分析ツール', frequencyNav: '頻度', compareNav: '比較', stopReplay: '再生を停止', find: '', in: '', patternSource: 'パターンのソース', toSearch: 'で検索', draft: 'Alt+Enter、または欄から移動して検索します。', run: '検索する', where: '文書の条件', edited: '編集済み · 検索して適用', ready: '検索できます。', stop: '検索を停止', retry: 'もう一度検索', during: '新しい検索の実行中は', keep: '完了した結果を表示', live: '途中の結果を表示', compare: '前回と今回を並べる', replay: '検索の経過を再生', replayNote: '検索の経過を再生しています。', emptyTitle: '調べたい表現から始める。', emptyBody: 'パターンや文書の条件を変えて検索できます。新しい検索の間も、完了した結果を保持します。', example: 'このパターンを検索', boundary: '検索結果について', boundaryBody: '選択したコーパスとコンパイル済みのパターンに基づく検索結果です。用例はページごとに表示します。検索中の件数は途中経過です。原文は検索条件で絞り込まずに表示します。', return: '用例に戻る', paragraph: '段落を読む', document: '作品を読む',
    current: '今回の結果', previous: '前回の完了した結果', completed: '検索完了', partial: '検索中 · 件数は途中経過', cancelled: '停止済み · 件数は途中経過', failed: '検索に失敗', retained: '表示中の用例', filter: 'このページを絞り込む', noLines: 'このパターンの用例はまだありません。', noFilter: '絞り込みに一致する用例はありません。', download: 'JSONL で保存', source: '検索時のパターン', copy: 'CLI コマンドをコピー', copied: 'コピーしました', savedQuery: '検索時の query.lx を保存', runNumber: '検索', scope: '文書の条件', all: 'すべての文書', soFar: '件が見つかりました', running: '検索しています', notTotal: '検索中の件数は途中経過です。', stopped: '検索を停止しました。途中の件数はコーパス全体の件数ではありません。', failedKeep: '検索に失敗しました。前回の検索結果を表示しています。', errorRetry: 'パターンや条件を確認して、もう一度検索してください。', draftRunning: '新しいパターンを編集中です。実行中の検索には影響しません。', keeping: '新しい検索の間、前回の完了した結果を表示しています。', loadingRead: '原文を読み込んでいます…', readError: '原文を読み込めませんでした。もう一度開いてください。', tokenContext: 'トークンによる前後の文脈', real: '検索', replayed: '記録した検索の再生', unknownProgress: '進捗の割合は不明',
};
const extra = { readingOptions: 'Reading options', questionActions: 'Question actions', stopReplay: 'Stop replay', current: 'Current results', previous: 'Previous results', completed: 'Complete', partial: 'Searching · counts are partial', cancelled: 'Stopped · counts are partial', failed: 'Search failed', retained: 'Displayed examples', filter: 'Filter this page', noLines: 'No lines for this pattern yet.', noFilter: 'No examples on this page contain this text.', download: 'Download JSONL', source: 'Pattern used for this search', copy: 'Copy CLI command', copied: 'Copied', savedQuery: 'Download this query.lx', runNumber: 'Search', scope: 'Document condition', all: 'All documents', soFar: 'matches found', running: 'Searching', notTotal: 'Counts are partial until the search finishes.', stopped: 'Search stopped. Counts are partial.', failedKeep: 'Search failed. Previous results are retained.', errorRetry: 'Check the pattern or condition and run the search again.', draftRunning: 'Editing the next pattern. The running search is unchanged.', keeping: 'Showing previous results while searching.', loadingRead: 'Loading the original text…', readError: 'Could not load the original text. Open it again to retry.', tokenContext: 'Token context', real: 'Live search', replayed: 'Recorded search replay', unknownProgress: 'Scan percentage unavailable' };
const t = (key) => (locale === 'ja' ? ja[key] : null) ?? en[key] ?? extra[key] ?? key;
Object.assign(ja, { emptyBody: 'パターンや文書の条件を変えて検索できます。', current: '用例', previous: '前回の完了した用例', boundaryBody: '検索中の件数は途中経過です。ダウンロードには全件を含みます。原文は文書の条件で絞り込まずに表示します。', return: '用例に戻る', during: '新しい検索の実行中は', keep: '前回の検索結果を表示' });
extra.current = 'Results';
extra.previous = 'Previous results';
let config;
let controls;
const tokenLab = $('#token-lab');
const focusedSentence = document.createElement('p');
focusedSentence.id = 'focused-sentence';
focusedSentence.lang = 'ja';
focusedSentence.tabIndex = -1;
const exampleSteps = document.createElement('nav');
exampleSteps.className = 'example-steps';
exampleSteps.setAttribute('aria-label', 'Examples');
exampleSteps.innerHTML = '<button type="button" data-example-step="-1"></button><button type="button" data-example-step="1"></button>';
const tokenDetails = document.createElement('details');
tokenDetails.id = 'token-details';
tokenDetails.open = !matchMedia('(max-width:480px)').matches;
const tokenSummary = document.createElement('summary');
tokenDetails.append(tokenSummary);
for (const element of [...tokenLab.children].slice(1))
    tokenDetails.append(element);
tokenLab.append(focusedSentence, exampleSteps, tokenDetails);
function localizeExample() { tokenSummary.textContent = locale === 'ja' ? 'トークンを調べる' : 'Inspect tokens'; $('[data-example-step="-1"]', exampleSteps).textContent = locale === 'ja' ? '前の用例' : 'Previous example'; $('[data-example-step="1"]', exampleSteps).textContent = locale === 'ja' ? '次の用例' : 'Next example'; }
const readerPanel = $('#inspector');
const readerStatus = document.createElement('p');
readerStatus.className = 'reader-status';
readerStatus.setAttribute('role', 'status');
$('#document-body').before(readerStatus);
let readerContentKey = null;
const readerDisabled = new WeakMap();
let readerTarget = null;
let tokenData = null;
let tokenIndex = 0;
let attributeKey = 'lemma';
let attributeMember;
let analysisExpectation = null;
let configRefresh;
const tokenCache = new Map();
let state = { current: null, completed: null, previous: null };
let stream;
let activeId = null;
let requestEpoch = 0;
let replayEpoch = 0;
let history = [];
let recorded = [];
let inspectionEpoch = 0;
let inspectionFailure = null;
let conditions = [];
let conditionSentence = '';
let inspecting;
const selection = new Map();
const filters = new Map();
const panels = new Map();
const documentSections = new Map();
let savedManifest = [];
let restoreController = null;
const expectedAnalyses = new Map();
const unavailableSearches = new Map();
let loadingSaved = false;
function recipeFor(run) { return { source: run.source, dataset: run.dataset, where: run.where, analysis: run.contract_run?.analysis_identity ?? savedManifest.find(link => link.id === run.id)?.recipe?.analysis ?? expectedAnalyses.get(run.id) ?? config.datasets[run.dataset]?.analysis_identity ?? null }; }
function sectionLinks() {
    const links = savedManifest.map(link => { const live = documentSections.get(link.id); return live ? { ...link, recipe: recipeFor(live.run), origin: live.origin } : link; });
    for (const [id, section] of documentSections)
        if (!links.some(link => link.id === id))
            links.push({ id, origin: section.origin, recipe: recipeFor(section.run) });
    if (!links.length && state.current)
        links.push({ id: state.current.id, origin: null, recipe: recipeFor(state.current) });
    return links;
}
function availableRun(id) { return documentSections.get(id)?.run ?? [state.current, state.completed, state.previous].find(run => run?.id === id) ?? undefined; }
function saveSections() { if (restoreController)
    return; if (!documentSections.size && state.current && savedManifest.every(link => availableRun(link.id)))
    savedManifest = [{ id: state.current.id, origin: null, recipe: recipeFor(state.current) }];
else
    savedManifest = sectionLinks(); try {
    sessionStorage.setItem('ling-questions-sections', JSON.stringify(savedManifest));
}
catch { } renderSavedSearches(); }
function renderSavedSearches() {
    const missing = savedManifest.filter(link => !availableRun(link.id));
    $('#saved-searches').hidden = !missing.length && !restoreController;
    const notice = $('#saved-searches-note');
    notice.textContent = restoreError ?? (restoreController ? (locale === 'ja' ? '保存した検索を順番に実行しています。表示中の用例はそのまま残ります。' : 'Running saved searches in order. Completed results remain visible.') : (locale === 'ja' ? '検索結果の保存期間が終了しました。保存した検索条件と、検索どうしのつながりは残っています。' : 'Results expire with the server session. Saved queries and their relationships are retained.'));
    $('#saved-searches-list').replaceChildren(...missing.map(link => { const item = document.createElement('li'); const recipe = link.recipe; item.textContent = (recipe ? `${config.datasets[recipe.dataset]?.label ?? recipe.dataset} · ${recipe.source}${recipe.where ? ' · ' + recipe.where : ''}` : link.id) + ' · ' + (!recipe ? (locale === 'ja' ? '古い保存形式には検索のソースが含まれていません。' : 'This older saved search does not include its query source.') : (unavailableSearches.get(link.id)?.message ?? (locale === 'ja' ? '保存した検索' : 'Saved search'))); return item; }));
    const button = $('#restore-searches');
    button.disabled = !!restoreController || loadingSaved || !missing.some(link => link.recipe) && ![...unavailableSearches.values()].some(value => !value.expired);
    button.textContent = [...unavailableSearches.values()].some(value => !value.expired) ? (locale === 'ja' ? '保存した検索を再読み込み' : 'Retry loading saved searches') : (locale === 'ja' ? '保存した検索を実行' : 'Run saved searches');
}
let restoreError = null;
async function loadSavedSections() {
    loadingSaved = true;
    renderSavedSearches();
    try {
        for (const link of savedManifest) {
            if (availableRun(link.id))
                continue;
            try {
                const run = await readRun(link.id);
                documentSections.set(link.id, { run, origin: link.origin });
                unavailableSearches.delete(link.id);
            }
            catch (error) {
                unavailableSearches.set(link.id, { expired: expiredSearch(error), message: errorMessage(error) });
            }
        }
        const loaded = new Map(documentSections);
        documentSections.clear();
        for (const link of savedManifest) {
            const section = loaded.get(link.id);
            if (section) {
                documentSections.set(link.id, section);
                loaded.delete(link.id);
            }
        }
        for (const [id, section] of loaded)
            documentSections.set(id, section);
    }
    finally {
        loadingSaved = false;
        renderSavedSearches();
    }
}
async function executeSavedSection(link, signal) {
    if (signal.aborted)
        throw new Error('Saved-search restore stopped. Run saved searches to continue.');
    const recipe = link.recipe;
    if (!Object.hasOwn(config.datasets, recipe.dataset))
        throw new Error(`The corpus for this saved query is unavailable: ${recipe.dataset}.`);
    $('#source').value = recipe.source;
    $('#corpus').value = recipe.dataset;
    analysisExpectation = recipe.analysis;
    controls.restore(new URLSearchParams({ ...controls.entries(), metadata: '{}', raw: recipe.where }));
    updateCorpus();
    const body = { source: recipe.source, dataset: recipe.dataset, where: recipe.where, ...(recipe.analysis ? { expected_analysis_identity: recipe.analysis } : {}) };
    const { id } = await post('api/questions/runs', body, signal);
    expectedAnalyses.set(id, recipe.analysis);
    if (signal.aborted) {
        await post(`api/questions/runs/${id}/cancel`);
        throw new Error('Saved-search restore stopped. Run saved searches to continue.');
    }
    state = begin(state, runningSnapshot(id, body));
    viewEvidence = null;
    activeId = id;
    render();
    return new Promise((resolve, reject) => {
        const events = new EventSource(serviceURL(`api/questions/runs/${id}/events`));
        stream = events;
        const stop = () => { events.close(); signal.removeEventListener('abort', cancel); if (activeId === id)
            activeId = null; };
        const cancel = () => { stop(); void post(`api/questions/runs/${id}/cancel`).catch(() => { }); if (state.current?.id === id)
            state = { ...state, current: { ...state.current, phase: 'cancelled' } }; reject(new Error('Saved-search restore stopped. Run saved searches to continue.')); };
        signal.addEventListener('abort', cancel, { once: true });
        events.addEventListener('update', event => {
            if (signal.aborted || activeId !== id)
                return;
            try {
                state = receiveUpdate(state, parseUpdate(event.data));
                const run = state.current;
                render();
                if (run.phase === 'running')
                    return;
                stop();
                if (run.phase !== 'complete') {
                    reject(new Error(run.error ?? 'Saved search did not complete. Run saved searches to retry.'));
                    return;
                }
                documentSections.set(id, { run, origin: link.origin });
                sessionStorage.setItem('ling-questions-completed', id);
                sessionStorage.setItem('ling-questions-run', id);
                resolve(id);
            }
            catch (error) {
                stop();
                void post(`api/questions/runs/${id}/cancel`).catch(() => { });
                reject(error);
            }
        });
        events.onerror = () => { if (activeId === id)
            $('#status').textContent = locale === 'ja' ? '接続を再開しています。表示中の用例はそのまま残ります。' : 'Reconnecting. Completed results remain visible.'; };
    });
}
$('#restore-searches').addEventListener('click', async () => {
    if (restoreController)
        return;
    if ([...unavailableSearches.values()].some(value => !value.expired)) {
        await loadSavedSections();
        render();
        return;
    }
    if (activeId) {
        $('#saved-searches-note').textContent = locale === 'ja' ? '実行中の検索を停止してから再試行してください。' : 'Stop the current search before restoring saved searches.';
        return;
    }
    restoreError = null;
    restoreController = new AbortController();
    ++requestEpoch;
    ++navigationEpoch;
    viewLocation = defaultView();
    render();
    renderSavedSearches();
    try {
        await restoreSections(savedManifest, new Set(savedManifest.filter(link => availableRun(link.id)?.phase === 'complete').map(link => link.id)), link => executeSavedSection(link, restoreController.signal), manifest => {
            savedManifest = manifest;
            unavailableSearches.clear();
            for (const link of manifest) {
                const section = documentSections.get(link.id);
                if (section)
                    section.origin = link.origin;
            }
            try {
                sessionStorage.setItem('ling-questions-sections', JSON.stringify(savedManifest));
            }
            catch { }
            renderSavedSearches();
        });
    }
    catch (error) {
        restoreError = restoreController?.signal.aborted ? (locale === 'ja' ? '保存した検索の実行を停止しました。再実行すると続きから再開します。' : 'Saved-search restore stopped. Run saved searches to continue.') : errorMessage(error);
    }
    finally {
        restoreController = null;
        render();
        renderSavedSearches();
        updateDraft();
    }
});
const documentViews = new Map();
let isReplay = false;
let viewLocation = defaultView();
let viewEvidence = null;
let navigationEpoch = 0;
let pendingView = null;
let revealedFailure = null;
const editError = document.createElement('button');
editError.type = 'button';
editError.id = 'edit-pattern-error';
editError.hidden = true;
$('#error').after(editError);
const patternFailure = (run) => run?.phase === 'failed' ? run.diagnostics?.find(d => d.data.stage === 'catalog' && d.data.severity === 'error') : undefined;
function editFailedPattern(focus = true) {
    const run = state.current, diagnostic = patternFailure(run);
    if (!run || !diagnostic)
        return;
    $('#source-editor').hidden = false;
    $('#edit-source').setAttribute('aria-expanded', 'true');
    const input = $('#source');
    if (focus) {
        input.focus();
        const span = diagnostic.data.location?.source_span;
        const range = span && input.value === run.source ? sourceSelection(run.source, span) : null;
        if (range)
            input.setSelectionRange(...range);
    }
}
editError.addEventListener('click', () => editFailedPattern());
const compactLayout = matchMedia('(max-width:480px)');
function adaptControls() { for (const selector of ['#question-actions'])
    for (const details of document.querySelectorAll(selector))
        details.open = !compactLayout.matches; }
adaptControls();
compactLayout.addEventListener('change', adaptControls);
async function request(path, options) {
    const response = await fetch(serviceURL(path), options);
    const result = await response.json();
    if (!response.ok)
        throw new ServiceError(response.status, result.error ?? `HTTP ${response.status}`);
    return result;
}
const post = (path, body = {}, signal) => request(path, { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify(body), signal });
function translate() {
    document.documentElement.lang = locale;
    localizeExample();
    for (const element of document.querySelectorAll('[data-t]'))
        element.textContent = t(element.dataset.t ?? '');
    $('#language').textContent = locale === 'en' ? '日本語' : 'English';
    $('.sentence-period').textContent = locale === 'ja' ? 'を探す。' : '.';
    $('#corpus-join').textContent = locale === 'ja' ? '' : ' in ';
    $('#corpus-comma').textContent = '';
    $('#scope-end').textContent = locale === 'ja' ? 'から ' : '';
    $('.question-sentence').insertBefore($('#edit-source'), locale === 'ja' ? $('.sentence-period') : $('#corpus-join'));
    for (const button of document.querySelectorAll('[data-reader-document]'))
        button.textContent = Number(button.dataset.readerDocument) < 0 ? (locale === 'ja' ? '前の作品' : 'Previous document') : (locale === 'ja' ? '次の作品' : 'Next document');
    controls?.localize();
    $('#where').placeholder = locale === 'ja' ? 'すべての文書 · 必要なら --where 条件を入力' : 'All documents · optional --where clause';
    $('#corpus').setAttribute('aria-label', locale === 'ja' ? 'コーパス' : 'Corpus');
    $('#metadata-field').setAttribute('aria-label', locale === 'ja' ? 'メタデータの項目' : 'Metadata field');
    $('#results').setAttribute('aria-label', locale === 'ja' ? '検索結果' : 'Search results');
    $('.search-status').setAttribute('aria-label', locale === 'ja' ? '今回の検索' : 'Current search');
    updateCorpus();
    updateDraft();
    render(true);
}
function updateCorpus() {
    const dataset = config?.datasets[$('#corpus').value];
    if (!dataset)
        return;
    const facts = [];
    if (dataset.documents != null)
        facts.push(`${count(dataset.documents)} ${locale === 'ja' ? '作品' : 'documents'}`);
    if (dataset.sentences != null)
        facts.push(`${count(dataset.sentences)} ${locale === 'ja' ? '文' : 'sentences'}`);
    if (dataset.readiness !== 'ready')
        facts.push(locale === 'ja' ? (dataset.readiness === 'failed' ? 'コーパスの準備に失敗しました。検索で再試行できます。' : 'コーパスを準備しています。検索すると準備の完了を待ちます。') : (dataset.readiness === 'failed' ? 'Corpus preparation failed. Search to retry.' : 'Corpus preparing · searches wait until it is ready.'));
    $('#corpus-meta').textContent = dataset.readiness === 'ready' ? '' : facts.join(' · ');
    $('#corpus').title = facts.join(' · ');
    $('#incumbent').href = serviceURL('workbench');
    clearTimeout(configRefresh);
    if (dataset.readiness !== 'ready')
        configRefresh = setTimeout(() => void refreshConfiguration(), 1500);
}
async function refreshConfiguration() {
    try {
        config = await request('api/questions/config');
    }
    catch { }
    updateCorpus();
}
function updateDraft(commit = true) {
    const source = $('#source').value;
    const names = state.current?.source === source ? state.current.definitions : [];
    $('#source-label').textContent = names?.length === 1 ? names[0] : (locale === 'ja' ? 'パターンのソース' : 'pattern source');
    const dirty = state.current && (source !== state.current.source || (controls?.where() ?? $('#where').value) !== state.current.where || $('#corpus').value !== state.current.dataset);
    $('#draft-badge').hidden = !dirty;
    if (!config || !controls)
        return;
    const evidence = displayedEvidence();
    const params = { ...questionDocument(), behavior: $('#behavior').value };
    delete params.format;
    delete params.question;
    delete params.sections;
    delete params.where;
    delete params.evidence;
    delete params.view;
    if (evidence && source === evidence.source)
        delete params.source;
    if (evidence && $('#corpus').value === evidence.dataset) {
        delete params.corpus;
        delete params.analysis;
    }
    params.follow = !viewEvidence && !inspecting ? '1' : '0';
    if (activeId) {
        params.attempt = activeId;
        if (state.completed)
            params.completed = state.completed.id;
    }
    const url = locationURL(location.href, params, evidence?.id ?? null, viewLocation);
    if (commit)
        window.history.replaceState(null, '', url);
}
function saveReplay(final) {
    const frames = history.map(({ id, phase, received, counts, elapsed, version, definitions }) => ({ id, phase, received, counts, elapsed, version, definitions }));
    try {
        sessionStorage.setItem('ling-questions-replay', JSON.stringify({ final, frames }));
    }
    catch { }
}
function restoreReplay(final) {
    try {
        const saved = JSON.parse(sessionStorage.getItem('ling-questions-replay') ?? 'null');
        if (saved?.final.id !== final.id)
            return [];
        return saved.frames.map(frame => ({ ...final, ...frame, rows: Object.fromEntries(Object.entries(final.rows).map(([name, rows]) => [name, rows.slice(0, frame.counts[name] ?? 0)])) }));
    }
    catch {
        return [];
    }
}
let sourceAtFocus = '';
async function search(event, derivedFrom) {
    event?.preventDefault();
    if (restoreController)
        return;
    const parent = displayedEvidence();
    const origin = derivedFrom ?? (documentSections.size && parent ? { parent: parent.id, view: viewLocation, label: locale === 'ja' ? '前の検索' : 'previous search' } : undefined);
    if (origin && parent && !documentSections.has(parent.id)) {
        documentSections.set(parent.id, { run: parent, origin: null });
        saveSections();
    }
    const epoch = ++requestEpoch;
    viewEvidence = null;
    pendingView = null;
    ++navigationEpoch;
    const body = { source: $('#source').value, dataset: $('#corpus').value, where: controls?.where() ?? $('#where').value, ...(analysisExpectation ? { expected_analysis_identity: analysisExpectation } : {}) };
    sourceAtFocus = body.source;
    ++replayEpoch;
    isReplay = false;
    $('#replay-note').hidden = true;
    $('#run').disabled = true;
    const prior = activeId;
    stream?.close();
    activeId = null;
    try {
        if (prior) {
            await post(`api/questions/runs/${prior}/cancel`);
            const section = documentSections.get(prior);
            if (section?.run.phase === 'running')
                section.run = { ...section.run, phase: 'cancelled' };
        }
        const result = await post('api/questions/runs', body);
        if (epoch !== requestEpoch) {
            await post(`api/questions/runs/${result.id}/cancel`);
            return;
        }
        activeId = result.id;
        expectedAnalyses.set(result.id, analysisExpectation);
        state = begin(state, runningSnapshot(result.id, body));
        if (origin && parent) {
            documentSections.set(result.id, { run: state.current, origin });
            saveSections();
        }
        history = [];
        sessionStorage.setItem('ling-questions-run', result.id);
        render(true);
        updateDraft();
        connect(result.id);
    }
    catch (error) {
        if (epoch !== requestEpoch)
            return;
        state = begin(state, runningSnapshot(`request-${epoch}`, body));
        state = { ...state, current: { ...runningSnapshot(`request-${epoch}`, body), phase: 'failed', error: errorMessage(error) } };
        render(true);
    }
    finally {
        if (epoch === requestEpoch)
            $('#run').disabled = false;
    }
}
function connect(id) {
    stream = new EventSource(serviceURL(`api/questions/runs/${id}/events`));
    stream.addEventListener('update', event => {
        if (id !== activeId)
            return;
        try {
            state = receiveUpdate(state, parseUpdate(event.data));
        }
        catch (error) {
            const failed = { ...state.current, phase: 'failed', error: locale === 'ja' ? '検索の更新を読み込めませんでした。もう一度検索してください。' : 'Search updates could not be read. Run the question again.' };
            state = receive(state, failed);
            stream?.close();
            activeId = null;
            post(`api/questions/runs/${id}/cancel`).catch(() => { });
            render(true);
            return;
        }
        const snapshot = state.current;
        if (viewEvidence?.id === snapshot.id)
            viewEvidence = snapshot;
        if (snapshot.phase !== 'running' || (history.length < 239 && (!history.length || snapshot.received !== history.at(-1).received)))
            history.push(snapshot);
        if (snapshot.phase !== 'running') {
            stream?.close();
            activeId = null;
            if (snapshot.phase === 'complete') {
                recorded = history;
                saveReplay(snapshot);
                if (pendingView) {
                    const target = pendingView;
                    pendingView = null;
                    void navigateView(hrefFor(snapshot, target), true, 'replace');
                }
                if (!state.previous && !$('#source-editor').contains(document.activeElement)) {
                    $('#source-editor').hidden = true;
                    $('#edit-source').setAttribute('aria-expanded', 'false');
                }
                sessionStorage.setItem('ling-questions-completed', snapshot.id);
                saveSections();
                void refreshConfiguration();
            }
        }
        render();
        updateDraft();
    });
    stream.onerror = () => {
        if (id !== activeId)
            return;
        $('#status').textContent = locale === 'ja' ? '接続を再開しています。結果はそのまま保持します。' : 'Reconnecting. Displayed results are retained.';
    };
}
function render(force = false) {
    if (!config || !controls)
        return;
    updateDraft(false);
    const current = state.current;
    if (current && documentSections.has(current.id))
        documentSections.get(current.id).run = current;
    $('#run').hidden = !!current;
    $('#timeline').max = String(Math.max(0, recorded.length - 1));
    if (!isReplay)
        $('#timeline').value = String(Math.max(0, recorded.length - 1));
    $('#cancel').hidden = !activeId && !isReplay && !restoreController;
    $('#cancel').textContent = restoreController ? (locale === 'ja' ? '保存した検索の実行を停止' : 'Stop saved searches') : t(isReplay ? 'stopReplay' : 'stop');
    $('#retry').hidden = !current || current.phase === 'running' || current.phase === 'complete';
    $('#replay').disabled = !recorded.length || !!activeId || isReplay;
    $('#error').hidden = !current?.error;
    const diagnostic = patternFailure(current), line = diagnostic?.data.location?.line;
    $('#error').textContent = (line ? (locale === 'ja' ? `${line} 行目: ` : `Line ${line}: `) : '') + (current?.error ?? '');
    editError.hidden = !diagnostic;
    editError.textContent = line ? (locale === 'ja' ? `${line} 行目のパターンを修正` : `Edit pattern at line ${line}`) : (locale === 'ja' ? 'パターンを修正' : 'Edit pattern');
    if (diagnostic && current && revealedFailure !== current.id) {
        revealedFailure = current.id;
        editFailedPattern($('#source').value === current.source);
    }
    $('#status-dot').className = 'status-dot ' + (current?.phase ?? '');
    if (current) {
        const prefix = isReplay ? t('replayed') : t('real');
        const elapsed = current.elapsed > 0 ? ` · ${Number(current.elapsed).toFixed(1)}s` : '';
        $('#status').textContent = current.phase === 'running' ? `${prefix} · ${t('running')} · ${count(current.received)} ${t('soFar')}${elapsed}` : current.phase === 'complete' ? `${isReplay ? t('replayed') + ' · ' : ''}${t('completed')}` : current.phase === 'cancelled' ? t('stopped') : state.previous ? t('failedKeep') : t('errorRetry');
        $('#live-counts').hidden = current.phase !== 'running';
        $('#live-counts').innerHTML = Object.entries(current.counts).map(([name, total]) => `<span>${escape(name)} <strong>${count(total)}</strong>${current.phase === 'running' ? ` <span>${locale === 'ja' ? '件（途中）' : 'so far'}</span>` : ''}</span>`).join('');
    }
    else
        $('#status').textContent = t('ready');
    const behavior = $('#behavior').value;
    const pinned = !documentSections.size && (!!viewEvidence && current?.phase === 'complete' && viewEvidence.id !== current.id || behavior === 'keep' && inspecting && tokenLab.isConnected && !tokenLab.hidden && state.current?.phase === 'complete' && inspecting.run.id !== state.current.id);
    $('#show-latest').hidden = !pinned;
    const earlier = isReplay ? state.previous : viewEvidence ?? state.previous;
    const compared = !documentSections.size && behavior === 'compare' && earlier && earlier.id !== current?.id;
    let visibleRuns = isReplay ? [current] : compared ? [earlier, current] : [viewEvidence ?? (pinned && inspecting ? inspecting.run : visible(state, behavior))];
    if (!isReplay && behavior === 'keep' && !viewEvidence && current?.phase === 'complete' && state.previous && state.previous.id !== current.id) {
        const pattern = selection.get(current.id) ?? current.definitions[0] ?? '', page = documentView(current, pattern);
        if (page.documents === null && !page.error) {
            visibleRuns = [state.previous];
            $('#status').textContent = locale === 'ja' ? '検索完了 · 新しい用例を準備しています…' : 'Search complete · preparing the new examples…';
            if (!page.loading)
                void loadDocumentPage(null, current, pattern);
        }
    }
    if (current?.phase === 'complete')
        $('#show-latest').setAttribute('href', hrefFor(current, defaultView()));
    const shown = [...new Map([...documentSections.values()].map(section => section.run).concat(visibleRuns.filter((run) => run != null)).map(run => [run.id, run])).values()];
    $('#empty').hidden = shown.length > 0;
    $('#result-panels').className = compared ? 'comparing' : '';
    const ids = shown.map(run => run.id);
    for (const [id, panel] of panels) {
        if (!ids.includes(id)) {
            if (panel.contains(readerPanel))
                document.body.append(readerPanel);
            if (panel.contains(tokenLab))
                document.body.append(tokenLab);
            panel.remove();
            panels.delete(id);
        }
    }
    for (const run of shown) {
        let panel = panels.get(run.id);
        if (!panel) {
            panel = document.createElement('article');
            panel.className = 'result-panel';
            panel.dataset.run = run.id;
            panel.innerHTML = '<p class="result-scope"></p><ul class="result-notices" hidden></ul><div class="panel-body"><aside class="pattern-list" aria-label="Patterns"></aside><div class="lines-area"><div class="result-reading"><span class="evidence-sentence"></span> <span class="document-scope" hidden></span> <span class="line-controls"><span></span> <input type="search"></span> <details class="document-nav" aria-label="Documents" hidden><summary class="document-jump"></summary><div class="document-finder" hidden></div><div class="document-choice"></div></details> <span class="result-header"></span></div><div class="kwic-list"></div><nav class="document-pages" aria-label="Example pages" hidden></nav><p class="line-note"></p></div></div><details class="query-tools-detail"><summary></summary><pre></pre><div class="query-tools"><button type="button" data-copy></button><button type="button" data-source></button></div></details>';
            $('.document-nav', panel).open = false;
            $('.line-controls input', panel).addEventListener('input', e => { filters.set(run.id, e.target.value); viewLocation = concordanceFor(runFor(run.id)); renderLines(panel, runFor(run.id), true); updateDraft(); });
            $('[data-copy]', panel).addEventListener('click', async (e) => {
                const model = runFor(run.id);
                const shell = (value) => "'" + value.replaceAll("'", "'\\''") + "'";
                let command = 'ling --json matches --corpus CORPUS.lingcorpus --patterns query.lx --context 24';
                if (model.where)
                    command += ' --where ' + shell(model.where);
                try {
                    await navigator.clipboard.writeText(command);
                    e.target.textContent = t('copied');
                }
                catch {
                    window.prompt(t('copy'), command);
                }
            });
            $('[data-source]', panel).addEventListener('click', () => downloadSource(runFor(run.id)));
            panels.set(run.id, panel);
        }
        panel.id = `search-${run.id}`;
        const section = documentSections.get(run.id);
        if (section?.origin) {
            let landmark = panel.querySelector('.section-origin');
            if (!landmark) {
                landmark = document.createElement('p');
                landmark.className = 'section-origin';
                panel.prepend(landmark);
            }
            const parent = documentSections.get(section.origin.parent)?.run;
            const label = `${locale === 'ja' ? '派生元' : 'From'} ${escape(section.origin.label)}`;
            const markup = `${parent ? `<a data-view data-section-origin href="${escape(hrefFor(parent, section.origin.view))}">${label}</a>` : `<span>${label} · ${locale === 'ja' ? '結果は未読込' : 'results unavailable'}</span>`} · <code>${escape(run.source)}</code>${parent && parent.where !== run.where ? ` · ${escape(run.where || t('all'))}` : ''}`;
            if (renderSignatures.get(landmark) !== markup) {
                landmark.innerHTML = markup;
                renderSignatures.set(landmark, markup);
            }
        }
        const root = $('#result-panels');
        const index = shown.indexOf(run);
        if (root.children[index] !== panel)
            root.insertBefore(panel, root.children[index] ?? null);
        const prior = run.id !== current?.id || (run === state.previous && current?.phase === 'running');
        const title = prior ? t('previous') : t('current');
        const phase = run.phase === 'complete' ? t('completed') : run.phase === 'running' ? t('partial') : run.phase === 'cancelled' ? t('cancelled') : t('failed');
        const scope = `${config.datasets[run.dataset]?.label ?? ''} · ${run.where || t('all')}`;
        const identity = `${t('runNumber')} ${run.id} · ${phase} · ${scope}`;
        const headerHTML = `${prior || compared ? `<span class="evidence-context">${title}${prior && current?.phase === 'running' ? ' · ' + t('keeping') : ''}</span>` : ''}${run.phase === 'complete' ? `<a href="api/questions/runs/${run.id}/export" aria-label="${locale === 'ja' ? '全結果を JSONL で保存' : 'Download all results as JSONL'}" title="${escape(identity)}">${t('download')}</a>` : ''}`;
        const header = $('.result-header', panel);
        header.hidden = !headerHTML;
        header.title = identity;
        if (renderSignatures.get(header) !== headerHTML) {
            header.innerHTML = headerHTML;
            renderSignatures.set(header, headerHTML);
        }
        const scopeRow = $('.result-scope', panel);
        scopeRow.textContent = '';
        scopeRow.hidden = true;
        const notices = $('.result-notices', panel), noticeSignature = JSON.stringify(run.notices ?? []);
        notices.hidden = !run.notices?.length;
        if (renderSignatures.get(notices) !== noticeSignature) {
            notices.replaceChildren(...(run.notices ?? []).map(text => { const item = document.createElement('li'); item.textContent = text; return item; }));
            renderSignatures.set(notices, noticeSignature);
        }
        const definitionNames = run.definitions.length ? run.definitions : Object.keys(run.counts);
        let chosen = selection.get(run.id) ?? '';
        if (!definitionNames.includes(chosen))
            chosen = definitionNames[0] ?? '';
        selection.set(run.id, chosen);
        const rail = $('.pattern-list', panel);
        rail.hidden = definitionNames.length < 2;
        if (renderSignatures.get(rail) !== JSON.stringify(definitionNames)) {
            rail.innerHTML = definitionNames.map(name => `<button class="pattern-button" type="button" data-pattern="${escape(name)}"><span>${escape(name)}</span><span class="count"></span></button>`).join('');
            renderSignatures.set(rail, JSON.stringify(definitionNames));
            for (const button of rail.querySelectorAll('button'))
                button.addEventListener('click', () => { selection.set(run.id, button.dataset.pattern ?? ''); void navigateView(hrefFor(run, { ...defaultView(), pattern: button.dataset.pattern ?? '' }), true); });
        }
        for (const button of rail.querySelectorAll('button')) {
            button.setAttribute('aria-pressed', String(button.dataset.pattern === chosen));
            $('.count', button).textContent = count(run.counts[button.dataset.pattern]);
        }
        $('.query-tools-detail summary', panel).textContent = t('source');
        $('.query-tools-detail pre', panel).textContent = run.source;
        $('[data-copy]', panel).textContent = t('copy');
        $('[data-source]', panel).textContent = t('savedQuery');
        const input = $('.line-controls input', panel);
        input.placeholder = t('filter');
        input.setAttribute('aria-label', t('filter'));
        renderLines(panel, run, force);
    }
    compactCompletedStatus();
}
function compactCompletedStatus() {
    const current = state.current, pattern = current ? (selection.get(current.id) ?? current.definitions[0] ?? '') : '';
    const ready = current?.phase === 'complete' && documentView(current, pattern).documents !== null;
    $('.search-status').classList.toggle('is-idle', !!ready && !activeId && !isReplay && $('#show-latest').hidden === true);
}
const runFor = (id) => { const run = [state.current, state.previous, state.completed, inspecting?.run, viewEvidence, documentSections.get(id)?.run].find(run => run?.id === id); if (!run)
    throw new Error('Search is no longer displayed.'); return run; };
function documentView(run, pattern) {
    const key = `${run.id}:${pattern}`;
    if (!documentViews.has(key))
        documentViews.set(key, { request: null, scopeDocument: null, returnAfter: -1, landmarks: [], start: 0, matchesTotal: 0, query: "", documentsTotal: 0, documentsOffset: 0, selectedDoc: null, position: null, previousDoc: null, nextDoc: null, documents: null, document: null, rows: [], after: -1, next: null, previousAfter: null, loading: false, error: null, epoch: 0, limit: 0 });
    return documentViews.get(key);
}
async function loadDocumentPage(panel, run, pattern, document = null, after = -1, documentsOffset = null, scopeDocument = documentView(run, pattern).scopeDocument) {
    const view = documentView(run, pattern), epoch = ++view.epoch, limit = controls.state.lines;
    view.request = { document, after, documentsOffset, scopeDocument };
    view.loading = true;
    view.error = null;
    if (panel)
        renderLines(panel, run);
    const query = new URLSearchParams({ pattern, limit: String(limit), q: view.query, continuous: scopeDocument == null ? '1' : '0', after: String(after) });
    const selected = scopeDocument ?? document;
    if (selected != null)
        query.set('document', String(selected));
    if (documentsOffset != null)
        query.set('documents_offset', String(documentsOffset));
    try {
        const data = await request(`api/questions/runs/${run.id}/documents?${query}`);
        if (epoch !== view.epoch)
            return;
        Object.assign(view, { request: null, scopeDocument, limit, after: data.after, document: data.document, start: data.start, matchesTotal: data.matches_total, previousAfter: data.previous, documentsTotal: data.documents_total, documentsOffset: data.documents_offset, selectedDoc: data.selected, position: data.position, previousDoc: data.previous_document, nextDoc: data.next_document, documents: data.documents, landmarks: data.landmarks, rows: data.rows, next: data.next, loading: false });
    }
    catch (error) {
        if (epoch !== view.epoch)
            return;
        view.loading = false;
        view.error = errorMessage(error);
    }
    if (panel?.isConnected) {
        renderLines(panel, runFor(run.id));
        compactCompletedStatus();
    }
    else
        render();
}
function retryDocumentPage(panel, run, pattern, view) {
    const target = view.request ?? { document: view.document, after: view.after, documentsOffset: null, scopeDocument: view.scopeDocument };
    return loadDocumentPage(panel, run, pattern, target.document, target.after, target.documentsOffset, target.scopeDocument);
}
function renderDocumentNavigation(panel, run, pattern, view) {
    const wrapper = $('.document-nav', panel), nav = $('.document-choice', wrapper), finder = $('.document-finder', wrapper), paging = $('.document-pages', panel);
    wrapper.hidden = isReplay || run.phase !== 'complete' || (view.documents !== null && view.documentsTotal <= 1 && !view.query && !view.error);
    paging.hidden = true;
    if (isReplay || run.phase !== 'complete')
        return;
    if (!finder.children.length) {
        finder.innerHTML = '<input type="search"><button type="button" data-titles="-1"></button><button type="button" data-titles="1"></button><span></span>';
        let timer;
        $('input', finder).addEventListener('input', event => { clearTimeout(timer); const query = event.target.value; timer = setTimeout(() => { view.query = query; loadDocumentPage(panel, run, pattern, view.document, -1, 0); }, 150); });
        for (const button of finder.querySelectorAll('[data-titles]'))
            button.addEventListener('click', () => loadDocumentPage(panel, run, pattern, view.document, view.after, view.documentsOffset + Number(button.dataset.titles) * 100));
    }
    finder.hidden = view.documentsTotal <= 100 && !view.query;
    const searchInput = $('input', finder);
    searchInput.placeholder = locale === 'ja' ? '作品名・著者を検索' : 'Find a document or author';
    searchInput.setAttribute('aria-label', searchInput.placeholder);
    $('[data-titles="-1"]', finder).textContent = locale === 'ja' ? '前の作品一覧' : 'Previous titles';
    $('[data-titles="1"]', finder).textContent = locale === 'ja' ? '次の作品一覧' : 'Next titles';
    $('[data-titles="-1"]', finder).disabled = view.documentsOffset === 0 || view.loading;
    $('[data-titles="1"]', finder).disabled = view.documentsOffset + 100 >= view.documentsTotal || view.loading;
    $('span', finder).textContent = `${count(view.documentsTotal ? view.documentsOffset + 1 : 0)}–${count(Math.min(view.documentsOffset + 100, view.documentsTotal))} / ${count(view.documentsTotal)}`;
    $('.document-jump', wrapper).textContent = locale === 'ja' ? `作品へ移動 · ${count(view.documentsTotal)} 作品` : `Jump to document · ${count(view.documentsTotal)} works`;
    const documents = view.documents ?? [];
    const options = view.selectedDoc && !documents.some(d => d.ordinal === view.selectedDoc.ordinal) ? [view.selectedDoc, ...documents] : documents;
    if (!nav.children.length) {
        nav.innerHTML = '<select></select><a data-view data-document-step="-1"></a><a data-view data-document-step="1"></a><span role="status"></span><button type="button" data-document-retry hidden></button>';
        $('select', nav).addEventListener('change', event => {
            const value = event.target.value, document = value === '' ? null : Number(value);
            void navigateView(hrefFor(run, { ...concordanceFor(run), document, scopeDocument: view.scopeDocument != null ? document : null, after: -1 }), true);
        });
        $('[data-document-retry]', nav).addEventListener('click', () => retryDocumentPage(panel, run, pattern, view));
    }
    const select = $('select', nav), optionSignature = JSON.stringify([options, locale]);
    if (renderSignatures.get(select) !== optionSignature) {
        select.innerHTML = `<option value="">${locale === 'ja' ? 'すべての作品' : 'All documents'}</option>${options.map(d => `<option value="${d.ordinal}">${escape(d.title)}${d.author ? ' · ' + escape(d.author) : ''} (${count(d.matches)})</option>`).join('')}`;
        renderSignatures.set(select, optionSignature);
    }
    select.setAttribute('aria-label', locale === 'ja' ? '作品へ移動' : 'Jump to document');
    select.value = String(view.document ?? '');
    for (const link of nav.querySelectorAll('[data-document-step]')) {
        const direction = Number(link.dataset.documentStep), target = direction < 0 ? view.previousDoc : view.nextDoc;
        link.hidden = view.scopeDocument == null;
        link.textContent = locale === 'ja' ? (direction < 0 ? '前の作品' : '次の作品') : (direction < 0 ? 'Previous document' : 'Next document');
        updateNavigationLink(link, target == null ? null : hrefFor(run, { ...concordanceFor(run), scopeDocument: target, document: target, after: -1 }), view.loading);
    }
    $('span', nav).textContent = view.error ?? (view.loading ? (locale === 'ja' ? '読み込み中…' : 'Loading…') : view.position != null ? `${view.position + 1} / ${count(view.documentsTotal)}` : `${count(view.documentsTotal)} ${locale === 'ja' ? '作品に用例' : 'matching documents'}`);
    const retry = $('[data-document-retry]', nav);
    retry.hidden = !view.error;
    retry.textContent = locale === 'ja' ? 'もう一度読み込む' : 'Retry loading documents';
    if (!paging.children.length) {
        paging.innerHTML = '<a data-view data-page-step="-1"></a><a data-view data-page-step="1"></a><button type="button" data-page-retry hidden></button>';
        $('[data-page-retry]', paging).addEventListener('click', () => retryDocumentPage(panel, run, pattern, view));
    }
    paging.hidden = !view.error && view.previousAfter == null && view.next == null;
    for (const link of paging.querySelectorAll('[data-page-step]')) {
        const direction = Number(link.dataset.pageStep), after = direction < 0 ? view.previousAfter : view.next;
        link.textContent = locale === 'ja' ? (direction < 0 ? '前の用例' : '次の用例') : (direction < 0 ? 'Previous examples' : 'Next examples');
        updateNavigationLink(link, after == null ? null : hrefFor(run, { ...concordanceFor(run), after }), view.loading);
    }
    const pageRetry = $('[data-page-retry]', paging);
    pageRetry.hidden = !view.error;
    pageRetry.textContent = locale === 'ja' ? '用例を再読み込み' : 'Retry loading examples';
}
function updateNavigationLink(link, href, busy) {
    if (href) {
        if (link.getAttribute('href') !== href)
            link.setAttribute('href', href);
    }
    else
        link.removeAttribute('href');
    const disabled = href == null || busy;
    link.setAttribute('aria-disabled', String(disabled));
    link.tabIndex = disabled ? -1 : 0;
}
let readerNavigationEpoch = 0;
function updateReaderNavigation() {
    const inspection = readerTarget ?? inspecting, epoch = ++readerNavigationEpoch;
    const nav = $('.reader-document-nav', readerPanel);
    const links = [...nav.querySelectorAll('[data-reader-document]')];
    nav.querySelector('[data-reader-retry]')?.remove();
    for (const link of links) {
        link.setAttribute('aria-disabled', 'true');
        link.removeAttribute('href');
    }
    if (!inspection || inspection.run.phase !== 'complete')
        return;
    const { run, row } = inspection;
    nav.setAttribute('aria-busy', 'true');
    void request(`api/questions/runs/${run.id}/documents?${new URLSearchParams({ pattern: row.pattern, document: String(row.document_ordinal), limit: '1' })}`).then(page => {
        if ((readerTarget ?? inspecting) !== inspection || epoch !== readerNavigationEpoch)
            return;
        nav.setAttribute('aria-busy', 'false');
        for (const link of links) {
            const target = Number(link.dataset.readerDocument) < 0 ? page.previous_document : page.next_document;
            link.setAttribute('aria-disabled', String(target == null));
            if (target != null)
                link.setAttribute('href', hrefFor(run, { kind: 'reader', pattern: row.pattern, document: target, sequence: null, returnTo: viewLocation.kind === 'reader' ? viewLocation.returnTo : { kind: 'example', pattern: row.pattern, document: row.document_ordinal, sequence: row.sequence, returnTo: concordanceFor(run) } }));
        }
    }).catch(error => {
        if ((readerTarget ?? inspecting) !== inspection || epoch !== readerNavigationEpoch)
            return;
        nav.setAttribute('aria-busy', 'false');
        const retry = document.createElement('button');
        retry.type = 'button';
        retry.dataset.readerRetry = '';
        retry.textContent = locale === 'ja' ? '作品の移動を再読み込み' : 'Retry document navigation';
        retry.title = errorMessage(error);
        retry.addEventListener('click', updateReaderNavigation);
        nav.append(retry);
    });
}
const renderSignatures = new WeakMap();
const concordanceNodes = new WeakMap();
function renderLines(panel, run, _force = false) {
    if (!run)
        return;
    const pattern = selection.get(run.id) ?? '', view = documentView(run, pattern);
    if (!isReplay && run.phase === 'complete' && !view.loading && !view.error && (view.documents === null || view.limit !== controls.state.lines)) {
        void loadDocumentPage(panel, run, pattern, view.document, view.after);
        return;
    }
    renderDocumentNavigation(panel, run, pattern, view);
    const pageReady = !isReplay && run.phase === 'complete' && view.documents !== null;
    const prefixVisible = !view.request || view.request.after === -1 && view.request.document == null && view.request.scopeDocument == null;
    const allRows = pageReady ? view.rows : prefixVisible ? (run.rows[pattern] ?? []) : [];
    const filter = filters.get(run.id) ?? '', limit = controls.state.lines, context = controls.state.context;
    const rows = allRows.filter(row => (words(row.left_context) + row.surface + words(row.right_context)).includes(filter)).slice(0, limit);
    const total = pageReady ? view.matchesTotal : run.counts[pattern] ?? 0;
    $('.line-controls span', panel).textContent = run.phase === 'complete' && !isReplay
        ? filter ? (locale === 'ja' ? `このページで ${count(rows.length)} / ${count(allRows.length)} 件 · 全 ${count(total)} 件` : `${count(rows.length)} of ${count(allRows.length)} on this page · ${count(total)} total matches`)
            : (locale === 'ja' ? `${count(rows.length ? view.start + 1 : 0)}–${count(view.start + rows.length)} 件を表示。` : `Reading ${count(rows.length ? view.start + 1 : 0)}–${count(view.start + rows.length)}.`)
        : locale === 'ja' ? `${count(total)} 件が見つかりました · ${count(rows.length)} 件を表示` : `${count(rows.length)} shown · ${count(total)} found`;
    if (view.loading && !rows.length)
        $('.line-controls span', panel).textContent = locale === 'ja' ? `全 ${count(total)} 件` : `${count(total)} total matches`;
    const matchingDocuments = pageReady ? (view.scopeDocument != null ? (total ? 1 : 0) : view.query ? null : view.documentsTotal) : null;
    $('.evidence-sentence', panel).textContent = locale === 'ja' ? `${count(total)} 件${matchingDocuments == null ? '' : ` · ${count(matchingDocuments)} 作品`}。` : `${count(total)} ${total === 1 ? 'match' : 'matches'}${matchingDocuments == null ? '' : ` in ${count(matchingDocuments)} ${matchingDocuments === 1 ? 'document' : 'documents'}`}.`;
    const scope = $('.document-scope', panel);
    scope.hidden = view.scopeDocument == null;
    if (!scope.hidden) {
        const markup = `<a data-view href="${escape(hrefFor(run, { ...concordanceFor(run), scopeDocument: null, document: null, after: view.returnAfter }))}">${locale === 'ja' ? 'すべての作品に戻る' : 'Return to all documents'}</a>`;
        if (renderSignatures.get(scope) !== markup) {
            scope.innerHTML = markup;
            renderSignatures.set(scope, markup);
        }
    }
    const note = $('.line-note', panel);
    note.setAttribute('role', 'status');
    note.textContent = view.loading ? (locale === 'ja' ? '用例を読み込んでいます…' : 'Loading examples…') : view.error ? (locale === 'ja' ? '用例を読み込めませんでした。再読み込みできます。' : 'Examples could not be loaded. Try loading them again.') : run.phase === 'complete' ? '' : t('notTotal');
    note.hidden = !note.textContent;
    const list = $('.kwic-list', panel);
    list.setAttribute('aria-busy', String(view.loading));
    let nodes = concordanceNodes.get(list);
    if (!nodes) {
        nodes = new Map();
        concordanceNodes.set(list, nodes);
    }
    const desired = [], used = new Set();
    const node = (key, tag, className) => { used.add(key); let value = nodes.get(key); if (!value) {
        value = document.createElement(tag);
        value.className = className;
        nodes.set(key, value);
    } return value; };
    let previousDocument;
    for (const row of rows) {
        if (previousDocument !== row.document_ordinal) {
            const heading = node(`document:${row.document_ordinal}`, 'div', 'document-heading');
            const landmark = view.landmarks.find(d => d.ordinal === row.document_ordinal);
            const shown = rows.filter(r => r.document_ordinal === row.document_ordinal).length;
            const href = hrefFor(run, { ...concordanceFor(run), scopeDocument: row.document_ordinal, returnAfter: view.scopeDocument == null ? view.after : view.returnAfter, document: row.document_ordinal, after: -1 });
            const markup = `<a data-view href="${escape(href)}">${escape(row.source_metadata?.document?.title ?? row.source_name)}</a>${row.source_metadata?.document?.author ? `<span>${escape(row.source_metadata.document.author)}</span>` : ''}<span class="document-match-count">${landmark ? count(landmark.matches) + (locale === 'ja' ? ' 件' : landmark.matches === 1 ? ' match' : ' matches') : count(shown) + (locale === 'ja' ? ' 件を表示' : ' shown')}</span>`;
            if (renderSignatures.get(heading) !== markup) {
                heading.innerHTML = markup;
                renderSignatures.set(heading, markup);
            }
            desired.push(heading);
            previousDocument = row.document_ordinal;
        }
        const link = node(`match:${row.sequence}`, 'a', 'kwic-row');
        if (!link.hasAttribute('data-sequence')) {
            link.dataset.sequence = String(row.sequence);
            link.setAttribute('data-view', '');
        }
        const href = hrefFor(run, hitLocation(run, row, 'example'));
        if (link.getAttribute('href') !== href)
            link.setAttribute('href', href);
        const ordinal = view.start + allRows.indexOf(row) + 1;
        const label = (row.source_metadata?.document?.title ?? row.source_name) + ', ' + ordinal + ': ' + words(row.left_context) + row.surface + words(row.right_context);
        if (link.getAttribute('aria-label') !== label)
            link.setAttribute('aria-label', label);
        const expanded = String(inspecting?.run.id === run.id && inspecting.row.sequence === row.sequence && !tokenLab.hidden);
        if (link.getAttribute('aria-expanded') !== expanded)
            link.setAttribute('aria-expanded', expanded);
        const signature = `${ordinal}:${context}`;
        if (renderSignatures.get(link) !== signature) {
            const content = `<span class="line-number">${ordinal}</span><span class="context-left" lang="ja"><span>${escape(words((row.left_context ?? []).slice(-context || undefined)).slice(context ? 0 : Infinity))}</span></span><mark lang="ja">${escape(row.surface)}</mark><span class="context-right" lang="ja">${escape(words((row.right_context ?? []).slice(0, context)))}</span>`;
            link.innerHTML = content;
            renderSignatures.set(link, signature);
        }
        desired.push(link);
        if (inspectionFailure?.runId === run.id && inspectionFailure.sequence === row.sequence) {
            const notice = node(`error:${row.sequence}`, 'p', 'line-note analysis-error');
            notice.setAttribute('role', 'alert');
            const signature = locale + inspectionFailure.message;
            if (renderSignatures.get(notice) !== signature) {
                notice.replaceChildren(document.createTextNode((locale === 'ja' ? '分析を読み込めませんでした。' : 'Could not load analysis. ') + inspectionFailure.message + ' '));
                const retry = document.createElement('button');
                retry.type = 'button';
                retry.textContent = locale === 'ja' ? '分析を再読み込み' : 'Retry analysis';
                retry.addEventListener('click', () => void inspect(run, row, link));
                notice.append(retry);
                renderSignatures.set(notice, signature);
            }
            desired.push(notice);
        }
        if (inspecting?.run.id === run.id && inspecting.row.sequence === row.sequence && !tokenLab.hidden) {
            desired.push(tokenLab);
            if (readerPanel.open)
                desired.push(readerPanel);
        }
    }
    if (!rows.length) {
        const empty = node('empty', 'p', 'line-note');
        empty.textContent = t(filter ? 'noFilter' : 'noLines');
        desired.push(empty);
    }
    for (const child of [...list.children])
        if (!desired.includes(child)) {
            if (child === tokenLab || child === readerPanel)
                document.body.append(child);
            else
                child.remove();
        }
    desired.forEach((child, index) => { if (list.children[index] !== child)
        list.insertBefore(child, list.children[index] ?? null); });
    for (const key of nodes.keys())
        if (!used.has(key))
            nodes.delete(key);
}
async function inspect(run, row, button) {
    const epoch = ++inspectionEpoch;
    inspectionFailure = null;
    button.setAttribute('aria-busy', 'true');
    try {
        const key = `${run.dataset}:${row.corpus_sentence_ordinal}`;
        let data = tokenCache.get(key);
        if (!data) {
            data = parseStoredSentence(await request(`api/questions/data/${encodeURIComponent(run.dataset)}/tokens?sentence=${row.corpus_sentence_ordinal}`));
            if (tokenCache.size >= 16)
                tokenCache.delete(tokenCache.keys().next().value);
            tokenCache.set(key, data);
        }
        if (epoch !== inspectionEpoch)
            return;
        if (data.analysis_identity !== run.contract_run.analysis_identity)
            throw new Error('The stored sentence belongs to another corpus realization. Run this question again.');
        if (conditionSentence !== key) {
            conditions = [];
            conditionSentence = key;
        }
        if (!inspecting)
            tokenDetails.open = !compactLayout.matches;
        inspecting = { run, row };
        tokenData = data;
        tokenIndex = row.span.start;
        focusedSentence.innerHTML = escape(words(data.tokens.slice(0, row.span.start))) + '<mark>' + escape(words(data.tokens.slice(row.span.start, row.span.end))) + '</mark>' + escape(words(data.tokens.slice(row.span.end)));
        const page = documentView(run, row.pattern), position = page.rows.findIndex(value => value.sequence === row.sequence);
        $('[data-example-step="-1"]', exampleSteps).disabled = position <= 0 && page.previousAfter == null;
        $('[data-example-step="1"]', exampleSteps).disabled = position === page.rows.length - 1 && page.next == null;
        $('#token-origin').textContent = `${row.source_metadata?.document?.title ?? row.source_name} · ${count(position < 0 ? row.sequence + 1 : page.start + position + 1)}`;
        $('#token-ribbon').innerHTML = data.tokens.map(token => `<button type="button" data-token="${token.index}" class="${token.index >= row.span.start && token.index < row.span.end ? 'is-hit' : ''}">${escape(token.attributes.text)}</button>`).join('');
        const keys = Object.keys(data.tokens[tokenIndex].attributes);
        attributeKey = keys.includes('lemma') ? 'lemma' : keys[0] ?? 'text';
        attributeMember = undefined;
        for (const token of $('#token-ribbon').querySelectorAll('button'))
            token.addEventListener('click', () => selectToken(Number(token.dataset.token)));
        selectToken(tokenIndex);
        tokenLab.hidden = false;
        button.after(tokenLab);
        button.setAttribute('aria-expanded', 'true');
        $('#document').setAttribute('href', hrefFor(run, hitLocation(run, row, 'reader')));
        $('#close-token').setAttribute('href', hrefFor(run, concordanceFor(run)));
        $('#close-inspector').setAttribute('href', hrefFor(run, viewLocation.kind === 'reader' ? viewLocation.returnTo : hitLocation(run, row, 'example')));
        const panel = panels.get(run.id);
        if (panel)
            renderLines(panel, run);
        if (readerPanel.open)
            updateReaderNavigation();
    }
    catch (error) {
        if (epoch === inspectionEpoch) {
            inspectionFailure = { runId: run.id, sequence: row.sequence, message: errorMessage(error) };
            const panel = panels.get(run.id);
            if (panel)
                renderLines(panel, run);
        }
    }
    finally {
        button.removeAttribute('aria-busy');
    }
}
for (const control of exampleSteps.querySelectorAll('button'))
    control.addEventListener('click', async () => {
        const origin = inspecting;
        if (!origin)
            return;
        const { run, row } = origin, step = Number(control.dataset.exampleStep), view = documentView(run, row.pattern), position = view.rows.findIndex(value => value.sequence === row.sequence);
        const top = tokenLab.getBoundingClientRect().top, epoch = navigationEpoch, hadFocus = document.activeElement === control;
        let successor = null;
        control.disabled = true;
        try {
            let next = view.rows[position + step];
            let page;
            if (!next) {
                const query = new URLSearchParams({ pattern: row.pattern, limit: String(controls.state.lines), continuous: view.scopeDocument == null ? '1' : '0', after: String(step < 0 ? view.previousAfter ?? -1 : row.sequence), q: view.query });
                if (view.scopeDocument != null)
                    query.set('document', String(view.scopeDocument));
                page = await request(`api/questions/runs/${run.id}/documents?${query}`);
                next = step < 0 ? page.rows.at(-1) : page.rows[0];
            }
            if (!next || epoch !== navigationEpoch || inspecting !== origin)
                return;
            const target = hitLocation(run, next, 'example');
            successor = next.sequence;
            await navigateView(hrefFor(run, page ? { ...target, returnTo: { ...concordanceFor(run), after: page.after, document: page.document } } : target), true);
            if (inspecting?.run.id === run.id && inspecting.row.sequence === next.sequence)
                window.scrollBy(0, tokenLab.getBoundingClientRect().top - top);
        }
        catch (error) {
            const notice = document.createElement('p');
            notice.className = 'error';
            notice.setAttribute('role', 'alert');
            notice.textContent = errorMessage(error);
            exampleSteps.after(notice);
        }
        finally {
            if (inspecting === origin)
                control.disabled = false;
            if (hadFocus && document.activeElement === document.body && (inspecting === origin || inspecting?.run.id === run.id && inspecting.row.sequence === successor)) {
                const target = control.disabled ? exampleSteps.querySelector('button:not(:disabled)') : control;
                (target ?? focusedSentence).focus({ preventScroll: true });
            }
        }
    });
function selectToken(index) {
    if (!tokenData)
        return;
    tokenIndex = index;
    const token = tokenData.tokens[index], field = attributeKey, members = attributeMembers(token.attributes[field]);
    attributeMember = attributeMember !== undefined && members.includes(attributeMember) ? attributeMember : members[0];
    for (const button of $('#token-ribbon').querySelectorAll('button')) {
        const token = tokenData.tokens[Number(button.dataset.token)];
        button.classList.toggle('selected', token.index === index);
        button.setAttribute('aria-pressed', String(token.index === index));
    }
    const entries = Object.entries(token.attributes);
    const common = ['text', 'lemma', 'norm', 'upos', 'xpos', 'unidic.cform', 'unidic.ctype', 'unidic.kana', 'unidic.pron', 'unidic.orthbase', 'unidic.lform', 'unidic.goshu', 'unidic.pos1', 'unidic.pos2', 'unidic.pos3', 'unidic.pos4', 'unidic.pronbase', 'unidic.kanabase'];
    const basics = [...common.filter(key => key in token.attributes), ...entries.map(([key]) => key).filter(key => !common.includes(key))].slice(0, 18).map(key => [key, token.attributes[key]]);
    const row = ([key, value]) => {
        const values = attributeMembers(value);
        return values.length ? values.map(member => { const selected = field === key && attributeMember === member; return `<button type="button" data-attribute="${escape(key)}" data-member="${escape(member)}" class="attribute-row ${selected ? 'selected' : ''}" aria-pressed="${selected}"><span>${escape(key)}</span><strong>${escape(member)}</strong></button>`; }).join('') : `<div class="attribute-row"><span>${escape(key)}</span><span>${locale === 'ja' ? '値なし' : 'No values'}</span></div>`;
    };
    const attributes = $('#token-attributes'), expanded = attributes.querySelector('details')?.open ?? false;
    attributes.innerHTML = `<div class="attribute-basics">${basics.map(row).join('')}</div><details><summary>${locale === 'ja' ? '全属性' : 'All attributes'} (${entries.length})</summary><div class="attribute-all">${entries.map(row).join('')}</div></details>`;
    attributes.querySelector('details').open = expanded;
    $('#attribute-remix').disabled = attributeMember === undefined || conditions.some(condition => condition.tokenIndex === tokenIndex && condition.key === field && condition.value === attributeMember);
    renderComposer();
}
let readerEpoch = 0;
function openReader() { const reader = $('#inspector'); tokenLab.after(reader); reader.show(); reader.setAttribute('aria-modal', 'false'); reader.scrollIntoView({ block: 'nearest' }); }
async function readDocument(scope, path = null) {
    const epoch = ++readerEpoch;
    const inspection = readerTarget ?? inspecting;
    if (!inspection)
        return;
    const { run, row } = inspection;
    const target = $('#document-body'), title = row.source_metadata?.document?.title ?? row.source_name;
    readerStatus.classList.remove('error');
    readerStatus.textContent = locale === 'ja' ? `${title}を読み込んでいます…` : `Loading ${title}…`;
    target.setAttribute('aria-busy', 'true');
    const targetKey = `${run.id}:${row.corpus_sentence_ordinal}`;
    for (const button of target.querySelectorAll('button')) {
        if (!readerDisabled.has(button))
            readerDisabled.set(button, button.disabled);
        button.disabled = readerContentKey !== targetKey || readerDisabled.get(button);
    }
    if (!readerContentKey) {
        $('#inspect-title').textContent = title;
        $('#inspect-origin').textContent = `${t('runNumber')} ${run.id.slice(0, 6)} · ${row.pattern}`;
        target.textContent = t('loadingRead');
    }
    try {
        const url = path ?? `api/read/${run.dataset}/${row.corpus_sentence_ordinal}/${row.span.start}/${row.span.end}/${scope}`;
        const response = await fetch(serviceURL(url));
        if (!response.ok)
            throw new Error(`HTTP ${response.status}`);
        const html = await response.text();
        if ((readerTarget ?? inspecting) !== inspection || epoch !== readerEpoch)
            return;
        const parsed = new DOMParser().parseFromString(html, 'text/html');
        parsed.querySelectorAll('script,style').forEach(element => element.remove());
        await localizeReader(parsed.body);
        if ((readerTarget ?? inspecting) !== inspection || epoch !== readerEpoch)
            return;
        const header = parsed.body.querySelector('header');
        header?.querySelector('h1,h2,h3')?.remove();
        header?.querySelector('.reading-source')?.remove();
        target.replaceChildren(...parsed.body.childNodes);
        readerContentKey = `${run.id}:${row.corpus_sentence_ordinal}`;
        $('#inspect-title').textContent = title;
        $('#inspect-origin').textContent = `${t('runNumber')} ${run.id.slice(0, 6)} · ${row.pattern}`;
        readerStatus.replaceChildren();
        for (const button of target.querySelectorAll('[hx-get]')) {
            const href = button.getAttribute('hx-get');
            button.removeAttribute('hx-get');
            button.addEventListener('click', event => { const owner = readerTarget ?? inspecting; if (!owner || `${owner.run.id}:${owner.row.corpus_sentence_ordinal}` !== targetKey) {
                event.preventDefault();
                return;
            } void readDocument(scope, href); });
        }
    }
    catch (error) {
        if ((readerTarget ?? inspecting) !== inspection || epoch !== readerEpoch)
            return;
        readerStatus.classList.add('error');
        readerStatus.textContent = (locale === 'ja' ? `${title}を読み込めませんでした。` : `Could not load ${title}. `) + errorMessage(error) + (readerContentKey ? (locale === 'ja' ? ' 前の原文を表示しています。' : ' Previous text remains visible. ') : ' ');
        if (!readerContentKey)
            target.replaceChildren();
        const retry = document.createElement('button');
        retry.type = 'button';
        retry.dataset.readRetry = '';
        retry.textContent = locale === 'ja' ? '原文を再読み込み' : 'Retry reading';
        retry.addEventListener('click', () => readDocument(scope, path));
        readerStatus.append(retry);
    }
    finally {
        if ((readerTarget ?? inspecting) === inspection && epoch === readerEpoch)
            target.setAttribute('aria-busy', 'false');
    }
}
async function localizeReader(root) {
    const response = await fetch(serviceURL(`assets/i18n/${locale}.json`));
    const dict = await response.json();
    for (const node of root.querySelectorAll('[data-i18n]')) {
        const template = dict[node.dataset.i18n ?? ''];
        if (!template)
            continue;
        let args = {};
        try {
            args = JSON.parse(node.dataset.i18nArgs ?? '{}');
        }
        catch { }
        node.textContent = template.replace(/\{(\w+)\}/g, (slot, key) => args[key] ?? slot);
    }
}
function downloadSource(run) {
    const url = URL.createObjectURL(new Blob([run.source], { type: 'text/plain;charset=utf-8' }));
    const link = document.createElement('a');
    link.href = url;
    link.download = 'query.lx';
    link.click();
    setTimeout(() => URL.revokeObjectURL(url), 1000);
}
async function replay() {
    if (!recorded.length || activeId)
        return;
    const epoch = ++replayEpoch;
    isReplay = true;
    $('#replay-note').hidden = false;
    const frames = recorded.slice();
    const final = frames.at(-1);
    state = begin(state, runningSnapshot(final.id, final));
    for (const frame of frames) {
        if (epoch !== replayEpoch)
            return;
        state = receive(state, frame);
        render();
        await new Promise(resolve => setTimeout(resolve, Math.max(100, Math.min(700, 9000 / frames.length))));
    }
    isReplay = false;
    render();
}
function questionDocument(overrides = {}) {
    return { format: 'lingrep.question/v1', evidence: displayedEvidence()?.id ?? '', view: JSON.stringify(viewLocation), question: '', sections: JSON.stringify(sectionLinks()), ...(documentSections.size && state.current ? { attempt: state.current.id } : {}), source: $('#source').value, corpus: $('#corpus').value, analysis: analysisExpectation ?? config.datasets[$('#corpus').value].analysis_identity ?? '', where: controls.where(), ...controls.entries(), ...overrides };
}
function openQuestion(doc) {
    questionSettings(new URLSearchParams(Object.entries(doc).map(([key, value]) => [key, String(value)])));
    const links = parseSections(typeof doc.sections === 'string' ? doc.sections : null);
    const dataset = Object.entries(config.datasets).find(([, value]) => value.analysis_identity === doc.analysis)?.[0] ?? doc.corpus;
    const { question: _question, ...settings } = doc;
    const params = { ...settings, corpus: dataset, fresh: '1', run: doc.evidence || links.length ? '0' : '1' };
    const url = new URL(location.href);
    url.hash = new URLSearchParams(params).toString();
    window.open(url.href, '_blank', 'noopener');
}
$('#remix').addEventListener('click', () => openQuestion(questionDocument()));
$('#save-question').addEventListener('click', () => { const url = URL.createObjectURL(new Blob([JSON.stringify(questionDocument(), null, 2)], { type: 'application/json' })); const a = document.createElement('a'); a.href = url; a.download = 'question.lingview.json'; a.click(); setTimeout(() => URL.revokeObjectURL(url), 1000); });
$('#open-question').addEventListener('change', async (event) => { try {
    const file = event.target.files?.[0];
    if (!file)
        return;
    if (file.size > 256 * 1024)
        throw new Error('Question documents may contain up to 256 KiB.');
    const doc = savedQuestion(JSON.parse(await file.text()));
    openQuestion(doc);
}
catch (error) {
    $('#error').hidden = false;
    $('#error').textContent = errorMessage(error);
} });
const testBuilder = document.createElement('section');
testBuilder.className = 'test-builder';
testBuilder.hidden = true;
testBuilder.innerHTML = '<ol></ol><p><code id="pattern-preview"></code></p><div class="test-builder-actions"><button type="button" id="use-pattern"></button><button type="button" id="clear-pattern"></button></div>';
$('.attribute-actions').after(testBuilder);
function renderComposer() {
    testBuilder.hidden = !conditions.length;
    if (!conditions.length)
        return;
    $('ol', testBuilder).innerHTML = conditions.map((condition, index) => `<li><span>${escape(tokenData?.tokens[condition.tokenIndex]?.attributes.text ?? '')} · <code>${escape(condition.key)}=${escape(condition.value)}</code></span> <button type="button" data-remove-condition="${index}" aria-label="${escape((locale === 'ja' ? '条件を削除: ' : 'Remove condition: ') + condition.key + '=' + condition.value)}">${locale === 'ja' ? '削除' : 'Remove'}</button></li>`).join('');
    for (const button of testBuilder.querySelectorAll('[data-remove-condition]'))
        button.addEventListener('click', () => { conditions = conditions.filter((_, index) => index !== Number(button.dataset.removeCondition)); selectToken(tokenIndex); });
    $('#pattern-preview').textContent = patternSource(conditions);
    $('#use-pattern').textContent = locale === 'ja' ? 'このパターンで検索' : 'Search this pattern';
    $('#clear-pattern').textContent = locale === 'ja' ? '条件を消去' : 'Clear conditions';
}
$('#attribute-remix').addEventListener('click', () => {
    if (!inspecting || !tokenData)
        return;
    const key = attributeKey, value = attributeMember;
    if (value === undefined)
        return;
    conditions = addCondition(conditions, { tokenIndex, key, value });
    selectToken(tokenIndex);
});
$('#clear-pattern').addEventListener('click', () => { conditions = []; selectToken(tokenIndex); });
$('#use-pattern').addEventListener('click', () => {
    if (!inspecting || !conditions.length)
        return;
    const run = inspecting.run;
    analysisExpectation = run.contract_run?.analysis_identity ?? null;
    const origin = { parent: run.id, view: hitLocation(run, inspecting.row, 'example'), label: `${inspecting.row.source_metadata?.document?.title ?? inspecting.row.source_name} · ${inspecting.row.surface}` };
    $('#source').value = patternSource(conditions);
    $('#corpus').value = run.dataset;
    controls.restore(new URLSearchParams({ ...controls.entries(), metadata: '{}', raw: run.where }));
    updateCorpus();
    updateDraft();
    const pending = search(undefined, origin), navigation = navigationEpoch;
    void pending.then(() => { const current = state.current; if (navigation === navigationEpoch && current && documentSections.get(current.id)?.origin === origin) {
        viewEvidence = current;
        viewLocation = defaultView();
        window.history.pushState(null, '', hrefFor(current, viewLocation));
        updateDraft();
        const panel = panels.get(current.id);
        panel?.scrollIntoView({ block: 'start' });
        panel?.querySelector('.section-origin a')?.focus({ preventScroll: true });
    } });
});
$('#timeline').addEventListener('input', () => { if (activeId || !recorded.length)
    return; ++replayEpoch; isReplay = true; state = { ...state, previous: state.completed, current: recorded[Number($('#timeline').value)] }; $('#replay-note').hidden = false; render(); });
$('#reading-options').addEventListener('keydown', event => { if (event.key === 'Enter' && !event.ctrlKey && !event.metaKey && !event.altKey)
    event.preventDefault(); });
$('#query-form').addEventListener('submit', search);
$('#example').addEventListener('click', search);
$('#source').addEventListener('input', updateDraft);
$('#source').addEventListener('focus', () => { sourceAtFocus = $('#source').value; });
$('#source').addEventListener('blur', event => {
    if ($('#source').value === sourceAtFocus)
        return;
    if (event.relatedTarget instanceof Element && event.relatedTarget.closest('#run,#example,#retry'))
        return;
    void search();
});
$('#corpus').addEventListener('change', () => { analysisExpectation = null; updateCorpus(); updateDraft(); });
$('#behavior').addEventListener('change', () => { updateDraft(); render(true); });
$('#cancel').addEventListener('click', async () => {
    if (restoreController) {
        restoreController.abort();
        return;
    }
    if (isReplay) {
        ++replayEpoch;
        isReplay = false;
        state = { ...state, current: state.completed };
        $('#replay-note').hidden = true;
        render();
        return;
    }
    if (activeId) {
        $('#cancel').disabled = true;
        try {
            await post(`api/questions/runs/${activeId}/cancel`);
        }
        catch (error) {
            $('#error').hidden = false;
            $('#error').textContent = errorMessage(error);
        }
        finally {
            $('#cancel').disabled = false;
        }
    }
});
$('#retry').addEventListener('click', search);
$('#language').addEventListener('click', () => { locale = locale === 'en' ? 'ja' : 'en'; localStorage.setItem('ling-questions-locale', locale); translate(); });
$('#edit-source').addEventListener('click', () => {
    const editor = $('#source-editor');
    editor.hidden = !editor.hidden;
    $('#edit-source').setAttribute('aria-expanded', String(!editor.hidden));
    if (!editor.hidden)
        $('#source').focus();
});
$('#token-attributes').addEventListener('click', event => { const button = event.target.closest('[data-attribute]'); if (!button)
    return; attributeKey = button.dataset.attribute; attributeMember = button.dataset.member; const all = !!button.closest('.attribute-all'); selectToken(tokenIndex); [...$('#token-attributes').querySelectorAll(all ? '.attribute-all [data-attribute]' : '.attribute-basics [data-attribute]')].find(item => item.dataset.attribute === attributeKey && item.dataset.member === attributeMember)?.focus({ preventScroll: true }); });
$('#inspector').addEventListener('close', () => { ++readerEpoch; if (viewLocation.kind === 'reader' && inspecting)
    void navigateView(hrefFor(inspecting.run, viewLocation.returnTo), true, 'replace'); if (document.activeElement === document.body)
    $('#results').focus({ preventScroll: true }); });
$('#replay').addEventListener('click', replay);
document.addEventListener('keydown', event => { if (event.key === 'Escape' && $('#inspector').open) {
    $('#inspector').close();
    event.preventDefault();
} if (!event.isComposing && (event.ctrlKey || event.metaKey || event.altKey) && event.key === 'Enter') {
    event.preventDefault();
    search();
} });
function displayedEvidence() {
    if (viewEvidence)
        return viewEvidence;
    if (inspecting && !tokenLab.hidden)
        return inspecting.run;
    return visible(state, $('#behavior').value);
}
function concordanceFor(run) {
    const pattern = selection.get(run.id) ?? run.definitions[0] ?? '', page = documentView(run, pattern);
    return { kind: 'concordance', pattern, scopeDocument: page.scopeDocument, returnAfter: page.returnAfter, document: null, after: page.after, filter: filters.get(run.id) ?? '' };
}
function hitLocation(run, row, kind) {
    const example = { kind: 'example', pattern: row.pattern, document: row.document_ordinal, sequence: row.sequence, returnTo: concordanceFor(run) };
    return kind === 'example' ? example : { ...example, kind: 'reader', returnTo: example };
}
function hrefFor(run, view) {
    const params = { ...controls.entries(), behavior: $('#behavior').value };
    if (controls.where() !== run.where) {
        params.metadata = '{}';
        params.raw = run.where;
    }
    for (const [key, value] of Object.entries({ metadata: '{}', raw: '', context: 12, lines: 20, behavior: 'keep' }))
        if (String(params[key]) === String(value))
            delete params[key];
    const url = locationURL(location.href, params, run.id, view);
    return url.slice(url.indexOf('#'));
}
async function readRun(identifier) {
    const run = parseSnapshot(await request(`api/questions/runs/${encodeURIComponent(identifier)}`));
    if (!Object.hasOwn(config.datasets, run.dataset) || run.contract_run && config.datasets[run.dataset].analysis_identity != null && config.datasets[run.dataset].analysis_identity !== run.contract_run.analysis_identity)
        throw new Error('The corpus for this saved search is unavailable.');
    return run;
}
function questionParams(url, run) {
    const params = new URLSearchParams(url.hash.slice(1));
    if (!params.has('source'))
        params.set('source', run.source);
    if (!params.has('corpus'))
        params.set('corpus', run.dataset);
    if (!Object.hasOwn(config.datasets, params.get('corpus')))
        throw new Error('The corpus for this question is not registered.');
    if (!params.has('metadata') && !params.has('raw')) {
        params.set('metadata', '{}');
        params.set('raw', run.where);
    }
    return params;
}
async function applyView(run, target, pin = true, epoch = ++navigationEpoch) {
    if (run.phase === 'complete' && target.pattern && !run.definitions.includes(target.pattern) && !Object.hasOwn(run.counts, target.pattern))
        throw new Error('This pattern is not part of the saved search.');
    viewEvidence = pin ? run : null;
    viewLocation = target;
    if (target.kind !== 'reader' && $('#inspector').open)
        $('#inspector').close();
    if (target.kind === 'reader' && target.returnTo.kind === 'example') {
        const origin = target.returnTo;
        if (!inspecting || inspecting.run.id !== run.id || inspecting.row.sequence !== origin.sequence) {
            await applyView(run, origin, pin, epoch);
            if (epoch !== navigationEpoch)
                return;
            viewLocation = target;
        }
        let row = target.sequence != null ? (documentView(run, target.pattern).rows.find(row => row.sequence === target.sequence) ?? run.rows[target.pattern]?.find(row => row.sequence === target.sequence)) : undefined;
        if (!row) {
            const page = await request(`api/questions/runs/${run.id}/documents?${new URLSearchParams({ pattern: target.pattern, document: String(target.document), after: String(target.sequence == null ? -1 : target.sequence - 1), limit: '1' })}`);
            row = page.rows[0];
        }
        if (epoch !== navigationEpoch)
            return;
        if (!row)
            throw new Error('This saved location has no matching example. Choose another document.');
        readerTarget = { run, row };
        viewLocation = { ...target, sequence: row.sequence, returnTo: origin.sequence == null && inspecting ? { ...origin, sequence: inspecting.row.sequence } : origin };
        if (!readerPanel.open)
            openReader();
        updateReaderNavigation();
        await readDocument('document');
        updateDraft();
        return;
    }
    if (target.kind !== 'concordance' && inspecting?.run.id === run.id && target.sequence === inspecting.row.sequence && !tokenLab.hidden) {
        $('#close-inspector').setAttribute('href', hrefFor(run, target.kind === 'reader' ? target.returnTo : target));
        if (target.kind === 'reader') {
            readerTarget = inspecting;
            if (!$('#inspector').open)
                openReader();
            updateReaderNavigation();
            await readDocument('document');
        }
        updateDraft();
        return;
    }
    ++inspectionEpoch;
    inspectionFailure = null;
    if (target.kind === 'concordance' || inspecting?.run.id !== run.id) {
        tokenLab.hidden = true;
        inspecting = null;
    }
    const parent = target.kind === 'concordance' ? target : target.returnTo;
    const context = parent.kind === 'concordance' ? parent : parent.returnTo;
    const pattern = target.pattern || run.definitions[0] || '';
    selection.set(run.id, pattern);
    filters.set(run.id, context.filter);
    const page = documentView(run, pattern), scope = context.scopeDocument ?? null;
    if (page.loading) {
        ++page.epoch;
        page.loading = false;
        page.request = null;
    }
    const needsPage = run.phase === 'complete' && (page.documents === null || page.scopeDocument !== scope || page.after !== context.after || context.document != null && context.document !== page.document);
    page.returnAfter = context.returnAfter ?? -1;
    render();
    const panel = panels.get(run.id);
    if (!panel)
        return;
    if (needsPage)
        await loadDocumentPage(panel, run, pattern, context.document, context.after, null, scope);
    if (epoch !== navigationEpoch)
        return;
    if (target.kind !== 'concordance') {
        let row = documentView(run, pattern).rows.find(row => target.sequence != null && row.sequence === target.sequence) ?? run.rows[pattern]?.find(row => target.sequence != null && row.sequence === target.sequence);
        if (!row && run.phase !== 'complete') {
            pendingView = target;
            return;
        }
        if (!row) {
            const page = await request(`api/questions/runs/${run.id}/documents?${new URLSearchParams({ pattern, document: String(target.document), after: String(target.sequence == null ? -1 : target.sequence - 1), limit: '1' })}`);
            row = page.rows[0];
        }
        if (epoch !== navigationEpoch)
            return;
        if (!row)
            throw new Error('This saved location has no matching example. Choose another document.');
        let link = panel.querySelector(`[data-sequence="${row.sequence}"]`);
        if (!link) {
            filters.set(run.id, '');
            await loadDocumentPage(panel, run, pattern, row.document_ordinal, row.sequence - 1);
            link = panel.querySelector(`[data-sequence="${row.sequence}"]`);
        }
        if (epoch !== navigationEpoch || !link)
            return;
        viewLocation = target.kind === 'reader' && target.returnTo.kind === 'example' && target.returnTo.sequence == null ? { ...target, sequence: row.sequence, returnTo: { ...target.returnTo, sequence: row.sequence } } : { ...target, sequence: row.sequence };
        await inspect(run, row, link);
        if (epoch !== navigationEpoch)
            return;
        if (target.kind === 'reader') {
            readerTarget = { run, row };
            if (!$('#inspector').open)
                openReader();
            updateReaderNavigation();
            await readDocument('document');
        }
    }
    updateDraft();
}
async function navigateView(href, preserveDraft = false, historyMode = 'push') {
    const epoch = ++navigationEpoch;
    if (isReplay) {
        ++replayEpoch;
        isReplay = false;
        state = { ...state, current: state.completed };
        $('#replay-note').hidden = true;
    }
    try {
        const url = new URL(href, location.href), params = new URLSearchParams(url.hash.slice(1)), identifier = params.get('evidence');
        if (!identifier)
            throw new Error('This link has no saved search.');
        const target = parseView(params.get('view'));
        const run = [viewEvidence, state.current, state.previous, state.completed, inspecting?.run, documentSections.get(identifier)?.run].find(run => run?.id === identifier) ?? await readRun(identifier);
        if (epoch !== navigationEpoch)
            return;
        if (!preserveDraft) {
            analysisExpectation = run.contract_run?.analysis_identity ?? null;
            const question = questionParams(url, run);
            $('#source').value = question.get('source') ?? run.source;
            $('#corpus').value = question.get('corpus') ?? run.dataset;
            controls.restore(question);
            updateCorpus();
        }
        if (historyMode !== 'none')
            window.history[historyMode === 'push' ? 'pushState' : 'replaceState'](null, '', url.href);
        await applyView(run, target, true, epoch);
    }
    catch (error) {
        if (epoch !== navigationEpoch)
            return;
        $('#error').hidden = false;
        $('#error').textContent = errorMessage(error);
    }
}
document.addEventListener('click', event => {
    if (event.defaultPrevented || event.button !== 0 || event.metaKey || event.ctrlKey || event.shiftKey || event.altKey)
        return;
    const link = event.target?.closest('a[data-view]');
    if (link?.getAttribute('aria-disabled') === 'true') {
        event.preventDefault();
        return;
    }
    if (!link?.hasAttribute('href'))
        return;
    const url = new URL(link.href);
    if (url.origin !== location.origin || url.pathname !== location.pathname)
        return;
    event.preventDefault();
    const pending = navigateView(link.href, true), navigation = navigationEpoch;
    void pending.then(() => {
        if (navigation === navigationEpoch && link.hasAttribute('data-section-origin') && inspecting) {
            const panel = panels.get(inspecting.run.id);
            const row = panel?.querySelector(`[data-sequence="${inspecting.row.sequence}"]`);
            row?.scrollIntoView({ block: 'center' });
            row?.focus({ preventScroll: true });
        }
    });
});
window.addEventListener('popstate', () => void navigateView(location.href, false, 'none'));
async function init() {
    config = await request('api/questions/config');
    const seed = document.querySelector('#question-seed');
    let params = seedParams(seed ? JSON.parse(seed.textContent ?? 'null') : null, location.hash);
    const initialView = parseView(params.get('view'));
    viewLocation = initialView;
    const seededQuestion = !!seed && !params.has('evidence') && !params.has('attempt') && !params.has('sections');
    if (params.get('fresh') === '1')
        for (const name of ['run', 'completed', 'replay', 'sections'])
            sessionStorage.removeItem(`ling-questions-${name}`);
    $('#corpus').innerHTML = Object.entries(config.datasets).map(([key, dataset]) => `<option value="${escape(key)}">${escape(dataset.label)}</option>`).join('');
    savedManifest = parseSections(params.get('sections') ?? (params.get('fresh') === '1' || seededQuestion ? null : sessionStorage.getItem('ling-questions-sections')));
    const recipe = savedManifest.find(link => link.id === params.get('evidence'))?.recipe;
    if (params.has('source'))
        savedManifest = savedManifest.map(link => link.id === (params.get('attempt') ?? params.get('evidence')) && !link.recipe ? { ...link, recipe: { source: params.get('source'), dataset: params.get('corpus') ?? Object.keys(config.datasets)[0], where: params.get('where') ?? params.get('raw') ?? '', analysis: params.get('analysis') || null } } : link);
    if (recipe) {
        for (const [key, value] of Object.entries({ source: recipe.source, corpus: recipe.dataset, raw: recipe.where, analysis: recipe.analysis ?? '' }))
            if (!params.has(key) && (key !== 'raw' || !params.has('metadata')))
                params.set(key, value);
    }
    const corpus = params.get('corpus') ?? Object.keys(config.datasets)[0];
    if (!corpus || !Object.hasOwn(config.datasets, corpus))
        throw new Error('The corpus for this question is not registered.');
    $('#corpus').value = corpus;
    analysisExpectation = params.get('analysis') || null;
    $('#source').value = params.get('source') ?? config.source;
    controls = initQuestion(config, { changed: updateDraft, viewChanged: () => render(true), search: () => search(), getLocale: () => locale }, params);
    if (['keep', 'live', 'compare'].includes(params.get('behavior') ?? ''))
        $('#behavior').value = params.get('behavior');
    const explicit = params.get('evidence'), attemptId = params.get('attempt') ?? (!explicit && !seededQuestion ? sessionStorage.getItem('ling-questions-run') : null), completedId = params.get('completed') ?? (!explicit && !seededQuestion ? sessionStorage.getItem('ling-questions-completed') : null);
    const ids = [...new Set([explicit, attemptId, completedId].filter((id) => !!id))];
    let expired = false;
    const loaded = new Map();
    for (const id of ids) {
        try {
            loaded.set(id, await readRun(id));
        }
        catch (error) {
            const expiry = expiredSearch(error);
            expired ||= expiry;
            unavailableSearches.set(id, { expired: expiry, message: errorMessage(error) });
            if (!savedManifest.some(link => link.id === id))
                savedManifest = [...savedManifest, { id, origin: null, ...(params.has('source') ? { recipe: { source: params.get('source'), dataset: corpus, where: controls.where(), analysis: analysisExpectation } } : {}) }];
        }
    }
    let evidence = explicit ? loaded.get(explicit) ?? null : null;
    const previous = completedId ? loaded.get(completedId) ?? null : null;
    if (!evidence && !explicit)
        evidence = previous;
    const attempt = attemptId ? loaded.get(attemptId) ?? null : null, current = attempt ?? evidence;
    if (current) {
        for (const [key, value] of new URLSearchParams({ source: current.source, corpus: current.dataset, raw: current.where }))
            if (!params.has(key) && (key !== 'raw' || !params.has('metadata')))
                params.set(key, value);
        if (!params.has('analysis'))
            analysisExpectation = current.contract_run?.analysis_identity ?? analysisExpectation;
        $('#corpus').value = params.get('corpus');
        $('#source').value = params.get('source');
        controls.restore(params);
    }
    state = { current: null, previous: null, completed: previous?.phase === 'complete' ? previous : evidence?.phase === 'complete' ? evidence : null };
    if (current) {
        if (savedManifest.length > 1)
            documentSections.set(current.id, { run: current, origin: savedManifest.find(link => link.id === current.id)?.origin ?? null });
        state = receive(begin(state, runningSnapshot(current.id, current)), current);
        if (current.phase === 'running') {
            activeId = current.id;
            connect(current.id);
        }
        else if (current.phase === 'complete')
            recorded = restoreReplay(current);
    }
    if (!recorded.length && state.completed)
        recorded = restoreReplay(state.completed);
    const follow = params.get('follow') === '1' || !explicit;
    viewEvidence = follow ? null : evidence;
    for (const link of savedManifest) {
        const run = loaded.get(link.id);
        if (run && savedManifest.length > 1)
            documentSections.set(link.id, { run, origin: link.origin });
    }
    await loadSavedSections();
    savedManifest = sectionLinks();
    try {
        sessionStorage.setItem('ling-questions-sections', JSON.stringify(savedManifest));
    }
    catch { }
    translate();
    renderSavedSearches();
    if (params.get('notice')) {
        $('#error').hidden = false;
        $('#error').textContent = params.get('notice');
    }
    window.__probe = { get state() { return state; }, get recorded() { return recorded; }, get controls() { return controls; }, get view() { return viewLocation; }, search, questionDocument };
    if (current)
        await applyView(follow && current.phase === 'complete' && initialView.kind === 'concordance' ? current : evidence ?? current, follow && current.phase === 'complete' && initialView.kind === 'concordance' ? defaultView() : initialView, !follow);
    if (!expired && !savedManifest.some(link => !availableRun(link.id)) && params.get('run') === '1' && (!current || current.source !== $('#source').value || current.dataset !== $('#corpus').value || current.where !== controls.where())) {
        await search();
        pendingView = current ? defaultView() : initialView;
    }
}
init().catch(error => { $('#status').textContent = errorMessage(error); $('#run').disabled = true; $('#example').disabled = true; $('#retry').hidden = true; });
