import test from 'node:test';
import assert from 'node:assert/strict';
import {evidenceWords,hasDependencies,bunsetsuStarts,dependencyUnits,focusLayout,verticalArcs} from './dependency.js';

const conllu=rows=>rows.map(([text,upos,head,rel,begins],index)=>({index,attributes:{text,upos,'dep.head':String(head),'dep.rel':rel,'conllu.misc':[`BunsetuBILabel=${begins?'B':'I'}`,'SpaceAfter=No']}}));
const ginza=rows=>rows.map(([text,upos,head,rel,begins],index)=>({index,attributes:{text,upos,'dep.head':String(head),'dep.rel':rel,'spacy.ginza.bunsetu':begins?'B':'I'}}));
const unlabelled=rows=>rows.map(([text,upos,head,rel],index)=>({index,attributes:{text,upos,'dep.head':String(head),'dep.rel':rel}}));

const KUSAMAKURA=[
  ['滑か','ADJ',3,'acl',1],['なる','AUX',1,'cop',0],['坂','NOUN',11,'obj',1],['を','ADP',3,'case',0],['、','PUNCT',3,'punct',0],
  ['護謨','NOUN',8,'nmod',1],['の','ADP',6,'case',0],['輪','NOUN',11,'nsubj',1],['が','ADP',8,'case',0],['緩々','ADV',11,'advmod',1],
  ['練り上る','VERB',22,'advcl',1],['如く','AUX',11,'aux',0],['、','PUNCT',11,'punct',0],['車','NOUN',22,'nsubj',1],['は','ADP',14,'case',0],
  ['静か','ADJ',22,'advcl',1],['に','AUX',16,'cop',0],['丘','NOUN',20,'nmod',1],['の','ADP',18,'case',0],['上','NOUN',22,'obl',1],['へ','ADP',20,'case',0],
  ['登っ','VERB',0,'root',1],['て','SCONJ',22,'mark',0],['行っ','VERB',22,'aux',0],['た','AUX',22,'aux',0],['。','PUNCT',22,'punct',0],
];
function longSentence(){
  const rows=[['坂','NOUN',null,'obj',1],['を','ADP',1,'case',0],['、','PUNCT',1,'punct',0]];
  for(let clause=0;clause<11;clause++){
    const base=rows.length;
    for(const [text,upos,offset,rel,begins] of [['護謨','NOUN',3,'nmod',1],['の','ADP',1,'case',0],['輪','NOUN',7,'nsubj',1],['が','ADP',3,'case',0],['緩々','ADV',7,'advmod',1],['と','ADP',5,'case',0],['練り上る','VERB',null,'advcl',1],['如く','AUX',7,'aux',0],['、','PUNCT',7,'punct',0]])
      rows.push([text,upos,offset===null?null:base+offset,rel,begins]);
  }
  const root=rows.length+1;
  rows.push(['上っ','VERB',0,'root',1],['た','AUX',root,'aux',0],['。','PUNCT',root,'punct',0]);
  return rows.map(([text,upos,head,rel,begins])=>[text,upos,head??root,rel,begins]);
}
const npv=(n,v)=>({span:{start:n,end:n+2},captures:[{name:'n',span:{start:n,end:n+1}},{name:'p',span:{start:n+1,end:n+2}}],graph_bindings:[{binding_kind:'node',name:'v',node:{node_kind:'word',token_index:v},attributes:[]}]});
const layout=(tokens,row,view)=>{const starts=view==='bunsetsu'?bunsetsuStarts(tokens):null;if(view==='bunsetsu')assert.ok(starts);return focusLayout(dependencyUnits(tokens,starts),evidenceWords(row,tokens.length));};

