import { tokenLiteral } from './document.js';

export interface TokenCondition {
  readonly tokenIndex: number;
  readonly key: string;
  readonly value: string;
}

export function addCondition(conditions: readonly TokenCondition[], next: TokenCondition): readonly TokenCondition[] {
  return [...conditions.filter(condition => condition.tokenIndex !== next.tokenIndex || condition.key !== next.key), next];
}

export function patternSource(conditions: readonly TokenCondition[]): string {
  if (!conditions.length) return '';
  const tokens = new Map<number, TokenCondition[]>();
  for (const condition of conditions) {
    const attributes = tokens.get(condition.tokenIndex) ?? [];
    attributes.push(condition);
    tokens.set(condition.tokenIndex, attributes);
  }
  const parts: string[] = [];
  let previous: number | undefined;
  for (const tokenIndex of [...tokens.keys()].sort((a, b) => a - b)) {
    if (previous !== undefined && tokenIndex > previous + 1)
      parts.push(`[]{${tokenIndex - previous - 1}}`);
    const attributes = tokens.get(tokenIndex)!;
    attributes.sort((a, b) => a.key < b.key ? -1 : a.key > b.key ? 1 : 0);
    parts.push(`[${attributes.map(({ key, value }) => `${key}=${tokenLiteral(value)}`).join(' & ')}]`);
    previous = tokenIndex;
  }
  return `probe := ${parts.join(' ')}.`;
}
