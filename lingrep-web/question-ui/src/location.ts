import { textOf } from './model.js';
import type { SavedQuestion, Token } from './model.js';
export interface ConcordanceLocation { readonly kind: 'concordance'; readonly scopeDocument?: number|null; readonly returnAfter?:number; readonly pattern: string; readonly document: number | null; readonly after: number; readonly filter: string; }
interface HitCoordinates { readonly pattern:string; readonly document:number; readonly sequence:number|null; }
export interface ExampleLocation extends HitCoordinates { readonly kind:'example'; readonly returnTo:ConcordanceLocation; }
export interface ReaderLocation extends HitCoordinates { readonly kind:'reader'; readonly returnTo:ConcordanceLocation|ExampleLocation; }
export type HitLocation=ExampleLocation|ReaderLocation;
export type ViewLocation = ConcordanceLocation | HitLocation;
export const defaultView = (): ConcordanceLocation => ({kind:'concordance',scopeDocument:null,returnAfter:-1,pattern:'',document:null,after:-1,filter:''});
const record = (value: unknown): value is Record<string,unknown> => typeof value==='object'&&value!==null&&!Array.isArray(value);
const ordinal = (value: unknown): value is number => typeof value==='number'&&Number.isSafeInteger(value)&&value>=0;
function concordance(value: unknown): value is ConcordanceLocation {
  return record(value)&&value.kind==='concordance'&&typeof value.pattern==='string'&&(value.document===null||ordinal(value.document))&&typeof value.after==='number'&&Number.isSafeInteger(value.after)&&value.after>=-1&&typeof value.filter==='string'&&(value.scopeDocument==null||ordinal(value.scopeDocument))&&(value.returnAfter==null||typeof value.returnAfter==='number'&&Number.isSafeInteger(value.returnAfter)&&value.returnAfter>=-1);
}
export function parseView(text: string | null): ViewLocation {
  if(!text)return defaultView();
  const value: unknown=JSON.parse(text);
  if(concordance(value))return value;
  if(record(value)&&(value.kind==='example'||value.kind==='reader')&&typeof value.pattern==='string'&&ordinal(value.document)&&(value.sequence===null||ordinal(value.sequence))){
    if(concordance(value.returnTo))return value as unknown as HitLocation;
    if(value.kind==='reader'&&record(value.returnTo)&&value.returnTo.kind==='example'&&concordance(value.returnTo.returnTo)&&typeof value.returnTo.pattern==='string'&&ordinal(value.returnTo.document)&&(value.returnTo.sequence===null||ordinal(value.returnTo.sequence)))return value as unknown as ReaderLocation;
  }
  throw new Error('The saved reading location is invalid. Open a new question.');
}
export function locationHash(params: Record<string,string|number>, evidence: string | null, view: ViewLocation): string {
  const hash=new URLSearchParams(Object.entries(params).map(([key,value])=>[key,String(value)]));
  if(evidence)hash.set('evidence',evidence);
  hash.set('view',JSON.stringify(view));
  return '#'+hash.toString();
}
export function locationURL(base: string, params: Record<string,string|number>, evidence: string | null, view: ViewLocation): string {
  const url=new URL(base);
  url.hash=locationHash(params,evidence,view);
  return url.href;
}

export function savedQuestion(value: unknown): SavedQuestion {
  if(!record(value)||!['lingrep.question/v1','lingrep.question-probe/v1'].includes(String(value.format))||!['question','source','corpus','analysis','where'].every(key=>typeof value[key]==='string'))throw new Error('Choose a saved lingrep question document.');
  if(!Object.values(value).every(field=>typeof field==='string'||typeof field==='number'&&Number.isFinite(field)))throw new Error('The question document contains invalid settings.');
  parseView(typeof value.view==='string'?value.view:null);
  return value as unknown as SavedQuestion;
}