test('evidence words are the span, every capture and every graph-bound word, as in ling matches --evidence',()=>{
  const row={span:{start:2,end:4},captures:[{name:'n',span:{start:2,end:3}},{name:'n',span:{start:2,end:3}},{name:'p',span:{start:3,end:4}}],graph_bindings:[
    {binding_kind:'node',name:'v',node:{node_kind:'word',token_index:10},attributes:[]},
    {binding_kind:'edge',name:'e',head:{node_kind:'word',token_index:10},dependent:{node_kind:'word',token_index:2},relation:'obj'},
    {binding_kind:'node',name:'x',node:{node_kind:'empty',boundary:3,subindex:1},attributes:[]},
    {binding_kind:'node',name:'far',node:{node_kind:'word',token_index:99},attributes:[]},
  ]};
  assert.deepEqual(evidenceWords(row,26),[
    {index:2,roles:['e.dependent','n'],inSpan:true},
    {index:3,roles:['p'],inSpan:true},
    {index:10,roles:['e.head','v'],inSpan:false},
  ]);
  assert.deepEqual(evidenceWords({span:{start:0,end:2}},4),[{index:0,roles:[],inSpan:true},{index:1,roles:[],inSpan:true}]);
});

test('文節 units fold GiNZA labels from conllu.misc and spacy.ginza.bunsetu identically, and need a label on every word',()=>{
  const fromMisc=bunsetsuStarts(conllu(KUSAMAKURA)),fromSpacy=bunsetsuStarts(ginza(KUSAMAKURA));
  assert.deepEqual(fromMisc,KUSAMAKURA.map(row=>row[4]===1));
  assert.deepEqual(fromSpacy,fromMisc);
  assert.deepEqual(dependencyUnits(conllu(KUSAMAKURA),fromMisc).map(unit=>[unit.first,unit.last,unit.head,unit.rel]),[
    [0,1,1,'acl'],[2,4,5,'obj'],[5,6,3,'nmod'],[7,8,5,'nsubj'],[9,9,5,'advmod'],[10,12,10,'advcl'],
    [13,14,10,'nsubj'],[15,16,10,'advcl'],[17,18,9,'nmod'],[19,20,10,'obl'],[21,25,null,'root'],
  ]);
  assert.equal(bunsetsuStarts(unlabelled(KUSAMAKURA)),null);
  const partial=conllu(KUSAMAKURA);partial[4]={index:4,attributes:{...partial[4].attributes,'conllu.misc':['SpaceAfter=No']}};
  assert.equal(bunsetsuStarts(partial),null);
});

test('focused 文節 view keeps the role 文節 and folds the rest into gap counts',()=>{
  const expected={items:[{kind:'gap',count:1},{kind:'unit',unit:1},{kind:'gap',count:3},{kind:'unit',unit:5},{kind:'gap',count:5}],arcs:[{from:3,to:1,rel:'obj',level:1,bound:true}]};
  assert.deepEqual(layout(conllu(KUSAMAKURA),npv(2,10),'bunsetsu'),expected);
  assert.deepEqual(layout(ginza(KUSAMAKURA),npv(2,10),'bunsetsu'),expected);
});

test('focused 語 view draws arcs head to dependent, stacked by nesting depth',()=>{
  assert.deepEqual(layout(conllu(KUSAMAKURA),npv(2,10),'words'),{
    items:[{kind:'gap',count:2},{kind:'unit',unit:2},{kind:'unit',unit:3},{kind:'gap',count:6},{kind:'unit',unit:10},{kind:'gap',count:15}],
    arcs:[{from:1,to:2,rel:'case',level:1,bound:true},{from:4,to:1,rel:'obj',level:2,bound:true}],
  });
  assert.deepEqual(layout(unlabelled(KUSAMAKURA),npv(2,10),'words'),layout(conllu(KUSAMAKURA),npv(2,10),'words'));
});

test('a 105-word sentence keeps a verb 100 words beyond the context window',()=>{
  const tokens=conllu(longSentence());
  assert.equal(tokens.length,105);
  assert.deepEqual(layout(tokens,npv(0,102),'words'),{
    items:[{kind:'unit',unit:0},{kind:'unit',unit:1},{kind:'gap',count:100},{kind:'unit',unit:102},{kind:'gap',count:2}],
    arcs:[{from:0,to:1,rel:'case',level:1,bound:true},{from:3,to:0,rel:'obj',level:2,bound:true}],
  });
  assert.deepEqual(layout(tokens,npv(0,102),'bunsetsu'),{
    items:[{kind:'unit',unit:0},{kind:'gap',count:44},{kind:'unit',unit:45}],
    arcs:[{from:2,to:0,rel:'obj',level:1,bound:true}],
  });
});

