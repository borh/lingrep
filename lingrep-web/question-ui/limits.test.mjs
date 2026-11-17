import test from 'node:test';
import assert from 'node:assert/strict';
import {overflowingDefinitions,cliCommand,diagnosticText} from './model.js';

test('an overflow is attributed to the definitions holding at least half of the largest count',()=>{
  assert.deepEqual(overflowingDefinitions({nouns:20228,npv:7257,npv_adjacent:4882,pos:85187,tags:85187}),['pos','tags']);
  assert.deepEqual(overflowingDefinitions({}),[]);
  assert.deepEqual(overflowingDefinitions({a:0,b:0}),[]);
});

test('the CLI command keeps the condition and selects the definition and document a limited search ran',()=>{
  assert.equal(cliCommand('',null),'ling --json matches --corpus CORPUS.lingcorpus --patterns query.lx --context 24');
  assert.equal(cliCommand("@document.author = 'x'",{pattern:null,document:null}),"ling --json matches --corpus CORPUS.lingcorpus --patterns query.lx --context 24 --where '@document.author = '\\''x'\\'''");
  assert.equal(cliCommand('',{pattern:'npv',document:5}),`ling --json matches --corpus CORPUS.lingcorpus --patterns query.lx --context 24 | jq -c 'select(.type == "match" and .data.pattern == "npv" and .data.document_ordinal == 5)'`);
  assert.equal(cliCommand('',{pattern:'npv',document:null}),`ling --json matches --corpus CORPUS.lingcorpus --patterns query.lx --context 24 | jq -c 'select(.type == "match" and .data.pattern == "npv")'`);
});

test('a catalog diagnostic names its line and column in the page language without the internal file name',()=>{
  const diagnostic={schema:'lingrep.diagnostic/v1',type:'diagnostic',data:{stage:'catalog',severity:'error',message:'invalid lingrep patterns syntax',location:{line:1,column:1,source_span:{start:0,end:17}}}};
  assert.match(diagnosticText(diagnostic,'ja'),/^1 行 1 文字目: パターンの書き方に誤りがあります。/);
  assert.match(diagnosticText(diagnostic,'en'),/^Line 1, column 1: The pattern is not written correctly\./);
  const other={...diagnostic,data:{...diagnostic.data,message:'unknown key `orth`',location:{line:3}}};
  assert.equal(diagnosticText(other,'ja'),'3 行: unknown key `orth`');
});

test('the question page reads the language setting shared with Frequencies before its former key',async()=>{
  const {storedLocale}=await import('./model.js');
  const storage=entries=>({getItem:key=>entries[key]??null});
  assert.equal(storedLocale(storage({'lingrep-web:locale:v1':'en','ling-questions-locale':'ja'}),'ja-JP'),'en');
  assert.equal(storedLocale(storage({'ling-questions-locale':'ja'}),'en-US'),'ja');
  assert.equal(storedLocale(storage({}),'ja'),'ja');
  assert.equal(storedLocale(null,'en-GB'),'en');
});

test('a document is named by its metadata title, else by its source name without the corpus file extension',async()=>{
  const {documentTitle}=await import('./model.js');
  assert.equal(documentTitle({source_name:'Natsume_S_Bocchan.conllu'}),'Natsume_S_Bocchan');
  assert.equal(documentTitle({source_name:'a.conllu',source_metadata:{document:{title:'坊っちゃん'}}}),'坊っちゃん');
  assert.equal(documentTitle({source_name:'notes.v2'}),'notes.v2');
});

test('a definition is found on its own line, after an optional keyword, and not inside another name or a body',async()=>{
  const {definitionOffset}=await import('./model.js');
  const source='# npv := comment\nkey: [upos]\npattern npv_adjacent := [].\n  npv := [upos=NOUN] @npv_adjacent.\n';
  assert.equal(definitionOffset(source,'npv'),source.indexOf('  npv :='));
  assert.equal(definitionOffset(source,'npv_adjacent'),source.indexOf('pattern npv_adjacent'));
  assert.equal(definitionOffset(source,'upos'),-1);
  assert.equal(definitionOffset('a.b := [].','a.b'),0);
  assert.equal(definitionOffset('axb := [].','a.b'),-1);
});

test('token details list the dependency right after the portable keys and leave raw CoNLL-U fields to all attributes',async()=>{
  const {basicAttributeKeys}=await import('./model.js');
  const token={text:'滑か',lemma:'滑らか',upos:'ADJ',xpos:'形状詞','conllu.misc':['BunsetuBILabel=B','SpaceAfter=No'],'conllu.feats':'_','dep.head':'3','dep.rel':'acl'};
  assert.deepEqual(basicAttributeKeys(token),['text','lemma','upos','xpos','dep.head','dep.rel']);
  assert.equal(basicAttributeKeys(Object.fromEntries(Array.from({length:30},(_,i)=>[`k${i}`,'v']))).length,18);
});