export function seedParams(seed:unknown,hash:string):URLSearchParams {
  const params=new URLSearchParams();
  if(seed!=null){
    if(!record(seed))throw new Error('The initial question could not be read.');
    for(const key of ['source','corpus','analysis','raw','run','notice','lines','context','pattern'])if(seed[key]!=null){
      if(typeof seed[key]!=='string'||key==='run'&&!['0','1'].includes(seed[key] as string))throw new Error('The initial question contains invalid settings.');
      params.set(key,seed[key] as string);
    }
  }
  const overrides=new URLSearchParams(hash.replace(/^#/,''));
  if(overrides.has('catalog'))params.delete('source');
  if(overrides.has('evidence')||overrides.has('attempt'))for(const key of ['source','corpus','analysis','raw','run','pattern'])params.delete(key);
  if(overrides.has('corpus')&&!overrides.has('analysis')&&overrides.get('corpus')!==params.get('corpus'))params.delete('analysis');
  for(const [key,value] of overrides)params.set(key,value);
  return params;
}

export interface ServedCatalog { readonly source:string; readonly fingerprint?:string|undefined; }
/** Replaces `source` with `catalog` only when the source matches the served catalog and its fingerprint is available. */
export function catalogParams(params:Record<string,string|number>,served:ServedCatalog):Record<string,string|number> {
  if(served.fingerprint===undefined||params.source!==served.source)return params;
  const {source:_source,...rest}=params;
  return {...rest,catalog:served.fingerprint};
}
/** Removes `catalog`, preserving an explicit `source`; otherwise expands the served source and reports a fingerprint mismatch in `changed`. */
export function resolveCatalog(params:URLSearchParams,served:ServedCatalog):{changed:string|null} {
  const linked=params.get('catalog');
  params.delete('catalog');
  if(linked===null||params.has('source'))return {changed:null};
  params.set('source',served.source);
  return {changed:linked===served.fingerprint?null:linked};
}

/** The analysis identity a search of `params`' corpus expects: the address's own, else `run`'s when `run` read that corpus. */
export function expectedAnalysis(params:URLSearchParams,run:{readonly dataset:string;readonly contract_run:{readonly analysis_identity:string}|null}|null):string|null {
  if(params.get('analysis'))return params.get('analysis');
  return run&&run.dataset===params.get('corpus')?run.contract_run?.analysis_identity??null:null;
}

/** A frequency line's occurrence: its pattern, corpus sentence, corpus document ID and role words, with the identity of the table it was counted in. */
export interface OccurrenceTarget { readonly pattern:string; readonly sentence:number; readonly document:number; readonly roles:readonly {readonly name:string;readonly start:number;readonly end:number}[]; readonly analysis:string; readonly fingerprint:string; }
export function parseOccurrence(text:string|null):OccurrenceTarget|null {
  if(!text)return null;
  let value:unknown;
  try{value=JSON.parse(text);}catch{value=null;}
  const role=(item:unknown)=>record(item)&&typeof item.name==='string'&&ordinal(item.start)&&ordinal(item.end)&&item.end>=item.start;
  if(!record(value)||typeof value.pattern!=='string'||!value.pattern||!ordinal(value.sentence)||!ordinal(value.document)||!Array.isArray(value.roles)||!value.roles.every(role)||typeof value.analysis!=='string'||typeof value.fingerprint!=='string')throw new Error('The frequency occurrence in this address is invalid. Open it again from the frequency view.');
  return {pattern:value.pattern,sentence:value.sentence,document:value.document,roles:value.roles.map(({name,start,end}:{name:string;start:number;end:number})=>({name,start,end})),analysis:value.analysis,fingerprint:value.fingerprint};
}
/** A frequency entry's words, its occurrence and document counts, the lines the frequency view listed for it with their spans and their documents' names, and the index among them of the line opened. */
export interface EntryLines { readonly words:readonly string[]; readonly total:number; readonly works?:number; readonly at:number; readonly names?:Readonly<Record<string,string>>; readonly lines:readonly EntryLine[]; }
/** One listed line: its sentence, its document, its match span when the hand-off carried it, and its role words. */
export type EntryLine=Pick<OccurrenceTarget,'sentence'|'document'|'roles'>&{readonly start?:number;readonly end?:number};
export function parseEntryLines(text:string|null):EntryLines|null {
  if(!text)return null;
  let value:unknown;
  try{value=JSON.parse(text);}catch{value=null;}
  const role=(item:unknown)=>record(item)&&typeof item.name==='string'&&ordinal(item.start)&&ordinal(item.end)&&item.end>=item.start;
  const span=(item:Record<string,unknown>)=>item.start===undefined&&item.end===undefined||ordinal(item.start)&&ordinal(item.end)&&item.end>item.start;
  const line=(item:unknown)=>record(item)&&ordinal(item.sentence)&&ordinal(item.document)&&span(item)&&Array.isArray(item.roles)&&item.roles.every(role);
  const names=(item:unknown)=>item===undefined||record(item)&&Object.values(item).every(name=>typeof name==='string');
  if(!record(value)||!Array.isArray(value.words)||!value.words.every(word=>typeof word==='string')||!Array.isArray(value.lines)||!value.lines.length||!value.lines.every(line)||!ordinal(value.total)||value.total<value.lines.length||value.works!==undefined&&!ordinal(value.works)||!names(value.names)||!ordinal(value.at)||value.at>=value.lines.length)throw new Error('The frequency entry in this address is invalid. Open it again from the frequency view.');
  return {words:value.words,total:value.total,...(value.works!==undefined?{works:value.works as number}:{}),at:value.at,...(value.names!==undefined?{names:value.names as Record<string,string>}:{}),lines:value.lines.map(({sentence,document,start,end,roles}:EntryLine)=>({sentence,document,...(start!==undefined?{start,end}:{}),roles:roles.map(({name,start,end})=>({name,start,end}))}))};
}
/** A listed line read as a concordance line from its sentence's `tokens`: up to `context` tokens on each side of its span, the span's text, the whole line's text, and the role words outside the span by token index. A line without a span spans its role words. */
export function entryContext(tokens:readonly Token[],line:EntryLine,context:number){
  const start=line.start??Math.min(...line.roles.map(role=>role.start)),end=line.end??Math.max(...line.roles.map(role=>role.end));
  const bound=new Map<number,string[]>();
  for(const role of line.roles)for(let index=role.start;index<role.end;index++)if(index<start||index>=end)bound.set(index,[...(bound.get(index)??[]),role.name]);
  const from=context?Math.max(0,start-context):start;
  return {left:tokens.slice(from,start),hit:textOf(tokens.slice(start,end)),right:tokens.slice(end,end+context),text:textOf(tokens.slice(from,end+context)),bound};
}
/** The frequency view address to return to, or null unless `value` is a relative `frequencies` address. */
export const frequencyReturn=(value:string|null):string|null=>value!==null&&/^frequencies(#|$)/.test(value)?value:null;