test('an arc between span words without roles is shown but not bound',()=>{
  assert.deepEqual(layout(unlabelled(KUSAMAKURA),{span:{start:2,end:4}},'words'),{
    items:[{kind:'gap',count:2},{kind:'unit',unit:2},{kind:'unit',unit:3},{kind:'gap',count:22}],
    arcs:[{from:1,to:2,rel:'case',level:1,bound:false}],
  });
});

test('the vertical tree draws every arc and gives overlapping arcs separate gutter columns',()=>{
  const tokens=unlabelled([['坂','NOUN',4,'obj'],['を','ADP',1,'case'],['一つ','NUM',4,'obl'],['上った','VERB',0,'root']]);
  assert.deepEqual(verticalArcs(tokens,evidenceWords(npv(0,3),4)),[
    {from:0,to:1,rel:'case',column:1,bound:true},
    {from:3,to:2,rel:'obl',column:1,bound:false},
    {from:3,to:0,rel:'obj',column:2,bound:true},
  ]);
});

test('dependency views require a basic head on every word',()=>{
  assert.equal(hasDependencies(conllu(KUSAMAKURA)),true);
  assert.equal(hasDependencies([{index:0,attributes:{text:'猫',upos:'NOUN'}}]),false);
  assert.equal(hasDependencies(unlabelled([['猫','NOUN',2,'nsubj']])),false);
  assert.equal(hasDependencies([]),false);
});

test('context words bound only by a where clause are reported with their roles, span words are not',async()=>{
  const {boundOutside}=await import('./dependency.js');
  const row={span:{start:3,end:5},captures:[{name:'n',span:{start:3,end:4}},{name:'p',span:{start:4,end:5}}],graph_bindings:[{binding_kind:'node',name:'v',node:{node_kind:'word',token_index:11}},{binding_kind:'node',name:'n',node:{node_kind:'word',token_index:3}}]};
  assert.deepEqual([...boundOutside(row,20)],[[11,['v']]]);
  assert.deepEqual([...boundOutside({span:{start:0,end:1}},5)],[]);
});

test('the dependency view is the reader\'s choice, else bunsetsu when labelled, else the whole sentence on a narrow screen',async()=>{
  const {dependencyView}=await import('./dependency.js');
  for(const [choice,bunsetsu,compact,view] of [[null,true,false,'bunsetsu'],[null,false,false,'words'],[null,true,true,'bunsetsu'],[null,false,true,'vertical'],['words',true,true,'words'],['bunsetsu',false,false,'words'],['bunsetsu',false,true,'words']])
    assert.equal(dependencyView(choice,bunsetsu,compact),view,JSON.stringify([choice,bunsetsu,compact]));
});

test('a head is named by its word and its 1-based number, and the root has none',async()=>{
  const {headLabel}=await import('./dependency.js');
  const tokens=ginza(KUSAMAKURA);
  assert.equal(headLabel(tokens,2),'練り上る·11');
  assert.equal(headLabel(tokens,21),null);
});

test('a value reads in plain words from the bundled names, a relation subtype from its base, and an unnamed value has none',async()=>{
  const {valueName}=await import('./dependency.js');
  const {readFileSync}=await import('node:fs');
  const names=JSON.parse(readFileSync(new URL('../../lingrep-core/src/fsm/label_names.json',import.meta.url),'utf8')).sets;
  for(const [attribute,value,locale,name] of [
    ['upos','NOUN','ja','名詞'],['upos','ADP','ja','格助詞など'],['upos','VERB','en','verb'],['upos','SPACE','ja','空白'],
    ['dep.rel','nsubj','ja','主語'],['dep.rel','nsubj:pass','ja','受け身の主語'],['dep.rel','obl:poss','en','oblique nominal'],['dep.rel','dobj','en','direct object'],
    ['xpos','NN','en','singular noun'],
    ['upos','名詞-普通名詞-一般','ja',null],['dep.rel','','en',null],['lemma','NOUN','en',null],
  ])assert.equal(valueName(names,attribute,value,locale),name,`${attribute} ${value} ${locale}`);
  assert.equal(valueName(undefined,'upos','NOUN','ja'),null);
});

