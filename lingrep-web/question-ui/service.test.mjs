import test from 'node:test';
import assert from 'node:assert/strict';
import {serviceURL} from './service.js';
import {seedParams,savedQuestion} from './location.js';

test('services and returned reading URLs stay within the mounted document base',()=>{
  for(const base of ['https://example.test/','https://example.test/nested/base/']){
    for(const path of ['api/questions/runs','/api/read/corpus/1/0/2/document','assets/i18n/ja.json'])assert.equal(serviceURL(path,base),base+path.replace(/^\//,''));
  }
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
