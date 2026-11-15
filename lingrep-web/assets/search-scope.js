import { t, localize, onLocaleChange } from "./i18n.js";

export function defaultScope(labels, preferred) {
    const selected = labels.filter(label => preferred.some(name => label.toLowerCase() === name.toLowerCase()));
    return selected.length
        ? { included_sections: selected.sort(), include_unlabelled: false }
        : { included_sections: null, include_unlabelled: true };
}

export function readScope(control) {
    const inputs = [...control.querySelectorAll("[data-scope-include]")];
    const selected = inputs.filter(input => input.checked).map(input => input.value).sort();
    const scope = {
        included_sections: selected.length === inputs.length ? null : selected,
        include_unlabelled: control.querySelector("[data-scope-unlabelled]").checked,
    };
    const clause = control.querySelector("[data-scope-clause]")?.value.trim();
    if (clause) scope.clause = clause;
    return scope;
}

let useDefault = false;

function seed(control, scope) {
    const clause = control.querySelector("[data-scope-clause]");
    if (clause && "clause" in scope) clause.value = scope.clause ?? "";
    control.querySelector("[data-scope-unlabelled]").checked = scope.include_unlabelled;
    for (const input of control.querySelectorAll("[data-scope-include]")) {
        input.checked = scope.included_sections === null || scope.included_sections.includes(input.value);
    }
}

function update(control, changed = true) {
    if (!document.querySelector(".corpus-context[data-corpus-digest]")) return;
    const scope = readScope(control);
    control.querySelector("[data-search-scope-value]").value = JSON.stringify(scope);
    const summary = control.querySelector("[data-scope-summary]");
    delete summary.dataset.i18n;
    const available = [...control.querySelectorAll("[data-scope-include]")].map(input => input.value);
    const imrad = defaultScope(available, JSON.parse(control.dataset.imradLabels));
    const labels = scope.included_sections === null ? [t("scope.all_sections")]
        : JSON.stringify(scope.included_sections) === JSON.stringify(imrad.included_sections)
            ? [t("scope.main")] : [...scope.included_sections];
    if (scope.include_unlabelled) labels.push(t("scope.unlabelled_short"));
    const sections = scope.included_sections === null && scope.include_unlabelled
        ? t("scope.all") : labels.join(", ") || t("scope.none");
    summary.textContent = scope.clause ? `${sections}; ${t("scope.where")} ${scope.clause}` : sections;
    if (changed) control.querySelector("[data-scope-pending]").hidden = false;
    control.dispatchEvent(new CustomEvent("scope:updated", { bubbles: true }));
}

function refreshVocabulary() {
    const control = document.querySelector("[data-search-scope]");
    if (!control || !document.querySelector(".corpus-context[data-corpus-digest]")) return;
    for (const input of control.querySelectorAll("button, input")) input.disabled = false;
    const labels = [...document.querySelectorAll("[data-corpus-section-label]")].map(node => node.dataset.corpusSectionLabel).sort();
    const options = control.querySelector("[data-search-scope-options]");
    const known = [...options.querySelectorAll("input")].map(input => input.value).sort();
    const stored = JSON.parse(control.querySelector("[data-search-scope-value]").value);
    const scope = useDefault ? defaultScope(labels, JSON.parse(control.dataset.imradLabels)) : stored;
    if (JSON.stringify(known) !== JSON.stringify(labels)) {
        options.replaceChildren(...labels.map(label => {
            const row = document.createElement("label");
            row.className = "section-scope-option";
            const input = document.createElement("input");
            input.type = "checkbox";
            input.dataset.scopeInclude = "";
            input.value = label;
            const text = document.createElement("span");
            text.textContent = label;
            row.append(input, text);
            return row;
        }));
    }
    seed(control, scope);
    update(control, false);
}

if (typeof document !== "undefined") {
    const control = document.querySelector("[data-search-scope]");
    if (control) {
        const ready = Boolean(document.querySelector(".corpus-context[data-corpus-digest]"));
        for (const input of control.querySelectorAll("button, input:not([type=hidden])")) input.disabled = !ready;
        const seeded = JSON.parse(control.querySelector("[data-search-scope-value]").value);
        useDefault = seeded === null;
        if (seeded) seed(control, seeded);
        refreshVocabulary();
    }
    const changed = event => {
        const control = event.target.closest("[data-search-scope]");
        if (control) { useDefault = false; update(control); }
    };
    document.addEventListener("change", changed);
    document.addEventListener("input", event => {
        if (event.target.matches("[data-scope-clause]")) changed(event);
    });
    document.addEventListener("click", event => {
        const preset = event.target.closest("[data-scope-preset]");
        if (!preset) return;
        const control = preset.closest("[data-search-scope]");
        const labels = [...control.querySelectorAll("[data-scope-include]")].map(input => input.value);
        useDefault = preset.dataset.scopePreset === "main";
        seed(control, preset.dataset.scopePreset === "main"
            ? defaultScope(labels, JSON.parse(control.dataset.imradLabels))
            : { included_sections: null, include_unlabelled: true });
        update(control);
    });
    document.addEventListener("htmx:configRequest", event => {
        const control = event.detail.elt.querySelector?.("[data-search-scope]");
        if (!control) return;
        if (!document.querySelector(".corpus-context[data-corpus-digest]")) {
            event.detail.elt.setAttribute("data-run-when-ready", "");
            event.preventDefault();
            return;
        }
        refreshVocabulary();
        update(control, false);
        event.detail.parameters.search_scope = control.querySelector("[data-search-scope-value]").value;
        control.querySelector("[data-scope-pending]").hidden = true;
        control.open = false;
        useDefault = false;
    });
    document.addEventListener("htmx:afterSwap", refreshVocabulary);
    onLocaleChange(() => {
        const control = document.querySelector("[data-search-scope]");
        if (control) { localize(control); update(control, false); }
    });
}
