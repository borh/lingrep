import test from 'node:test';
import assert from 'node:assert/strict';
import {parseSnapshot,parseStoredSentence,runningSnapshot,attributeMembers} from './model.js';
import {addCondition,patternSource} from './composer.js';

test('provider string sets survive snapshot and sentence parsing without becoming scalar JSON',()=>{
  const token={index:0,attributes:{text:'猫',lemma:'猫','dep.enhanced':['nsubj','obl:agent']}};
  const row={sequence:0,pattern:'p',span:{start:0,end:1},corpus_sentence_ordinal:3,document_ordinal:1,source_name:'work',surface:'猫',tokens:[token]};
  const snapshot={...runningSnapshot('run',{source:'p := 猫.',dataset:'ud',where:''}),phase:'complete',rows:{p:[row]}};
  assert.deepEqual(parseSnapshot(snapshot).rows.p[0].tokens[0],token);
  assert.deepEqual(parseStoredSentence({analysis_identity:'identity',sentence:3,tokens:[token]}).tokens[0],token);
  assert.deepEqual(attributeMembers(token.attributes['dep.enhanced']),['nsubj','obl:agent']);
  assert.deepEqual(attributeMembers('猫'),['猫']);
  assert.deepEqual(attributeMembers(undefined),[]);
  for(const value of [123,['nsubj',3],null])assert.throws(()=>parseStoredSentence({analysis_identity:'i',sentence:0,tokens:[{index:0,attributes:{text:'猫',bad:value}}]}));
  assert.throws(()=>parseStoredSentence({analysis_identity:'i',sentence:0,tokens:[{index:0,attributes:{text:['猫']}}]}));
});

test('selecting an individual provider-set member composes that atom and replaces the prior member',()=>{
  const members=attributeMembers(['nsubj','obl:agent']);
  const first=addCondition([],{tokenIndex:0,key:'dep.enhanced',value:members[0]});
  const next=addCondition(first,{tokenIndex:0,key:'dep.enhanced',value:members[1]});
  assert.equal(patternSource(first),'probe := [dep.enhanced=nsubj].');
  assert.equal(patternSource(next),'probe := [dep.enhanced=obl\\u003aagent].');
  assert.equal(first[0].value,'nsubj');
});
