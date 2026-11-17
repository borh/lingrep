export type Locale = 'en' | 'ja';
export type Behavior = 'keep' | 'live' | 'compare';
export interface Question { readonly source: string; readonly dataset: string; readonly where: string; }
export type AttributeValue = string | readonly string[];
export interface Token { readonly index: number; readonly attributes: Readonly<Record<string, AttributeValue>> & {readonly text:string}; }
export const attributeMembers = (value:AttributeValue|undefined):readonly string[] => value===undefined?[]:typeof value==='string'?[value]:value;
export interface GraphNode { readonly node_kind: string; readonly token_index?: number; }
export type GraphBinding = { readonly binding_kind: 'node'; readonly name: string; readonly node: GraphNode } | { readonly binding_kind: 'edge'; readonly name: string; readonly head: GraphNode; readonly dependent: GraphNode; readonly relation: string };
export interface MatchRecord {
  readonly sequence: number;
  readonly pattern: string;
  readonly span: { readonly start: number; readonly end: number };
  readonly corpus_sentence_ordinal: number;
  readonly document_ordinal: number;
  readonly source_name: string;
  readonly source_metadata?: { readonly document?: { readonly title?: string; readonly author?: string } };
  readonly surface: string;
  readonly captures?: readonly { readonly name: string; readonly span: { readonly start: number; readonly end: number } }[];
  readonly graph_bindings?: readonly GraphBinding[];
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
  readonly error_code?: 'analysis_changed' | 'result_limit' | null;
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
export interface Configuration { datasets: Record<string, Dataset>; source: string; question: string; labels: Record<string, { labels?: Partial<Record<Locale, string>>; values?: Record<string, { labels: Partial<Record<Locale, string>> }> }>; value_names?: Readonly<Record<string,Readonly<Record<string,Readonly<Record<string,Readonly<Partial<Record<Locale,string>>>>>>>>>; }
export type MetadataValue = string | number | boolean;
export type MetadataCondition = { numeric: true; min: string; max: string } | { numeric?: false; list: boolean; exclude: boolean; values: MetadataValue[]; labels?: Record<string, string> };
export interface QuestionSettings { metadata: Record<string, MetadataCondition>; raw: string; context: number; lines: number; }
export interface QuestionControls { state: QuestionSettings; boundExamples(maximum:number|null): void; where(): string; entries(): Record<string, string | number>; localize(): void; restore(params: URLSearchParams): void; }
export interface Field { key: string; list: boolean; numeric: boolean; min: number; max: number; present: number; }
export interface MetadataResult { fields: Field[]; documents: number; values: { value: MetadataValue; label: string; documents: number }[]; next: string | null; }
export interface StoredSentence { analysis_identity: string; sentence: number; tokens: readonly Token[]; }
export interface DocumentSummary { ordinal: number; title: string; author: string; matches: number; }
export interface DocumentPage { landmarks: DocumentSummary[]; after: number; start: number; matches_total: number; previous: number | null; documents_total: number; documents_offset: number; selected: DocumentSummary | null; position: number | null; previous_document: number | null; next_document: number | null; documents: DocumentSummary[]; rows: MatchRecord[]; next: number | null; document: number | null; }
export interface DocumentView { request: {document:number|null;after:number;documentsOffset:number|null;scopeDocument:number|null;limit:number}|null; scopeDocument: number | null; returnAfter: number; landmarks: DocumentSummary[]; start: number; matchesTotal: number; query: string; documentsTotal: number; documentsOffset: number; selectedDoc: DocumentSummary | null; position: number | null; previousDoc: number | null; nextDoc: number | null; documents: DocumentSummary[] | null; document: number | null; rows: MatchRecord[]; after: number; next: number | null; previousAfter: number | null; loading: boolean; error: string | null; epoch: number; limit: number; }
export interface SavedQuestion { format: 'lingrep.question/v1' | 'lingrep.question-probe/v1'; question: string; source: string; corpus: string; analysis: string; where: string; [key: string]: string | number; }
/** A run's failure text, translated when the server reports a known failure code. */
export const runError = (run: Pick<RunSnapshot,'error'|'error_code'>, t: (key: string) => string): string | null => run.error_code === 'analysis_changed' ? t('analysisChanged') : run.error_code === 'result_limit' ? t('resultLimit') : run.error;
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
    error:v=>v===null||typeof v==='string',error_code:v=>v===null||v==='analysis_changed'||v==='result_limit',definitions:v=>Array.isArray(v)&&v.every(x=>typeof x==='string'),
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

/** The offset of the line defining `name` in a catalog source (`name :=`, after an optional definition keyword), or -1. */
export function definitionOffset(source:string, name:string):number {
  const escaped=name.replace(/[.*+?^${}()|[\]\\]/g,'\\$&');
  return source.search(new RegExp(`^[ \\t]*(?:(?:pattern|fragment|lexicon|set)[ \\t]+)?${escaped}[ \\t]*:=`,'m'));
}
export function parseStoredSentence(value:unknown):StoredSentence {
  if(!objectValue(value)||typeof value.analysis_identity!=='string'||!Number.isSafeInteger(value.sentence)||Number(value.sentence)<0||!Array.isArray(value.tokens)||!value.tokens.every(tokenValue))throw new Error('The stored token analysis could not be read.');
  return value as unknown as StoredSentence;
}

/** A search's restriction to one published definition, one corpus document, or both, as a run request's `pattern` and `document` fields carry it. */
export interface SearchLimit { readonly pattern: string | null; readonly document: number | null; }

/** The definitions holding at least half of the largest count: when a run overflows its result budget, these filled it. */
export function overflowingDefinitions(counts: Readonly<Record<string, number>>): string[] {
  const largest = Math.max(0, ...Object.values(counts));
  return largest > 0 ? Object.keys(counts).filter(name => counts[name] * 2 >= largest) : [];
}

const shellWord = (value: string) => "'" + value.replaceAll("'", "'\\''") + "'";
/** The `ling` command that repeats a search of `query.lx` over a local copy of its corpus, keeping its condition and limit. */
export function cliCommand(where: string, limit: SearchLimit | null): string {
  let command = 'ling --json matches --corpus CORPUS.lingcorpus --patterns query.lx --context 24';
  if (where) command += ' --where ' + shellWord(where);
  const tests = [
    ...(limit?.pattern ? [`.data.pattern == ${JSON.stringify(limit.pattern)}`] : []),
    ...(limit?.document != null ? [`.data.document_ordinal == ${limit.document}`] : []),
  ];
  if (tests.length) command += ' | jq -c ' + shellWord(`select(.type == "match" and ${tests.join(' and ')})`);
  return command;
}

const diagnosticMessages: Readonly<Record<string, Readonly<Record<Locale, string>>>> = {
  'invalid lingrep patterns syntax': {
    en: 'The pattern is not written correctly. Check that every bracket is closed and every definition ends with a period.',
    ja: 'パターンの書き方に誤りがあります。括弧が閉じているか、各定義がピリオドで終わっているかを確認してください。',
  },
};
/** A catalog diagnostic as one located sentence in `locale`; messages without a translation stay as the compiler wrote them. */
export function diagnosticText(diagnostic: Diagnostic, locale: Locale): string {
  const {line, column} = diagnostic.data.location ?? {};
  const message = diagnosticMessages[diagnostic.data.message]?.[locale] ?? diagnostic.data.message;
  if (!line) return message;
  const place = locale === 'ja' ? `${line} 行${column ? ` ${column} 文字目` : ''}` : `Line ${line}${column ? `, column ${column}` : ''}`;
  return `${place}: ${message}`;
}

export const LOCALE_KEY = 'lingrep-web:locale:v1';
/** The saved page language, shared with the other lingrep-web pages: that key, else this page's former `ling-questions-locale` key, else the browser language. */
export function storedLocale(storage: Pick<Storage, 'getItem'> | null | undefined, language: string): Locale {
  for (const key of [LOCALE_KEY, 'ling-questions-locale']) {
    try {
      const stored = storage?.getItem(key);
      if (stored === 'ja' || stored === 'en') return stored;
    } catch { /* Fall through to the browser language. */ }
  }
  return language.toLowerCase().startsWith('ja') ? 'ja' : 'en';
}

/** The name a match's document is shown by: its metadata title, else its source name without a corpus file extension. */
export function documentTitle(row: Pick<MatchRecord, 'source_name' | 'source_metadata'>): string {
  return row.source_metadata?.document?.title ?? row.source_name.replace(/\.(conllu|jsonl|txt|md|xml)$/i, '');
}
