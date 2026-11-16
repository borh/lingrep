import test from 'node:test';
import assert from 'node:assert/strict';
import { defaultView, locationURL, parseView, savedQuestion } from './location.js';
import { questionSettings, scopeClause } from './document.js';

test('reader locations preserve their document, occurrence and return page through a URL',()=>{
  const context={...defaultView(),pattern:'かもしれない',document:9,after:21,filter:'こと'};
  const reader={kind:'reader',pattern:context.pattern,document:9,sequence:25,returnTo:context};
  const url=new URL(locationURL('http://localhost:9992/',{question:'どの作品で使うか',context:12},'a'.repeat(32),reader));
  const params=new URLSearchParams(url.hash.slice(1));
  assert.deepEqual(parseView(params.get('view')),reader);
  assert.equal(params.get('evidence'),'a'.repeat(32));
  assert.equal(params.get('question'),'どの作品で使うか');
  assert.equal(params.has('source'),false);
  assert.throws(()=>parseView(JSON.stringify({...reader,sequence:-3})),/invalid/);
});

test('question conditions retain raw codes and numeric bounds and reject invalid imported ranges',()=>{
  const metadata={ndc:{list:true,exclude:false,values:['913'],labels:{913:'913 日本文学・小説・物語'}},source_edition_year:{numeric:true,min:'1940',max:'1960'}};
  const settings=questionSettings(new URLSearchParams({metadata:JSON.stringify(metadata),context:'4',lines:'3'}));
  assert.equal(scopeClause(settings.metadata,settings.raw),'@document.ndc contains "913" & @document.source_edition_year >= 1940 & @document.source_edition_year <= 1960');
  assert.equal(JSON.stringify(settings.metadata),JSON.stringify(metadata));
  assert.equal(scopeClause({},'@document.title = "作品"'),'@document.title = "作品"');
  assert.throws(()=>questionSettings(new URLSearchParams({metadata:JSON.stringify({year:{numeric:true,min:'1960',max:'1940'}})})),/range/);
  assert.throws(()=>savedQuestion({format:'lingrep.question-probe/v1',source:'p := か.'}),/question document/);
});


test('compiler byte spans select the same text after Japanese and astral characters', async () => {
  const {sourceSelection}=await import('./model.js');
  const source='猫😀\nbroken := [upos=VERB.';
  const start=new TextEncoder().encode('猫😀\nbroken := ').length;
  assert.deepEqual(sourceSelection(source,{start,end:start+1}),[source.indexOf('['),source.indexOf('[')+1]);
  assert.deepEqual(sourceSelection(source,{start,end:start}),[source.indexOf('['),source.indexOf('[')]);
  assert.equal(sourceSelection(source,{start:1,end:2}),null);
  assert.equal(sourceSelection(source,{start:0,end:999}),null);
});


test('work-scoped locations retain the unscoped return position and reject invalid work identities',()=>{
  const location={...defaultView(),pattern:'p',scopeDocument:17,returnAfter:41,document:17};
  assert.deepEqual(parseView(JSON.stringify(location)),location);
  assert.throws(()=>parseView(JSON.stringify({...location,scopeDocument:-1})),/invalid/);
  assert.throws(()=>parseView(JSON.stringify({...location,returnAfter:-2})),/invalid/);
});
