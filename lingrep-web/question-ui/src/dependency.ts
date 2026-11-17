import {attributeMembers,basicAttributeKeys} from './model.js';
import type {Token,MatchRecord,Locale} from './model.js';

/** A word the match binds: the span, every capture, and every word a `where` clause binds, as `ling matches --evidence` lists them. */
export interface EvidenceWord { readonly index:number; readonly roles:readonly string[]; readonly inSpan:boolean; }
/** A word or a 文節: tokens `first..=last`, attached to unit `head` (null at the root) by `rel`. */
export interface Unit { readonly first:number; readonly last:number; readonly head:number|null; readonly rel:string; }
export type FocusItem = {readonly kind:'unit';readonly unit:number} | {readonly kind:'gap';readonly count:number};
/** An arc from the head's item position to the dependent's; `bound` when both ends carry roles. */
export interface Arc { readonly from:number; readonly to:number; readonly rel:string; readonly level:number; readonly bound:boolean; }
/** A whole-sentence arc between token indices, drawn in gutter `column` (1 nearest the words). */
export interface GutterArc { readonly from:number; readonly to:number; readonly rel:string; readonly column:number; readonly bound:boolean; }

type Row = Pick<MatchRecord,'span'|'captures'|'graph_bindings'>;
const scalar=(token:Token,key:string)=>{const value=token.attributes[key];return typeof value==='string'?value:undefined;};

/** The 0-based head index, or null at the root. */
export function headIndex(token:Token):number|null {
  const head=Number(scalar(token,'dep.head'));
  return head>0?head-1:null;
}

export function hasDependencies(tokens:readonly Token[]):boolean {
  return tokens.length>0&&tokens.every(token=>{const head=scalar(token,'dep.head');return head!==undefined&&/^\d+$/.test(head)&&Number(head)<=tokens.length;});
}

export function evidenceWords(row:Row,length:number):EvidenceWord[] {
  const roles=new Map<number,Set<string>>();
  const add=(index:number,role?:string)=>{if(!Number.isSafeInteger(index)||index<0||index>=length)return;const set=roles.get(index)??new Set();if(role)set.add(role);roles.set(index,set);};
  for(let index=row.span.start;index<row.span.end;index++)add(index);
  for(const capture of row.captures??[])for(let index=capture.span.start;index<capture.span.end;index++)add(index,capture.name);
  const bind=(node:{node_kind:string;token_index?:number},role:string)=>{if(node.node_kind==='word'&&node.token_index!==undefined)add(node.token_index,role);};
  for(const binding of row.graph_bindings??[]){
    if(binding.binding_kind==='node')bind(binding.node,binding.name);
    else{bind(binding.head,`${binding.name}.head`);bind(binding.dependent,`${binding.name}.dependent`);}
  }
  return [...roles].sort(([a],[b])=>a-b).map(([index,set])=>({index,roles:[...set].sort(),inSpan:index>=row.span.start&&index<row.span.end}));
}

/** Which words begin a 文節, from GiNZA's labels; null unless every word carries one. */
export function bunsetsuStarts(tokens:readonly Token[]):boolean[]|null {
  const starts=tokens.map(token=>{
    const label=scalar(token,'spacy.ginza.bunsetu')??attributeMembers(token.attributes['conllu.misc']).find(entry=>entry.startsWith('BunsetuBILabel='))?.slice(15);
    return label==='B'?true:label==='I'?false:null;
  });
  return starts.length&&starts.every(start=>start!==null)?starts as boolean[]:null;
}

/** One unit per word, or per 文節 when `starts` is given; a 文節 attaches through its first word whose head lies outside it. */
export function dependencyUnits(tokens:readonly Token[],starts:readonly boolean[]|null):Unit[] {
  const spans:[number,number][]=[];
  tokens.forEach((_,index)=>{if(!spans.length||starts?.[index]!==false)spans.push([index,index]);else spans[spans.length-1][1]=index;});
  const unitOf=new Map<number,number>();
  spans.forEach(([first,last],unit)=>{for(let index=first;index<=last;index++)unitOf.set(index,unit);});
  return spans.map(([first,last],unit)=>{
    let headWord=first;
    for(let index=first;index<=last;index++){const head=headIndex(tokens[index]);if(head===null||unitOf.get(head)!==unit){headWord=index;break;}}
    const head=headIndex(tokens[headWord]);
    return {first,last,head:head===null?null:unitOf.get(head)??null,rel:scalar(tokens[headWord],'dep.rel')??''};
  });
}

/** Arc levels by nesting: an arc rises one level above the highest arc it encloses. Input arcs must be sorted shortest first. */
function nest<T extends {from:number;to:number}>(arcs:readonly T[]):(T&{level:number})[] {
  const placed:(T&{level:number})[]=[];
  for(const arc of arcs){
    const lo=Math.min(arc.from,arc.to),hi=Math.max(arc.from,arc.to);
    placed.push({...arc,level:1+Math.max(0,...placed.filter(inner=>Math.min(inner.from,inner.to)>=lo&&Math.max(inner.from,inner.to)<=hi).map(inner=>inner.level))});
  }
  return placed;
}
const byExtent=(a:{from:number;to:number},b:{from:number;to:number})=>Math.abs(a.from-a.to)-Math.abs(b.from-b.to)||Math.min(a.from,a.to)-Math.min(b.from,b.to);

