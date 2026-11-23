import assert from 'node:assert/strict';
import test from 'node:test';
import { workExcerpts } from './model.js';

const token = (index, text, space_before = true) => ({index, attributes:{text}, space_before});
const sentence = ['the', 'two', 'cat', 'examples', 'arrived', 'today'].map((text,index)=>token(index,text));
const match = (sequence, start, end, ordinal = 0) => ({
  sequence,
  corpus_sentence_ordinal: ordinal,
  span: {start,end},
  left_context: sentence.slice(0,start),
  tokens: sentence.slice(start,end),
  right_context: sentence.slice(end),
});

test('work excerpts share context in source order and keep overlapping hits addressable', () => {
  const rows = [match(5,2,4), match(6,3,4), match(7,4,5)];
  const [passage] = workExcerpts(rows,1);
  assert.deepEqual(passage.hits.map(hit=>[hit.row.sequence,hit.before,hit.text]),[
    [5,'two ','cat examples'],
    [6,'',''],
    [7,' ','arrived'],
  ]);
  assert.equal(passage.after,' today');
});

test('work excerpts separate gaps, sentence boundaries and omitted previews', () => {
  const rows = [match(1,0,1), match(2,4,5), match(3,0,1,1),
    {...match(4,1,2,1),preview_omitted:true,tokens:[]}, match(5,2,3,1)];
  assert.deepEqual(workExcerpts(rows,0).map(passage=>passage.hits.map(hit=>hit.row.sequence)),
    [[1],[2],[3],[4],[5]]);
});
