import test from 'node:test';
import assert from 'node:assert/strict';
import {serviceURL,workbenchLinkRequest,workbenchHref} from './service.js';
import {seedParams,savedQuestion} from './location.js';

test('services and returned reading URLs stay within the mounted document base',()=>{
  for(const base of ['https://example.test/','https://example.test/nested/base/']){
    for(const path of ['api/questions/runs','/api/read/corpus/1/0/2/document','assets/i18n/ja.json'])assert.equal(serviceURL(path,base),base+path.replace(/^\//,''));
  }
});

test('the analysis-tools link opens the Workbench on the minted permalink within the mount',()=>{
  const body=new URLSearchParams(workbenchLinkRequest('npv := [upos=NOUN] & 名詞.\n','mori'));
  assert.deepEqual([...body],[['pattern','npv := [upos=NOUN] & 名詞.\n'],['corpus','mori'],['derive_compounds','false']]);
  for(const base of ['https://example.test/','https://example.test/nested/base/'])assert.equal(workbenchHref({path:'?v=1&q=abc&k=mori&c=0123456789ab'},base),base+'workbench?v=1&q=abc&k=mori&c=0123456789ab');
  for(const payload of [null,{},{path:42},{path:'/workbench?v=1'},{path:'https://elsewhere.test/?v=1'},{error:'too_large'}])assert.equal(workbenchHref(payload,'https://example.test/'),null);
});

test('hash choices override an inert query seed including an explicit no-run request',()=>{
  const seed={source:'p := 猫.',corpus:'custom',analysis:'expected',raw:'year > 1900',run:'1',notice:'Saved search',lines:'8'};
  const params=seedParams(seed,'#source=q+%3A%3D+犬.&run=0');
  assert.equal(params.get('source'),'q := 犬.');assert.equal(params.get('run'),'0');assert.equal(params.get('corpus'),'custom');assert.equal(params.get('analysis'),'expected');assert.equal(params.get('raw'),'year > 1900');assert.equal(params.get('lines'),'8');
  const live=seedParams(seed,'#evidence=session-result');
  assert.equal(live.get('source'),null);assert.equal(live.get('corpus'),null);assert.equal(live.get('analysis'),null);assert.equal(live.get('run'),null);
  assert.throws(()=>seedParams({run:'yes'},''));
  const doc={format:'lingrep.question/v1',question:'',source:'p := 猫.',corpus:'custom',analysis:'',where:''};
  assert.equal(savedQuestion(doc).format,doc.format);
  assert.equal(savedQuestion({...doc,format:'lingrep.question-probe/v1'}).format,'lingrep.question-probe/v1');
});
