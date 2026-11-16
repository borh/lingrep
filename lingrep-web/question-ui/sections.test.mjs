import test from 'node:test';
import assert from 'node:assert/strict';
import {parseSections} from './sections.js';
import {defaultView} from './location.js';

test('linked searches retain ordered parent references and the originating example',()=>{
  const origin={parent:'first',label:'作品 · かもしれない',view:{kind:'example',pattern:'first',document:56,sequence:13,returnTo:defaultView()}};
  const links=[{id:'first',origin:null},{id:'second',origin},{id:'third',origin:{...origin,parent:'second'}}];
  assert.deepEqual(parseSections(JSON.stringify(links)),links);
  assert.deepEqual(parseSections(null),[]);
});

test('linked search documents reject duplicate identities, missing parents, cycles, and invalid example locations',()=>{
  for(const links of [
    [{id:'same',origin:null},{id:'same',origin:null}],
    [{id:'child',origin:{parent:'missing',label:'',view:defaultView()}}],
    [{id:'self',origin:{parent:'self',label:'',view:defaultView()}}],
    [{id:'root',origin:null},{id:'child',origin:{parent:'root',label:'',view:{kind:'example'}}}],
  ])assert.throws(()=>parseSections(JSON.stringify(links)));
  assert.throws(()=>parseSections('{'),/The linked searches could not be read/);
});

test('saved linked recipes survive expiry and sequential replay remaps parent identities',async()=>{
  const {restoreSections}=await import('./sections.js');
  const recipe={source:'p := 猫.',dataset:'custom',where:'year > 1900',analysis:'expected'};
  const links=parseSections(JSON.stringify([{id:'old-root',origin:null,recipe},{id:'old-child',origin:{parent:'old-root',label:'猫',view:defaultView()},recipe:{...recipe,source:'q := 犬.'}}]));
  let active=0;const seen=[],progress=[];
  const result=await restoreSections(links,new Set(),async link=>{assert.equal(++active,1);seen.push(link);await Promise.resolve();--active;return 'new-'+link.id;},manifest=>progress.push(manifest));
  assert.deepEqual(seen.map(link=>link.recipe.source),['p := 猫.','q := 犬.']);
  assert.equal(seen[1].origin.parent,'new-old-root');
  assert.equal(result[1].origin.parent,'new-old-root');
  assert.deepEqual(progress[0][1].recipe,links[1].recipe);
  assert.equal(links[1].origin.parent,'old-root');
});

test('interrupted restore retains recipes and completed replacements for an explicit retry',async()=>{
  const {restoreSections}=await import('./sections.js');
  const recipe={source:'p := 猫.',dataset:'custom',where:'',analysis:null};
  const links=[{id:'root',origin:null,recipe},{id:'child',origin:{parent:'root',label:'猫',view:defaultView()},recipe}];
  let retained=links;
  await assert.rejects(restoreSections(links,new Set(),async link=>{if(link.id==='child')throw new Error('interrupted');return 'new-root';},next=>{retained=next;}),/interrupted/);
  assert.equal(retained[0].id,'new-root');assert.equal(retained[1].origin.parent,'new-root');assert.deepEqual(retained.map(s=>s.recipe),[recipe,recipe]);
  const calls=[];
  const result=await restoreSections(retained,new Set(['new-root']),async link=>{calls.push(link.id);return 'new-child';},()=>{});
  assert.deepEqual(calls,['child']);assert.equal(result[1].origin.parent,'new-root');
  const legacy=[{id:'legacy',origin:null}];
  assert.deepEqual(await restoreSections(legacy,new Set(),async()=>{throw Error('must not scan');},()=>{}),legacy);
});
