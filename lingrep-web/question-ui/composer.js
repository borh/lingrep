import { tokenLiteral } from './document.js';
export function addCondition(conditions, next) {
    return [...conditions.filter(condition => condition.tokenIndex !== next.tokenIndex || condition.key !== next.key), next];
}
export function patternSource(conditions) {
    if (!conditions.length)
        return '';
    const tokens = new Map();
    for (const condition of conditions) {
        const attributes = tokens.get(condition.tokenIndex) ?? [];
        attributes.push(condition);
        tokens.set(condition.tokenIndex, attributes);
    }
    const parts = [];
    let previous;
    for (const tokenIndex of [...tokens.keys()].sort((a, b) => a - b)) {
        if (previous !== undefined && tokenIndex > previous + 1)
            parts.push(`[]{${tokenIndex - previous - 1}}`);
        const attributes = tokens.get(tokenIndex);
        attributes.sort((a, b) => a.key < b.key ? -1 : a.key > b.key ? 1 : 0);
        parts.push(`[${attributes.map(({ key, value }) => `${key}=${tokenLiteral(value)}`).join(' & ')}]`);
        previous = tokenIndex;
    }
    return `probe := ${parts.join(' ')}.`;
}
