import { parseView } from './location.js';
export function parseSections(text) {
    if (!text)
        return [];
    let values;
    try {
        values = JSON.parse(text);
    }
    catch {
        throw new Error('The linked searches could not be read. Open an individual search.');
    }
    if (!Array.isArray(values))
        throw new Error('The linked searches could not be read. Open an individual search.');
    const ids = new Set();
    return values.map((value) => {
        if (!value || typeof value !== 'object' || !('id' in value) || typeof value.id !== 'string' || !value.id || ids.has(value.id) || !('origin' in value))
            throw new Error('The linked searches contain an invalid search reference.');
        let origin = null;
        if (value.origin !== null) {
            const item = value.origin;
            if (!item || typeof item !== 'object' || !('parent' in item) || typeof item.parent !== 'string' || !ids.has(item.parent) || !('label' in item) || typeof item.label !== 'string' || !('view' in item))
                throw new Error('The linked searches contain an invalid parent reference.');
            origin = { parent: item.parent, label: item.label, view: parseView(JSON.stringify(item.view)) };
        }
        let recipe;
        if ('recipe' in value) {
            const item = value.recipe;
            if (!item || typeof item !== 'object' || !('source' in item) || typeof item.source !== 'string' || !('dataset' in item) || typeof item.dataset !== 'string' || !('where' in item) || typeof item.where !== 'string' || !('analysis' in item) || (item.analysis !== null && typeof item.analysis !== 'string'))
                throw new Error('A linked search contains an invalid saved query.');
            recipe = { source: item.source, dataset: item.dataset, where: item.where, analysis: item.analysis };
        }
        ids.add(value.id);
        return { id: value.id, origin, ...(recipe ? { recipe } : {}) };
    });
}
export async function restoreSections(links, available, execute, progress) {
    let manifest = links;
    for (let index = 0; index < manifest.length; index++) {
        const link = manifest[index];
        if (available.has(link.id))
            continue;
        if (!link.recipe)
            continue;
        const id = await execute({ ...link, recipe: link.recipe });
        manifest = manifest.map(item => ({ ...item, id: item.id === link.id ? id : item.id, origin: item.origin?.parent === link.id ? { ...item.origin, parent: id } : item.origin }));
        progress(manifest);
    }
    return manifest;
}