test('token details gloss values as the evidence table does, say how a word outside the span is bound, and list all attributes only when there are more',async()=>{
  const {tokenDetails}=await import('./dependency.js');
  const {readFileSync}=await import('node:fs');
  const names=JSON.parse(readFileSync(new URL('../../lingrep-core/src/fsm/label_names.json',import.meta.url),'utf8')).sets;
  const token=(index,text,upos,head,rel,extra={})=>({index,attributes:{text,lemma:text,upos,'dep.head':String(head),'dep.rel':rel,...extra}});
  const tokens=[token(0,'坂','NOUN',3,'obj'),token(1,'を','ADP',1,'case'),token(2,'上る','VERB',0,'root',{'conllu.misc':'SpaceAfter=No'})];
  const row={span:{start:0,end:2},captures:[{name:'n',span:{start:0,end:1}},{name:'p',span:{start:1,end:2}}],graph_bindings:[{binding_kind:'node',name:'v',node:{node_kind:'word',token_index:2}}]};
  const verb=tokenDetails(tokens,2,row,names,'ja','文の中心');
  assert.deepEqual([verb.place,verb.roles],['bound',['v']]);
  const {valueName}=await import('./dependency.js');
  assert.deepEqual(verb.basics.map(({key,name,values})=>[key,name,values.map(({value,gloss})=>[value,gloss])]),[
    ['text','照合形',[['上る',null]]],['lemma','辞書形',[['上る',null]]],['upos','品詞',[['VERB','動詞']]],['dep.head','係り先',[['0','文の中心']]],['dep.rel','関係',[['root',valueName(names,'dep.rel','root','ja')]]],
  ]);
  assert.deepEqual(verb.all.map(item=>item.key),['text','lemma','upos','dep.head','dep.rel','conllu.misc']);
  const noun=tokenDetails(tokens,0,row,names,'en','heads the sentence');
  assert.deepEqual([noun.place,noun.roles],['span',['n']]);
  assert.deepEqual(noun.basics.find(item=>item.key==='dep.head').values,[{value:'3',gloss:'上る·3'}]);
  assert.deepEqual(noun.basics.find(item=>item.key==='dep.rel').values,[{value:'obj',gloss:'object'}]);
  assert.equal(noun.all,null);
  assert.equal(tokenDetails(tokens,2,{span:{start:0,end:2}},names,'ja','文の中心').place,'outside');
  const variant=[token(0,'ガラス','NOUN',0,'root',{surface:'ｶﾞﾗｽ'})];
  assert.deepEqual(tokenDetails(variant,0,row,names,'ja','文の中心').basics.slice(0,2).map(({key,name,values})=>[key,name,values.map(({value})=>value)]),[['surface','表層形',['ｶﾞﾗｽ']],['text','照合形',['ガラス']]]);
});

test('stored bunsetsu governors address the sentence span table across layers', async () => {
  const {annotatedBunsetsu} = await import('./dependency.js');
  const spans = [
    {kind:'bunsetsu',start:0,end:2,head:0,governor:2,label:'NP'},
    {kind:'entity',start:0,end:1,head:null,governor:null,label:null},
    {kind:'bunsetsu',start:2,end:3,head:2,governor:null,label:'VP'},
  ];
  assert.deepEqual(annotatedBunsetsu(spans, unlabelled([['猫','NOUN',3,'nsubj'],['が','ADP',1,'case'],['来る','VERB',0,'root']])), [
    {first:0,last:1,head:1,rel:'nsubj'},
    {first:2,last:2,head:null,rel:''},
  ]);
});
