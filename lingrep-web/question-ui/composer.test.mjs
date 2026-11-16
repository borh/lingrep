import test from 'node:test';
import assert from 'node:assert/strict';
import { addCondition, patternSource } from './composer.js';

test('attributes selected on one token form one conjunctive token predicate', () => {
  const conditions = [
    { tokenIndex: 4, key: 'upos', value: 'VERB' },
    { tokenIndex: 4, key: 'lemma', value: '書く' },
  ];
  assert.equal(patternSource(conditions), 'probe := [lemma=書く & upos=VERB].');
  assert.deepEqual(conditions.map(condition => condition.key), ['upos', 'lemma']);
  assert.equal(patternSource([]), '');
});

test('token positions determine sequence order and exact omitted-token counts', () => {
  assert.equal(patternSource([
    { tokenIndex: 9, key: 'lemma', value: '読む' },
    { tokenIndex: 3, key: 'text', value: '本' },
    { tokenIndex: 4, key: 'upos', value: 'ADP' },
    { tokenIndex: 6, key: 'lemma', value: 'よく' },
  ]), 'probe := [text=本] [upos=ADP] []{1} [lemma=よく] []{2} [lemma=読む].');
});

test('adding a condition replaces only the same token and attribute without mutating prior selections', () => {
  const prior = Object.freeze([
    Object.freeze({ tokenIndex: 2, key: 'lemma', value: '書く' }),
    Object.freeze({ tokenIndex: 2, key: 'upos', value: 'VERB' }),
    Object.freeze({ tokenIndex: 3, key: 'lemma', value: '書く' }),
  ]);
  const next = { tokenIndex: 2, key: 'lemma', value: '読む' };
  const replaced = addCondition(prior, next);
  assert.equal(prior[0].value, '書く');
  assert.equal(replaced.length, 3);
  assert.equal(patternSource(replaced), 'probe := [lemma=読む & upos=VERB] [lemma=書く].');
  assert.deepEqual(addCondition(replaced, next), replaced);
  assert.equal(addCondition(replaced, { tokenIndex: 3, key: 'upos', value: 'NOUN' }).length, 4);
});

test('literal punctuation, spaces, backslashes and astral text remain attribute values', () => {
  assert.equal(patternSource([
    { tokenIndex: 0, key: 'text', value: '猫.[ ]&|\\😀' },
  ]), 'probe := [text=猫\\u002e\\u005b\\u0020\\u005d\\u0026\\u007c\\u005c😀].');
});
