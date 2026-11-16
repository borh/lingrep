export type Locale = 'en' | 'ja';
export type Behavior = 'keep' | 'live' | 'compare';
export interface Question { readonly source: string; readonly dataset: string; readonly where: string; }
export type AttributeValue = string | readonly string[];
export interface Token { readonly index: number; readonly attributes: Readonly<Record<string, AttributeValue>> & {readonly text:string}; }
export const attributeMembers = (value:AttributeValue|undefined):readonly string[] => value===undefined?[]:typeof value==='string'?[value]:value;
export interface MatchRecord {
  readonly sequence: number;
  readonly pattern: string;
  readonly span: { readonly start: number; readonly end: number };
  readonly corpus_sentence_ordinal: number;
  readonly document_ordinal: number;
  readonly source_name: string;
  readonly source_metadata?: { readonly document?: { readonly title?: string; readonly author?: string } };
  readonly surface: string;
  readonly tokens: readonly Token[];
  readonly left_context?: readonly Token[];
  readonly right_context?: readonly Token[];
}
export interface Diagnostic { readonly schema: "lingrep.diagnostic/v1"; readonly type: "diagnostic"; readonly data: { readonly stage: string; readonly severity: string; readonly message: string; readonly location?: { readonly line?: number; readonly column?: number; readonly source_span?: {readonly start:number;readonly end:number} } | null; readonly [key:string]:unknown }; }
export interface SnapshotFields extends Question {
  readonly diagnostics?: readonly Diagnostic[];
  readonly notices?: readonly string[];
  readonly id: string;
  readonly counts: Readonly<Record<string, number>>;
  readonly rows: Readonly<Record<string, readonly MatchRecord[]>>;
  readonly definitions: readonly string[];
  readonly bodies: Readonly<Record<string, string>>;
  readonly elapsed: number;
  readonly received: number;
  readonly version: number;
  readonly started: number;
  readonly error: string | null;
  readonly counters: { readonly counters: { readonly documents_admitted: number } } | null;
  readonly contract_run: { readonly analysis_identity: string } | null;
}
export type RunningSnapshot = SnapshotFields & { readonly phase: 'running' };
export type CompletedSnapshot = SnapshotFields & { readonly phase: 'complete' };
export type RunSnapshot = RunningSnapshot | CompletedSnapshot | (SnapshotFields & { readonly phase: 'failed' | 'cancelled' });
export interface SearchState {
  readonly current: RunSnapshot | null;
  readonly completed: CompletedSnapshot | null;
  readonly previous: CompletedSnapshot | null;
}
export interface Dataset { label:string; documents:number|null; sentences:number|null; analysis_identity:string|null; readiness:'ready'|'checking'|'building'|'failed'; }
export interface Configuration { datasets: Record<string, Dataset>; source: string; question: string; labels: Record<string, { labels: Record<Locale, string> }>; }
export type MetadataValue = string | number | boolean;
export type MetadataCondition = { numeric: true; min: string; max: string } | { numeric?: false; list: boolean; exclude: boolean; values: MetadataValue[]; labels?: Record<string, string> };
export interface QuestionSettings { metadata: Record<string, MetadataCondition>; raw: string; context: number; lines: number; }
export interface QuestionControls { state: QuestionSettings; where(): string; entries(): Record<string, string | number>; localize(): void; restore(params: URLSearchParams): void; }
export interface Field { key: string; list: boolean; numeric: boolean; min: number; max: number; present: number; }
export interface MetadataResult { fields: Field[]; documents: number; values: { value: MetadataValue; label: string; documents: number }[]; next: string | null; }
export interface StoredSentence { analysis_identity: string; sentence: number; tokens: readonly Token[]; }
export interface DocumentSummary { ordinal: number; title: string; author: string; matches: number; }
export interface DocumentPage { landmarks: DocumentSummary[]; after: number; start: number; matches_total: number; previous: number | null; documents_total: number; documents_offset: number; selected: DocumentSummary | null; position: number | null; previous_document: number | null; next_document: number | null; documents: DocumentSummary[]; rows: MatchRecord[]; next: number | null; document: number | null; }
export interface DocumentView { request: {document:number|null;after:number;documentsOffset:number|null;scopeDocument:number|null;limit:number}|null; scopeDocument: number | null; returnAfter: number; landmarks: DocumentSummary[]; start: number; matchesTotal: number; query: string; documentsTotal: number; documentsOffset: number; selectedDoc: DocumentSummary | null; position: number | null; previousDoc: number | null; nextDoc: number | null; documents: DocumentSummary[] | null; document: number | null; rows: MatchRecord[]; after: number; next: number | null; previousAfter: number | null; loading: boolean; error: string | null; epoch: number; limit: number; }
export interface SavedQuestion { format: 'lingrep.question/v1' | 'lingrep.question-probe/v1'; question: string; source: string; corpus: string; analysis: string; where: string; [key: string]: string | number; }
export const errorMessage = (error: unknown): string => error instanceof Error ? error.message : String(error);
export function element<T extends HTMLElement = HTMLElement>(selector: string, root: ParentNode = document): T {
  const node = root.querySelector<T>(selector);
  if (!node) throw new Error(`Missing interface element: ${selector}`);
  return node;
}
export function runningSnapshot(id: string, question: Question): RunningSnapshot {
  return { ...question, id, phase: 'running', counts: {}, rows: {}, definitions: [], bodies: {}, elapsed: 0, received: 0, version: 0, started: 0, error: null, counters: null, contract_run: null };
}
export interface SearchUpdate {
  readonly id: string;
  readonly reset: boolean;
  readonly patch: Partial<SnapshotFields> & { readonly phase?: RunSnapshot['phase'] };
  readonly append: SnapshotFields['rows'];
}
const objectValue = (value: unknown): value is Record<string, unknown> => typeof value === 'object' && value !== null && !Array.isArray(value);
const finiteNumber = (value: unknown): value is number => typeof value === 'number' && Number.isFinite(value);
const stringMap = (value: unknown): boolean => objectValue(value) && Object.values(value).every(value => typeof value === 'string');
const tokenValue = (value: unknown): boolean => objectValue(value) && Number.isSafeInteger(value.index) && Number(value.index)>=0 && objectValue(value.attributes) && typeof value.attributes.text==='string' && Object.values(value.attributes).every(v=>typeof v==='string'||Array.isArray(v)&&v.every(member=>typeof member==='string'));
function diagnosticValue(value:unknown):boolean {
  if(!objectValue(value)||value.schema!=='lingrep.diagnostic/v1'||value.type!=='diagnostic'||!objectValue(value.data))return false;
  const data=value.data;
  if(!['stage','severity','message'].every(key=>typeof data[key]==='string'))return false;
  if(data.location==null)return true;
  if(!objectValue(data.location))return false;
  const location=data.location;
  if(!['line','column'].every(key=>location[key]==null||Number.isSafeInteger(location[key])&&Number(location[key])>0))return false;
  const span=location.source_span;
  return span==null||objectValue(span)&&Number.isSafeInteger(span.start)&&Number.isSafeInteger(span.end)&&Number(span.start)>=0&&Number(span.end)>=Number(span.start);
}
function matchValue(value: unknown): boolean {
  if (!objectValue(value) || !objectValue(value.span)) return false;
  if (!['sequence','corpus_sentence_ordinal','document_ordinal'].every(key => finiteNumber(value[key]))) return false;
  if (!['pattern','source_name','surface'].every(key => typeof value[key] === 'string')) return false;
  if (!finiteNumber(value.span.start) || !finiteNumber(value.span.end) || value.span.end < value.span.start) return false;
  return ['tokens','left_context','right_context'].every(key => value[key] === undefined && key !== 'tokens' || Array.isArray(value[key]) && value[key].every(tokenValue));
}
export function parseUpdate(text: string): SearchUpdate {
  const value: unknown = JSON.parse(text);
  if (!objectValue(value) || typeof value.id !== 'string' || typeof value.reset !== 'boolean' || !objectValue(value.patch) || !objectValue(value.append)) throw new Error('Invalid search update envelope.');
  if(value.patch.id!==undefined&&value.patch.id!==value.id)throw new Error('Search update identity mismatch.');
  const checks: Record<string,(value: unknown)=>boolean> = {
    id: v=>typeof v==='string', source:v=>typeof v==='string',dataset:v=>typeof v==='string',where:v=>typeof v==='string',
    phase:v=>typeof v==='string'&&['running','complete','failed','cancelled'].includes(v),
    elapsed:finiteNumber,received:finiteNumber,version:finiteNumber,started:finiteNumber,
    diagnostics:v=>Array.isArray(v)&&v.every(diagnosticValue),
    notices:v=>Array.isArray(v)&&v.every(value=>typeof value==='string'),
    error:v=>v===null||typeof v==='string',definitions:v=>Array.isArray(v)&&v.every(x=>typeof x==='string'),
    bodies:stringMap, counts:v=>objectValue(v)&&Object.values(v).every(finiteNumber),
    contract_run:v=>v===null||objectValue(v)&&typeof v.analysis_identity==='string',
    counters:v=>v===null||objectValue(v)&&objectValue(v.counters)&&finiteNumber(v.counters.documents_admitted),
  };
  for(const [key,field] of Object.entries(value.patch))if(checks[key]&&!checks[key](field))throw new Error(`Invalid search update field: ${key}.`);
  for(const rows of Object.values(value.append))if(!Array.isArray(rows)||!rows.every(matchValue))throw new Error('Invalid example records in search update.');
  if(Object.values(value.append).some(rows=>(rows as unknown[]).length)&&!finiteNumber(value.patch.version))throw new Error('Example update has no version.');
  return value as unknown as SearchUpdate;
}