/** Units holding evidence words, with the units between them folded into gaps, and the arcs among the kept units. */
export function focusLayout(units:readonly Unit[],words:readonly EvidenceWord[]):{items:FocusItem[];arcs:Arc[]} {
  const kept=new Set<number>(),bound=new Set<number>();
  for(const word of words){const unit=units.findIndex(unit=>word.index>=unit.first&&word.index<=unit.last);if(unit<0)continue;kept.add(unit);if(word.roles.length)bound.add(unit);}
  const items:FocusItem[]=[],position=new Map<number,number>();
  units.forEach((_,unit)=>{
    if(kept.has(unit)){position.set(unit,items.length);items.push({kind:'unit',unit});return;}
    const last=items.at(-1);
    if(last?.kind==='gap')items[items.length-1]={kind:'gap',count:last.count+1};else items.push({kind:'gap',count:1});
  });
  const arcs=[...kept].flatMap(unit=>{const head=units[unit].head;return head!==null&&head!==unit&&kept.has(head)?[{from:position.get(head)!,to:position.get(unit)!,rel:units[unit].rel,bound:bound.has(unit)&&bound.has(head)}]:[];});
  return {items,arcs:nest(arcs.sort(byExtent)).map(({from,to,rel,level,bound})=>({from,to,rel,level,bound}))};
}

export function verticalArcs(tokens:readonly Token[],words:readonly EvidenceWord[]):GutterArc[] {
  const roles=new Set(words.filter(word=>word.roles.length).map(word=>word.index));
  const arcs=tokens.flatMap((token,to)=>{const from=headIndex(token);return from===null?[]:[{from,to,rel:scalar(token,'dep.rel')??'',bound:roles.has(from)&&roles.has(to)}];}).sort(byExtent);
  const placed:GutterArc[]=[];
  for(const arc of arcs){
    const lo=Math.min(arc.from,arc.to),hi=Math.max(arc.from,arc.to);
    let column=1;
    while(placed.some(other=>other.column===column&&!(Math.max(other.from,other.to)<lo||hi<Math.min(other.from,other.to))))column++;
    placed.push({...arc,column});
  }
  return placed;
}

/** The context words a match binds outside its span, by sentence token index, with their roles. */
export function boundOutside(row:Row,length:number):ReadonlyMap<number,readonly string[]> {
  return new Map(evidenceWords(row,length).filter(word=>!word.inSpan&&word.roles.length).map(word=>[word.index,word.roles]));
}

export type DependencyView='bunsetsu'|'words'|'vertical';
/** The reader's choice of view, else the whole sentence on a narrow screen, else 文節 when every word is labelled with one; a 文節 choice without labels shows words. */
export function dependencyView(choice:DependencyView|null,bunsetsu:boolean,compact:boolean):DependencyView {
  if(choice)return choice==='bunsetsu'&&!bunsetsu?'words':choice;
  return compact?'vertical':bunsetsu?'bunsetsu':'words';
}

/** The word a token attaches to and its 1-based number, as `抜かし·17`; null at the root. */
export function headLabel(tokens:readonly Token[],index:number):string|null {
  const head=headIndex(tokens[index]);
  return head===null||!tokens[head]?null:`${scalar(tokens[head],'text')??''}·${head+1}`;
}

/** Display names of token-attribute values, by label set, attribute and value, as the question configuration's `value_names` carries them. */
export type ValueNames=Readonly<Record<string,Readonly<Record<string,Readonly<Record<string,Readonly<Partial<Record<Locale,string>>>>>>>>>;
const LABEL_SETS=['ud-v2','spacy','clearnlp','penn'];
/** `value` of `attribute` in plain words, from the first label set that names it, or its base relation's name for a `dep.rel` subtype; English where a name has no Japanese; null when no set names it. */
export function valueName(names:ValueNames|undefined,attribute:string,value:string,locale:Locale):string|null {
  const entry=(value:string)=>LABEL_SETS.map(set=>names?.[set]?.[attribute]?.[value]).find(Boolean);
  const found=entry(value)??(attribute==='dep.rel'&&value.includes(':')?entry(value.slice(0,value.indexOf(':'))):undefined);
  return found?.[locale]??found?.en??null;
}

/** A token attribute's value with its plain-word name, when one is known. */
export interface DetailValue { readonly value:string; readonly gloss:string|null; }
/** A token attribute in the details: its key, the key's plain-word name when it has one, and its values. */
export interface DetailRow { readonly key:string; readonly name:string|null; readonly values:readonly DetailValue[]; }
/** Whether a token lies in the match span, is bound by the pattern's condition outside it (with `roles`), or neither; its basic attributes; and all of them when there are more than the basics. */
export interface TokenDetails { readonly place:'span'|'bound'|'outside'; readonly roles:readonly string[]; readonly basics:readonly DetailRow[]; readonly all:readonly DetailRow[]|null; }
const KEY_NAMES:Readonly<Record<string,Readonly<Record<Locale,string>>>>={text:{ja:'表層形',en:'surface'},lemma:{ja:'辞書形',en:'dictionary form'},upos:{ja:'品詞',en:'part of speech'},'dep.head':{ja:'係り先',en:'attaches to'},'dep.rel':{ja:'関係',en:'relation'}};
/** Token `index`'s details as the evidence table words them: `dep.head` as the head's word and number (`root` at the root) and other values by their names in `names`. */
export function tokenDetails(tokens:readonly Token[],index:number,row:Row,names:ValueNames|undefined,locale:Locale,root:string):TokenDetails {
  const attributes=tokens[index].attributes,word=evidenceWords(row,tokens.length).find(word=>word.index===index);
  const detail=(key:string):DetailRow=>({key,name:KEY_NAMES[key]?.[locale]??null,values:attributeMembers(attributes[key]).map(value=>({value,gloss:key==='dep.head'?(value==='0'?root:headLabel(tokens,index)):valueName(names,key,value,locale)}))});
  const basics=basicAttributeKeys(attributes).map(detail),keys=Object.keys(attributes);
  return {place:word?.inSpan?'span':word?.roles.length?'bound':'outside',roles:word?.roles??[],basics,all:keys.length>basics.length?keys.map(detail):null};
}
