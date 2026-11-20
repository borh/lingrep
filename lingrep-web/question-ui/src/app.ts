import {serviceURL,ServiceError,expiredSearch,workbenchLinkRequest,workbenchHref} from './service.js';
import {parseSections,restoreSections} from './sections.js';
import type {SectionOrigin,SectionLink,QueryRecipe} from './sections.js';
import {addCondition,patternSource} from './composer.js';
import type {TokenCondition} from './composer.js';
import {expectedAnalysis,defaultView,parseView,locationHash,locationURL,savedQuestion,seedParams,catalogParams,resolveCatalog,parseOccurrence,parseEntryLines,frequencyReturn,entryContext} from './location.js';
import type {ViewLocation,ConcordanceLocation,HitLocation,OccurrenceTarget,EntryLines} from './location.js';
import { begin, exportable, receive, receiveUpdate, visible } from './state.mjs';
import { initQuestion, questionSettings } from './document.js';
import {boundOutside,dependencyView,evidenceWords,hasDependencies,bunsetsuStarts,dependencyUnits,focusLayout,verticalArcs,headLabel,valueName,tokenDetails as detailsOf} from './dependency.js';
import type {Arc,DependencyView,EvidenceWord,DetailRow} from './dependency.js';

import { element as $, definitionOffset, errorMessage, runError, runningSnapshot, parseUpdate, parseSnapshot, parseStoredSentence, surfaceOf, attributeMembers, sourceSelection, documentTitle, storedLocale, LOCALE_KEY, overflowingDefinitions, cliCommand, diagnosticText } from './model.js';
import type { SearchLimit, Behavior, Locale, Configuration, CatalogDefinition, QuestionControls, RunSnapshot, CompletedSnapshot, SearchState, StoredSentence, MatchRecord, Token, DocumentPage, DocumentView, SavedQuestion, Question } from './model.js';
const escape = (value: unknown) => String(value ?? '').replace(/[&<>"']/g, c => ({'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;',"'":'&#39;'}[c]??c));
const words = (tokens: readonly Token[] | undefined) => (tokens ?? []).map(surfaceOf).join('');
const count = (value: number | undefined) => Number(value ?? 0).toLocaleString(locale === 'ja' ? 'ja-JP' : 'en-US');
let locale: Locale = storedLocale(globalThis.localStorage,navigator.language);
const en = Object.fromEntries([...document.querySelectorAll<HTMLElement>('[data-t]')].map(element => [element.dataset.t, element.textContent]));
const ja: Record<string,string> = {
  readingOptions:'表示の設定',questionActions:'問いの操作',questionNav:'問い',remix:'問いを複製',saveQuestion:'問いを保存',openQuestion:'開く',searchFor:'',among:'の',update:'検索する',done:'完了',exclude:'選んだ値を除外',previousValues:'前の値',nextValues:'次の値',clearConditions:'条件を消去',textClause:'条件式を直接編集',read:'表示する用例は',examplesWith:'件、前後の文脈は',contextTokens:'トークンずつ',closeAnalysis:'用例を閉じる',tokenHelp:'トークンを選ぶと、属性を確認できます。',remixTest:'条件を追加',showLatest:'新しい結果を表示',prototypeOptions:'検索の設定',searchNav:'検索',incumbentNav:'分析ツール',frequencyNav:'頻度',compareNav:'比較',helpNav:'ヘルプ',stopReplay:'再生を停止', find:'', in:'', patternSource:'パターンのソース', toSearch:'で検索', draft:'Alt+Enter または「検索する」で検索します。',formatSource:'パターンを整形',run:'検索する',where:'文書の条件',edited:'編集済み · 検索して適用',ready:'検索できます。',stop:'検索を停止',retry:'もう一度検索',during:'新しい検索の実行中は',keep:'完了した結果を表示',live:'途中の結果を表示',compare:'前回と今回を並べる',replay:'検索の経過を再生',replayNote:'検索の経過を再生しています。',boundary:'検索結果について',boundaryBody:'選択したコーパスとコンパイル済みのパターンに基づく検索結果です。用例はページごとに表示します。検索中の件数は途中経過です。原文は検索条件で絞り込まずに表示します。',return:'用例に戻る',paragraph:'段落を読む',document:'文書を読む',
  current:'今回の結果',previous:'前回の完了した結果',completed:'検索完了',partial:'検索中 · 件数は途中経過',cancelled:'停止済み · 件数は途中経過',failed:'検索に失敗',retained:'表示中の用例',noLines:'このパターンの用例はまだありません。',download:'JSONL で保存',source:'検索時のパターン',copy:'CLI コマンドをコピー',copied:'コピーしました',savedQuery:'検索時の query.lx を保存',runNumber:'検索',scope:'文書の条件',all:'すべての文書',soFar:'件が見つかりました',running:'検索しています',notTotal:'検索中の件数は途中経過です。',stopped:'検索を停止しました。途中の件数はコーパス全体の件数ではありません。',failedKeep:'検索に失敗しました。前回の検索結果を表示しています。',errorRetry:'パターンや条件を確認して、もう一度検索してください。',draftRunning:'新しいパターンを編集中です。実行中の検索には影響しません。',keeping:'新しい検索の間、前回の完了した結果を表示しています。',loadingRead:'原文を読み込んでいます…',readError:'原文を読み込めませんでした。もう一度開いてください。',tokenContext:'トークンによる前後の文脈',real:'検索',replayed:'記録した検索の再生',unknownProgress:'進捗の割合は不明',dependencies:'係り受け',viewBunsetsu:'文節',viewWords:'語',viewVertical:'全文（縦）',evidenceCaption:'この用例が結び付ける語',evidenceWord:'語番号',evidenceRoles:'役割',evidenceText:'表層形',evidenceLemma:'辞書形',evidencePos:'品詞',evidenceHead:'係り先',evidenceRel:'関係',root:'文の中心',frequencyReturn:'頻度に戻る',occurrenceMissing:'頻度表の用例をこの検索で開けませんでした。',
};
const extra: Record<string,string> = {readingOptions:'Reading options',questionActions:'Question actions',stopReplay:'Stop replay',current:'Current results',previous:'Previous results',completed:'Complete',partial:'Searching · counts are partial',cancelled:'Stopped · counts are partial',failed:'Search failed',retained:'Displayed examples',noLines:'No lines for this pattern yet.',download:'Download JSONL',source:'Pattern used for this search',copy:'Copy CLI command',copied:'Copied',savedQuery:'Download this query.lx',runNumber:'Search',scope:'Document condition',all:'All documents',soFar:'matches found',running:'Searching',notTotal:'Counts are partial until the search finishes.',stopped:'Search stopped. Counts are partial.',failedKeep:'Search failed. Previous results are retained.',errorRetry:'Check the pattern or condition and run the search again.',draftRunning:'Editing the next pattern. The running search is unchanged.',keeping:'Showing previous results while searching.',loadingRead:'Loading the original text…',readError:'Could not load the original text. Open it again to retry.',tokenContext:'Token context',real:'Live search',replayed:'Recorded search replay',unknownProgress:'Scan percentage unavailable',occurrenceMissing:'Could not open the frequency table occurrence in this search.'};
const t = (key: string) => (locale === 'ja' ? ja[key] : null) ?? en[key] ?? extra[key] ?? key;
Object.assign(ja,{catalogChanged:'このリンクを作ったときから、サーバーのパターンが変わっています。今のパターンで検索すると、リンクを作ったときと結果が異なることがあります。',catalogSearch:'今のパターンで検索',catalogDefault:'既定の問いを開く',exportRun:'全結果を JSONL で保存',completeUndisplayed:'検索は完了しましたが、結果をここに表示できません。',completeUnexportable:'検索は完了しましたが、結果をここで表示することも保存することもできません。パターンを絞り込むか、コマンドラインの ling matches で全件を保存してください。',skipQuestion:'問いへ移動',skip:'検索結果へ移動',readingEnd:'。',noMatches:'このパターンに一致する用例はありません。',partialCount:'途中までの件数',limitChoice:'定義を 1 つだけ検索:',chooseDefinition:'検索する定義を 1 つ選んでください。',occurrenceNotFound:'この検索には、その文で同じ語が同じ役割に入る用例がありません。',occurrenceChanged:'頻度表を作ったときとコーパスまたはパターンが異なるため、用例を特定できません。',analysisChanged:'この問いを保存したときから、コーパスの内容が変わっています。現在のコーパスで検索し直せます。',searchCurrentCorpus:'現在のコーパスで検索',resultLimit:'結果が多すぎて保持できません。パターンや文書の条件を絞り込んで、もう一度検索してください。',current:'用例',previous:'前回の完了した用例',boundaryBody:'検索中の件数は途中経過です。ダウンロードには全件を含みます。原文は文書の条件で絞り込まずに表示します。',return:'用例に戻る',during:'新しい検索の実行中は',keep:'前回の検索結果を表示'});
Object.assign(extra,{completeUndisplayed:'The search is complete, but its results cannot be displayed here.',completeUnexportable:'The search is complete, but its results can be neither displayed nor downloaded here. Narrow the pattern, or save every match with ling matches on the command line.',root:'heads the sentence',noMatches:'No examples match this pattern.',partialCount:'Count so far',limitChoice:'Search one definition only:',chooseDefinition:'Choose one definition to search.',occurrenceNotFound:'This search has no example in that sentence with the same words in the same roles.',occurrenceChanged:'The corpus or pattern differs from the one the frequency table counted, so the example cannot be located.'});extra.current='Results';extra.resultLimit='Too many results to retain. Narrow the pattern or document conditions and search again.';extra.analysisChanged='The corpus has changed since this question was saved. You can search the current corpus instead.';extra.searchCurrentCorpus='Search the current corpus';extra.previous='Previous results';
let config: Configuration;
const named=(attribute:string,value:string)=>valueName(config?.value_names,attribute,value,locale);
let controls: QuestionControls;
const tokenLab=$('#token-lab');
const focusedSentence=document.createElement('p');focusedSentence.id='focused-sentence';focusedSentence.lang='ja';focusedSentence.tabIndex=-1;
const exampleSteps=document.createElement('nav');exampleSteps.className='example-steps';exampleSteps.setAttribute('aria-label','Examples');
exampleSteps.innerHTML='<button type="button" data-example-step="-1"></button><button type="button" data-example-step="1"></button>';
const tokenDetails=document.createElement('details');tokenDetails.id='token-details';tokenDetails.open=!matchMedia('(max-width:480px)').matches;
const tokenSummary=document.createElement('summary');tokenDetails.append(tokenSummary);
const dependencyPanel=$('#dependency-panel');
for(const element of [...tokenLab.children].slice(1))if(element!==dependencyPanel)tokenDetails.append(element);
tokenLab.append(focusedSentence,exampleSteps,dependencyPanel,tokenDetails);
function localizeExample(){tokenSummary.textContent=locale==='ja'?'トークンを調べる':'Inspect tokens';$('[data-example-step="-1"]',exampleSteps).textContent=locale==='ja'?'前の用例':'Previous example';$('[data-example-step="1"]',exampleSteps).textContent=locale==='ja'?'次の用例':'Next example';}
const readerPanel=$<HTMLDialogElement>('#inspector');
const readerStatus=document.createElement('p');readerStatus.className='reader-status';readerStatus.setAttribute('role','status');$('#document-body').before(readerStatus);
let readerContentKey:string|null=null;
const readerDisabled=new WeakMap<HTMLButtonElement,boolean>();
let readerTarget:{run:RunSnapshot;row:MatchRecord}|null=null;
let tokenData: StoredSentence | null=null;
let tokenIndex=0;
let attributeKey='lemma';
let attributeMember:string|undefined;
let analysisExpectation:string|null=null;
let configRefresh:ReturnType<typeof setTimeout>|undefined;
const tokenCache=new Map<string,StoredSentence>();
let state: SearchState = { current:null, completed:null, previous:null };
let stream: EventSource | undefined;
let activeId: string | null = null;
let requestEpoch = 0;
let replayEpoch = 0;
let history: RunSnapshot[] = [];
let recorded: RunSnapshot[] = [];
let inspectionEpoch=0;
let inspectionFailure:{runId:string;sequence:number;message:string}|null=null;
let conditions:readonly TokenCondition[]=[];
let conditionSentence='';
let inspecting: { run: RunSnapshot; row: MatchRecord } | null | undefined;
const selection = new Map<string,string>();
const filters = new Map<string,string>();
const panels = new Map<string,HTMLElement>();
const documentSections=new Map<string,{run:RunSnapshot;origin:SectionOrigin|null}>();
let savedManifest:readonly SectionLink[]=[];
let restoreController:AbortController|null=null;
const expectedAnalyses=new Map<string,string|null>();
const unavailableSearches=new Map<string,{expired:boolean;message:string}>();
let loadingSaved=false;
function recipeFor(run:RunSnapshot):QueryRecipe{return {source:run.source,dataset:run.dataset,where:run.where,analysis:run.contract_run?.analysis_identity??savedManifest.find(link=>link.id===run.id)?.recipe?.analysis??expectedAnalyses.get(run.id)??config.datasets[run.dataset]?.analysis_identity??null};}
function sectionLinks():readonly SectionLink[]{
  const links=savedManifest.map(link=>{const live=documentSections.get(link.id);return live?{...link,recipe:recipeFor(live.run),origin:live.origin}:link;});
  for(const [id,section] of documentSections)if(!links.some(link=>link.id===id))links.push({id,origin:section.origin,recipe:recipeFor(section.run)});
  if(!links.length&&state.current)links.push({id:state.current.id,origin:null,recipe:recipeFor(state.current)});
  return links;
}
function availableRun(id:string):RunSnapshot|undefined{return documentSections.get(id)?.run??[state.current,state.completed,state.previous].find(run=>run?.id===id)??undefined;}
function saveSections(){if(restoreController)return; if(!documentSections.size&&state.current&&savedManifest.every(link=>availableRun(link.id)))savedManifest=[{id:state.current.id,origin:null,recipe:recipeFor(state.current)}];else savedManifest=sectionLinks();try{sessionStorage.setItem('ling-questions-sections',JSON.stringify(savedManifest));}catch{}renderSavedSearches();}
function renderSavedSearches(){
  const missing=savedManifest.filter(link=>!availableRun(link.id));
  $('#saved-searches').hidden=!missing.length&&!restoreController;
  const notice=$('#saved-searches-note');
  notice.textContent=restoreError??(restoreController?(locale==='ja'?'保存した検索を順番に実行しています。表示中の用例はそのまま残ります。':'Running saved searches in order. Completed results remain visible.'):(locale==='ja'?'検索結果の保存期間が終了しました。保存した検索条件と、検索どうしのつながりは残っています。':'Results expire with the server session. Saved queries and their relationships are retained.'));
  $('#saved-searches-list').replaceChildren(...missing.map(link=>{const item=document.createElement('li');const recipe=link.recipe;item.textContent=(recipe?`${config.datasets[recipe.dataset]?.label??recipe.dataset} · ${recipe.source}${recipe.where?' · '+recipe.where:''}`:link.id)+' · '+(!recipe?(locale==='ja'?'古い保存形式には検索のソースが含まれていません。':'This older saved search does not include its query source.'):(unavailableSearches.get(link.id)?.message??(locale==='ja'?'保存した検索':'Saved search')));return item;}));
  const button=$<HTMLButtonElement>('#restore-searches');button.disabled=!!restoreController||loadingSaved||!missing.some(link=>link.recipe)&&![...unavailableSearches.values()].some(value=>!value.expired);
  button.textContent=[...unavailableSearches.values()].some(value=>!value.expired)?(locale==='ja'?'保存した検索を再読み込み':'Retry loading saved searches'):(locale==='ja'?'保存した検索を実行':'Run saved searches');
}
let restoreError:string|null=null;
async function loadSavedSections(){
  loadingSaved=true;renderSavedSearches();
  try{for(const link of savedManifest){
    if(availableRun(link.id))continue;
    try{const run=await readRun(link.id);documentSections.set(link.id,{run,origin:link.origin});unavailableSearches.delete(link.id);}
    catch(error){unavailableSearches.set(link.id,{expired:expiredSearch(error),message:errorMessage(error)});}
  }
  const loaded=new Map(documentSections);documentSections.clear();
  for(const link of savedManifest){const section=loaded.get(link.id);if(section){documentSections.set(link.id,section);loaded.delete(link.id);}}
  for(const [id,section] of loaded)documentSections.set(id,section);
  }finally{loadingSaved=false;renderSavedSearches();}
}
async function executeSavedSection(link:SectionLink&{recipe:QueryRecipe},signal:AbortSignal):Promise<string>{
  if(signal.aborted)throw new Error('Saved-search restore stopped. Run saved searches to continue.');
  const recipe=link.recipe;
  if(!Object.hasOwn(config.datasets,recipe.dataset))throw new Error(`The corpus for this saved query is unavailable: ${recipe.dataset}.`);
  setSource(recipe.source);$<HTMLSelectElement>('#corpus').value=recipe.dataset;
  analysisExpectation=recipe.analysis;controls.restore(new URLSearchParams({...controls.entries(),metadata:'{}',raw:recipe.where}));updateCorpus();
  const body={source:recipe.source,dataset:recipe.dataset,where:recipe.where,...(recipe.analysis?{expected_analysis_identity:recipe.analysis}:{})};
  const {id}=await post('api/questions/runs',body,signal);expectedAnalyses.set(id,recipe.analysis);
  if(signal.aborted){await post(`api/questions/runs/${id}/cancel`);throw new Error('Saved-search restore stopped. Run saved searches to continue.');}
  state=begin(state,runningSnapshot(id,body));viewEvidence=null;activeId=id;render();
  return new Promise<string>((resolve,reject)=>{
    const events=new EventSource(serviceURL(`api/questions/runs/${id}/events`));stream=events;
    const stop=()=>{events.close();signal.removeEventListener('abort',cancel);if(activeId===id)activeId=null;};
    const cancel=()=>{stop();void post(`api/questions/runs/${id}/cancel`).catch(()=>{});if(state.current?.id===id)state={...state,current:{...state.current,phase:'cancelled'}};reject(new Error('Saved-search restore stopped. Run saved searches to continue.'));};
    signal.addEventListener('abort',cancel,{once:true});
    events.addEventListener('update',event=>{
      if(signal.aborted||activeId!==id)return;
      try{
        state=receiveUpdate(state,parseUpdate(event.data));const run=state.current!;render();
        if(run.phase==='running')return;
        stop();
        if(run.phase!=='complete'){reject(new Error(runError(run,t)??'Saved search did not complete. Run saved searches to retry.'));return;}
        documentSections.set(id,{run,origin:link.origin});sessionStorage.setItem('ling-questions-completed',id);sessionStorage.setItem('ling-questions-run',id);resolve(id);
      }catch(error){stop();void post(`api/questions/runs/${id}/cancel`).catch(()=>{});reject(error);}
    });
    events.onerror=()=>{if(activeId===id)$('#status').textContent=locale==='ja'?'接続を再開しています。表示中の用例はそのまま残ります。':'Reconnecting. Completed results remain visible.';};
  });
}
$('#restore-searches').addEventListener('click',async()=>{
  if(restoreController)return;
  if([...unavailableSearches.values()].some(value=>!value.expired)){await loadSavedSections();render();return;}
  if(activeId){$('#saved-searches-note').textContent=locale==='ja'?'実行中の検索を停止してから再試行してください。':'Stop the current search before restoring saved searches.';return;}
  restoreError=null;restoreController=new AbortController();++requestEpoch;++navigationEpoch;viewLocation=defaultView();render();renderSavedSearches();
  try{
    await restoreSections(savedManifest,new Set(savedManifest.filter(link=>availableRun(link.id)?.phase==='complete').map(link=>link.id)),link=>executeSavedSection(link,restoreController!.signal),manifest=>{
      savedManifest=manifest;unavailableSearches.clear();
      for(const link of manifest){const section=documentSections.get(link.id);if(section)section.origin=link.origin;}
      try{sessionStorage.setItem('ling-questions-sections',JSON.stringify(savedManifest));}catch{}
      renderSavedSearches();
    });
  }catch(error){restoreError=restoreController?.signal.aborted?(locale==='ja'?'保存した検索の実行を停止しました。再実行すると続きから再開します。':'Saved-search restore stopped. Run saved searches to continue.'):errorMessage(error);}
  finally{restoreController=null;render();renderSavedSearches();updateDraft();}
});

const documentViews=new Map<string,DocumentView>();
let isReplay = false;
let viewLocation: ViewLocation=defaultView();
let viewEvidence: RunSnapshot | null=null;
let navigationEpoch=0;
let pendingView: ViewLocation | null=null;
let pendingOccurrence: OccurrenceTarget | null=null;
let entryLines:(EntryLines&Pick<OccurrenceTarget,'pattern'|'analysis'|'fingerprint'>)|null=null;
const entryLine=(at:number):OccurrenceTarget=>({...entryLines!.lines[at],pattern:entryLines!.pattern,analysis:entryLines!.analysis,fingerprint:entryLines!.fingerprint});
function entryActive():boolean{return !!entryLines&&!!inspecting&&!tokenLab.hidden&&inspecting.row.pattern===entryLines.pattern&&inspecting.row.corpus_sentence_ordinal===entryLines.lines[entryLines.at].sentence;}
const entryRuns=new Set<string>();
const entryShown=(run:RunSnapshot|null|undefined)=>!!entryLines&&!!run&&entryRuns.has(run.id);
let searchLimit:(SearchLimit&{readonly source:string})|null=null;
const runLimits=new Map<string,SearchLimit>();
const documentTitles=new Map<string,string>();
let preferredPattern='';
function activeLimit():SearchLimit|null{const source=$<HTMLTextAreaElement>('#source').value;return searchLimit&&(searchLimit.source===source||!!searchLimit.pattern&&definitionOffset(source,searchLimit.pattern)>=0)&&(searchLimit.pattern||searchLimit.document!=null)?searchLimit:null;}
const documentName=(dataset:string,document:number)=>documentTitles.get(`${dataset}:${document}`)??(locale==='ja'?`文書${document}`:`document ${document}`);
let revealedFailure:string|null=null;
const editError=document.createElement('button');editError.type='button';editError.id='edit-pattern-error';editError.hidden=true;$('#error').after(editError);
const patternFailure=(run:RunSnapshot|null)=>run?.phase==='failed'?run.diagnostics?.find(d=>d.data.stage==='catalog'&&d.data.severity==='error'):undefined;
type PatternEditor={mountPatternEditor(textarea:HTMLTextAreaElement,options:object):Promise<boolean>;selectSource(textarea:HTMLTextAreaElement,from:number,to?:number):void;recheck(textarea:HTMLTextAreaElement):void;formatCurrent(textarea:HTMLTextAreaElement):Promise<unknown>};
let patternEditor:Promise<PatternEditor|null>|null=null,editorLocale:((locale:string)=>Promise<void>)|null=null;
function loadPatternEditor():Promise<PatternEditor|null>{
  patternEditor??=(async()=>{
    const stylesheet=document.createElement('link');stylesheet.rel='stylesheet';stylesheet.href=new URL('../editor/pattern-editor.css',import.meta.url).href;document.head.append(stylesheet);
    try{
      const [editor,i18n]=await Promise.all([import(new URL('../editor/pattern-editor.js',import.meta.url).href),import(new URL('../i18n.js',import.meta.url).href)]);
      editorLocale=i18n.useLocale;await i18n.useLocale(locale);
      const mounted=await editor.mountPatternEditor($<HTMLTextAreaElement>('#source'),{corpus:()=>$<HTMLSelectElement>('#corpus').value||null,formatOnBlur:false,describedBy:'draft-note'});
      if(mounted)$('#format-source').hidden=false;
      return mounted?editor as PatternEditor:null;
    }catch{return null;}
  })();
  return patternEditor;
}
function setSource(value:string){const input=$<HTMLTextAreaElement>('#source');input.value=value;input.dispatchEvent(new Event('pattern:sync'));}
function focusSource(from:number|null,to=from){
  const input=$<HTMLTextAreaElement>('#source');input.focus();if(from!=null)input.setSelectionRange(from,to??from);
  void loadPatternEditor().then(editor=>{if(editor&&!$('#source-editor').hidden)editor.selectSource(input,from??0,to??from??0);});
}
function editFailedPattern(focus=true){
  const run=state.current,diagnostic=patternFailure(run);if(!run||!diagnostic)return;
  $('#source-editor').hidden=false;$('#edit-source').setAttribute('aria-expanded','true');void loadPatternEditor();
  const input=$<HTMLTextAreaElement>('#source');
  if(focus){const span=diagnostic.data.location?.source_span;const range=span&&input.value===run.source?sourceSelection(run.source,span):null;focusSource(range?.[0]??null,range?.[1]);}
}
editError.addEventListener('click',()=>editFailedPattern());
const limitChoices=document.createElement('p');limitChoices.id='limit-choices';limitChoices.className='limit-choices';limitChoices.hidden=true;editError.after(limitChoices);
const currentCorpus=document.createElement('button');currentCorpus.type='button';currentCorpus.id='search-current-corpus';currentCorpus.hidden=true;limitChoices.after(currentCorpus);
currentCorpus.addEventListener('click',()=>{analysisExpectation=null;void search();});
const overflowed=(run:RunSnapshot|null)=>run?.phase==='failed'&&run.error_code==='result_limit';
function renderLimitChoices(run:RunSnapshot|null){
  const names=overflowed(run)&&run!.definitions.length>1?run!.definitions:[],full=new Set(overflowingDefinitions(run?.counts??{}));
  const rest=names.filter(name=>!full.has(name)),choices=rest.length?rest:names;
  limitChoices.hidden=!choices.length;
  const signature=JSON.stringify([locale,choices]);if(renderSignatures.get(limitChoices)===signature)return;renderSignatures.set(limitChoices,signature);
  limitChoices.replaceChildren(document.createTextNode(t('limitChoice')+' '),...choices.map(name=>{const button=document.createElement('button');button.type='button';button.dataset.limitPattern=name;button.textContent=name;return button;}));
}
limitChoices.addEventListener('click',event=>{
  const button=(event.target as Element).closest<HTMLButtonElement>('[data-limit-pattern]'),run=state.current;if(!button||!run)return;
  const pattern=button.dataset.limitPattern!;
  setSource(run.source);$<HTMLSelectElement>('#corpus').value=run.dataset;
  searchLimit={source:run.source,pattern,document:searchLimit?.source===run.source?searchLimit.document:null};preferredPattern=pattern;
  updateCorpus();void search();
});
function overflowText(run:RunSnapshot):string|null{
  const names=overflowed(run)?overflowingDefinitions(run.counts):[];
  if(!names.length||names.length>=Object.keys(run.counts).length)return null;
  return locale==='ja'?`結果が多すぎて保持できません。途中までの件数が多い定義: ${names.map(name=>`${name}（${count(run.counts[name])} 件）`).join('、')}。`:`Too many results to retain. Highest counts so far: ${names.map(name=>`${name} (${count(run.counts[name])})`).join(', ')}.`;
}


const compactLayout=matchMedia('(max-width:480px)');
function adaptControls(){for(const selector of ['#question-actions'])for(const details of document.querySelectorAll<HTMLDetailsElement>(selector))details.open=!compactLayout.matches;}
adaptControls();compactLayout.addEventListener('change',adaptControls);

async function request<T>(path: string, options?: RequestInit): Promise<T> {
  const response = await fetch(serviceURL(path), options);
  const result = await response.json();
  if (!response.ok) throw new ServiceError(response.status,result.error ?? `HTTP ${response.status}`);
  return result as T;
}
const post = <T = {id: string}>(path:string,body:object={},signal?:AbortSignal) => request<T>(path,{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify(body),signal});

function translate() {
  document.documentElement.lang = locale;void editorLocale?.(locale);
  localizeExample();renderDependencies();if(tokenData&&inspecting)selectToken(tokenIndex);
  for (const element of document.querySelectorAll<HTMLElement>('[data-t]')) element.textContent = t(element.dataset.t??'');
  $('#language').textContent = locale === 'en' ? '日本語' : 'English';
  $('.sentence-period').textContent = locale === 'ja' ? 'を探す。' : '.';
  $('#corpus-join').textContent=locale==='ja'?'':' in ';
  $('#corpus-comma').textContent='';
  $('#scope-end').textContent=locale==='ja'?'から ':'';
  $('.question-sentence').insertBefore($('#edit-source'),locale==='ja'?$('.sentence-period'):$('#corpus-join'));
  for(const button of document.querySelectorAll<HTMLButtonElement>('[data-reader-document]'))button.textContent=Number(button.dataset.readerDocument)<0?(locale==='ja'?'前の文書':'Previous document'):(locale==='ja'?'次の文書':'Next document');
  controls?.localize();
  $<HTMLInputElement>('#metadata-query').placeholder=locale==='ja'?'値を検索…':'Find a value…';$<HTMLInputElement>('#metadata-query').setAttribute('aria-label',locale==='ja'?'メタデータの値を検索':'Find a metadata value');
  $<HTMLInputElement>('#where').placeholder = locale === 'ja' ? 'すべての文書 · 必要なら --where 条件を入力' : 'All documents · optional --where clause';
  $<HTMLSelectElement>('#corpus').setAttribute('aria-label',locale==='ja'?'コーパス':'Corpus');
  $<HTMLSelectElement>('#metadata-field').setAttribute('aria-label',locale==='ja'?'メタデータの項目':'Metadata field');
  $('#results').setAttribute('aria-label',locale==='ja'?'検索結果':'Search results');
  $('.search-status').setAttribute('aria-label',locale==='ja'?'今回の検索':'Current search');
  updateCorpus();
  updateDraft();
  render(true);
}

function updateCorpus() {
  const dataset=config?.datasets[$<HTMLSelectElement>('#corpus').value];if(!dataset)return;
  const facts:string[]=[];
  if(dataset.documents!=null)facts.push(`${count(dataset.documents)} ${locale==='ja'?'文書':'documents'}`);
  if(dataset.sentences!=null)facts.push(`${count(dataset.sentences)} ${locale==='ja'?'文':'sentences'}`);
  if(dataset.readiness!=='ready')facts.push(locale==='ja'?(dataset.readiness==='failed'?'コーパスの準備に失敗しました。検索で再試行できます。':'コーパスを準備しています。検索すると準備の完了を待ちます。'):(dataset.readiness==='failed'?'Corpus preparation failed. Search to retry.':'Corpus preparing · searches wait until it is ready.'));
  $('#corpus-meta').textContent=dataset.readiness==='ready'?'':facts.join(' · ');
  $('#corpus').title=facts.join(' · ');
  scheduleWorkbenchLink();
  clearTimeout(configRefresh);
  if(dataset.readiness!=='ready')configRefresh=setTimeout(()=>void refreshConfiguration(),1500);
}
let workbenchMinted:{body:string;href:Promise<string>}|null=null,workbenchShown='',workbenchTimer:ReturnType<typeof setTimeout>|undefined;
const workbenchBody=()=>workbenchLinkRequest($<HTMLTextAreaElement>('#source').value,$<HTMLSelectElement>('#corpus').value);
function mintWorkbenchLink():Promise<string>{
  clearTimeout(workbenchTimer);
  const body=workbenchBody();
  if(workbenchMinted?.body===body)return workbenchMinted.href;
  const href=fetch(serviceURL('api/permalink'),{method:'POST',headers:{'Content-Type':'application/x-www-form-urlencoded'},body}).then(response=>response.json()).then(payload=>workbenchHref(payload)).catch(()=>null).then(href=>href??serviceURL('workbench'));
  const minted=workbenchMinted={body,href};
  void href.then(value=>{if(workbenchMinted===minted){$<HTMLAnchorElement>('#incumbent').href=value;workbenchShown=body;}});
  return href;
}
function scheduleWorkbenchLink(){clearTimeout(workbenchTimer);workbenchTimer=setTimeout(()=>void mintWorkbenchLink(),250);}
$<HTMLAnchorElement>('#incumbent').addEventListener('click',event=>{
  if(workbenchShown===workbenchBody()||event.button!==0||event.ctrlKey||event.metaKey||event.shiftKey||event.altKey)return;
  event.preventDefault();
  const tab=window.open('about:blank','_blank');if(tab)tab.opener=null;
  void mintWorkbenchLink().then(href=>{if(tab)tab.location.href=href;});
});
const servedCatalog=()=>({source:config.source,fingerprint:config.catalog_fingerprint});
let changedCatalog:string|null=null;
function settleCatalog(){changedCatalog=null;$('#catalog-notice').hidden=true;}
$('#catalog-search').addEventListener('click',()=>{settleCatalog();$('#question-sentence').focus();void search();});
$('#catalog-default').addEventListener('click',()=>{window.history.replaceState(null,'',location.pathname+'#fresh=1');location.reload();});
async function refreshConfiguration(){
  try{config=await request<Configuration>('api/questions/config');}catch{}
  updateCorpus();
}

function updateDraft(commit: boolean | Event=true) {
  const source = $<HTMLTextAreaElement>('#source').value;
  scheduleWorkbenchLink();
  const names=state.current?.source===source?state.current.definitions:[];
  if(searchLimit?.document!=null&&(controls?.where()??$<HTMLInputElement>('#where').value))searchLimit={...searchLimit,document:null};
  const limit=activeLimit(),ranLimit=state.current?runLimits.get(state.current.id):undefined;
  $('#source-label').textContent=limit?.pattern??(names?.length===1?names[0]:menuDefinitions().length?(locale==='ja'?'すべての定義':'every definition'):(locale==='ja'?'パターンのソース':'pattern source'));
  if(!definitionMenu.hidden)renderDefinitionMenu();
  renderLimits(limit);
  const dirty = state.current && (source !== state.current.source || (controls?.where()??$<HTMLInputElement>('#where').value) !== state.current.where || $<HTMLSelectElement>('#corpus').value !== state.current.dataset || (limit?.pattern??null)!==(ranLimit?.pattern??null) || (limit?.document??null)!==(ranLimit?.document??null));
  $('#draft-badge').hidden = !dirty;
  if(!config||!controls)return;
  const evidence=displayedEvidence();
  const params: Record<string,string|number>={...questionDocument(),behavior:$<HTMLSelectElement>('#behavior').value};
  delete params.format;delete params.question;delete params.sections;delete params.where;delete params.evidence;delete params.view;
  if(evidence&&source===evidence.source)delete params.source;
  if(evidence&&$<HTMLSelectElement>('#corpus').value===evidence.dataset){delete params.corpus;delete params.analysis;}
  params.follow=!viewEvidence&&!inspecting?'1':'0';
  if(activeId){params.attempt=activeId;if(state.completed)params.completed=state.completed.id;}
  const linked=catalogParams(params,servedCatalog());
  if(changedCatalog!==null&&'catalog' in linked)linked.catalog=changedCatalog;
  const url=locationURL(location.href,linked,evidence?.id??null,viewLocation);
  if(commit)window.history.replaceState(null,'',url);

}

const limitChips=document.createElement('div');limitChips.className='scope-chips limit-chips';$('#scope-chips').after(limitChips);
function renderLimits(limit:SearchLimit|null){
  const label=$('#scope-label'),dataset=$<HTMLSelectElement>('#corpus').value;
  if(limit?.document!=null){label.textContent=documentName(dataset,limit.document);label.dataset.limited='';}
  else if(label.dataset.limited!==undefined){delete label.dataset.limited;label.textContent=locale==='ja'?'すべての文書':'all documents';}
  const chips=[...(limit?.pattern?[['pattern',locale==='ja'?`定義: ${limit.pattern}`:`Definition: ${limit.pattern}`]]:[]),...(limit?.document!=null?[['document',locale==='ja'?`文書: ${documentName(dataset,limit.document)}`:`Document: ${documentName(dataset,limit.document)}`]]:[]),...(entryShown(displayedEvidence())?[['entry',locale==='ja'?`用例: ${entryLines!.words.join(' ')}`:`Examples: ${entryLines!.words.join(' ')}`]]:[])];
  const signature=JSON.stringify(chips);if(renderSignatures.get(limitChips)===signature)return;renderSignatures.set(limitChips,signature);
  limitChips.replaceChildren(...chips.map(([kind,text])=>{const button=document.createElement('button');button.type='button';button.className='scope-chip';button.dataset.clearLimit=kind;button.setAttribute('aria-label',kind==='pattern'&&menuDefinitions().some(definition=>definition.count_only)?(locale==='ja'?`${text} を別の定義に変える`:`Choose another definition than ${text}`):locale==='ja'?`${text} の限定を外す`:`Remove the limit ${text}`);button.innerHTML=`${escape(text)}<span aria-hidden="true"> ×</span>`;return button;}));
}
limitChips.addEventListener('click',event=>{const button=(event.target as Element).closest<HTMLButtonElement>('[data-clear-limit]');if(button?.dataset.clearLimit==='entry'){leaveEntry();return;}if(!button||!searchLimit)return;if(button.dataset.clearLimit==='pattern'&&menuDefinitions().some(definition=>definition.count_only)){openDefinitionMenu();return;}searchLimit=button.dataset.clearLimit==='pattern'?{...searchLimit,pattern:null}:{...searchLimit,document:null};updateDraft();});
function restoreLimit(params:URLSearchParams){
  const pattern=params.get('pattern')||null,document=params.get('document');
  searchLimit=pattern||document?{source:$<HTMLTextAreaElement>('#source').value,pattern,document:document&&/^\d+$/.test(document)?Number(document):null}:null;
}

function saveReplay(final: CompletedSnapshot) {
  const frames=history.map(({id,phase,received,counts,elapsed,version,definitions})=>({id,phase,received,counts,elapsed,version,definitions}));
  try {sessionStorage.setItem('ling-questions-replay',JSON.stringify({final,frames}));} catch {}
}
function restoreReplay(final: CompletedSnapshot): RunSnapshot[] {
  try {
    const saved=JSON.parse(sessionStorage.getItem('ling-questions-replay')??'null');
    if(saved?.final.id!==final.id) return [];
    return (saved.frames as Pick<RunSnapshot,'id'|'phase'|'received'|'counts'|'elapsed'|'version'|'definitions'>[]).map(frame=>({...final,...frame,rows:Object.fromEntries(Object.entries(final.rows).map(([name,rows])=>[name,rows.slice(0,frame.counts[name]??0)]))}));
  } catch {return [];}
}

async function search(event?: Event,derivedFrom?:SectionOrigin,occurrence?:OccurrenceTarget) {
  event?.preventDefault();
  if(restoreController)return;
  if(changedCatalog!==null)settleCatalog();
  const parent=displayedEvidence();
  const origin=derivedFrom??(documentSections.size&&parent?{parent:parent.id,view:viewLocation,label:locale==='ja'?'前の検索':'previous search'}:undefined);
  if(origin&&parent&&!documentSections.has(parent.id)){documentSections.set(parent.id,{run:parent,origin:null});saveSections();}
  const epoch = ++requestEpoch;
  viewEvidence=null;pendingView=null;++navigationEpoch;
  const source=$<HTMLTextAreaElement>('#source').value;
  if(occurrence)searchLimit={source,pattern:occurrence.pattern,document:occurrence.document};
  const where=controls?.where()??$<HTMLInputElement>('#where').value;
  if(where&&searchLimit?.document!=null)searchLimit={...searchLimit,document:null};
  const limit=activeLimit();
  const body={source,dataset:$<HTMLSelectElement>('#corpus').value,where,...(analysisExpectation?{expected_analysis_identity:analysisExpectation}:{}),...(limit?.pattern?{pattern:limit.pattern}:{}),...(limit?.document!=null?{document:limit.document}:{})};
  ++replayEpoch;
  isReplay = false;
  $('#replay-note').hidden = true;
  $<HTMLButtonElement>('#run').disabled = true;
  const prior = activeId;
  stream?.close();
  activeId = null;
  try {
    if(prior){await post(`api/questions/runs/${prior}/cancel`);const section=documentSections.get(prior);if(section?.run.phase==='running')section.run={...section.run,phase:'cancelled'};}
    const result = await post('api/questions/runs',body);
    if (epoch !== requestEpoch) {
      await post(`api/questions/runs/${result.id}/cancel`);
      return;
    }
    activeId = result.id;if(occurrence)entryRuns.add(result.id);expectedAnalyses.set(result.id,analysisExpectation);runLimits.set(result.id,{pattern:limit?.pattern??null,document:limit?.document??null});
    state = begin(state,runningSnapshot(result.id,body));
    if(origin&&parent){documentSections.set(result.id,{run:state.current!,origin});saveSections();}
    history = [];
    sessionStorage.setItem('ling-questions-run',result.id);
    render(true);
    updateDraft();
    connect(result.id);
  } catch(error) {
    if (epoch!==requestEpoch) return;
    state=begin(state,runningSnapshot(`request-${epoch}`,body));
    state={...state,current:{...runningSnapshot(`request-${epoch}`,body),phase:'failed',error:errorMessage(error)}};
    render(true);
  } finally {
    if (epoch === requestEpoch) $<HTMLButtonElement>('#run').disabled=false;
  }
}

function connect(id: string) {
  stream = new EventSource(serviceURL(`api/questions/runs/${id}/events`));
  stream.addEventListener('update',event=>{
    if (id !== activeId) return;
    try {state=receiveUpdate(state,parseUpdate(event.data));}
    catch(error){const failed={...state.current!,phase:'failed' as const,error:locale==='ja'?'検索の更新を読み込めませんでした。もう一度検索してください。':'Search updates could not be read. Run the question again.'};state=receive(state,failed);stream?.close();activeId=null;post(`api/questions/runs/${id}/cancel`).catch(()=>{});render(true);return;}
    const snapshot=state.current!;
    if(viewEvidence?.id===snapshot.id)viewEvidence=snapshot;
    if (snapshot.phase!=='running' || (history.length<239 && (!history.length || snapshot.received!==history.at(-1)!.received))) history.push(snapshot);
    if (snapshot.phase!=='running') {
      stream?.close();
      activeId=null;
      if (snapshot.phase==='complete') {
        recorded=history;
        saveReplay(snapshot);
        const view=pendingView,occurrence=pendingOccurrence;pendingView=null;pendingOccurrence=null;
        if(view)void navigateView(hrefFor(snapshot,view),true,'replace');
        if(occurrence)void openOccurrence(snapshot,occurrence);
        if (!state.previous && !$('#source-editor').contains(document.activeElement)) {
          $('#source-editor').hidden=true;
          $('#edit-source').setAttribute('aria-expanded','false');
        }
        sessionStorage.setItem('ling-questions-completed',snapshot.id);
        saveSections();void refreshConfiguration();
      }
    }
    render();
    updateDraft();
  });
  stream.onerror=()=>{
    if (id!==activeId) return;
    $('#status').textContent=locale==='ja'?'接続を再開しています。結果はそのまま保持します。':'Reconnecting. Displayed results are retained.';
  };
}

function render(force=false) {
  if (!config||!controls) return;
  const evidence=displayedEvidence(),pattern=evidence?(selection.get(evidence.id)??evidence.definitions[0]??''):'';
  controls.examplesFound(evidence?.phase==='complete'?evidence.counts[pattern]??0:null);
  updateDraft(false);
  const current=state.current;
  if(current&&documentSections.has(current.id))documentSections.get(current.id)!.run=current;
  $('#run').hidden=current?.phase==='running';
  $<HTMLInputElement>('#timeline').max=String(Math.max(0,recorded.length-1));
  if(!isReplay)$<HTMLInputElement>('#timeline').value=String(Math.max(0,recorded.length-1));
  $<HTMLButtonElement>('#cancel').hidden=!activeId&&!isReplay&&!restoreController;
  $<HTMLButtonElement>('#cancel').textContent=restoreController?(locale==='ja'?'保存した検索の実行を停止':'Stop saved searches'):t(isReplay?'stopReplay':'stop');
  $<HTMLButtonElement>('#retry').hidden=!current || current.phase==='running' || current.phase==='complete' || !!current.search_complete || overflowed(current) || current.error_code==='analysis_changed' || !!patternFailure(current);
  $<HTMLButtonElement>('#replay').disabled=!recorded.length || !!activeId || isReplay;
  $('#error').hidden=!current?.error;
  const diagnostic=patternFailure(current),line=diagnostic?.data.location?.line;
  const located=current?.phase==='failed'?(current.diagnostics??[]).filter(d=>d.data.stage==='catalog'&&d.data.severity==='error'):[];
  $('#error').textContent=located.length?located.map(d=>diagnosticText(d,locale)).join('\n'):current?overflowText(current)??runError(current,t)??'':'';
  renderLimitChoices(current);
  currentCorpus.hidden=!(current?.phase==='failed'&&current.error_code==='analysis_changed');currentCorpus.textContent=t('searchCurrentCorpus');
  editError.hidden=!diagnostic;
  editError.textContent=line?(locale==='ja'?`${line} 行目のパターンを修正`:`Edit pattern at line ${line}`):(locale==='ja'?'パターンを修正':'Edit pattern');
  if(diagnostic&&current&&revealedFailure!==current.id){revealedFailure=current.id;editFailedPattern($<HTMLTextAreaElement>('#source').value===current.source);}
  $('#status-dot').className='status-dot '+(current?.phase ?? '');
  if (current) {
    const prefix=isReplay?t('replayed'):t('real');
    const elapsed=current.elapsed>0?` · ${Number(current.elapsed).toFixed(1)}s`:'';
    const keeping=current.phase==='running'&&!isReplay&&$<HTMLSelectElement>('#behavior').value==='keep'&&!!state.completed&&state.completed.id!==current.id;
    $('#status').textContent = current.phase==='running' ? `${prefix} · ${t('running')} · ${count(current.received)} ${t('soFar')}${elapsed}${keeping?' · '+t('keeping'):''}` : current.phase==='complete' ? `${isReplay?t('replayed')+' · ':''}${t('completed')}` : current.phase==='cancelled' ? t('stopped') : current.search_complete ? t(exportable(current)?'completeUndisplayed':'completeUnexportable') : overflowed(current)&&!limitChoices.hidden?t('chooseDefinition') : state.previous?t('failedKeep'):t('errorRetry');
    $('#live-counts').innerHTML=Object.entries(current.counts).map(([name,total])=>`<span>${escape(name)} <strong>${count(total)}</strong>${current.phase==='running'?` <span>${locale==='ja'?'件（途中）':'so far'}</span>`:locale==='ja'?' 件':''}</span>`).join('');
  } else $('#status').textContent=t('ready');
  const exportRun=$<HTMLAnchorElement>('#export-run');
  exportRun.hidden=!(current?.phase==='failed'&&exportable(current));
  if(current&&!exportRun.hidden)exportRun.href=`api/questions/runs/${current.id}/export`;
  const behavior=$<HTMLSelectElement>('#behavior').value as Behavior;
  const pinned=!documentSections.size&&(!!viewEvidence&&current?.phase==='complete'&&viewEvidence.id!==current.id||behavior==='keep'&&inspecting&&tokenLab.isConnected&&!tokenLab.hidden&&state.current?.phase==='complete'&&inspecting.run.id!==state.current.id);
  $('#show-latest').hidden=!pinned;
  const earlier=isReplay?state.previous:viewEvidence??state.previous;
  const compared=!documentSections.size&&behavior==='compare' && earlier && earlier.id!==current?.id;
  let visibleRuns=isReplay?[current]:compared?[earlier,current]:[viewEvidence??(pinned&&inspecting?inspecting.run:visible(state,behavior))];
  if(!isReplay&&behavior==='keep'&&!viewEvidence&&current?.phase==='complete'&&state.previous&&state.previous.id!==current.id){
    const pattern=selection.get(current.id)??current.definitions[0]??'',page=documentView(current,pattern);
    if(page.documents===null&&!page.error){
      visibleRuns=[state.previous];
      $('#status').textContent=locale==='ja'?'検索完了 · 新しい用例を準備しています…':'Search complete · preparing the new examples…';
      if(!page.loading)void loadDocumentPage(null,current,pattern);
    }
  }
  if(current?.phase==='complete')$<HTMLAnchorElement>('#show-latest').setAttribute('href',hrefFor(current,defaultView()));
  $('.search-status').classList.toggle('has-update',!!pinned);
  if(pinned&&current){
    const shownRun=viewEvidence??inspecting?.run,label=config.datasets[current.dataset]?.label??current.dataset,other=shownRun&&shownRun.dataset!==current.dataset?config.datasets[shownRun.dataset]?.label??shownRun.dataset:null;
    $('#status').textContent=locale==='ja'?`${other?`${label} の`:'新しい'}検索が完了しました。開いている用例は${other?` ${other} の`:''}前回の検索結果です。`:`The ${other?`search of ${label}`:'new search'} is complete. The open example is from the previous search${other?` of ${other}`:''}.`;
  }
  const shown=[...new Map([...documentSections.values()].map(section=>section.run).concat(visibleRuns.filter((run):run is RunSnapshot=>run!=null&&!(run.phase==='failed'&&!Object.keys(run.counts).length))).map(run=>[run.id,run])).values()];
  $('#result-panels').className=compared?'comparing':'';
  const ids=shown.map(run=>run.id);
  for (const [id,panel] of panels) {
    if (!ids.includes(id)) {if(panel.contains(readerPanel))document.body.append(readerPanel);if(panel.contains(tokenLab))document.body.append(tokenLab);panel.remove();panels.delete(id);}
  }
  for (const run of shown) {
    const limited=runLimits.get(run.id)?.document,titled=limited==null?undefined:Object.values(run.rows).flat()[0];
    if(titled&&limited!=null)documentTitles.set(`${run.dataset}:${limited}`,documentTitle(titled));
    let panel=panels.get(run.id);
    if (!panel) {
      panel=document.createElement('article');
      panel.className='result-panel';panel.dataset.run=run.id;
      panel.innerHTML='<p class="result-scope"></p><ul class="result-notices" hidden></ul><div class="panel-body"><aside class="pattern-list" aria-label="Patterns"></aside><div class="lines-area"><div class="result-reading"><span class="evidence-sentence"></span> <span class="document-scope" hidden></span> <span class="line-controls"><span></span> <input type="search"></span> <details class="document-nav" aria-label="Documents" hidden><summary class="document-jump"></summary><div class="document-finder" hidden></div><div class="document-choice"></div></details> <span class="result-header"></span> <span class="line-note" role="status" hidden></span></div><div class="kwic-list"></div><nav class="document-pages" aria-label="Example pages" hidden></nav></div></div><details class="query-tools-detail"><summary></summary><pre></pre><div class="query-tools"><button type="button" data-copy></button><button type="button" data-source></button></div></details>';
      $<HTMLDetailsElement>('.document-nav',panel).open=false;
      $('.kwic-list',panel).addEventListener('click',event=>{const row=(event.target as Element).closest<HTMLElement>('[data-entry-line]');if(row)void openEntryLine(runFor(run.id),Number(row.dataset.entryLine));});
      $<HTMLInputElement>('.line-controls input',panel).addEventListener('input',e=>{filters.set(run.id,(e.target as HTMLInputElement).value);viewLocation=concordanceFor(runFor(run.id));renderLines(panel!,runFor(run.id),true);updateDraft();});
      $('[data-copy]',panel).addEventListener('click',async e=>{
        const model=runFor(run.id);
        const command=cliCommand(model.where,runLimits.get(model.id)??null);
        try { await navigator.clipboard.writeText(command);(e.target as HTMLElement).textContent=t('copied'); }
        catch {window.prompt(t('copy'),command);}
      });
      $('[data-source]',panel).addEventListener('click',()=>downloadSource(runFor(run.id)));
      panels.set(run.id,panel);
    }
    panel.id=`search-${run.id}`;
    const section=documentSections.get(run.id);
    if(section?.origin){
      let landmark=panel.querySelector<HTMLElement>('.section-origin');
      if(!landmark){landmark=document.createElement('p');landmark.className='section-origin';panel.prepend(landmark);}
      const parent=documentSections.get(section.origin.parent)?.run;
      const label=`${locale==='ja'?'派生元':'From'} ${escape(section.origin.label)}`;
      const markup=`${parent?`<a data-view data-section-origin href="${escape(hrefFor(parent,section.origin.view))}">${label}</a>`:`<span>${label} · ${locale==='ja'?'結果は未読込':'results unavailable'}</span>`} · <code>${escape(run.source)}</code>${parent&&parent.where!==run.where?` · ${escape(run.where||t('all'))}`:''}`;if(renderSignatures.get(landmark)!==markup){landmark.innerHTML=markup;renderSignatures.set(landmark,markup);}
    }
    panel.classList.toggle('is-running',run.phase==='running');
    const root=$('#result-panels');
    const index=shown.indexOf(run);
    if (root.children[index]!==panel) root.insertBefore(panel,root.children[index]??null);
    const prior=run.id!==current?.id || (run===state.previous && current?.phase==='running');
    const title=prior?t('previous'):t('current');
    const phase=run.phase==='complete'?t('completed'):run.phase==='running'?t('partial'):run.phase==='cancelled'?t('cancelled'):t('failed');
    const scope=`${config.datasets[run.dataset]?.label ?? ''} · ${run.where||t('all')}`;
    const identity=`${t('runNumber')} ${run.id} · ${phase} · ${scope}`;
    const headerHTML=`${compared||prior&&current?.phase!=='running'&&current?.phase!=='complete'?`<span class="evidence-context">${title}</span>`:''}${run.phase==='complete'&&Object.values(run.counts).some(total=>total>0)?`<a href="api/questions/runs/${run.id}/export" aria-label="${locale==='ja'?'全結果を JSONL で保存':'Download all results as JSONL'}" title="${escape(identity)}">${t('download')}</a>`:''}`;
    const header=$('.result-header',panel);
    header.hidden=!headerHTML;
    header.title=identity;
    if (renderSignatures.get(header)!==headerHTML) {header.innerHTML=headerHTML;renderSignatures.set(header,headerHTML);}
    const scopeRow=$('.result-scope',panel);
    scopeRow.textContent='';scopeRow.hidden=true;
    const notices=$('.result-notices',panel),noticeSignature=JSON.stringify(run.notices??[]);
    notices.hidden=!run.notices?.length;
    if(renderSignatures.get(notices)!==noticeSignature){
      notices.replaceChildren(...(run.notices??[]).map(text=>{const item=document.createElement('li');item.textContent=text;return item;}));
      renderSignatures.set(notices,noticeSignature);
    }
    const definitionNames=run.definitions.length?run.definitions:Object.keys(run.counts);
    let chosen=selection.get(run.id)??'';
    if (!definitionNames.includes(chosen)) chosen=definitionNames.includes(preferredPattern)?preferredPattern:definitionNames[0]??'';
    selection.set(run.id,chosen);
    const rail=$('.pattern-list',panel);
    rail.hidden=definitionNames.length<2;
    if (renderSignatures.get(rail)!==JSON.stringify(definitionNames)) {
      rail.innerHTML=definitionNames.map(name=>`<button class="pattern-button" type="button" data-pattern="${escape(name)}"><span>${escape(name)}</span><span class="count"></span></button>`).join('');
      renderSignatures.set(rail,JSON.stringify(definitionNames));
      for (const button of rail.querySelectorAll<HTMLButtonElement>('button')) button.addEventListener('click',()=>{preferredPattern=button.dataset.pattern??'';selection.set(run.id,preferredPattern);void navigateView(hrefFor(run,{...defaultView(),pattern:button.dataset.pattern??''}),true);});
    }
    for (const button of rail.querySelectorAll<HTMLButtonElement>('button')) {
      button.setAttribute('aria-pressed',String(button.dataset.pattern===chosen));
      const total=run.counts[button.dataset.pattern!],partial=run.phase!=='complete';
      $('.count',button).textContent=total==null?'–':count(total)+(partial?'+':'');
      button.classList.toggle('partial',partial);
      button.setAttribute('aria-label',`${button.dataset.pattern} ${total==null?'':count(total)}${partial?' · '+t('partialCount'):''}`);
      button.title=partial?t('partialCount'):'';
    }
    $('.query-tools-detail summary',panel).textContent=t('source');
    $('.query-tools-detail pre',panel).textContent=run.source;
    $('[data-copy]',panel).textContent=t('copy');
    $('[data-source]',panel).textContent=t('savedQuery');
    renderLines(panel,run,force);
  }
}
const runFor=(id: string): RunSnapshot=>{const run=[state.current,state.previous,state.completed,inspecting?.run,viewEvidence,documentSections.get(id)?.run].find(run=>run?.id===id);if(!run)throw new Error('Search is no longer displayed.');return run;};

function documentView(run: RunSnapshot,pattern: string): DocumentView {
  const key=`${run.id}:${pattern}`;
  if(!documentViews.has(key))documentViews.set(key,{request:null,scopeDocument:null,returnAfter:-1,landmarks:[],start:0,matchesTotal:0,query:"",documentsTotal:0,documentsOffset:0,selectedDoc:null,position:null,previousDoc:null,nextDoc:null,documents:null,document:null,rows:[],after:-1,next:null,previousAfter:null,loading:false,error:null,epoch:0,limit:0});
  return documentViews.get(key)!;
}
async function loadDocumentPage(panel: HTMLElement|null,run: RunSnapshot,pattern: string,document: number | null=null,after=-1,documentsOffset: number | null=null,scopeDocument=documentView(run,pattern).scopeDocument) {
  const view=documentView(run,pattern),epoch=++view.epoch,limit=controls.state.lines;
  view.request={document,after,documentsOffset,scopeDocument,limit};view.loading=true;view.error=null;
  if(panel)renderLines(panel,run);
  const query=new URLSearchParams({pattern,limit:String(limit),q:view.query,continuous:scopeDocument==null?'1':'0',after:String(after)});
  const selected=scopeDocument??document;
  if(selected!=null)query.set('document',String(selected));
  if(documentsOffset!=null)query.set('documents_offset',String(documentsOffset));
  try {
    const data=await request<DocumentPage>(`api/questions/runs/${run.id}/documents?${query}`);
    if(epoch!==view.epoch)return;
    Object.assign(view,{request:null,scopeDocument,limit,after:data.after,document:data.document,start:data.start,matchesTotal:data.matches_total,previousAfter:data.previous,documentsTotal:data.documents_total,documentsOffset:data.documents_offset,selectedDoc:data.selected,position:data.position,previousDoc:data.previous_document,nextDoc:data.next_document,documents:data.documents,landmarks:data.landmarks,rows:data.rows,next:data.next,loading:false});
  } catch(error){if(epoch!==view.epoch)return;view.loading=false;view.error=errorMessage(error);}
  if(panel?.isConnected){renderLines(panel,runFor(run.id));}
  else render();
}
function retryDocumentPage(panel:HTMLElement,run:RunSnapshot,pattern:string,view:DocumentView){
  const target=view.request??{document:view.document,after:view.after,documentsOffset:null,scopeDocument:view.scopeDocument};
  return loadDocumentPage(panel,run,pattern,target.document,target.after,target.documentsOffset,target.scopeDocument);
}

function renderDocumentNavigation(panel: HTMLElement,run: RunSnapshot,pattern: string,view: DocumentView) {
  const wrapper=$('.document-nav',panel),nav=$('.document-choice',wrapper),finder=$('.document-finder',wrapper),paging=$('.document-pages',panel);
  wrapper.hidden=isReplay||run.phase!=='complete'||(view.documents!==null&&view.documentsTotal<=1&&!view.query&&!view.error);paging.hidden=true;
  if(isReplay||run.phase!=='complete')return;
  if(!finder.children.length){
    finder.innerHTML='<input type="search"><button type="button" data-titles="-1"></button><button type="button" data-titles="1"></button><span></span>';
    let timer: ReturnType<typeof setTimeout> | undefined;
    $<HTMLInputElement>('input',finder).addEventListener('input',event=>{clearTimeout(timer);const query=(event.target as HTMLInputElement).value;timer=setTimeout(()=>{view.query=query;loadDocumentPage(panel,run,pattern,view.document,-1,0);},150);});
    for(const button of finder.querySelectorAll<HTMLButtonElement>('[data-titles]'))button.addEventListener('click',()=>loadDocumentPage(panel,run,pattern,view.document,view.after,view.documentsOffset+Number(button.dataset.titles)*100));
  }
  finder.hidden=view.documentsTotal<=100&&!view.query;
  const searchInput=$<HTMLInputElement>('input',finder);
  searchInput.placeholder=locale==='ja'?'題名・著者を検索':'Find a document or author';searchInput.setAttribute('aria-label',searchInput.placeholder);
  $('[data-titles="-1"]',finder).textContent=locale==='ja'?'前の文書一覧':'Previous titles';
  $('[data-titles="1"]',finder).textContent=locale==='ja'?'次の文書一覧':'Next titles';
  $<HTMLButtonElement>('[data-titles="-1"]',finder).disabled=view.documentsOffset===0||view.loading;
  $<HTMLButtonElement>('[data-titles="1"]',finder).disabled=view.documentsOffset+100>=view.documentsTotal||view.loading;
  $('span',finder).textContent=`${count(view.documentsTotal?view.documentsOffset+1:0)}–${count(Math.min(view.documentsOffset+100,view.documentsTotal))} / ${count(view.documentsTotal)}`;
  $('.document-jump',wrapper).textContent=locale==='ja'?`文書へ移動 · ${count(view.documentsTotal)} 文書`:`Jump to document · ${count(view.documentsTotal)} documents`;
  const documents=view.documents??[];
  const options=view.selectedDoc&&!documents.some(d=>d.ordinal===view.selectedDoc!.ordinal)?[view.selectedDoc,...documents]:documents;
  if(!nav.children.length){
    nav.innerHTML='<select></select><a data-view data-document-step="-1"></a><a data-view data-document-step="1"></a><span role="status"></span><button type="button" data-document-retry hidden></button>';
    $<HTMLSelectElement>('select',nav).addEventListener('change',event=>{
      const value=(event.target as HTMLSelectElement).value,document=value===''?null:Number(value);
      void navigateView(hrefFor(run,{...concordanceFor(run),document,scopeDocument:view.scopeDocument!=null?document:null,after:-1}),true);
    });
    $('[data-document-retry]',nav).addEventListener('click',()=>retryDocumentPage(panel,run,pattern,view));
  }
  const select=$<HTMLSelectElement>('select',nav),optionSignature=JSON.stringify([options,locale]);
  if(renderSignatures.get(select)!==optionSignature){
    select.innerHTML=`<option value="">${locale==='ja'?'すべての文書':'All documents'}</option>${options.map(d=>`<option value="${d.ordinal}">${escape(d.title)}${d.author?' · '+escape(d.author):''} (${count(d.matches)})</option>`).join('')}`;
    renderSignatures.set(select,optionSignature);
  }
  select.setAttribute('aria-label',locale==='ja'?'文書へ移動':'Jump to document');select.value=String(view.document??'');
  for(const link of nav.querySelectorAll<HTMLAnchorElement>('[data-document-step]')){
    const direction=Number(link.dataset.documentStep),target=direction<0?view.previousDoc:view.nextDoc;
    link.hidden=view.scopeDocument==null;
    link.textContent=locale==='ja'?(direction<0?'前の文書':'次の文書'):(direction<0?'Previous document':'Next document');
    updateNavigationLink(link,target==null?null:hrefFor(run,{...concordanceFor(run),scopeDocument:target,document:target,after:-1}),view.loading);
  }
  $('span',nav).textContent=view.error??(view.loading?(locale==='ja'?'読み込み中…':'Loading…'):view.position!=null?`${view.position+1} / ${count(view.documentsTotal)}`:`${count(view.documentsTotal)} ${locale==='ja'?'文書に用例':'matching documents'}`);
  const retry=$<HTMLButtonElement>('[data-document-retry]',nav);retry.hidden=!view.error;retry.textContent=locale==='ja'?'もう一度読み込む':'Retry loading documents';
  if(!paging.children.length){paging.innerHTML='<a data-view data-page-step="-1"></a><a data-view data-page-step="1"></a><button type="button" data-page-retry hidden></button>'; $('[data-page-retry]',paging).addEventListener('click',()=>retryDocumentPage(panel,run,pattern,view));}
  paging.hidden=!view.error&&view.previousAfter==null&&view.next==null;
  for(const link of paging.querySelectorAll<HTMLAnchorElement>('[data-page-step]')){
    const direction=Number(link.dataset.pageStep),after=direction<0?view.previousAfter:view.next;
    const lines=count(controls.state.lines);link.textContent=locale==='ja'?(direction<0?`前の${lines}件`:`次の${lines}件`):(direction<0?`Previous ${lines}`:`Next ${lines}`);
    updateNavigationLink(link,after==null?null:hrefFor(run,{...concordanceFor(run),after}),view.loading);
  }
  const pageRetry=$<HTMLButtonElement>('[data-page-retry]',paging);pageRetry.hidden=!view.error;pageRetry.textContent=locale==='ja'?'用例を再読み込み':'Retry loading examples';

}
function updateNavigationLink(link:HTMLAnchorElement,href:string|null,busy:boolean){
  if(href){if(link.getAttribute('href')!==href)link.setAttribute('href',href);}else link.removeAttribute('href');
  const disabled=href==null||busy;link.setAttribute('aria-disabled',String(disabled));link.tabIndex=disabled?-1:0;
}
let readerNavigationEpoch=0;
function updateReaderNavigation() {
  const inspection=readerTarget??inspecting,epoch=++readerNavigationEpoch;
  const nav=$('.reader-document-nav',readerPanel);
  const links=[...nav.querySelectorAll<HTMLAnchorElement>('[data-reader-document]')];
  nav.querySelector('[data-reader-retry]')?.remove();
  for(const link of links){link.setAttribute('aria-disabled','true');link.removeAttribute('href');}
  if(!inspection||inspection.run.phase!=='complete')return;
  const {run,row}=inspection;
  nav.setAttribute('aria-busy','true');
  void request<DocumentPage>(`api/questions/runs/${run.id}/documents?${new URLSearchParams({pattern:row.pattern,document:String(row.document_ordinal),limit:'1'})}`).then(page=>{
    if((readerTarget??inspecting)!==inspection||epoch!==readerNavigationEpoch)return;
    nav.setAttribute('aria-busy','false');
    for(const link of links){
      const target=Number(link.dataset.readerDocument)<0?page.previous_document:page.next_document;
      link.setAttribute('aria-disabled',String(target==null));
      if(target!=null)link.setAttribute('href',hrefFor(run,{kind:'reader',pattern:row.pattern,document:target,sequence:null,returnTo:viewLocation.kind==='reader'?viewLocation.returnTo:{kind:'example',pattern:row.pattern,document:row.document_ordinal,sequence:row.sequence,returnTo:concordanceFor(run)}}));
    }
  }).catch(error=>{
    if((readerTarget??inspecting)!==inspection||epoch!==readerNavigationEpoch)return;
    nav.setAttribute('aria-busy','false');
    const retry=document.createElement('button');retry.type='button';retry.dataset.readerRetry='';retry.textContent=locale==='ja'?'文書の移動を再読み込み':'Retry document navigation';retry.title=errorMessage(error);retry.addEventListener('click',updateReaderNavigation);nav.append(retry);
  });
}

const renderSignatures=new WeakMap<HTMLElement,string>();
const concordanceNodes=new WeakMap<HTMLElement,Map<string,HTMLElement>>();
const concordanceContent=new WeakMap<HTMLElement,{row:MatchRecord|null;ordinal:number;context:number;number:HTMLElement;left:HTMLElement;hit:HTMLElement;right:HTMLElement}>();
const concordanceHeadings=new WeakMap<HTMLElement,{link:HTMLAnchorElement;author:HTMLElement;count:HTMLElement}>();
function setConcordanceText(element:HTMLElement,text:string){
  if(element.childNodes.length===1&&element.firstChild!.nodeType===Node.TEXT_NODE){if(element.firstChild!.nodeValue!==text)element.firstChild!.nodeValue=text;}
  else element.replaceChildren(text);
}
function setContext(element:HTMLElement,tokens:readonly Token[],bound:ReadonlyMap<number,readonly string[]>){
  if(!tokens.some(token=>bound.has(token.index))){setConcordanceText(element,words(tokens));return;}
  element.replaceChildren(...tokens.map(token=>{const roles=bound.get(token.index);if(!roles)return document.createTextNode(surfaceOf(token));const mark=document.createElement('mark');mark.className='bound-word';mark.title=roles.join(', ');mark.textContent=surfaceOf(token);return mark;}));
}
function renderLines(panel: HTMLElement,run: RunSnapshot | undefined,_force=false) {
  if (!run) return;
  panel.classList.toggle('listing-entry',entryShown(run));
  if(entryShown(run)){renderEntryList(panel,run);return;}
  const pattern=selection.get(run.id)??'',view=documentView(run,pattern);
  if(!isReplay&&run.phase==='complete'&&!view.loading&&(!view.error&&(view.documents===null||view.limit!==controls.state.lines)||view.error&&view.request?.limit!==controls.state.lines)){void loadDocumentPage(panel,run,pattern,view.document,view.after);return;}
  renderDocumentNavigation(panel,run,pattern,view);
  const pageReady=!isReplay&&run.phase==='complete'&&view.documents!==null;
  const prefixVisible=!view.request||view.request.after===-1&&view.request.document==null&&view.request.scopeDocument==null;
  const allRows=pageReady?view.rows:prefixVisible?(run.rows[pattern]??[]):[];
  const filter=filters.get(run.id)??'',limit=controls.state.lines,context=controls.state.context;
  const rows=allRows.filter(row=>(words(row.left_context)+row.surface+words(row.right_context)).includes(filter)).slice(0,limit);
  const shownCounts=new Map<number,number>(),landmarks=new Map(view.landmarks.map(document=>[document.ordinal,document]));
  for(const row of rows)shownCounts.set(row.document_ordinal,(shownCounts.get(row.document_ordinal)??0)+1);
  const total=pageReady?view.matchesTotal:run.counts[pattern]??0,shownLines=Math.min(allRows.length,limit);
  const input=$<HTMLInputElement>('.line-controls input',panel),hint=locale==='ja'?`表示中の${count(shownLines)}件から絞り込む`:`Filter the ${count(shownLines)} lines shown`;
  input.placeholder=hint;input.setAttribute('aria-label',hint);
  $('.line-controls span',panel).textContent=run.phase==='complete'&&!isReplay
    ? filter?(locale==='ja'?`表示中の ${count(shownLines)} 件のうち ${count(rows.length)} 件 · 全 ${count(total)} 件`:`${count(rows.length)} of the ${count(shownLines)} lines shown · ${count(total)} total matches`)
    :!rows.length?'':(locale==='ja'?`${count(view.start+1)}–${count(view.start+rows.length)} 件を表示。`:`Reading ${count(rows.length?view.start+1:0)}–${count(view.start+rows.length)}.`)
    : locale==='ja'?`${count(total)} 件が見つかりました · ${count(rows.length)} 件を表示`:`${count(rows.length)} shown · ${count(total)} found`;
  if(view.loading&&!rows.length)$('.line-controls span',panel).textContent=locale==='ja'?`全 ${count(total)} 件`:`${count(total)} total matches`;
  const matchingDocuments=pageReady?(view.scopeDocument!=null?(total?1:0):view.query?null:view.documentsTotal):null;
  $('.evidence-sentence',panel).textContent=run.phase!=='complete'&&!isReplay?(locale==='ja'?`途中までに ${count(total)} 件。`:`${count(total)} ${total===1?'match':'matches'} so far.`):locale==='ja'?`${count(total)} 件${matchingDocuments==null?'':` · ${count(matchingDocuments)} 文書`}。`:`${count(total)} ${total===1?'match':'matches'}${matchingDocuments==null?'':` in ${count(matchingDocuments)} ${matchingDocuments===1?'document':'documents'}`}.`;
  const scope=$('.document-scope',panel);
  scope.hidden=view.scopeDocument==null;
  if(!scope.hidden){
    const markup=`<a data-view href="${escape(hrefFor(run,{...concordanceFor(run),scopeDocument:null,document:null,after:view.returnAfter}))}">${locale==='ja'?'すべての文書に戻る':'Return to all documents'}</a>`;
    if(renderSignatures.get(scope)!==markup){scope.innerHTML=markup;renderSignatures.set(scope,markup);}
  }
  const note=$('.line-note',panel);note.setAttribute('role','status');
  note.textContent=view.loading&&(!rows.length||view.documents!==null)?(locale==='ja'?'用例を読み込んでいます…':'Loading examples…'):view.loading?'':view.error?(locale==='ja'?`用例を読み込めませんでした。${view.error}`:`Examples could not be loaded. ${view.error}`):run.phase==='running'?t('notTotal'):'';
  note.hidden=!note.textContent;
  const list=$('.kwic-list',panel);list.setAttribute('aria-busy',String(view.loading));
  let nodes=concordanceNodes.get(list);if(!nodes){nodes=new Map();concordanceNodes.set(list,nodes);}
  const desired:HTMLElement[]=[],used=new Set<string>();
  const node=(key:string,tag:'div'|'a'|'p',className:string)=>{used.add(key);let value=nodes!.get(key);if(!value){value=document.createElement(tag);value.className=className;nodes!.set(key,value);}return value;};
  let previousDocument:number|undefined,sourceOffset=0;
  for(const row of rows){
    if(previousDocument!==row.document_ordinal){
      const heading=node(`document:${row.document_ordinal}`,'div','document-heading');
      const landmark=landmarks.get(row.document_ordinal);
      const shown=shownCounts.get(row.document_ordinal)??0;
      const href=hrefFor(run,{...concordanceFor(run),scopeDocument:row.document_ordinal,returnAfter:view.scopeDocument==null?view.after:view.returnAfter,document:row.document_ordinal,after:-1});
      let content=concordanceHeadings.get(heading);
      if(!content){
        heading.innerHTML='<a data-view></a><span></span><span class="document-match-count"></span>';
        content={link:heading.children[0] as HTMLAnchorElement,author:heading.children[1] as HTMLElement,count:heading.children[2] as HTMLElement};
        concordanceHeadings.set(heading,content);
      }
      if(content.link.getAttribute('href')!==href)content.link.setAttribute('href',href);
      setConcordanceText(content.link,documentTitle(row));
      const author=row.source_metadata?.document?.author??'';
      if(content.author.hidden!==!author)content.author.hidden=!author;
      setConcordanceText(content.author,author);
      setConcordanceText(content.count,landmark?count(landmark.matches)+(locale==='ja'?' 件':landmark.matches===1?' match':' matches'):count(shown)+(locale==='ja'?' 件を表示':' shown'));
      desired.push(heading);previousDocument=row.document_ordinal;
    }
    const link=node(`match:${row.sequence}`,'a','kwic-row') as HTMLAnchorElement;
    if(!link.hasAttribute('data-sequence')){link.dataset.sequence=String(row.sequence);link.setAttribute('data-view','');}
    const href=hrefFor(run,hitLocation(run,row,'example'));if(link.getAttribute('href')!==href)link.setAttribute('href',href);
    const rowIndex=allRows.indexOf(row,sourceOffset),ordinal=view.start+rowIndex+1;sourceOffset=rowIndex+1;
    const label=(documentTitle(row))+', '+ordinal+': '+words(row.left_context)+row.surface+words(row.right_context);if(link.getAttribute('aria-label')!==label)link.setAttribute('aria-label',label);
    const expanded=String(inspecting?.run.id===run.id&&inspecting.row.sequence===row.sequence&&!tokenLab.hidden);if(link.getAttribute('aria-expanded')!==expanded)link.setAttribute('aria-expanded',expanded);
    let content=concordanceContent.get(link);
    if(!content){
      link.innerHTML='<span class="line-number"></span><span class="context-left" lang="ja"><span></span></span><mark lang="ja"></mark><span class="context-right" lang="ja"></span>';
      content={row:null,ordinal:-1,context:-1,number:link.children[0] as HTMLElement,left:link.children[1].firstElementChild as HTMLElement,hit:link.children[2] as HTMLElement,right:link.children[3] as HTMLElement};
      concordanceContent.set(link,content);
    }
    if(content.row!==row||content.ordinal!==ordinal||content.context!==context){
      const bound=boundOutside(row,row.span.end+(row.right_context?.length??0));
      for(const [element,text] of [[content.number,String(ordinal)],[content.hit,row.surface]] as const)setConcordanceText(element,text);
      setContext(content.left,context?(row.left_context??[]).slice(-context):[],bound);setContext(content.right,(row.right_context??[]).slice(0,context),bound);
      content.row=row;content.ordinal=ordinal;content.context=context;
    }
    desired.push(link);
    if(inspectionFailure?.runId===run.id&&inspectionFailure.sequence===row.sequence){
      const notice=node(`error:${row.sequence}`,'p','line-note analysis-error');notice.setAttribute('role','alert');
      const signature=locale+inspectionFailure.message;
      if(renderSignatures.get(notice)!==signature){notice.replaceChildren(document.createTextNode((locale==='ja'?'分析を読み込めませんでした。':'Could not load analysis. ')+inspectionFailure.message+' '));const retry=document.createElement('button');retry.type='button';retry.textContent=locale==='ja'?'分析を再読み込み':'Retry analysis';retry.addEventListener('click',()=>void inspect(run,row,link));notice.append(retry);renderSignatures.set(notice,signature);}
      desired.push(notice);
    }
    if(inspecting?.run.id===run.id&&inspecting.row.sequence===row.sequence&&!tokenLab.hidden){desired.push(tokenLab);if(readerPanel.open)desired.push(readerPanel);}
  }
  if(!rows.length){const empty=node('empty','p','line-note');if(filter)filterEmpty(empty,run,pattern,filter,shownLines);else{renderSignatures.delete(empty);empty.textContent=t(run.phase==='complete'?'noMatches':'noLines');}desired.push(empty);}
  const desiredNodes=new Set(desired);
  for(const child of [...list.children])if(!desiredNodes.has(child as HTMLElement)){
    if(child===tokenLab||child===readerPanel)document.body.append(child);else child.remove();
  }
  desired.forEach((child,index)=>{if(list.children[index]!==child)list.insertBefore(child,list.children[index]??null);});
  roveList(list);
  for(const key of nodes.keys())if(!used.has(key))nodes.delete(key);
}

const listStops=new WeakMap<Element,HTMLElement>();
const listItems=(list:Element)=>[...list.querySelectorAll<HTMLElement>(':scope>.kwic-row,:scope>.document-heading>a')];
function roveList(list:Element){
  const items=listItems(list),remembered=listStops.get(list);
  const stop=remembered&&items.includes(remembered)?remembered:items.find(item=>item.getAttribute('aria-expanded')==='true')??items[0];
  for(const item of items){const index=item===stop?'0':'-1';if(item.getAttribute('tabindex')!==index)item.setAttribute('tabindex',index);}
}
$('#result-panels').addEventListener('focusin',event=>{const list=(event.target as Element).closest('.kwic-list');if(list&&listItems(list).includes(event.target as HTMLElement)){listStops.set(list,event.target as HTMLElement);roveList(list);}});
$('#result-panels').addEventListener('keydown',event=>{
  const list=(event.target as Element).closest('.kwic-list');
  if(!list||event.altKey||event.ctrlKey||event.metaKey||event.shiftKey||!['ArrowDown','ArrowUp','Home','End'].includes(event.key))return;
  const items=listItems(list),at=items.indexOf(event.target as HTMLElement);
  if(at<0)return;
  event.preventDefault();
  items[event.key==='Home'?0:event.key==='End'?items.length-1:Math.max(0,Math.min(items.length-1,at+(event.key==='ArrowDown'?1:-1)))].focus();
});

const entryTexts=new Map<string,readonly string[]|{error:string}>();
let entryReading=false;
async function readEntryTexts(run:RunSnapshot){
  if(entryReading||!entryLines)return;
  const keys=[...new Set(entryLines.lines.map(line=>`${run.dataset}:${line.sentence}`))].filter(key=>!entryTexts.has(key));
  if(!keys.length)return;
  entryReading=true;
  try{
    for(let at=0;at<keys.length;at+=6)await Promise.all(keys.slice(at,at+6).map(async key=>{
      const sentence=key.slice(key.lastIndexOf(':')+1);
      try{const data=tokenCache.get(key)??parseStoredSentence(await request<unknown>(`api/questions/data/${encodeURIComponent(run.dataset)}/tokens?sentence=${sentence}`));
        entryTexts.set(key,entryLines&&data.analysis_identity!==entryLines.analysis?{error:t('occurrenceChanged')}:data.tokens.map(surfaceOf));}
      catch(error){entryTexts.set(key,{error:errorMessage(error)});}
    }));
  }finally{entryReading=false;}
  render(true);
}
function renderEntryList(panel:HTMLElement,run:RunSnapshot){
  const entry=entryLines!,pattern=entry.pattern,context=controls.state.context,list=$('.kwic-list',panel);
  void readEntryTexts(run);
  const works=entry.works??new Set(entry.lines.map(line=>line.document)).size,name=entry.words.join('');
  $('.evidence-sentence',panel).textContent=locale==='ja'?`${name} · ${count(entry.total)} 件 · ${count(works)} 文書。`:`${name} · ${count(entry.total)} ${entry.total===1?'match':'matches'} in ${count(works)} ${works===1?'document':'documents'}.`;
  const note=$('.line-note',panel);
  note.textContent=entry.lines.length<entry.total?(locale==='ja'?`頻度表に並んだ最初の ${count(entry.lines.length)} 件です。`:`The first ${count(entry.lines.length)} lines listed in Frequencies.`):'';note.hidden=!note.textContent;
  let exit=panel.querySelector<HTMLButtonElement>('.entry-exit');
  if(!exit){exit=document.createElement('button');exit.type='button';exit.className='entry-exit';exit.addEventListener('click',leaveEntry);$('.evidence-sentence',panel).after(exit);}
  const limited=runLimits.get(run.id)?.document,total=count(run.counts[pattern]??0);
  exit.hidden=run.phase!=='complete';
  exit.textContent=limited!=null?(locale==='ja'?`${documentName(run.dataset,limited)}の${pattern}をすべて表示（${total} 件）`:`Show all ${total} ${pattern} matches in ${documentName(run.dataset,limited)}`):(locale==='ja'?`${pattern}の結果をすべて表示（${total} 件）`:`Show all ${total} ${pattern} matches`);
  let nodes=concordanceNodes.get(list);if(!nodes){nodes=new Map();concordanceNodes.set(list,nodes);}
  const desired:HTMLElement[]=[],used=new Set<string>();
  const node=(key:string,tag:'div'|'button',className:string)=>{used.add(key);let value=nodes!.get(key);if(!value){value=document.createElement(tag);value.className=className;nodes!.set(key,value);}return value;};
  const open=entryActive();
  entry.lines.forEach((line,index)=>{
    const title=entry.names?.[line.document]??documentName(run.dataset,line.document);
    if(index===0||entry.lines[index-1].document!==line.document){
      const heading=node(`entry-document:${index}`,'div','document-heading'),signature=title;
      if(renderSignatures.get(heading)!==signature){heading.replaceChildren(Object.assign(document.createElement('span'),{className:'entry-document',textContent:title}));renderSignatures.set(heading,signature);}
      desired.push(heading);
    }
    const row=node(`entry:${index}`,'button','kwic-row entry-row') as HTMLButtonElement;row.type='button';row.dataset.entryLine=String(index);
    const texts=entryTexts.get(`${run.dataset}:${line.sentence}`),expanded=String(open&&index===entry.at);
    if(row.getAttribute('aria-expanded')!==expanded)row.setAttribute('aria-expanded',expanded);
    const failure=texts&&'error' in texts?texts.error:null,read=texts&&!('error' in texts)?entryContext(texts,line,context):null,signature=JSON.stringify([locale,context,read?0:failure??null,title]);
    if(renderSignatures.get(row)!==signature){
      row.innerHTML='<span class="line-number"></span><span class="context-left" lang="ja"><span></span></span><mark lang="ja"></mark><span class="context-right" lang="ja"></span>';
      const [number,left,hit,right]=[row.children[0] as HTMLElement,row.children[1].firstElementChild as HTMLElement,row.children[2] as HTMLElement,row.children[3] as HTMLElement];
      number.textContent=String(index+1);
      if(read){setContext(left,read.left,read.bound);hit.textContent=read.hit;setContext(right,read.right,read.bound);row.setAttribute('aria-label',`${title}, ${index+1}: ${words(read.left)}${read.hit}${words(read.right)}`);}
      else{right.textContent=failure!=null?(locale==='ja'?`文を読み込めませんでした。${failure}`:`Could not load the sentence. ${failure}`):(locale==='ja'?'読み込んでいます…':'Loading…');row.setAttribute('aria-label',`${title}, ${index+1}`);}
      renderSignatures.set(row,signature);
    }
    desired.push(row);
    if(open&&index===entry.at){desired.push(tokenLab);if(readerPanel.open)desired.push(readerPanel);}
  });
  const desiredNodes=new Set(desired);
  for(const child of [...list.children])if(!desiredNodes.has(child as HTMLElement)){if(child===tokenLab||child===readerPanel)document.body.append(child);else child.remove();}
  desired.forEach((child,index)=>{if(list.children[index]!==child)list.insertBefore(child,list.children[index]??null);});
  roveList(list);
  for(const key of nodes.keys())if(!used.has(key))nodes.delete(key);
  list.setAttribute('aria-busy','false');
}
function leaveEntry(){
  entryLines=null;
  for(const exit of document.querySelectorAll<HTMLElement>('.entry-exit'))exit.remove();
  renderExampleSteps();updateDraft();render(true);
  const open=inspecting;if(!open||tokenLab.hidden){$('#results').focus({preventScroll:true});return;}
  const panel=panels.get(open.run.id),view=documentView(open.run,open.row.pattern);
  if(panel&&!view.rows.some(row=>row.sequence===open.row.sequence))void loadDocumentPage(panel,open.run,open.row.pattern,open.row.document_ordinal,open.row.sequence-1);
  $('#token-origin').focus({preventScroll:true});
}

let frequencyPairs:readonly {corpus:string;catalog:string;source:string;patterns:readonly {name:string}[]}[]|null|undefined;
function loadFrequencyPairs(){frequencyPairs=null;return request<{pairs:typeof frequencyPairs}>('api/frequency/meta').then(meta=>{frequencyPairs=meta.pairs??[];},()=>{frequencyPairs=[];});}
function filterEmpty(element:HTMLElement,run:RunSnapshot,pattern:string,filter:string,shown:number){
  if(frequencyPairs===undefined)void loadFrequencyPairs().then(()=>render(true));
  const pair=frequencyPairs?.find(pair=>pair.corpus===run.dataset&&pair.patterns.some(item=>item.name===pattern));
  const signature=JSON.stringify([locale,run.id,pattern,filter,shown,pair?.catalog??null]);if(renderSignatures.get(element)===signature)return;renderSignatures.set(element,signature);
  element.textContent=locale==='ja'?`表示中の${count(shown)}件に「${filter}」を含む用例はありません。`:`None of the ${count(shown)} lines shown contain “${filter}”.`;
  if(pair){
    const link=document.createElement('a');link.href=`frequencies#${new URLSearchParams({corpus:pair.corpus,catalog:pair.catalog,pattern,q:filter})}`;
    link.textContent=locale==='ja'?`頻度表で「${filter}」を含む${pattern}を数える`:`Count ${pattern} entries containing “${filter}” in Frequencies`;
    element.append(' ',link);
  }else element.append(locale==='ja'?' 絞り込みは表示中の用例だけを対象にします。コーパス全体で探すには、パターンを変えて検索してください。':' The filter searches only these lines. To search the whole corpus, change the pattern and search again.');
}
async function inspect(run: RunSnapshot,row: MatchRecord,button: HTMLElement) {
  const epoch=++inspectionEpoch;inspectionFailure=null;
  button.setAttribute('aria-busy','true');
  try {
    const key=`${run.dataset}:${row.corpus_sentence_ordinal}`;
    let data=tokenCache.get(key);
    if(!data){data=parseStoredSentence(await request<unknown>(`api/questions/data/${encodeURIComponent(run.dataset)}/tokens?sentence=${row.corpus_sentence_ordinal}`));if(tokenCache.size>=16)tokenCache.delete(tokenCache.keys().next().value!);tokenCache.set(key,data);}
    if(epoch!==inspectionEpoch)return;
    if(data.analysis_identity!==run.contract_run!.analysis_identity)throw new Error('The stored sentence belongs to another corpus realization. Run this question again.');
    if(conditionSentence!==key){conditions=[];conditionSentence=key;}
    if(!inspecting)tokenDetails.open=!compactLayout.matches;
    inspecting={run,row};tokenData=data;tokenIndex=row.span.start;
    focusedSentence.innerHTML=escape(words(data.tokens.slice(0,row.span.start)))+'<mark>'+escape(words(data.tokens.slice(row.span.start,row.span.end)))+'</mark>'+escape(words(data.tokens.slice(row.span.end)));
    const roles=new Map(evidenceWords(row,data.tokens.length).filter(word=>word.roles.length).map(word=>[word.index,word.roles.join(' ')]));
    $('#token-ribbon').innerHTML=data.tokens.map(token=>{const bound=roles.get(token.index);return `<button type="button" data-token="${token.index}" class="${token.index>=row.span.start&&token.index<row.span.end?'is-hit':''}${bound?' bound':''}"${bound?` data-roles="${escape(bound)}"`:''}>${escape(surfaceOf(token))}</button>`;}).join('');
    const keys=Object.keys(data.tokens[tokenIndex].attributes);
    attributeKey=keys.includes('lemma')?'lemma':keys[0]??'text';attributeMember=undefined;
    for(const token of $('#token-ribbon').querySelectorAll<HTMLButtonElement>('button'))token.addEventListener('click',()=>{selectToken(Number(token.dataset.token));$<HTMLDetailsElement>('#token-attributes>details').open=true;});
    selectToken(tokenIndex);
    tokenLab.hidden=false;button.after(tokenLab);button.setAttribute('aria-expanded','true');renderExampleSteps();
    renderDependencies();
    $<HTMLAnchorElement>('#document').setAttribute('href',hrefFor(run,hitLocation(run,row,'reader')));
    $<HTMLAnchorElement>('#close-token').setAttribute('href',hrefFor(run,concordanceFor(run)));
    $<HTMLAnchorElement>('#close-inspector').setAttribute('href',hrefFor(run,viewLocation.kind==='reader'?viewLocation.returnTo:hitLocation(run,row,'example')));
    const panel=panels.get(run.id);if(panel)renderLines(panel,run);
    if(readerPanel.open)updateReaderNavigation();
  } catch(error){if(epoch===inspectionEpoch){inspectionFailure={runId:run.id,sequence:row.sequence,message:errorMessage(error)};const panel=panels.get(run.id);if(panel)renderLines(panel,run);}}
  finally{button.removeAttribute('aria-busy');}
}

function renderExampleSteps(){
  if(!inspecting)return;
  const {run,row}=inspecting,previous=$<HTMLButtonElement>('[data-example-step="-1"]',exampleSteps),next=$<HTMLButtonElement>('[data-example-step="1"]',exampleSteps);
  if(entryActive()){
    const {at,lines,total,words}=entryLines!;
    previous.disabled=at<=0;next.disabled=at>=lines.length-1;
    $('#token-origin').textContent=`${words.join('')} · ${count(at+1)} / ${count(total)} · ${documentTitle(row)}`;
    return;
  }
  const page=documentView(run,row.pattern),position=page.rows.findIndex(value=>value.sequence===row.sequence);
  previous.disabled=position<=0&&page.previousAfter==null;
  next.disabled=position===page.rows.length-1&&page.next==null;
  $('#token-origin').textContent=`${documentTitle(row)} · ${count(position<0?row.sequence+1:page.start+position+1)}`;
}
async function openEntryLine(run:RunSnapshot,at:number){
  const from=entryLines!.at,target=entryLine(at),limited=runLimits.get(run.id)?.document;
  entryLines={...entryLines!,at};
  if(limited==null||limited===target.document){if(!await openOccurrence(run,target)&&entryLines)entryLines={...entryLines,at:from};return;}
  await search(undefined,undefined,target);pendingView=null;pendingOccurrence=target;
}
for(const control of exampleSteps.querySelectorAll<HTMLButtonElement>('button'))control.addEventListener('click',async()=>{
  const origin=inspecting;if(!origin)return;
  if(entryActive()){control.disabled=true;try{await openEntryLine(origin.run,entryLines!.at+Number(control.dataset.exampleStep));}finally{if(inspecting===origin)renderExampleSteps();}return;}
  const {run,row}=origin,step=Number(control.dataset.exampleStep),view=documentView(run,row.pattern),position=view.rows.findIndex(value=>value.sequence===row.sequence);
  const top=tokenLab.getBoundingClientRect().top,epoch=navigationEpoch,hadFocus=document.activeElement===control;let successor:number|null=null;control.disabled=true;
  try{
    let next=view.rows[position+step];let page:DocumentPage|undefined;
    if(!next){
      const query=new URLSearchParams({pattern:row.pattern,limit:String(controls.state.lines),continuous:view.scopeDocument==null?'1':'0',after:String(step<0?view.previousAfter??-1:row.sequence),q:view.query});
      if(view.scopeDocument!=null)query.set('document',String(view.scopeDocument));
      page=await request<DocumentPage>(`api/questions/runs/${run.id}/documents?${query}`);next=step<0?page.rows.at(-1)!:page.rows[0];
    }
    if(!next||epoch!==navigationEpoch||inspecting!==origin)return;
    const target=hitLocation(run,next,'example');successor=next.sequence;
    await navigateView(hrefFor(run,page?{...target,returnTo:{...concordanceFor(run),after:page.after,document:page.document}}:target),true);
    if(inspecting?.run.id===run.id&&inspecting.row.sequence===next.sequence)window.scrollBy(0,tokenLab.getBoundingClientRect().top-top);
  }catch(error){const notice=document.createElement('p');notice.className='error';notice.setAttribute('role','alert');notice.textContent=errorMessage(error);exampleSteps.after(notice);}
  finally{
    if(inspecting===origin)control.disabled=false;
    if(hadFocus&&document.activeElement===document.body&&(inspecting===origin||inspecting?.run.id===run.id&&inspecting.row.sequence===successor)){const target=control.disabled?exampleSteps.querySelector<HTMLButtonElement>('button:not(:disabled)'):control;(target??focusedSentence).focus({preventScroll:true});}
  }
});
function selectToken(index: number) {
  if(!tokenData)return;tokenIndex=index;
  const token=tokenData.tokens[index],field=attributeKey,members=attributeMembers(token.attributes[field]);
  attributeMember=attributeMember!==undefined&&members.includes(attributeMember)?attributeMember:members[0];
  for(const button of $('#token-ribbon').querySelectorAll<HTMLButtonElement>('button')){const token=tokenData.tokens[Number(button.dataset.token)];button.classList.toggle('selected',token.index===index);button.setAttribute('aria-pressed',String(token.index===index));}
  const details=detailsOf(tokenData.tokens,index,inspecting?.row??{span:{start:0,end:0}},config?.value_names,locale,t('root'));
  const row=({key,name,values}:DetailRow)=>{
    const label=name?`${escape(name)} <code>${escape(key)}</code>`:escape(key);
    return values.length?values.map(({value,gloss})=>{const selected=field===key&&attributeMember===value;return `<button type="button" data-attribute="${escape(key)}" data-member="${escape(value)}" class="attribute-row ${selected?'selected':''}" aria-pressed="${selected}"><span>${label}</span><strong>${escape(gloss??value)}${gloss&&gloss!==value?` <code>${escape(value)}</code>`:''}</strong></button>`;}).join(''):`<div class="attribute-row"><span>${label}</span><span>${locale==='ja'?'値なし':'No values'}</span></div>`;
  };
  const attributes=$('#token-attributes'),more=attributes.querySelector<HTMLDetailsElement>(':scope>details')?.open??false,expanded=attributes.querySelector<HTMLDetailsElement>('.attribute-more')?.open??false;
  const roles=details.roles.join(', ');
  const place=locale==='ja'?{span:'一致範囲の語です。',bound:`パターンの条件で結び付いた語（${roles}）です。一致範囲の外にあります。`,outside:'一致範囲の外の語です。'}[details.place]:{span:'This word is inside the match.',bound:`The pattern's condition binds this word as ${roles}; it lies outside the match.`,outside:'This word is outside the match.'}[details.place];
  attributes.innerHTML=`<details><summary>${locale==='ja'?'詳しく':'Details'}</summary><p class="attribute-span">${escape(place)}</p><div class="attribute-basics">${details.basics.map(row).join('')}</div>${details.all?`<details class="attribute-more"><summary>${locale==='ja'?'全属性':'All attributes'} (${details.all.length})</summary><div class="attribute-all">${details.all.map(row).join('')}</div></details>`:''}</details>`;
  attributes.querySelector<HTMLDetailsElement>(':scope>details')!.open=more;const all=attributes.querySelector<HTMLDetailsElement>('.attribute-more');if(all)all.open=expanded;
  $<HTMLButtonElement>('#attribute-remix').disabled=attributeMember===undefined||conditions.some(condition=>condition.tokenIndex===tokenIndex&&condition.key===field&&condition.value===attributeMember);
  renderComposer();
}

let dependencyChoice:DependencyView|null=null;
const ARC_STEP=26,ARC_LABEL=18,TREE_ROW=26,TREE_COLUMN=9;
const arcLayouts=new WeakMap<HTMLElement,readonly Arc[]>();
const svgNode=(name:string,attributes:Record<string,string|number>)=>{const node=document.createElementNS('http://www.w3.org/2000/svg',name);for(const [key,value] of Object.entries(attributes))node.setAttribute(key,String(value));return node;};
function drawArcs(chips:HTMLElement){
  const arcs=arcLayouts.get(chips);if(!arcs)return;
  chips.querySelector(':scope>svg')?.remove();
  const items=[...chips.querySelectorAll<HTMLElement>(':scope>[data-item]')],top=parseFloat(chips.style.paddingTop);
  const x=(position:number)=>items[position].offsetLeft+items[position].offsetWidth/2;
  const svg=svgNode('svg',{width:chips.scrollWidth,height:top,'aria-hidden':'true'});
  for(const arc of [...arcs].sort((a,b)=>Number(a.bound)-Number(b.bound))){
    const x1=x(arc.from),x2=x(arc.to),offset=x1<x2?4:-4,y=top-1,h=arc.level*ARC_STEP,kind=arc.bound?'dependency-arc bound':'dependency-arc';
    svg.append(svgNode('path',{class:kind,d:`M${x1+offset},${y} C${x1+offset},${y-h} ${x2-offset},${y-h} ${x2-offset},${y} M${x2-offset-4},${y-7} L${x2-offset},${y} L${x2-offset+4},${y-7}`}));
    const label=svgNode('text',{class:kind,x:(x1+x2)/2,y:y-h*0.75-4,'text-anchor':'middle'});label.textContent=arc.rel;
    const gloss=named('dep.rel',arc.rel);if(gloss){const title=svgNode('title',{});title.textContent=gloss;label.append(title);}
    svg.append(label);
  }
  chips.prepend(svg);
}
const arcObserver=new ResizeObserver(entries=>{for(const entry of entries)drawArcs(entry.target as HTMLElement);});
addEventListener('beforeprint',()=>{for(const chips of document.querySelectorAll<HTMLElement>('.dependency-chips'))drawArcs(chips);});
function relationCell(rel:string){const gloss=named('dep.rel',rel);return gloss?`${escape(gloss)} <code>${escape(rel)}</code>`:escape(rel);}
function relationGloss(rel:string){const gloss=named('dep.rel',rel);return gloss?` <span class="dependency-gloss">${escape(gloss)}</span>`:'';}
function roleTags(roles:readonly string[]){return roles.map(role=>`<span class="dependency-role">${escape(role)}</span>`).join('');}
function renderDependencies(){
  const tokens=tokenData?.tokens,row=inspecting?.row;
  dependencyPanel.hidden=!tokens||!row||!hasDependencies(tokens);
  if(!tokens||!row||dependencyPanel.hidden)return;
  const words=evidenceWords(row,tokens.length),byIndex=new Map(words.map(word=>[word.index,word])),starts=bunsetsuStarts(tokens);
  const view=dependencyView(dependencyChoice,!!starts,compactLayout.matches);
  for(const button of dependencyPanel.querySelectorAll<HTMLButtonElement>('[data-dependency-view]')){button.hidden=button.dataset.dependencyView==='bunsetsu'&&!starts;button.setAttribute('aria-pressed',String(button.dataset.dependencyView===view));}
  const diagram=$('#dependency-diagram'),value=(index:number,key:string)=>{const value=tokens[index].attributes[key];return typeof value==='string'?value:'';};
  arcObserver.disconnect();
  if(view==='vertical'){
    const arcs=verticalArcs(tokens,words),gutter=Math.max(0,...arcs.map(arc=>arc.column))*TREE_COLUMN+14;
    const svg=svgNode('svg',{width:gutter,height:tokens.length*TREE_ROW,'aria-hidden':'true'});
    for(const arc of [...arcs].sort((a,b)=>Number(a.bound)-Number(b.bound))){
      const y1=(arc.from+.5)*TREE_ROW,y2=(arc.to+.5)*TREE_ROW,x=gutter-6-arc.column*TREE_COLUMN;
      svg.append(svgNode('path',{class:arc.bound?'dependency-arc bound':'dependency-arc',d:`M${gutter},${y1} H${x} V${y2} H${gutter-3} M${gutter-8},${y2-4} L${gutter-3},${y2} L${gutter-8},${y2+4}`}));
    }
    diagram.innerHTML=`<div class="dependency-tree" style="--tree-row:${TREE_ROW}px;--tree-gutter:${gutter}px">${tokens.map((token,index)=>{const word=byIndex.get(index);return `<span class="dependency-word${word?.inSpan?' in-span':''}${word?.roles.length?' bound':''}">${roleTags(word?.roles??[])}${escape(surfaceOf(token))}</span><span class="dependency-meta" title="${escape(named('dep.rel',value(index,'dep.rel'))??'')}">${index+1}${value(index,'dep.head')==='0'?'':'→'+escape(value(index,'dep.head'))}${relationGloss(value(index,'dep.rel'))} ${escape(value(index,'upos'))} ${escape(value(index,'dep.rel'))}</span>`;}).join('')}</div>`;
    diagram.firstElementChild!.prepend(svg);
  }else{
    const units=dependencyUnits(tokens,view==='bunsetsu'?starts:null),{items,arcs}=focusLayout(units,words);
    const gap=(count:number)=>locale==='ja'?`… ${count}${view==='bunsetsu'?'文節':'語'} …`:`… ${count} ${view==='bunsetsu'?'bunsetsu':count===1?'word':'words'} …`;
    const chips=document.createElement('div');chips.className='dependency-chips';chips.style.paddingTop=`${Math.max(0,...arcs.map(arc=>arc.level))*ARC_STEP+ARC_LABEL}px`;
    chips.innerHTML=items.map((item,position)=>{
      if(item.kind==='gap')return `<span class="dependency-gap" data-item="${position}">${escape(gap(item.count))}</span>`;
      const unit=units[item.unit],held:EvidenceWord[]=[];let text='';
      for(let index=unit.first;index<=unit.last;index++){text+=surfaceOf(tokens[index]);const word=byIndex.get(index);if(word)held.push(word);}
      const roles=held.flatMap(word=>word.roles);
      return `<span class="dependency-chip${held.some(word=>word.inSpan)?' in-span':''}${roles.length?' bound':''}" data-item="${position}"><span>${roleTags(roles)}${escape(text)}</span><small>${escape([...new Set(held.map(word=>value(word.index,'upos')))].join(' '))}</small></span>`;
    }).join('');
    diagram.replaceChildren(chips);arcLayouts.set(chips,arcs);arcObserver.observe(chips);
  }
  $('#dependency-evidence').innerHTML=words.map(word=>`<tr${word.inSpan?' class="in-span"':''}><td>${word.index+1}</td><td>${escape(named('upos',value(word.index,'upos'))??value(word.index,'upos'))}${word.roles.length?` <small class="role-names">${escape(word.roles.join(', '))}</small>`:''}</td><td>${escape(surfaceOf(tokens[word.index]))}</td>${['lemma','upos'].map(key=>`<td>${escape(value(word.index,key))}</td>`).join('')}<td>${escape(headLabel(tokens,word.index)??t('root'))}</td><td>${relationCell(value(word.index,'dep.rel'))}</td></tr>`).join('');
  scrollCue();
}
compactLayout.addEventListener('change',()=>{if(!dependencyChoice)renderDependencies();});
const evidenceScroll=$('.dependency-evidence-scroll');
function scrollCue(){const {scrollLeft,scrollWidth,clientWidth}=evidenceScroll,left=scrollLeft>1,right=scrollLeft+clientWidth<scrollWidth-1;evidenceScroll.dataset.more=left&&right?'both':left?'left':right?'right':'';}
evidenceScroll.addEventListener('scroll',scrollCue,{passive:true});new ResizeObserver(scrollCue).observe(evidenceScroll);
dependencyPanel.addEventListener('click',event=>{const button=(event.target as Element).closest<HTMLButtonElement>('[data-dependency-view]');if(!button)return;dependencyChoice=button.dataset.dependencyView as DependencyView;renderDependencies();});

async function openOccurrence(run:RunSnapshot,target:OccurrenceTarget):Promise<boolean>{
  try{
    const query=new URLSearchParams({pattern:target.pattern,sentence:String(target.sentence),roles:JSON.stringify(target.roles),analysis:target.analysis,fingerprint:target.fingerprint});
    const found=await request<{document:number;sequence:number}>(`api/questions/runs/${run.id}/occurrence?${query}`);
    await navigateView(hrefFor(run,{kind:'example',pattern:target.pattern,document:found.document,sequence:found.sequence,returnTo:{...defaultView(),pattern:target.pattern}}),true,'replace');
    if(!tokenLab.hidden&&!tokenLab.contains(document.activeElement)){const heading=$('#token-origin');heading.tabIndex=-1;heading.focus({preventScroll:true});tokenLab.scrollIntoView({block:'nearest'});}
    return true;
  }catch(error){
    const detail=error instanceof ServiceError&&error.status===404?t('occurrenceNotFound'):error instanceof ServiceError&&error.status===409&&/frequency table/.test(error.message)?t('occurrenceChanged'):errorMessage(error);
    $('#error').hidden=false;$('#error').textContent=`${t('occurrenceMissing')} ${detail}`;
    return false;
  }
}
let readerEpoch=0;
function openReader(){const reader=$<HTMLDialogElement>('#inspector');tokenLab.after(reader);reader.show();reader.setAttribute('aria-modal','false');reader.scrollIntoView({block:'nearest'});}
async function readDocument(scope: string,path: string | null=null) {
  const epoch=++readerEpoch;
  const inspection=readerTarget??inspecting;
  if(!inspection)return;
  const {run,row}=inspection;
  const target=$('#document-body'),title=documentTitle(row);
  readerStatus.classList.remove('error');readerStatus.textContent=locale==='ja'?`${title}を読み込んでいます…`:`Loading ${title}…`;target.setAttribute('aria-busy','true');
  const targetKey=`${run.id}:${row.corpus_sentence_ordinal}`;
  for(const button of target.querySelectorAll<HTMLButtonElement>('button')){if(!readerDisabled.has(button))readerDisabled.set(button,button.disabled);button.disabled=readerContentKey!==targetKey||readerDisabled.get(button)!;}
  if(!readerContentKey){$('#inspect-title').textContent=title;$('#inspect-origin').textContent=row.pattern;target.textContent=t('loadingRead');}
  try {
    const url=path??`api/read/${run.dataset}/${row.corpus_sentence_ordinal}/${row.span.start}/${row.span.end}/${scope}`;
    const response=await fetch(serviceURL(url));
    if (!response.ok) throw new Error(`HTTP ${response.status}`);
    const html=await response.text();
    if ((readerTarget??inspecting)!==inspection||epoch!==readerEpoch) return;
    const parsed=new DOMParser().parseFromString(html,'text/html');
    parsed.querySelectorAll<HTMLElement>('script,style').forEach(element=>element.remove());
    await localizeReader(parsed.body);
    if((readerTarget??inspecting)!==inspection||epoch!==readerEpoch)return;
    const header=parsed.body.querySelector('header');header?.querySelector('h1,h2,h3')?.remove();
    header?.querySelector('.reading-source')?.remove();
    target.replaceChildren(...parsed.body.childNodes);
    readerContentKey=`${run.id}:${row.corpus_sentence_ordinal}`;$('#inspect-title').textContent=title;$('#inspect-origin').textContent=row.pattern;readerStatus.replaceChildren();
    for (const button of target.querySelectorAll<HTMLElement>('[hx-get]')) {
      const href=button.getAttribute('hx-get')!;
      button.removeAttribute('hx-get');
      button.addEventListener('click',event=>{const owner=readerTarget??inspecting;if(!owner||`${owner.run.id}:${owner.row.corpus_sentence_ordinal}`!==targetKey){event.preventDefault();return;}void readDocument(scope,href);});
    }
  } catch(error) {
    if((readerTarget??inspecting)!==inspection||epoch!==readerEpoch)return;
    readerStatus.classList.add('error');readerStatus.textContent=(locale==='ja'?`${title}を読み込めませんでした。`:`Could not load ${title}. `)+errorMessage(error)+(readerContentKey?(locale==='ja'?' 前の原文を表示しています。':' Previous text remains visible. '):' ');
    if(!readerContentKey)target.replaceChildren();
    const retry=document.createElement('button');retry.type='button';retry.dataset.readRetry='';retry.textContent=locale==='ja'?'原文を再読み込み':'Retry reading';retry.addEventListener('click',()=>readDocument(scope,path));readerStatus.append(retry);
  }finally{if((readerTarget??inspecting)===inspection&&epoch===readerEpoch)target.setAttribute('aria-busy','false');}
}

async function localizeReader(root: HTMLElement) {
  const response=await fetch(serviceURL(`assets/i18n/${locale}.json`));
  const dict=await response.json() as Record<string,string>;
  for (const node of root.querySelectorAll<HTMLElement>('[data-i18n]')) {
    const template=dict[node.dataset.i18n??''];
    if (!template) continue;
    let args: Record<string,string>={};
    try {args=JSON.parse(node.dataset.i18nArgs??'{}');} catch {}
    node.textContent=template.replace(/\{(\w+)\}/g,(slot,key)=>args[key]??slot);
  }
}

function downloadSource(run: RunSnapshot) {
  const url=URL.createObjectURL(new Blob([run.source],{type:'text/plain;charset=utf-8'}));
  const link=document.createElement('a');link.href=url;link.download='query.lx';link.click();setTimeout(()=>URL.revokeObjectURL(url),1000);
}
async function replay() {
  if (!recorded.length || activeId) return;
  const epoch=++replayEpoch;
  isReplay=true;$('#replay-note').hidden=false;
  const frames=recorded.slice();
  const final=frames.at(-1)!;
  state=begin(state,runningSnapshot(final.id,final));
  for (const frame of frames) {
    if (epoch!==replayEpoch) return;
    state=receive(state,frame);render();
    await new Promise(resolve=>setTimeout(resolve,Math.max(100,Math.min(700,9000/frames.length))));
  }
  isReplay=false;render();
}

function questionDocument(overrides: Partial<SavedQuestion>={}): SavedQuestion {
  return {format:'lingrep.question/v1',evidence:displayedEvidence()?.id??'',view:JSON.stringify(viewLocation),question:'',sections:JSON.stringify(sectionLinks()),...(documentSections.size&&state.current?{attempt:state.current.id}:{}),source:$<HTMLTextAreaElement>('#source').value,...limitFields(),corpus:$<HTMLSelectElement>('#corpus').value,analysis:analysisExpectation??config.datasets[$<HTMLSelectElement>('#corpus').value].analysis_identity??'',where:controls.where(),...controls.entries(),...overrides};
}
function limitFields():{pattern?:string;document?:number}{const limit=activeLimit();return {...(limit?.pattern?{pattern:limit.pattern}:{}),...(limit?.document!=null?{document:limit.document}:{})};}
function openQuestion(doc: SavedQuestion) {
  questionSettings(new URLSearchParams(Object.entries(doc).map(([key,value])=>[key,String(value)])));
  const links=parseSections(typeof doc.sections==='string'?doc.sections:null);
  const dataset=Object.entries(config.datasets).find(([,value])=>value.analysis_identity===doc.analysis)?.[0]??doc.corpus;
  const {question: _question,...settings}=doc;
  const params={...settings,corpus:dataset,fresh:'1',run:doc.evidence||links.length?'0':'1'};
  const url=new URL(location.href);url.hash=new URLSearchParams(params).toString();
  window.open(url.href,'_blank','noopener');
}
$('#remix').addEventListener('click',()=>openQuestion(questionDocument()));
$('#save-question').addEventListener('click',()=>{const url=URL.createObjectURL(new Blob([JSON.stringify(questionDocument(),null,2)],{type:'application/json'}));const a=document.createElement('a');a.href=url;a.download='question.lingview.json';a.click();setTimeout(()=>URL.revokeObjectURL(url),1000);});
$('#open-question').addEventListener('change',async event=>{try{const file=(event.target as HTMLInputElement).files?.[0];if(!file)return;if(file.size>256*1024)throw new Error('Question documents may contain up to 256 KiB.');const doc=savedQuestion(JSON.parse(await file.text()));openQuestion(doc);}catch(error){$('#error').hidden=false;$('#error').textContent=errorMessage(error);}});
const testBuilder=document.createElement('section');testBuilder.className='test-builder';testBuilder.hidden=true;
testBuilder.innerHTML='<ol></ol><p><code id="pattern-preview"></code></p><div class="test-builder-actions"><button type="button" id="use-pattern"></button><button type="button" id="clear-pattern"></button></div>';
$('.attribute-actions').after(testBuilder);
function renderComposer(){
  testBuilder.hidden=!conditions.length;if(!conditions.length)return;
  $('ol',testBuilder).innerHTML=conditions.map((condition,index)=>`<li><span>${escape(tokenData?.tokens[condition.tokenIndex]?surfaceOf(tokenData.tokens[condition.tokenIndex]):'')} · <code>${escape(condition.key)}=${escape(condition.value)}</code></span> <button type="button" data-remove-condition="${index}" aria-label="${escape((locale==='ja'?'条件を削除: ':'Remove condition: ')+condition.key+'='+condition.value)}">${locale==='ja'?'削除':'Remove'}</button></li>`).join('');
  for(const button of testBuilder.querySelectorAll<HTMLButtonElement>('[data-remove-condition]'))button.addEventListener('click',()=>{conditions=conditions.filter((_,index)=>index!==Number(button.dataset.removeCondition));selectToken(tokenIndex);});
  $('#pattern-preview').textContent=patternSource(conditions);
  $('#use-pattern').textContent=locale==='ja'?'このパターンで検索':'Search this pattern';
  $('#clear-pattern').textContent=locale==='ja'?'条件を消去':'Clear conditions';
}
$<HTMLButtonElement>('#attribute-remix').addEventListener('click',()=>{
  if(!inspecting||!tokenData)return;
  const key=attributeKey,value=attributeMember;if(value===undefined)return;
  conditions=addCondition(conditions,{tokenIndex,key,value});selectToken(tokenIndex);
});
$('#clear-pattern').addEventListener('click',()=>{conditions=[];selectToken(tokenIndex);});
$('#use-pattern').addEventListener('click',()=>{
  if(!inspecting||!conditions.length)return;
  const run=inspecting.run;analysisExpectation=run.contract_run?.analysis_identity??null;
  const origin:SectionOrigin={parent:run.id,view:hitLocation(run,inspecting.row,'example'),label:`${documentTitle(inspecting.row)} · ${inspecting.row.surface}`};
  setSource(patternSource(conditions));$<HTMLSelectElement>('#corpus').value=run.dataset;
  controls.restore(new URLSearchParams({...controls.entries(),metadata:'{}',raw:run.where}));updateCorpus();updateDraft();
  const pending=search(undefined,origin),navigation=navigationEpoch;
  void pending.then(()=>{const current=state.current;if(navigation===navigationEpoch&&current&&documentSections.get(current.id)?.origin===origin){viewEvidence=current;viewLocation=defaultView();window.history.pushState(null,'',hrefFor(current,viewLocation));updateDraft();const panel=panels.get(current.id);panel?.scrollIntoView({block:'start'});panel?.querySelector<HTMLAnchorElement>('.section-origin a')?.focus({preventScroll:true});}});
});

$<HTMLInputElement>('#timeline').addEventListener('input',()=>{if(activeId||!recorded.length)return;++replayEpoch;isReplay=true;state={...state,previous:state.completed,current:recorded[Number($<HTMLInputElement>('#timeline').value)]};$('#replay-note').hidden=false;render();});

$('#reading-options').addEventListener('keydown',event=>{if(event.key==='Enter'&&!event.ctrlKey&&!event.metaKey&&!event.altKey)event.preventDefault();});
$('#query-form').addEventListener('submit',search);
$<HTMLTextAreaElement>('#source').addEventListener('input',updateDraft);

$<HTMLSelectElement>('#corpus').addEventListener('change',()=>{analysisExpectation=null;updateCorpus();updateDraft();void patternEditor?.then(editor=>editor?.recheck($<HTMLTextAreaElement>('#source')));});
$('#format-source').addEventListener('click',()=>{void patternEditor?.then(editor=>editor?.formatCurrent($<HTMLTextAreaElement>('#source')));});
$<HTMLSelectElement>('#behavior').addEventListener('change',()=>{updateDraft();render(true);});
$<HTMLButtonElement>('#cancel').addEventListener('click',async()=>{
  if(restoreController){restoreController.abort();return;}
  if(isReplay){++replayEpoch;isReplay=false;state={...state,current:state.completed};$('#replay-note').hidden=true;render();return;}
  if (activeId) {
    $<HTMLButtonElement>('#cancel').disabled=true;
    try {await post(`api/questions/runs/${activeId}/cancel`);}catch(error){$('#error').hidden=false;$('#error').textContent=errorMessage(error);}
    finally {$<HTMLButtonElement>('#cancel').disabled=false;}
  }
});
$<HTMLButtonElement>('#retry').addEventListener('click',search);
$('#language').addEventListener('click',()=>{locale=locale==='en'?'ja':'en';try{localStorage.setItem(LOCALE_KEY,locale);}catch{}translate();});
$('#edit-source').addEventListener('click',()=>{
  if(menuDefinitions().length){if(definitionMenu.hidden)openDefinitionMenu();else closeDefinitionMenu();return;}
  openSourceEditor();
});
function openSourceEditor(){
  const editor=$('#source-editor');editor.hidden=!editor.hidden;$('#edit-source').setAttribute('aria-expanded',String(!editor.hidden));
  if(editor.hidden)return;
  const input=$<HTMLTextAreaElement>('#source'),at=definitionOffset(input.value,$('#source-label').textContent??'');
  focusSource(at<0?null:at);
  if(at<0)return;
  const line=parseFloat(getComputedStyle(input).lineHeight)||20;
  input.scrollTop=Math.max(0,(input.value.slice(0,at).split('\n').length-2)*line);
}
const definitionMenu=$('#definition-menu');
function menuDefinitions():readonly CatalogDefinition[]{
  if(!config)return [];
  const source=$<HTMLTextAreaElement>('#source').value,run=state.current;
  if(source===config.source)return config.definitions??[];
  return run?.source===source&&!runLimits.get(run.id)?.pattern?run.definitions.map(name=>({name,count_only:false})):[];
}
function renderDefinitionMenu(){
  const definitions=menuDefinitions(),chosen=activeLimit()?.pattern??null,corpus=$<HTMLSelectElement>('#corpus').value,source=$<HTMLTextAreaElement>('#source').value;
  const counted=definitions.filter(definition=>definition.count_only),ja=locale==='ja';
  const table=(name:string)=>frequencyPairs?.find(pair=>pair.corpus===corpus&&pair.source===source&&pair.patterns.some(item=>item.name===name));
  const item=(definition:CatalogDefinition)=>{
    const pair=definition.count_only?table(definition.name):undefined;
    if(pair)return `<li><a href="frequencies#${escape(new URLSearchParams({corpus,catalog:pair.catalog,pattern:definition.name}).toString())}">${escape(definition.name)}</a> <span class="definition-note">${ja?'頻度表で開く':'opens in Frequencies'}</span></li>`;
    return `<li><button type="button" data-definition="${escape(definition.name)}" aria-pressed="${definition.name===chosen}">${escape(definition.name)}</button>${definition.count_only?` <span class="definition-note">${ja?'用例が多く、検索が途中で止まることがあります':'may stop: too many examples to keep'}</span>`:''}</li>`;
  };
  const markup=`<h2 id="definition-menu-title">${ja?'検索する定義':'Definition to search'}</h2><ul>${definitions.filter(definition=>!definition.count_only).map(item).join('')}</ul>${counted.length?`<h3>${ja?'頻度表のみ':'Counts only (see Frequencies)'}</h3><p>${ja?'語ごとの件数を数える定義です。用例を一覧するには数が多すぎます。':'These definitions count words; they have too many examples to list.'}</p><ul>${counted.map(item).join('')}</ul>`:''}<p class="definition-actions">${counted.length?'':`<button type="button" data-definition="" aria-pressed="${chosen===null}">${ja?'すべての定義を検索':'Search every definition'}</button>`}<button type="button" data-edit-source>${ja?'パターンのソースを編集':'Edit the pattern source'}</button></p>`;
  definitionMenu.setAttribute('aria-labelledby','definition-menu-title');
  if(renderSignatures.get(definitionMenu)===markup)return;
  const focused=definitionMenu.contains(document.activeElement)?(document.activeElement as HTMLElement).textContent:null;
  definitionMenu.innerHTML=markup;renderSignatures.set(definitionMenu,markup);
  if(focused!=null)[...definitionMenu.querySelectorAll<HTMLElement>('button,a')].find(control=>control.textContent===focused)?.focus();
}
function openDefinitionMenu(){
  $('#source-editor').hidden=true;definitionMenu.hidden=false;$('#edit-source').setAttribute('aria-expanded','true');
  renderDefinitionMenu();
  if(frequencyPairs===undefined)void loadFrequencyPairs().then(()=>{if(!definitionMenu.hidden)renderDefinitionMenu();});
  (definitionMenu.querySelector<HTMLElement>('[aria-pressed=true]')??definitionMenu.querySelector<HTMLElement>('button,a'))?.focus();
}
function closeDefinitionMenu(focus=true){definitionMenu.hidden=true;$('#edit-source').setAttribute('aria-expanded','false');if(focus)$('#edit-source').focus();}
definitionMenu.addEventListener('keydown',event=>{if(event.key==='Escape'){event.preventDefault();event.stopPropagation();closeDefinitionMenu();}});
definitionMenu.addEventListener('click',event=>{
  const target=event.target as Element;
  if(target.closest('[data-edit-source]')){closeDefinitionMenu(false);openSourceEditor();return;}
  const button=target.closest<HTMLButtonElement>('[data-definition]');if(!button)return;
  const pattern=button.dataset.definition||null,source=$<HTMLTextAreaElement>('#source').value;
  searchLimit={source,pattern,document:activeLimit()?.document??null};if(pattern)preferredPattern=pattern;
  closeDefinitionMenu();updateDraft();void search();
});
$('#token-attributes').addEventListener('click',event=>{const button=(event.target as Element).closest<HTMLButtonElement>('[data-attribute]');if(!button)return;attributeKey=button.dataset.attribute!;attributeMember=button.dataset.member;const all=!!button.closest('.attribute-all');selectToken(tokenIndex);[...$('#token-attributes').querySelectorAll<HTMLButtonElement>(all?'.attribute-all [data-attribute]':'.attribute-basics [data-attribute]')].find(item=>item.dataset.attribute===attributeKey&&item.dataset.member===attributeMember)?.focus({preventScroll:true});});
$<HTMLDialogElement>('#inspector').addEventListener('close',()=>{++readerEpoch;if(viewLocation.kind==='reader'&&inspecting)void navigateView(hrefFor(inspecting.run,viewLocation.returnTo),true,'replace');if(document.activeElement===document.body)$('#results').focus({preventScroll:true});});
$<HTMLButtonElement>('#replay').addEventListener('click',replay);
document.addEventListener('keydown',event=>{if(event.key==='Escape'&&$<HTMLDialogElement>('#inspector').open){$<HTMLDialogElement>('#inspector').close();event.preventDefault();}
  else if(event.key==='Escape'&&!event.defaultPrevented&&!tokenLab.hidden&&(tokenLab.contains(document.activeElement)||!document.activeElement?.closest('input,textarea,select,[contenteditable]:not([contenteditable=false]),dialog'))){event.preventDefault();const close=$<HTMLAnchorElement>('#close-token');close.focus({preventScroll:true});close.click();}if(!event.isComposing&&(event.ctrlKey||event.metaKey||event.altKey)&&event.key==='Enter'){event.preventDefault();search();}});

function displayedEvidence(): RunSnapshot | null {
  if(viewEvidence)return viewEvidence;
  if(inspecting&&!tokenLab.hidden)return inspecting.run;
  return visible(state,$<HTMLSelectElement>('#behavior').value as Behavior);
}
function concordanceFor(run:RunSnapshot):ConcordanceLocation {
  const pattern=selection.get(run.id)??run.definitions[0]??'',page=documentView(run,pattern);
  return {kind:'concordance',pattern,scopeDocument:page.scopeDocument,returnAfter:page.returnAfter,document:null,after:page.after,filter:filters.get(run.id)??''};
}
function hitLocation(run:RunSnapshot,row:MatchRecord,kind:'example'|'reader'):HitLocation {
  const example={kind:'example' as const,pattern:row.pattern,document:row.document_ordinal,sequence:row.sequence,returnTo:concordanceFor(run)};
  return kind==='example'?example:{...example,kind:'reader',returnTo:example};
}
function hrefFor(run:RunSnapshot,view:ViewLocation):string {
  const params:Record<string,string|number>={...controls.entries(),behavior:$<HTMLSelectElement>('#behavior').value};
  if(controls.where()!==run.where){params.metadata='{}';params.raw=run.where;}
  for(const [key,value] of Object.entries({metadata:'{}',raw:'',context:12,lines:20,behavior:'keep'}))if(String(params[key])===String(value))delete params[key];
  return locationHash(params,run.id,view);
}
async function readRun(identifier:string):Promise<RunSnapshot> {
  const run=parseSnapshot(await request<unknown>(`api/questions/runs/${encodeURIComponent(identifier)}`));
  if(!Object.hasOwn(config.datasets,run.dataset)||run.contract_run&&config.datasets[run.dataset].analysis_identity!=null&&config.datasets[run.dataset].analysis_identity!==run.contract_run.analysis_identity)throw new Error('The corpus for this saved search is unavailable.');
  return run;
}
function questionParams(url:URL,run:RunSnapshot):URLSearchParams {
  const params=new URLSearchParams(url.hash.slice(1));
  resolveCatalog(params,servedCatalog());
  if(!params.has('source'))params.set('source',run.source);
  if(!params.has('corpus'))params.set('corpus',run.dataset);
  if(!Object.hasOwn(config.datasets,params.get('corpus')!))throw new Error('The corpus for this question is not registered.');
  if(!params.has('metadata')&&!params.has('raw')){params.set('metadata','{}');params.set('raw',run.where);}
  return params;
}
async function applyView(run:RunSnapshot,target:ViewLocation,pin=true,epoch=++navigationEpoch):Promise<void> {
  if(run.phase==='complete'&&target.pattern&&!run.definitions.includes(target.pattern)&&!Object.hasOwn(run.counts,target.pattern))throw new Error('This pattern is not part of the saved search.');
  viewEvidence=pin?run:null;viewLocation=target;
  if(target.kind!=='reader'&&$<HTMLDialogElement>('#inspector').open)$<HTMLDialogElement>('#inspector').close();
  if(target.kind==='reader'&&target.returnTo.kind==='example'){
    const origin=target.returnTo;
    if(!inspecting||inspecting.run.id!==run.id||inspecting.row.sequence!==origin.sequence){
      await applyView(run,origin,pin,epoch);if(epoch!==navigationEpoch)return;viewLocation=target;
    }
    let row=target.sequence!=null?(documentView(run,target.pattern).rows.find(row=>row.sequence===target.sequence)??run.rows[target.pattern]?.find(row=>row.sequence===target.sequence)):undefined;
    if(!row){const page=await request<DocumentPage>(`api/questions/runs/${run.id}/documents?${new URLSearchParams({pattern:target.pattern,document:String(target.document),after:String(target.sequence==null?-1:target.sequence-1),limit:'1'})}`);row=page.rows[0];}
    if(epoch!==navigationEpoch)return;
    if(!row)throw new Error('This saved location has no matching example. Choose another document.');
    readerTarget={run,row};viewLocation={...target,sequence:row.sequence,returnTo:origin.sequence==null&&inspecting?{...origin,sequence:inspecting.row.sequence}:origin};
    if(!readerPanel.open)openReader();updateReaderNavigation();await readDocument('document');updateDraft();return;
  }

  if(target.kind!=='concordance'&&inspecting?.run.id===run.id&&target.sequence===inspecting.row.sequence&&!tokenLab.hidden){
    $<HTMLAnchorElement>('#close-inspector').setAttribute('href',hrefFor(run,target.kind==='reader'?target.returnTo:target));
    if(target.kind==='reader'){readerTarget=inspecting;if(!$<HTMLDialogElement>('#inspector').open)openReader();updateReaderNavigation();await readDocument('document');}
    updateDraft();return;
  }
  const returnFocus=target.kind==='concordance'&&inspecting?.run.id===run.id&&tokenLab.contains(document.activeElement)?inspecting.row.sequence:null;
  ++inspectionEpoch;inspectionFailure=null;
  if(target.kind==='concordance'||inspecting?.run.id!==run.id){tokenLab.hidden=true;inspecting=null;}
  const parent=target.kind==='concordance'?target:target.returnTo;
  const context=parent.kind==='concordance'?parent:parent.returnTo;
  const pattern=target.pattern||run.definitions[0]||'';
  selection.set(run.id,pattern);filters.set(run.id,context.filter);
  const page=documentView(run,pattern),scope=context.scopeDocument??null;
  if(page.loading){++page.epoch;page.loading=false;page.request=null;}
  const needsPage=run.phase==='complete'&&(page.documents===null||page.scopeDocument!==scope||page.after!==context.after||context.document!=null&&context.document!==page.document);
  page.returnAfter=context.returnAfter??-1;
  render();
  const panel=panels.get(run.id);if(!panel)return;
  if(needsPage)await loadDocumentPage(panel,run,pattern,context.document,context.after,null,scope);
  if(epoch!==navigationEpoch)return;
  if(returnFocus!=null&&document.activeElement===document.body)(panel.querySelector<HTMLElement>(`[data-sequence="${returnFocus}"]`)??$('#results')).focus({preventScroll:true});
  if(target.kind!=='concordance') {
    let row=documentView(run,pattern).rows.find(row=>target.sequence!=null&&row.sequence===target.sequence)??run.rows[pattern]?.find(row=>target.sequence!=null&&row.sequence===target.sequence);
    if(!row&&run.phase!=='complete'){pendingView=target;return;}
    if(!row){
      const page=await request<DocumentPage>(`api/questions/runs/${run.id}/documents?${new URLSearchParams({pattern,document:String(target.document),after:String(target.sequence==null?-1:target.sequence-1),limit:'1'})}`);
      row=page.rows[0];
    }
    if(epoch!==navigationEpoch)return;
    if(!row)throw new Error('This saved location has no matching example. Choose another document.');
    const entryRow=entryShown(run)&&entryLines!.lines[entryLines!.at].sentence===row.corpus_sentence_ordinal?panel.querySelector<HTMLElement>(`[data-entry-line="${entryLines!.at}"]`):null;
    let link=entryRow??panel.querySelector<HTMLElement>(`[data-sequence="${row.sequence}"]`);
    if(!link){filters.set(run.id,'');await loadDocumentPage(panel,run,pattern,row.document_ordinal,row.sequence-1);link=panel.querySelector<HTMLElement>(`[data-sequence="${row.sequence}"]`);}
    if(epoch!==navigationEpoch||!link)return;
    viewLocation=target.kind==='reader'&&target.returnTo.kind==='example'&&target.returnTo.sequence==null?{...target,sequence:row.sequence,returnTo:{...target.returnTo,sequence:row.sequence}}:{...target,sequence:row.sequence};
    await inspect(run,row,link);
    if(epoch!==navigationEpoch)return;
    if(target.kind==='reader') {readerTarget={run,row};if(!$<HTMLDialogElement>('#inspector').open)openReader();updateReaderNavigation();await readDocument('document');}
  }
  updateDraft();
}
async function navigateView(href:string,preserveDraft=false,historyMode:'push'|'replace'|'none'='push'):Promise<void> {
  const epoch=++navigationEpoch;pendingView=null;pendingOccurrence=null;
  if(isReplay){++replayEpoch;isReplay=false;state={...state,current:state.completed};$('#replay-note').hidden=true;}
  try {
    const url=new URL(href,location.href),params=new URLSearchParams(url.hash.slice(1)),identifier=params.get('evidence');
    if(!identifier)throw new Error('This link has no saved search.');
    const target=parseView(params.get('view'));
    const run=[viewEvidence,state.current,state.previous,state.completed,inspecting?.run,documentSections.get(identifier)?.run].find(run=>run?.id===identifier)??await readRun(identifier);
    if(epoch!==navigationEpoch)return;
    if(!preserveDraft){analysisExpectation=run.contract_run?.analysis_identity??null;const question=questionParams(url,run);setSource(question.get('source')??run.source);$<HTMLSelectElement>('#corpus').value=question.get('corpus')??run.dataset;restoreLimit(question);controls.restore(question);updateCorpus();}
    if(historyMode!=='none')window.history[historyMode==='push'?'pushState':'replaceState'](null,'',url.href);
    await applyView(run,target,true,epoch);
  }catch(error){if(epoch!==navigationEpoch)return;$('#error').hidden=false;$('#error').textContent=errorMessage(error);}
}
document.addEventListener('click',event=>{
  if(event.defaultPrevented||event.button!==0||event.metaKey||event.ctrlKey||event.shiftKey||event.altKey)return;
  const link=(event.target as Element | null)?.closest<HTMLAnchorElement>('a[data-view]');
  if(link?.getAttribute('aria-disabled')==='true'){event.preventDefault();return;}
  if(!link?.hasAttribute('href'))return;
  const url=new URL(link.href);
  if(url.origin!==location.origin||url.pathname!==location.pathname)return;
  event.preventDefault();const pending=navigateView(link.href,true),navigation=navigationEpoch;void pending.then(()=>{
    if(navigation===navigationEpoch&&link.hasAttribute('data-section-origin')&&inspecting){const panel=panels.get(inspecting.run.id);const row=panel?.querySelector<HTMLElement>(`[data-sequence="${inspecting.row.sequence}"]`);row?.scrollIntoView({block:'center'});row?.focus({preventScroll:true});}
  });
});
window.addEventListener('popstate',()=>void navigateView(location.href,false,'none'));

async function init() {
  config=await request<Configuration>('api/questions/config');
  const seed=document.querySelector<HTMLScriptElement>('#question-seed');
  let params=seedParams(seed?JSON.parse(seed.textContent??'null'):null,location.hash);
  changedCatalog=resolveCatalog(params,servedCatalog()).changed;
  $('#catalog-notice').hidden=changedCatalog===null;
  const initialView=parseView(params.get('view'));viewLocation=initialView;
  const seededQuestion=!!seed&&!params.has('evidence')&&!params.has('attempt')&&!params.has('sections');
  if(params.get('fresh')==='1')for(const name of ['run','completed','replay','sections'])sessionStorage.removeItem(`ling-questions-${name}`);
  $('#corpus').innerHTML=Object.entries(config.datasets).map(([key,dataset])=>`<option value="${escape(key)}">${escape(dataset.label)}</option>`).join('');
  savedManifest=parseSections(params.get('sections')??(params.get('fresh')==='1'||seededQuestion?null:sessionStorage.getItem('ling-questions-sections')));
  const recipe=savedManifest.find(link=>link.id===params.get('evidence'))?.recipe;
  if(params.has('source'))savedManifest=savedManifest.map(link=>link.id===(params.get('attempt')??params.get('evidence'))&&!link.recipe?{...link,recipe:{source:params.get('source')!,dataset:params.get('corpus')??Object.keys(config.datasets)[0],where:params.get('where')??params.get('raw')??'',analysis:params.get('analysis')||null}}:link);
  if(recipe){for(const [key,value] of Object.entries({source:recipe.source,corpus:recipe.dataset,raw:recipe.where,analysis:recipe.analysis??''}))if(!params.has(key)&&(key!=='raw'||!params.has('metadata')))params.set(key,value);}
  const corpus=params.get('corpus')??Object.keys(config.datasets)[0];
  if(!corpus||!Object.hasOwn(config.datasets,corpus))throw new Error('The corpus for this question is not registered.');
  $<HTMLSelectElement>('#corpus').value=corpus;analysisExpectation=params.get('analysis')||null;
  setSource(params.get('source')??config.source);
  controls=initQuestion(config,{changed:updateDraft,viewChanged:()=>render(true),search:()=>search(),getLocale:()=>locale},params);
  if(['keep','live','compare'].includes(params.get('behavior')??''))$<HTMLSelectElement>('#behavior').value=params.get('behavior')!;
  const explicit=params.get('evidence'),attemptId=params.get('attempt')??(!explicit&&!seededQuestion?sessionStorage.getItem('ling-questions-run'):null),completedId=params.get('completed')??(!explicit&&!seededQuestion?sessionStorage.getItem('ling-questions-completed'):null);
  const ids=[...new Set([explicit,attemptId,completedId].filter((id):id is string=>!!id))];
  let expired=false;
  const loaded=new Map<string,RunSnapshot>();
  for(const id of ids){try{loaded.set(id,await readRun(id));}catch(error){
    const expiry=expiredSearch(error);expired ||= expiry;unavailableSearches.set(id,{expired:expiry,message:errorMessage(error)});
    if(!savedManifest.some(link=>link.id===id))savedManifest=[...savedManifest,{id,origin:null,...(params.has('source')?{recipe:{source:params.get('source')!,dataset:corpus,where:controls.where(),analysis:analysisExpectation}}:{})}];
  }}
  let evidence=explicit?loaded.get(explicit)??null:null;
  const previous=completedId?loaded.get(completedId)??null:null;
  if(!evidence&&!explicit)evidence=previous;
  const attempt=attemptId?loaded.get(attemptId)??null:null,current=attempt??evidence;
  if(current){
    for(const [key,value] of new URLSearchParams({source:current.source,corpus:current.dataset,raw:current.where}))if(!params.has(key)&&(key!=='raw'||!params.has('metadata')))params.set(key,value);
    analysisExpectation=expectedAnalysis(params,current);
    $<HTMLSelectElement>('#corpus').value=params.get('corpus')!;setSource(params.get('source')!);controls.restore(params);
  }
  state={current:null,previous:null,completed:previous?.phase==='complete'?previous:evidence?.phase==='complete'?evidence:null};
  if(current){if(savedManifest.length>1)documentSections.set(current.id,{run:current,origin:savedManifest.find(link=>link.id===current.id)?.origin??null});state=receive(begin(state,runningSnapshot(current.id,current)),current);if(current.phase==='running'){activeId=current.id;connect(current.id);}else if(current.phase==='complete')recorded=restoreReplay(current);}
  if(!recorded.length&&state.completed)recorded=restoreReplay(state.completed);
  const follow=params.get('follow')==='1'||!explicit;viewEvidence=follow?null:evidence;
  for(const link of savedManifest){const run=loaded.get(link.id);if(run&&savedManifest.length>1)documentSections.set(link.id,{run,origin:link.origin});}
  await loadSavedSections();
  savedManifest=sectionLinks();
  try{sessionStorage.setItem('ling-questions-sections',JSON.stringify(savedManifest));}catch{}
  restoreLimit(params);
  if(current&&searchLimit?.source===current.source&&!runLimits.has(current.id))runLimits.set(current.id,{pattern:searchLimit.pattern,document:searchLimit.document});
  translate();renderSavedSearches();
  if(params.get('notice')){$('#error').hidden=false;$('#error').textContent=params.get('notice')!;}
  const returnHref=frequencyReturn(params.get('return'));if(returnHref){$<HTMLAnchorElement>('#frequency-return').href=returnHref;$('#frequency-return').hidden=false;}
  const occurrence=parseOccurrence(params.get('occurrence')),entry=parseEntryLines(params.get('entry'));
  if(occurrence&&entry)entryLines={...entry,pattern:occurrence.pattern,analysis:occurrence.analysis,fingerprint:occurrence.fingerprint};
  (window as Window & {__probe?:object}).__probe={get state(){return state;},get recorded(){return recorded;},get controls(){return controls;},get view(){return viewLocation;},search,questionDocument};
  if(current)await applyView(follow&&current.phase==='complete'&&initialView.kind==='concordance'?current:evidence??current,follow&&current.phase==='complete'&&initialView.kind==='concordance'?defaultView():initialView,!follow);
  if(changedCatalog===null&&!expired&&!savedManifest.some(link=>!availableRun(link.id))&&params.get('run')==='1'&&(!current||current.source!==$<HTMLTextAreaElement>('#source').value||current.dataset!==$<HTMLSelectElement>('#corpus').value||current.where!==controls.where())){await search(undefined,undefined,occurrence??undefined);pendingView=current?defaultView():initialView;if(occurrence){pendingView=null;pendingOccurrence=occurrence;}}
  else if(occurrence&&current?.phase==='complete')await openOccurrence(current,occurrence);
}

init().catch(error=>{$('#status').textContent=errorMessage(error);$<HTMLButtonElement>('#run').disabled=true;$('#retry').hidden=true;});
