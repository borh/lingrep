import test from 'node:test';
import assert from 'node:assert/strict';
import { defaultView, locationHash, locationURL, parseView, savedQuestion } from './location.js';
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

test('a frequency handoff names one occurrence and returns only to the frequency view',async()=>{
  const {parseOccurrence,frequencyReturn}=await import('./location.js');
  const target={pattern:'npv',sentence:4,roles:[{name:'n',start:0,end:1},{name:'p',start:1,end:2},{name:'v',start:3,end:4}],analysis:'a'.repeat(64),fingerprint:'f'.repeat(64)};
  assert.deepEqual(parseOccurrence(JSON.stringify({...target,extra:1})),target);
  assert.equal(parseOccurrence(null),null);
  for(const bad of ['{',JSON.stringify({...target,sentence:-1}),JSON.stringify({...target,roles:[{name:'n',start:2,end:1}]}),JSON.stringify({...target,analysis:undefined})])
    assert.throws(()=>parseOccurrence(bad),/frequency occurrence/);
  assert.equal(frequencyReturn('frequencies#corpus=c&entry=x'),'frequencies#corpus=c&entry=x');
  for(const bad of ['https://example.org/frequencies#x','//evil/frequencies','javascript:alert(1)','frequenciesx',null])assert.equal(frequencyReturn(bad),null);
});
test('example counts above thirty survive saved settings while context remains bounded',()=>{
  const settings=questionSettings(new URLSearchParams({lines:'1000',context:'999'}));
  assert.equal(settings.lines,1000);
  assert.equal(settings.context,24);
});


test('number drags preserve small steps and cover large ranges with a short gesture',async()=>{
  const {numberDragDelta}=await import('./document.js');
  assert.equal(numberDragDelta(16),2);
  assert.ok(numberDragDelta(360)>=100000);
  assert.equal(numberDragDelta(360,1,true),45);
  for(let distance=8;distance<=400;distance+=8){
    assert.equal(numberDragDelta(-distance),-numberDragDelta(distance));
    assert.ok(numberDragDelta(distance)>=numberDragDelta(distance-8));
  }
  assert.equal(numberDragDelta(1000000),Number.MAX_SAFE_INTEGER);
});


test('standalone question fragments preserve reader locations and literal query text',()=>{
  const view={kind:'example',pattern:'かもしれない',document:5,sequence:12,returnTo:defaultView()};
  for(const evidence of [null,'a'.repeat(32)]){
    const values={source:'p := [text="a+b & \'()!"].',context:24,lines:8000};
    const hash=locationHash(values,evidence,view);
    assert.equal(hash,new URL(locationURL('http://localhost:9992/?old=1#old',values,evidence,view)).hash);
    const params=new URLSearchParams(hash.slice(1));
    assert.equal(params.get('source'),values.source);
    assert.equal(params.get('lines'),'8000');
    assert.equal(params.get('evidence'),evidence);
    assert.deepEqual(parseView(params.get('view')),view);
  }
});
