import test from 'node:test';
import assert from 'node:assert/strict';
import { begin, receive, receiveUpdate, visible } from './state.mjs';
import { runningSnapshot, parseUpdate } from './model.js';
const attempt = id => runningSnapshot(id,{source:'pattern',dataset:'author',where:''});

const completed = { ...attempt('first'), phase:'complete', source:'first pattern', received:43 };
const initial = { current:completed, completed, previous:null };

test('late results from a superseded search leave the current and displayed searches unchanged', () => {
  const current = begin(initial,attempt('second'));
  assert.equal(receive(current,{id:'first',phase:'complete',received:99}),current);
  assert.equal(visible(current,'keep'),completed);
});

test('failed and stopped replacements preserve the last completed result and its download identity', () => {
  for (const phase of ['failed','cancelled']) {
    const failed=receive(begin(initial,attempt('second')),{id:'second',phase,received:8});
    assert.equal(failed.completed,completed);
    assert.equal(visible(failed,'keep').id,'first');
    assert.equal(begin(failed,attempt('third')).previous,completed);
  }
});

test('only completion replaces the result shown under the keep-completed behavior', () => {
  const running=receive(begin(initial,attempt('second')),{id:'second',phase:'running',received:8});
  assert.equal(visible(running,'keep'),completed);
  assert.equal(visible(running,'live'),running.current);
  const final={id:'second',phase:'complete',received:13};
  const state=receive(running,final);
  assert.equal(visible(state,'keep'),final);
  assert.equal(state.completed,final);
  assert.equal(receive(state,{id:'second',phase:'running',received:8}),state);
});


test('incremental updates preserve omitted counts and share unchanged example records',()=>{
  const row={sequence:1,pattern:'p',span:{start:0,end:1},corpus_sentence_ordinal:3,document_ordinal:2,source_name:'work',surface:'か',tokens:[{index:0,attributes:{text:'か'}}]};
  const current=begin({current:null,previous:null,completed:null},attempt('stream'));
  const first=receiveUpdate(current,parseUpdate(JSON.stringify({id:'stream',reset:true,patch:{...attempt('stream'),version:1,counts:{p:1,q:0}},append:{p:[row]}})));
  const second=receiveUpdate(first,{id:'stream',reset:false,patch:{version:2,counts:{p:2},received:2},append:{p:[{...row,sequence:2}]}});
  assert.deepEqual(second.current.counts,{p:2,q:0});
  assert.equal(second.current.rows.p[0],first.current.rows.p[0]);
  const progress=receiveUpdate(second,{id:'stream',reset:false,patch:{elapsed:2},append:{}});
  assert.equal(progress.current.rows,second.current.rows);
  const final=receiveUpdate(progress,{id:'stream',reset:false,patch:{phase:'complete',version:3},append:{}});
  assert.equal(final.completed,final.current);
  assert.equal(receiveUpdate(final,{id:'stream',reset:false,patch:{phase:'failed',version:4},append:{}}),final);
  assert.equal(receiveUpdate(second,{id:'stream',reset:false,patch:{version:2},append:{p:[row]}}),second);
  assert.equal(receiveUpdate(second,{id:'older',reset:true,patch:{version:99},append:{}}),second);
});

test('stream updates reject changed query identity and invalid example records',()=>{
  const current=begin(initial,attempt('stream'));
  assert.throws(()=>receiveUpdate(current,{id:'stream',reset:false,patch:{source:'another query'},append:{}}),/identity/);
  assert.throws(()=>parseUpdate(JSON.stringify({id:'stream',reset:true,patch:{version:1},append:{p:[{sequence:1}]}})),/example/);
});

test('pattern names that match object properties retain their example records',()=>{
  const row={sequence:1,pattern:'constructor',span:{start:0,end:1},corpus_sentence_ordinal:3,document_ordinal:2,source_name:'work',surface:'か',tokens:[{index:0,attributes:{text:'か'}}]};
  const current=begin({current:null,previous:null,completed:null},attempt('stream'));
  const initial=receiveUpdate(current,{id:'stream',reset:true,patch:{version:1},append:{}});
  const state=receiveUpdate(initial,{id:'stream',reset:false,patch:{version:2,definitions:['constructor'],counts:{constructor:1}},append:{constructor:[row]}});
  assert.deepEqual(state.current.rows.constructor,[row]);
});


test('search notices survive progress patches and remain attached to retained evidence',()=>{
  const first=receiveUpdate(begin({current:null,previous:null,completed:null},attempt('warned')),parseUpdate(JSON.stringify({id:'warned',reset:true,patch:{version:1,notices:['Attribute dep.rel is unavailable.','2 oversized sentences were omitted.']},append:{}})));
  const completed=receiveUpdate(first,{id:'warned',reset:false,patch:{version:2,phase:'complete'},append:{}});
  const replacement=begin(completed,attempt('replacement'));
  assert.deepEqual(visible(replacement,'keep').notices,['Attribute dep.rel is unavailable.','2 oversized sentences were omitted.']);
  for(const notices of ['warning',[7],[null],{}])assert.throws(()=>parseUpdate(JSON.stringify({id:'warned',reset:false,patch:{notices},append:{}})),/notices/);
});

test('changed-analysis and result-limit failures are shown in the page language and other errors verbatim',async()=>{
  const {runError}=await import('./model.js');
  const t=key=>({analysisChanged:'この問いを保存したときのコーパスは変更されています。',resultLimit:'結果が多すぎて保持できません。'})[key]??key;
  const failed={...runningSnapshot('r',{source:'',dataset:'d',where:''}),phase:'failed',error:'The corpus saved with this question is unavailable.'};
  assert.equal(runError({...failed,error_code:'analysis_changed'},t),'この問いを保存したときのコーパスは変更されています。');
  assert.equal(runError({...failed,error_code:'result_limit'},t),'結果が多すぎて保持できません。');
  assert.equal(parseUpdate(JSON.stringify({id:'r',reset:false,patch:{phase:'failed',error:'x',error_code:'result_limit',version:1},append:{}})).patch.error_code,'result_limit');
  assert.equal(runError({...failed,error_code:null},t),'The corpus saved with this question is unavailable.');
  assert.equal(runError(runningSnapshot('r',{source:'',dataset:'d',where:''}),t),null);
  assert.equal(parseUpdate(JSON.stringify({id:'r',reset:false,patch:{phase:'failed',error:'x',error_code:'analysis_changed',version:1},append:{}})).patch.error_code,'analysis_changed');
  assert.throws(()=>parseUpdate(JSON.stringify({id:'r',reset:false,patch:{error_code:7,version:1},append:{}})),/error_code/);
});
