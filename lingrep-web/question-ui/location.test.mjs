import test from 'node:test';
import assert from 'node:assert/strict';
import { defaultView, locationHash, locationURL, parseView, savedQuestion } from './location.js';
import { questionSettings, scopeClause, metadataValueLabel } from './document.js';

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
  const target={pattern:'npv',sentence:4,document:2,roles:[{name:'n',start:0,end:1},{name:'p',start:1,end:2},{name:'v',start:3,end:4}],analysis:'a'.repeat(64),fingerprint:'f'.repeat(64)};
  assert.deepEqual(parseOccurrence(JSON.stringify({...target,extra:1})),target);
  assert.equal(parseOccurrence(null),null);
  for(const bad of ['{',JSON.stringify({...target,sentence:-1}),JSON.stringify({...target,document:undefined}),JSON.stringify({...target,roles:[{name:'n',start:2,end:1}]}),JSON.stringify({...target,analysis:undefined})])
    assert.throws(()=>parseOccurrence(bad),/frequency occurrence/);
  assert.equal(frequencyReturn('frequencies#corpus=c&entry=x'),'frequencies#corpus=c&entry=x');
  for(const bad of ['https://example.org/frequencies#x','//evil/frequencies','javascript:alert(1)','frequenciesx',null])assert.equal(frequencyReturn(bad),null);
});
test("a frequency entry's lines name each occurrence and the position of the opened one",async()=>{
  const {parseEntryLines}=await import('./location.js');
  const lines=[{sentence:3,document:1,roles:[{name:'n',start:0,end:1}]},{sentence:41,document:5,roles:[]}];
  const entry={words:['坂','を','上る'],total:12,at:1,lines};
  assert.deepEqual(parseEntryLines(JSON.stringify({...entry,extra:1})),entry);
  assert.equal(parseEntryLines(null),null);
  const located={...entry,works:2,names:{5:'Natsume_S_Meian'},lines:[{...lines[0],start:0,end:2},lines[1]]};
  assert.deepEqual(parseEntryLines(JSON.stringify(located)),located);
  for(const bad of [{...located,works:-1},{...located,names:{5:3}},{...located,lines:[{...lines[0],start:2,end:1},lines[1]]}])
    assert.throws(()=>parseEntryLines(JSON.stringify(bad)),/frequency entry/);
  for(const bad of ['[',JSON.stringify({...entry,at:2}),JSON.stringify({...entry,at:-1}),JSON.stringify({...entry,words:[1]}),JSON.stringify({...entry,total:1}),JSON.stringify({...entry,lines:[{sentence:-1,document:1,roles:[]}]})])
    assert.throws(()=>parseEntryLines(bad),/frequency entry/);
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

test('translated metadata names retain raw codes and leave predicates unchanged', () => {
  const config={labels:{'@document.subject':{values:{lit:{labels:{en:'Literature',ja:'文学'}},other:{labels:{en:'Literature'}}}}}};
  assert.equal(metadataValueLabel(config,'subject','lit','en'),'lit Literature');
  assert.equal(metadataValueLabel(config,'subject','lit','ja'),'lit 文学');
  assert.equal(metadataValueLabel(config,'subject','other','ja','other Literature'),'other');
  assert.equal(metadataValueLabel(config,'subject',1,'en'),'1');
  assert.equal(metadataValueLabel(config,'subject','unknown','en'),'unknown');
  assert.equal(scopeClause({subject:{list:false,exclude:false,values:['lit'],labels:{lit:'lit 文学'}}}),'@document.subject in {"lit"}');
});

test('a corpus chosen in the address drops the seed analysis that described another corpus',async()=>{
  const {seedParams}=await import('./location.js');
  const seed={source:'p := 猫.',corpus:'natsume',analysis:'natsume-analysis',run:'0'};
  const chosen=seedParams(seed,'#fresh=1&corpus=sample');
  assert.equal(chosen.get('corpus'),'sample');assert.equal(chosen.get('analysis'),null);
  assert.equal(seedParams(seed,'#corpus=sample&analysis=sample-analysis').get('analysis'),'sample-analysis');
  assert.equal(seedParams(seed,'#corpus=natsume').get('analysis'),'natsume-analysis');
});

test('the seeded definition belongs to the seeded source, so a saved search drops it',async()=>{
  const {seedParams}=await import('./location.js');
  const seed={source:'key: [upos]\npos := [].\nnpv := [upos=NOUN] [upos=ADP].',run:'0',pattern:'npv'};
  assert.equal(seedParams(seed,'').get('pattern'),'npv');
  assert.equal(seedParams(seed,'#evidence=r1').get('pattern'),null);
  assert.equal(seedParams(seed,'#pattern=pos').get('pattern'),'pos');
});

test('a stored run supplies the expected analysis only for its own corpus',async()=>{
  const {expectedAnalysis}=await import('./location.js');
  const run={dataset:'natsume',contract_run:{analysis_identity:'natsume-analysis'}};
  assert.equal(expectedAnalysis(new URLSearchParams('corpus=natsume'),run),'natsume-analysis');
  assert.equal(expectedAnalysis(new URLSearchParams('corpus=sample'),run),null);
  assert.equal(expectedAnalysis(new URLSearchParams('corpus=sample&analysis=saved'),run),'saved');
  assert.equal(expectedAnalysis(new URLSearchParams('corpus=natsume'),null),null);
});

test("an entry line reads as a concordance line: its span, its context and the role words outside the span",async()=>{
  const {entryContext}=await import('./location.js');
  const texts=['肥っ','て','坂','を','上る','とき','。'];
  const line={sentence:1,document:0,start:2,end:4,roles:[{name:'n',start:2,end:3},{name:'p',start:3,end:4},{name:'v',start:4,end:5}]};
  const read=entryContext(texts,line,1);
  assert.deepEqual(read.left.map(token=>[token.index,token.attributes.text]),[[1,'て']]);
  assert.equal(read.hit,'坂を');
  assert.deepEqual(read.right.map(token=>[token.index,token.attributes.text]),[[4,'上る']]);
  assert.deepEqual([...read.bound],[[4,['v']]]);
  assert.equal(entryContext(texts,{...line,start:undefined,end:undefined},0).hit,'坂を上る');
});

test('an address names the served catalog by its fingerprint and carries any other source in full',async()=>{
  const {catalogParams,resolveCatalog,locationURL,defaultView}=await import('./location.js');
  const source='key: [lemma]\nかもしれない := [lemma=かも] [lemma=知れる] [lemma=ない].\n'.repeat(40);
  const served={source,fingerprint:'f'.repeat(64)};
  const address=(params)=>new URLSearchParams(new URL(locationURL('http://localhost/',params,null,defaultView())).hash.slice(1));
  const unchanged=address(catalogParams({source,corpus:'natsume',pattern:'かもしれない'},served));
  assert.equal(unchanged.get('source'),null);
  assert.equal(unchanged.get('catalog'),served.fingerprint);
  assert.ok(unchanged.toString().length<address({source}).toString().length/10);
  assert.deepEqual(resolveCatalog(unchanged,served),{changed:null});
  assert.equal(unchanged.get('source'),source);assert.equal(unchanged.get('catalog'),null);
  assert.equal(unchanged.get('pattern'),'かもしれない');
  const edited=address(catalogParams({source:source+'extra := [].\n',corpus:'natsume'},served));
  assert.equal(edited.get('source'),source+'extra := [].\n');assert.equal(edited.get('catalog'),null);
  assert.deepEqual(resolveCatalog(edited,served),{changed:null});
  assert.equal(edited.get('source'),source+'extra := [].\n');
  assert.equal(address(catalogParams({source},{source})).get('source'),source);
});

test('an address written before catalog fingerprints keeps its source',async()=>{
  const {resolveCatalog}=await import('./location.js');
  const params=new URLSearchParams({source:'old := [upos=NOUN].',run:'1'});
  assert.deepEqual(resolveCatalog(params,{source:'served := [].',fingerprint:'f'.repeat(64)}),{changed:null});
  assert.equal(params.get('source'),'old := [upos=NOUN].');assert.equal(params.get('run'),'1');
});

test('a fingerprint of another catalog reports the change and holds the served source for confirmation',async()=>{
  const {resolveCatalog}=await import('./location.js');
  const served={source:'served := [].',fingerprint:'f'.repeat(64)};
  for(const [linked,catalog] of [['a'.repeat(64),served],['a'.repeat(64),{source:served.source}],['',served]]){
    const params=new URLSearchParams({catalog:linked,run:'1',pattern:'かもしれない'});
    assert.deepEqual(resolveCatalog(params,catalog),{changed:linked});
    assert.equal(params.get('source'),served.source);assert.equal(params.get('catalog'),null);
    assert.equal(params.get('run'),'1');assert.equal(params.get('pattern'),'かもしれない');
  }
});

test('a fingerprint in the address replaces the seeded source',async()=>{
  const {seedParams}=await import('./location.js');
  const params=seedParams({source:'seed := [].',run:'0',pattern:'seed'},'#catalog='+'a'.repeat(64));
  assert.equal(params.get('source'),null);assert.equal(params.get('catalog'),'a'.repeat(64));
});
