import { serviceURL } from './service.js';
import { element as $, errorMessage } from './model.js';
const esc = (value) => String(value ?? '').replace(/[&<>"']/g, c => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' }[c] ?? c));
export const tokenLiteral = (value) => [...String(value)].map(c => /[\p{L}\p{N}\p{M}_-]/u.test(c) ? c : c.codePointAt(0) <= 0xffff ? '\\u' + c.codePointAt(0).toString(16).padStart(4, '0') : c).join('');
const scopeValue = (value) => typeof value === 'string' ? '"' + value + '"' : String(value);
export function scopeClause(metadata, raw = '') {
    const clauses = [];
    if (raw.trim())
        clauses.push(raw.trim());
    for (const [key, condition] of Object.entries(metadata)) {
        const field = `@document.${key}`;
        if (condition.numeric) {
            if (condition.min !== '' && condition.min != null)
                clauses.push(`${field} >= ${Number(condition.min)}`);
            if (condition.max !== '' && condition.max != null)
                clauses.push(`${field} <= ${Number(condition.max)}`);
        }
        else if (condition.values?.length) {
            if (condition.list) {
                const tests = condition.values.map(v => `${field} ${condition.exclude ? 'not contains' : 'contains'} ${scopeValue(v)}`);
                clauses.push(tests.length === 1 ? tests[0] : `(${tests.join(condition.exclude ? ' & ' : ' | ')})`);
            }
            else
                clauses.push(`${field} ${condition.exclude ? 'not in' : 'in'} {${condition.values.map(scopeValue).join(', ')}}`);
        }
    }
    if (raw.trim() && clauses.length > 1)
        clauses[0] = `(${clauses[0]})`;
    return clauses.join(' & ');
}
export function metadataValueLabel(config, field, value, locale, fallback = String(value)) {
    const names = typeof value === 'string' ? config.labels?.[`@document.${field}`]?.values?.[value]?.labels : undefined;
    const name = names?.[locale];
    return name === undefined ? (names ? String(value) : fallback) : name === value ? String(value) : `${value} ${name}`;
}
export function numberDragDelta(distance, step = 1, precise = false) {
    const steps = Math.trunc(distance / 8);
    return Math.sign(steps) * Math.min(Number.MAX_SAFE_INTEGER, Math.round(precise || Math.abs(steps) <= 4 ? Math.abs(steps) : 4 + 4 * Math.expm1((Math.abs(steps) - 4) / 4)) * step);
}
function paintNumber(input) { input.style.width = `${Math.max(2, String(input.value || input.placeholder).length) + .8}ch`; }
function clampNumber(input, commit = true) {
    if (input.value !== '' && Number.isFinite(input.valueAsNumber))
        input.value = String(Math.max(commit && input.min !== '' ? Number(input.min) : -Number.MAX_SAFE_INTEGER, Math.min(input.max !== '' ? Number(input.max) : Number.MAX_SAFE_INTEGER, input.valueAsNumber)));
    paintNumber(input);
}
function scrubNumber(input, changed, committed = () => { }) {
    let drag;
    input.title = document.documentElement.lang === 'ja' ? '左右にドラッグ、または数値を入力。Shift キーで細かく調整' : 'Drag horizontally or type a number. Hold Shift for precise dragging';
    input.addEventListener('input', () => { clampNumber(input, false); if (input.checkValidity()) {
        input.removeAttribute('aria-invalid');
        changed(false);
    } });
    input.addEventListener('change', () => { const previous = input.value; clampNumber(input); const valid = input.checkValidity(); input.setAttribute('aria-invalid', String(!valid)); if (valid && input.value !== previous)
        changed(false); committed(); });
    input.addEventListener('pointerdown', event => {
        if (event.button !== 0)
            return;
        drag = { id: event.pointerId, x: event.clientX, lastX: event.clientX, value: Number(input.value || input.placeholder), moved: false, precise: event.shiftKey };
        input.setPointerCapture(event.pointerId);
    });
    input.addEventListener('pointermove', event => {
        if (!drag || event.pointerId !== drag.id)
            return;
        if (event.shiftKey !== drag.precise) {
            drag.x = drag.lastX;
            drag.value = Number(input.value || input.placeholder);
            drag.precise = event.shiftKey;
        }
        drag.lastX = event.clientX;
        const delta = numberDragDelta(event.clientX - drag.x, Number(input.step || 1), drag.precise);
        if (!delta && !drag.moved)
            return;
        if (!drag.moved) {
            drag.moved = true;
            input.setPointerCapture(event.pointerId);
        }
        event.preventDefault();
        input.value = String(drag.value + delta);
        clampNumber(input);
        changed(true);
    });
    const finish = (event) => { if (!drag || event.pointerId !== drag.id)
        return; if (drag.moved)
        committed(); drag = undefined; };
    input.addEventListener('pointerup', finish);
    input.addEventListener('pointercancel', finish);
    input.addEventListener('lostpointercapture', () => { if (drag?.moved)
        committed(); drag = undefined; });
    paintNumber(input);
}
const hiddenMetadataFields = new Set(['identifier', 'filename', 'source_bundle_hash', 'source_content_hash', 'release', 'url', 'doi']);
export function questionSettings(params) {
    const metadata = Object.create(null);
    const parsed = JSON.parse(params.get('metadata') ?? '{}');
    if (!parsed || typeof parsed !== 'object' || Array.isArray(parsed))
        throw new Error('The document conditions are invalid. Open a new question.');
    for (const [key, raw] of Object.entries(parsed)) {
        if (!raw || typeof raw !== 'object' || Array.isArray(raw))
            throw new Error('The document conditions are invalid.');
        const value = raw;
        if (value.numeric === true) {
            if (typeof value.min !== 'string' || typeof value.max !== 'string' || [value.min, value.max].some(bound => bound !== '' && !Number.isFinite(Number(bound))) || value.min !== '' && value.max !== '' && Number(value.min) > Number(value.max))
                throw new Error('The saved numeric range is invalid.');
            metadata[key] = { numeric: true, min: value.min, max: value.max };
        }
        else {
            if (typeof value.list !== 'boolean' || typeof value.exclude !== 'boolean' || !Array.isArray(value.values) || !value.values.every(v => typeof v === 'string' || typeof v === 'number' && Number.isFinite(v) || typeof v === 'boolean'))
                throw new Error('The saved metadata values are invalid.');
            const labels = value.labels && typeof value.labels === 'object' && !Array.isArray(value.labels) && Object.values(value.labels).every(v => typeof v === 'string') ? value.labels : undefined;
            metadata[key] = { list: value.list, exclude: value.exclude, values: value.values, labels };
        }
    }
    const number = (key, fallback, min, max) => { const value = Number(params.get(key) ?? fallback); return Number.isFinite(value) ? Math.max(min, Math.min(max, Math.round(value))) : fallback; };
    return { metadata, raw: params.get('raw') ?? (params.has('metadata') ? '' : params.get('where') ?? ''), context: number('context', 12, 0, 24), lines: number('lines', 20, 1, Number.MAX_SAFE_INTEGER) };
}
export function initQuestion(config, { changed, viewChanged, search, getLocale }, initialParams = new URLSearchParams(location.hash.slice(1))) {
    const params = initialParams;
    const state = questionSettings(params);
    $('#where').value = state.raw;
    state.context = Number.isFinite(state.context) ? Math.round(state.context) : 12;
    $('#context-size').value = String(state.context);
    state.lines = Number.isFinite(state.lines) ? Math.max(1, Math.min(Number.MAX_SAFE_INTEGER, Math.round(state.lines))) : 20;
    $('#line-limit').value = String(state.lines);
    let pickerState = 'closed';
    let pickerEpoch = 0;
    let fields = [];
    let key = '';
    let after = '';
    let previous = [];
    let requestId = 0;
    let searchTimer;
    const text = (en, ja) => getLocale() === 'ja' ? ja : en;
    function localizeNumberErrors() {
        for (const [id, en, ja] of [
            ['reading-validation', 'Enter a whole number for the example count and context size.', '用例数と文脈の長さは整数で入力してください。'],
            ['metadata-validation', 'Use whole numbers within this field’s bounds, with the minimum no greater than the maximum.', '項目の範囲内の整数で、下限が上限を超えない値を入力してください。'],
        ]) {
            const node = $('#' + id), message = text(en, ja);
            if (node.textContent !== message)
                node.textContent = message;
        }
    }
    const label = (field) => config.labels?.[`@document.${field}`]?.labels?.[getLocale()] ?? (getLocale() === 'en' ? field.replaceAll('_', ' ') : field);
    const api = async (path) => { const response = await fetch(serviceURL(path)); const data = await response.json(); if (!response.ok)
        throw new Error(data.error ?? `HTTP ${response.status}`); return data; };
    function changedScope(runSearch = true) {
        state.raw = $('#where').value;
        renderChips();
        changed(runSearch);
        clearTimeout(searchTimer);
        if (runSearch)
            searchTimer = setTimeout(search, 300);
    }
    function renderChips() {
        const active = Object.entries(state.metadata).filter(([, c]) => c.numeric ? (c.min !== '' && c.min != null) || (c.max !== '' && c.max != null) : c.values?.length);
        $('#scope-label').textContent = active.length || state.raw ? text('selected documents', '条件に合う作品') : text('all documents', 'すべての作品');
        $('#scope-chips').innerHTML = active.map(([key, c]) => `<button type="button" class="scope-chip" data-field="${esc(key)}">${esc(label(key))}: ${c.numeric ? `${esc(c.min ?? '…')}–${esc(c.max ?? '…')}` : esc(c.values.map(v => metadataValueLabel(config, key, v, getLocale(), c.labels && Object.hasOwn(c.labels, String(v)) ? c.labels[String(v)] : String(v))).join(', '))}<span aria-hidden="true"> ×</span></button>`).join('');
        for (const button of $('#scope-chips').querySelectorAll('button'))
            button.addEventListener('click', () => { delete state.metadata[button.dataset.field ?? '']; changedScope(); if (button.dataset.field === key)
                loadValues(); });
        $('#clause-preview').textContent = scopeClause(state.metadata, state.raw) || text('All documents', 'すべての作品');
    }
    async function loadFields() {
        const epoch = ++requestId;
        $('#metadata-state').textContent = text('Reading corpus metadata…', 'コーパスのメタデータを読み込んでいます…');
        try {
            const data = await api(`api/questions/data/${$('#corpus').value}/metadata`);
            if (epoch !== requestId)
                return;
            fields = data.fields.filter(field => !hiddenMetadataFields.has(field.key));
            const initialKey = fields.some(f => f.key === 'author') ? 'author' : fields[0]?.key;
            const initialField = fields.find(f => f.key === initialKey);
            const initialValues = initialField && !initialField.numeric ? await api(`api/questions/data/${$('#corpus').value}/metadata?${new URLSearchParams({ field: initialKey, q: '', after: '', locale: getLocale() })}`) : undefined;
            if (epoch !== requestId)
                return;
            $('#metadata-field').innerHTML = fields.map(f => `<option value="${esc(f.key)}">${esc(label(f.key))} (${f.present.toLocaleString()})</option>`).join('');
            key = initialKey;
            $('#metadata-query').value = '';
            $('#metadata-field').value = key;
            $('#metadata-state').textContent = text(`${fields.length} metadata fields · counts are documents in this corpus.`, `${fields.length} 個のメタデータ項目 · 件数はコーパス内の作品数です。`);
            after = '';
            previous = [];
            await loadValues(initialValues);
        }
        catch (error) {
            if (epoch === requestId)
                $('#metadata-state').textContent = errorMessage(error);
        }
    }
    async function loadValues(initialValues) {
        const field = fields.find(f => f.key === key);
        if (!field)
            return;
        const epoch = ++requestId;
        const values = $('#metadata-values');
        values.setAttribute('aria-busy', 'true');
        values.inert = true;
        $('#metadata-next').disabled = true;
        $('#metadata-previous').disabled = true;
        const condition = state.metadata[key] ?? { list: field.list, exclude: false, values: [] };
        $('#metadata-exclude').checked = ('exclude' in condition ? condition.exclude : false);
        $('#metadata-exclude').closest('label').hidden = field.numeric;
        $('#metadata-query').hidden = field.numeric;
        $('#metadata-validation').hidden = true;
        $('#metadata-vocabulary').hidden = key !== 'ndc';
        if (field.numeric) {
            const bounds = condition.numeric ? condition : { numeric: true, min: '', max: '' };
            const numericField = field;
            $('#metadata-values').innerHTML = `<div class="range-fields"><span class="number-range"><input type="number" class="number-span" aria-label="${text('Minimum', '下限')}" data-bound="min" aria-describedby="metadata-validation" aria-errormessage="metadata-validation" step="1" min="${numericField.min}" max="${bounds.max || numericField.max}" value="${esc(bounds.min ?? '')}" placeholder="${numericField.min}"><button type="button" class="range-window" role="slider" tabindex="0" aria-label="${text('Move range', '範囲を移動')}" aria-valuemin="${numericField.min}" aria-valuemax="${numericField.max}">–</button><input type="number" class="number-span" aria-label="${text('Maximum', '上限')}" data-bound="max" aria-describedby="metadata-validation" aria-errormessage="metadata-validation" step="1" min="${bounds.min || numericField.min}" max="${numericField.max}" value="${esc(bounds.max ?? '')}" placeholder="${numericField.max}"></span></div>`;
            const lower = $('[data-bound=min]'), upper = $('[data-bound=max]'), windowControl = $('.range-window');
            function validRange() {
                const valid = lower.checkValidity() && upper.checkValidity();
                for (const input of [lower, upper])
                    input.setAttribute('aria-invalid', String(!input.checkValidity()));
                $('#metadata-validation').hidden = valid;
                localizeNumberErrors();
                return valid;
            }
            function updateRange(dragging = false) {
                lower.max = upper.value || String(numericField.max);
                upper.min = lower.value || String(numericField.min);
                if (!validRange()) {
                    clearTimeout(searchTimer);
                    return;
                }
                state.metadata[key] = { numeric: true, min: lower.value, max: upper.value };
                windowControl.setAttribute('aria-valuenow', lower.value || String(numericField.min));
                windowControl.setAttribute('aria-valuetext', `${lower.value || numericField.min}–${upper.value || numericField.max}`);
                changedScope(!dragging);
            }
            function commitRange() { if (validRange())
                changedScope(); }
            for (const input of [lower, upper])
                scrubNumber(input, updateRange, commitRange);
            windowControl.setAttribute('aria-valuenow', lower.value || String(numericField.min));
            windowControl.setAttribute('aria-valuetext', `${lower.value || numericField.min}–${upper.value || numericField.max}`);
            let drag;
            const moveRange = (delta, initialLower = Number(lower.value || numericField.min), initialUpper = Number(upper.value || numericField.max), dragging = false) => {
                delta = Math.max(numericField.min - initialLower, Math.min(numericField.max - initialUpper, delta));
                lower.value = String(initialLower + delta);
                upper.value = String(initialUpper + delta);
                updateRange(dragging);
            };
            windowControl.addEventListener('keydown', event => { const delta = { ArrowLeft: -1, ArrowDown: -1, ArrowRight: 1, ArrowUp: 1, PageDown: -Math.max(10, Math.round((numericField.max - numericField.min) / 10)), PageUp: Math.max(10, Math.round((numericField.max - numericField.min) / 10)), Home: -Infinity, End: Infinity }[event.key]; if (delta != null) {
                event.preventDefault();
                moveRange(delta);
            } });
            windowControl.addEventListener('pointerdown', event => { if (event.button !== 0)
                return; event.preventDefault(); windowControl.focus(); windowControl.setPointerCapture(event.pointerId); drag = { x: event.clientX, lastX: event.clientX, lower: Number(lower.value || numericField.min), upper: Number(upper.value || numericField.max), precise: event.shiftKey }; });
            windowControl.addEventListener('pointermove', event => { if (!drag)
                return; if (event.shiftKey !== drag.precise) {
                drag.x = drag.lastX;
                drag.lower = Number(lower.value || numericField.min);
                drag.upper = Number(upper.value || numericField.max);
                drag.precise = event.shiftKey;
            } drag.lastX = event.clientX; moveRange(numberDragDelta(event.clientX - drag.x, 1, drag.precise), drag.lower, drag.upper, true); });
            const finish = () => { if (drag) {
                drag = undefined;
                commitRange();
            } };
            windowControl.addEventListener('pointerup', finish);
            windowControl.addEventListener('pointercancel', finish);
            windowControl.addEventListener('lostpointercapture', finish);
            $('#metadata-next').disabled = true;
            $('#metadata-previous').disabled = true;
            values.removeAttribute('aria-busy');
            values.inert = false;
            return;
        }
        const category = condition.numeric ? { list: false, values: [], exclude: false } : condition;
        const query = new URLSearchParams({ field: key, q: $('#metadata-query').value, after, locale: getLocale() });
        try {
            const data = initialValues ?? await api(`api/questions/data/${$('#corpus').value}/metadata?${query}`);
            if (epoch !== requestId)
                return;
            $('#metadata-values').innerHTML = data.values.map((item, i) => `<label class="metadata-value"><input type="checkbox" data-value="${i}" ${category.values?.some(v => JSON.stringify(v) === JSON.stringify(item.value)) ? 'checked' : ''} ${typeof item.value === 'string' && /["\n\r]/.test(item.value) ? 'disabled' : ''}><span>${esc(item.label)}</span><span class="value-count">${item.documents.toLocaleString()}</span></label>`).join('') || `<p>${text('No matching values.', '一致する値がありません。')}</p>`;
            for (const input of $('#metadata-values').querySelectorAll('input'))
                input.addEventListener('change', () => {
                    const stored = state.metadata[key];
                    const current = stored && !stored.numeric ? stored : { list: field.list, exclude: $('#metadata-exclude').checked, values: [] };
                    const value = data.values[Number(input.dataset.value)].value;
                    current.labels = { ...(current.labels ?? {}), [String(value)]: data.values[Number(input.dataset.value)].label };
                    current.values = current.values.filter(v => JSON.stringify(v) !== JSON.stringify(value));
                    if (input.checked)
                        current.values.push(value);
                    state.metadata[key] = current;
                    changedScope();
                });
            $('#metadata-next').disabled = !data.next;
            $('#metadata-next').onclick = () => { previous.push(after); after = data.next ?? ''; loadValues(); };
            $('#metadata-previous').disabled = !previous.length;
            $('#metadata-previous').onclick = () => { after = previous.pop() ?? ''; loadValues(); };
        }
        catch (error) {
            if (epoch === requestId)
                $('#metadata-values').textContent = errorMessage(error);
        }
        finally {
            if (epoch === requestId) {
                values.removeAttribute('aria-busy');
                values.inert = false;
            }
        }
    }
    function closePicker() { ++pickerEpoch; pickerState = 'closed'; $('#metadata-picker').hidden = true; $('#scope-toggle').setAttribute('aria-expanded', 'false'); $('#scope-toggle').removeAttribute('aria-busy'); }
    async function openPicker() {
        const epoch = ++pickerEpoch;
        pickerState = 'opening';
        $('#scope-toggle').setAttribute('aria-busy', 'true');
        if (!fields.length)
            await loadFields();
        if (epoch !== pickerEpoch)
            return;
        pickerState = 'open';
        $('#metadata-picker').hidden = false;
        $('#scope-toggle').setAttribute('aria-expanded', 'true');
        $('#scope-toggle').removeAttribute('aria-busy');
    }
    $('#scope-toggle').addEventListener('click', () => { if (pickerState === 'closed')
        void openPicker();
    else
        closePicker(); });
    $('#metadata-close').addEventListener('click', () => { closePicker(); $('#scope-toggle').focus(); });
    $('#metadata-field').addEventListener('change', () => { key = $('#metadata-field').value; after = ''; previous = []; $('#metadata-query').value = ''; loadValues(); });
    let valueTimer;
    $('#metadata-query').addEventListener('input', () => { clearTimeout(valueTimer); valueTimer = setTimeout(() => { after = ''; previous = []; loadValues(); }, 120); });
    $('#metadata-exclude').addEventListener('change', () => { const field = fields.find(f => f.key === key); if (!field || field.numeric)
        return; const stored = state.metadata[key]; state.metadata[key] = { ...(stored && !stored.numeric ? stored : { list: field.list, exclude: false, values: [] }), exclude: $('#metadata-exclude').checked }; changedScope(); });
    $('#where').addEventListener('input', () => changedScope());
    $('#metadata-clear').addEventListener('click', () => { state.metadata = Object.create(null); state.raw = ''; $('#where').value = ''; changedScope(); loadValues(); });
    $('#corpus').addEventListener('change', () => { const reopen = pickerState !== 'closed'; closePicker(); ++requestId; state.metadata = Object.create(null); fields = []; $('#metadata-field').replaceChildren(); $('#metadata-values').replaceChildren(); renderChips(); if (reopen)
        void openPicker(); clearTimeout(searchTimer); changed(); search(); });
    function validateReadingNumbers() {
        const inputs = [$('#context-size'), $('#line-limit')], valid = inputs.every(input => input.checkValidity());
        for (const input of inputs)
            input.setAttribute('aria-invalid', String(!input.checkValidity()));
        $('#reading-validation').hidden = valid;
        localizeNumberErrors();
    }
    scrubNumber($('#context-size'), () => { if (!$('#context-size').checkValidity() || $('#context-size').value === '')
        return; state.context = Number($('#context-size').value); validateReadingNumbers(); viewChanged(); changed(); }, validateReadingNumbers);
    scrubNumber($('#line-limit'), () => { if (!$('#line-limit').checkValidity() || $('#line-limit').value === '')
        return; state.lines = Number($('#line-limit').value); validateReadingNumbers(); viewChanged(); changed(); }, validateReadingNumbers);
    renderChips();
    return { state, boundExamples(maximum) { const input = $('#line-limit'); input.disabled = maximum === 0; input.max = String(maximum == null || maximum === 0 ? Number.MAX_SAFE_INTEGER : Math.max(1, Math.min(Number.MAX_SAFE_INTEGER, maximum))); state.lines = Math.min(state.lines, Number(input.max)); if (input.validity.rangeOverflow && !input.validity.stepMismatch && !input.validity.badInput)
            clampNumber(input); if (input.value !== '' && input.checkValidity())
            state.lines = Number(input.value); }, where: () => scopeClause(state.metadata, $('#where').value), entries: () => ({ metadata: JSON.stringify(state.metadata), raw: $('#where').value, context: state.context, lines: state.lines }), restore(params) {
            clearTimeout(searchTimer);
            clearTimeout(valueTimer);
            ++requestId;
            Object.assign(state, questionSettings(params));
            $('#where').value = state.raw;
            $('#context-size').value = String(state.context);
            $('#line-limit').value = String(state.lines);
            for (const input of document.querySelectorAll('.number-span'))
                input.removeAttribute('aria-invalid');
            $('#reading-validation').hidden = true;
            $('#metadata-validation').hidden = true;
            fields = [];
            renderChips();
            if (!$('#metadata-picker').hidden)
                loadFields();
        }, localize() { localizeNumberErrors(); for (const input of document.querySelectorAll('.number-span'))
            input.title = text('Drag horizontally or type a number. Hold Shift for precise dragging', '左右にドラッグ、または数値を入力。Shift キーで細かく調整'); $('#line-limit').setAttribute('aria-label', text('Number of examples', '表示する用例数')); $('#context-size').setAttribute('aria-label', text('Context tokens', '前後の文脈のトークン数')); renderChips(); if (fields.length) {
            const selected = $('#metadata-field').value;
            $('#metadata-field').innerHTML = fields.map(f => `<option value="${esc(f.key)}">${esc(label(f.key))} (${f.present.toLocaleString()})</option>`).join('');
            $('#metadata-field').value = selected;
            if (!$('#metadata-picker').hidden) {
                after = '';
                previous = [];
                loadValues();
            }
        } } };
}