export function parseSnapshot(value: unknown): RunSnapshot {
  if(!objectValue(value)||typeof value.id!=='string'||!['source','dataset','where','phase'].every(key=>typeof value[key]==='string')||!objectValue(value.rows)||!objectValue(value.counts)||!Array.isArray(value.definitions))throw new Error('The saved search could not be read. Run the question again.');
  parseUpdate(JSON.stringify({id:value.id,reset:true,patch:Object.fromEntries(Object.entries(value).filter(([key])=>key!=='rows')),append:value.rows}));
  return value as unknown as RunSnapshot;
}

export function sourceSelection(source:string, span:{readonly start:number;readonly end:number}):readonly [number,number] | null {
  const bytes=new TextEncoder().encode(source);
  if(!Number.isSafeInteger(span.start)||!Number.isSafeInteger(span.end)||span.start<0||span.end<span.start||span.end>bytes.length)return null;
  const decoder=new TextDecoder('utf-8',{fatal:true});
  try{return [decoder.decode(bytes.subarray(0,span.start)).length,decoder.decode(bytes.subarray(0,span.end)).length];}catch{return null;}
}

export function parseStoredSentence(value:unknown):StoredSentence {
  if(!objectValue(value)||typeof value.analysis_identity!=='string'||!Number.isSafeInteger(value.sentence)||Number(value.sentence)<0||!Array.isArray(value.tokens)||!value.tokens.every(tokenValue))throw new Error('The stored token analysis could not be read.');
  return value as unknown as StoredSentence;
}
