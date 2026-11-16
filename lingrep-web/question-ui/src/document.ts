import {serviceURL} from './service.js';
import { element as $, errorMessage } from './model.js';
import type { Configuration, Locale, MetadataValue, MetadataCondition, MetadataResult, QuestionControls, QuestionSettings, Field } from './model.js';
const esc = (value: unknown) => String(value??'').replace(/[&<>"']/g,c=>({'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;',"'":'&#39;'}[c]??c));
export const tokenLiteral = (value: string) => [...String(value)].map(c=>/[\p{L}\p{N}\p{M}_-]/u.test(c)?c:c.codePointAt(0)!<=0xffff?'\\u'+c.codePointAt(0)!.toString(16).padStart(4,'0'):c).join('');
const scopeValue = (value: MetadataValue) => typeof value==='string'?'"'+value+'"':String(value);
export function scopeClause(metadata: Record<string,MetadataCondition>, raw='') {
  const clauses=[];
  if(raw.trim()) clauses.push(raw.trim());
  for(const [key,condition] of Object.entries(metadata)) {
    const field=`@document.${key}`;
    if(condition.numeric) {
      if(condition.min!==''&&condition.min!=null) clauses.push(`${field} >= ${Number(condition.min)}`);
      if(condition.max!==''&&condition.max!=null) clauses.push(`${field} <= ${Number(condition.max)}`);
    } else if(condition.values?.length) {
      if(condition.list) {
        const tests=condition.values.map(v=>`${field} ${condition.exclude?'not contains':'contains'} ${scopeValue(v)}`);
        clauses.push(tests.length===1?tests[0]:`(${tests.join(condition.exclude?' & ':' | ')})`);
      } else clauses.push(`${field} ${condition.exclude?'not in':'in'} {${condition.values.map(scopeValue).join(', ')}}`);
    }
  }
  if(raw.trim()&&clauses.length>1)clauses[0]=`(${clauses[0]})`;
  return clauses.join(' & ');
}

function scrubNumber(input: HTMLInputElement, changed: (dragging: boolean)=>void, committed=()=>{}) {
  let drag: {id: number; x:number;value:number;moved:boolean} | undefined;
  const paint=()=>{input.style.width=`${Math.max(2,String(input.value||input.placeholder).length)+.8}ch`;};
  input.title=document.documentElement.lang==='ja'?'左右にドラッグ、または数値を入力':'Drag horizontally or type a number';
  input.addEventListener('input',()=>{paint();changed(false);});
  input.addEventListener('change',()=>{if(input.checkValidity())committed();});
  input.addEventListener('pointerdown',event=>{
    if(event.button!==0)return;
    drag={id:event.pointerId,x:event.clientX,value:Number(input.value||input.placeholder),moved:false};
    input.setPointerCapture(event.pointerId);
  });
  input.addEventListener('pointermove',event=>{
    if(!drag||event.pointerId!==drag.id)return;
    const steps=Math.trunc((event.clientX-drag.x)/8);
    if(!steps&&!drag.moved)return;
    if(!drag.moved){drag.moved=true;input.setPointerCapture(event.pointerId);}
    event.preventDefault();
    input.value=String(Math.max(Number(input.min),Math.min(input.max?Number(input.max):Infinity,drag.value+steps*Number(input.step||1))));
    paint();changed(true);
  });
  const finish=(event: PointerEvent)=>{if(!drag||event.pointerId!==drag.id)return;if(drag.moved)committed();drag=undefined;};
  input.addEventListener('pointerup',finish);input.addEventListener('pointercancel',finish);
  input.addEventListener('lostpointercapture',()=>{if(drag?.moved)committed();drag=undefined;});
  paint();
}

export function questionSettings(params: URLSearchParams): QuestionSettings {
  const metadata: Record<string,MetadataCondition>=Object.create(null);
  const parsed: unknown=JSON.parse(params.get('metadata')??'{}');
  if(!parsed||typeof parsed!=='object'||Array.isArray(parsed))throw new Error('The document conditions are invalid. Open a new question.');
  for(const [key,raw] of Object.entries(parsed)) {
    if(!raw||typeof raw!=='object'||Array.isArray(raw))throw new Error('The document conditions are invalid.');
    const value=raw as Record<string,unknown>;
    if(value.numeric===true){
      if(typeof value.min!=='string'||typeof value.max!=='string'||[value.min,value.max].some(bound=>bound!==''&&!Number.isFinite(Number(bound)))||value.min!==''&&value.max!==''&&Number(value.min)>Number(value.max))throw new Error('The saved numeric range is invalid.');
      metadata[key]={numeric:true,min:value.min,max:value.max};
    } else {
      if(typeof value.list!=='boolean'||typeof value.exclude!=='boolean'||!Array.isArray(value.values)||!value.values.every(v=>typeof v==='string'||typeof v==='number'&&Number.isFinite(v)||typeof v==='boolean'))throw new Error('The saved metadata values are invalid.');
      const labels=value.labels&&typeof value.labels==='object'&&!Array.isArray(value.labels)&&Object.values(value.labels).every(v=>typeof v==='string')?value.labels as Record<string,string>:undefined;
      metadata[key]={list:value.list,exclude:value.exclude,values:value.values as MetadataValue[],labels};
    }
  }
  const number=(key:string,fallback:number,min:number,max:number)=>{const value=Number(params.get(key)??fallback);return Number.isFinite(value)?Math.max(min,Math.min(max,Math.round(value))):fallback;};
  return {metadata,raw:params.get('raw')??(params.has('metadata')?'':params.get('where')??''),context:number('context',12,0,24),lines:number('lines',20,1,Number.MAX_SAFE_INTEGER)};
}

export function initQuestion(config: Configuration, {changed,viewChanged,search,getLocale}: {changed(commit?: boolean | Event): void;viewChanged(): void;search(): void;getLocale(): Locale}, initialParams=new URLSearchParams(location.hash.slice(1))): QuestionControls {
  const params=initialParams;
  const state=questionSettings(params);
  $<HTMLInputElement>('#where').value=state.raw;
  state.context=Number.isFinite(state.context)?Math.round(state.context):12;
  $<HTMLInputElement>('#context-size').value=String(state.context);
  state.lines=Number.isFinite(state.lines)?Math.max(1,Math.min(Number.MAX_SAFE_INTEGER,Math.round(state.lines))):20;
  $<HTMLInputElement>('#line-limit').value=String(state.lines);
  let pickerState:'closed'|'opening'|'open'='closed';let pickerEpoch=0;
  let fields: Field[]=[];let key='';let after='';let previous: string[]=[];let requestId=0;let searchTimer: ReturnType<typeof setTimeout> | undefined;
  const text=(en: string,ja: string)=>getLocale()==='ja'?ja:en;
  const label=(field: string)=>config.labels?.[`@document.${field}`]?.labels?.[getLocale()]??field;
  const api=async (path: string): Promise<MetadataResult>=>{const response=await fetch(serviceURL(path));const data=await response.json();if(!response.ok)throw new Error(data.error??`HTTP ${response.status}`);return data;};
  function changedScope(runSearch=true) {
    state.raw=$<HTMLInputElement>('#where').value;
    renderChips();changed(runSearch);
    clearTimeout(searchTimer);if(runSearch)searchTimer=setTimeout(search,300);
  }
  function renderChips() {
    const active=Object.entries(state.metadata).filter(([,c])=>c.numeric?(c.min!==''&&c.min!=null)||(c.max!==''&&c.max!=null):c.values?.length);
    $('#scope-label').textContent=active.length||state.raw?text('selected documents','条件に合う作品'):text('all documents','すべての作品');
    $('#scope-chips').innerHTML=active.map(([key,c])=>`<button type="button" class="scope-chip" data-field="${esc(key)}">${esc(label(key))}: ${c.numeric?`${esc(c.min??'…')}–${esc(c.max??'…')}`:esc(c.values.map(v=>c.labels&&Object.hasOwn(c.labels,String(v))?c.labels[String(v)]:String(v)).join(', '))}<span aria-hidden="true"> ×</span></button>`).join('');
    for(const button of $('#scope-chips').querySelectorAll<HTMLButtonElement>('button'))button.addEventListener('click',()=>{delete state.metadata[button.dataset.field??''];changedScope();if(button.dataset.field===key)loadValues();});
    $('#clause-preview').textContent=scopeClause(state.metadata,state.raw)||text('All documents','すべての作品');
  }
  async function loadFields() {
    const epoch=++requestId;
    $('#metadata-state').textContent=text('Reading corpus metadata…','コーパスのメタデータを読み込んでいます…');
    try {
      const data=await api(`api/questions/data/${$<HTMLSelectElement>('#corpus').value}/metadata`);
      if(epoch!==requestId)return;
      const initialKey=data.fields.some(f=>f.key==='author')?'author':data.fields[0]?.key;
      const initialField=data.fields.find(f=>f.key===initialKey);
      const initialValues=initialField&&!initialField.numeric?await api(`api/questions/data/${$<HTMLSelectElement>('#corpus').value}/metadata?${new URLSearchParams({field:initialKey,q:'',after:''})}`):undefined;
      if(epoch!==requestId)return;
      fields=data.fields;
      $<HTMLSelectElement>('#metadata-field').innerHTML=fields.map(f=>`<option value="${esc(f.key)}">${esc(label(f.key))} (${f.present.toLocaleString()})</option>`).join('');
      key=initialKey;$<HTMLInputElement>('#metadata-query').value='';
      $<HTMLSelectElement>('#metadata-field').value=key;
      $('#metadata-state').textContent=text(`${fields.length} metadata fields · counts are documents in this corpus.`,`${fields.length} 個のメタデータ項目 · 件数はコーパス内の作品数です。`);
      after='';previous=[];await loadValues(initialValues);
    }catch(error){if(epoch===requestId)$('#metadata-state').textContent=errorMessage(error);}
  }
  async function loadValues(initialValues?: MetadataResult) {
    const field=fields.find(f=>f.key===key);if(!field)return;
    const epoch=++requestId;
    const values=$('#metadata-values');values.setAttribute('aria-busy','true');values.inert=true;
    $<HTMLButtonElement>('#metadata-next').disabled=true;$<HTMLButtonElement>('#metadata-previous').disabled=true;
    const condition: MetadataCondition=state.metadata[key]??{list:field.list,exclude:false,values:[]};
    $<HTMLInputElement>('#metadata-exclude').checked=('exclude' in condition?condition.exclude:false);
    $<HTMLInputElement>('#metadata-exclude').closest('label')!.hidden=field.numeric;
    $<HTMLInputElement>('#metadata-query').hidden=field.numeric;
    $('#metadata-validation').hidden=true;
    $('#metadata-vocabulary').hidden=key!=='ndc';
    if(field.numeric) {
      const bounds=condition.numeric?condition:{numeric:true,min:'',max:''};
      const numericField=field;
      $('#metadata-values').innerHTML=`<div class="range-fields"><span class="number-range"><input type="number" class="number-span" aria-label="${text('Minimum','下限')}" data-bound="min" step="1" min="${numericField.min}" max="${bounds.max||numericField.max}" value="${esc(bounds.min??'')}" placeholder="${numericField.min}"><button type="button" class="range-window" role="slider" tabindex="0" aria-label="${text('Move range','範囲を移動')}" aria-valuemin="${numericField.min}" aria-valuemax="${numericField.max}">–</button><input type="number" class="number-span" aria-label="${text('Maximum','上限')}" data-bound="max" step="1" min="${bounds.min||numericField.min}" max="${numericField.max}" value="${esc(bounds.max??'')}" placeholder="${numericField.max}"></span></div>`;
      const lower=$<HTMLInputElement>('[data-bound=min]'),upper=$<HTMLInputElement>('[data-bound=max]'),windowControl=$('.range-window');
      function validRange() {
        const valid=lower.checkValidity()&&upper.checkValidity();
        $('#metadata-validation').hidden=valid;
        $('#metadata-validation').textContent=text('Use values within this field’s bounds, with the minimum no greater than the maximum.','項目の範囲内で、下限が上限を超えない値を入力してください。');
        return valid;
      }
      function updateRange(dragging=false) {
        lower.max=upper.value||String(numericField.max);upper.min=lower.value||String(numericField.min);
        if(!validRange()){clearTimeout(searchTimer);return;}
        state.metadata[key]={numeric:true,min:lower.value,max:upper.value};
        windowControl.setAttribute('aria-valuenow',lower.value||String(numericField.min));
        windowControl.setAttribute('aria-valuetext',`${lower.value||numericField.min}–${upper.value||numericField.max}`);
        changedScope(!dragging);
      }
      function commitRange(){if(validRange())changedScope();}
      for(const input of [lower,upper])scrubNumber(input,updateRange,commitRange);
      windowControl.setAttribute('aria-valuenow',lower.value||String(numericField.min));
      windowControl.setAttribute('aria-valuetext',`${lower.value||numericField.min}–${upper.value||numericField.max}`);
      let drag: {x:number;lower:number;upper:number} | undefined;
      const moveRange=(delta: number,initialLower=Number(lower.value||numericField.min),initialUpper=Number(upper.value||numericField.max),dragging=false)=>{
        delta=Math.max(numericField.min-initialLower,Math.min(numericField.max-initialUpper,delta));
        lower.value=String(initialLower+delta);upper.value=String(initialUpper+delta);updateRange(dragging);
      };
      windowControl.addEventListener('keydown',event=>{const delta: number | undefined=({ArrowLeft:-1,ArrowDown:-1,ArrowRight:1,ArrowUp:1,PageDown:-10,PageUp:10,Home:-Infinity,End:Infinity} as Record<string,number>)[event.key];if(delta!=null){event.preventDefault();moveRange(delta);}});
      windowControl.addEventListener('pointerdown',event=>{if(event.button!==0)return;event.preventDefault();windowControl.focus();windowControl.setPointerCapture(event.pointerId);drag={x:event.clientX,lower:Number(lower.value||numericField.min),upper:Number(upper.value||numericField.max)};});
      windowControl.addEventListener('pointermove',event=>{if(drag)moveRange(Math.trunc((event.clientX-drag.x)/8),drag.lower,drag.upper,true);});
      const finish=()=>{if(drag){drag=undefined;commitRange();}};
      windowControl.addEventListener('pointerup',finish);windowControl.addEventListener('pointercancel',finish);windowControl.addEventListener('lostpointercapture',finish);
      $<HTMLButtonElement>('#metadata-next').disabled=true;$<HTMLButtonElement>('#metadata-previous').disabled=true;values.removeAttribute('aria-busy');values.inert=false;return;
    }
    const category=condition.numeric?{list:false,values:[] as MetadataValue[],exclude:false}:condition;
    const query=new URLSearchParams({field:key,q:$<HTMLInputElement>('#metadata-query').value,after});
    try {
      const data=initialValues??await api(`api/questions/data/${$<HTMLSelectElement>('#corpus').value}/metadata?${query}`);
      if(epoch!==requestId)return;
      $('#metadata-values').innerHTML=data.values.map((item,i)=>`<label class="metadata-value"><input type="checkbox" data-value="${i}" ${category.values?.some(v=>JSON.stringify(v)===JSON.stringify(item.value))?'checked':''} ${typeof item.value==='string'&&/["\n\r]/.test(item.value)?'disabled':''}><span>${esc(item.label)}</span><span class="value-count">${item.documents.toLocaleString()}</span></label>`).join('')||`<p>${text('No matching values.','一致する値がありません。')}</p>`;
      for(const input of $('#metadata-values').querySelectorAll<HTMLInputElement>('input'))input.addEventListener('change',()=>{
        const stored=state.metadata[key];
        const current=stored&&!stored.numeric?stored:{list:field.list,exclude:$<HTMLInputElement>('#metadata-exclude').checked,values:[]};
        const value=data.values[Number(input.dataset.value)].value;
        current.labels={...(current.labels??{}),[String(value)]:data.values[Number(input.dataset.value)].label};
        current.values=current.values.filter(v=>JSON.stringify(v)!==JSON.stringify(value));
        if(input.checked)current.values.push(value);
        state.metadata[key]=current;changedScope();
      });
      $<HTMLButtonElement>('#metadata-next').disabled=!data.next;
      $<HTMLButtonElement>('#metadata-next').onclick=()=>{previous.push(after);after=data.next??'';loadValues();};
      $<HTMLButtonElement>('#metadata-previous').disabled=!previous.length;
      $<HTMLButtonElement>('#metadata-previous').onclick=()=>{after=previous.pop()??'';loadValues();};
    }catch(error){if(epoch===requestId)$('#metadata-values').textContent=errorMessage(error);}
    finally{if(epoch===requestId){values.removeAttribute('aria-busy');values.inert=false;}}
  }
  function closePicker(){++pickerEpoch;pickerState='closed';$('#metadata-picker').hidden=true;$('#scope-toggle').setAttribute('aria-expanded','false');$('#scope-toggle').removeAttribute('aria-busy');}
  async function openPicker(){
    const epoch=++pickerEpoch;pickerState='opening';$('#scope-toggle').setAttribute('aria-busy','true');
    if(!fields.length)await loadFields();
    if(epoch!==pickerEpoch)return;
    pickerState='open';$('#metadata-picker').hidden=false;$('#scope-toggle').setAttribute('aria-expanded','true');$('#scope-toggle').removeAttribute('aria-busy');
  }
  $('#scope-toggle').addEventListener('click',()=>{if(pickerState==='closed')void openPicker();else closePicker();});
  $('#metadata-close').addEventListener('click',()=>{closePicker();$('#scope-toggle').focus();});
  $<HTMLSelectElement>('#metadata-field').addEventListener('change',()=>{key=$<HTMLSelectElement>('#metadata-field').value;after='';previous=[];$<HTMLInputElement>('#metadata-query').value='';loadValues();});
  let valueTimer: ReturnType<typeof setTimeout> | undefined;
  $<HTMLInputElement>('#metadata-query').addEventListener('input',()=>{clearTimeout(valueTimer);valueTimer=setTimeout(()=>{after='';previous=[];loadValues();},120);});
  $<HTMLInputElement>('#metadata-exclude').addEventListener('change',()=>{const field=fields.find(f=>f.key===key);if(!field||field.numeric)return;const stored=state.metadata[key];state.metadata[key]={...(stored&&!stored.numeric?stored:{list:field.list,exclude:false,values:[]}),exclude:$<HTMLInputElement>('#metadata-exclude').checked};changedScope();});
  $<HTMLInputElement>('#where').addEventListener('input',()=>changedScope());
  $('#metadata-clear').addEventListener('click',()=>{state.metadata=Object.create(null);state.raw='';$<HTMLInputElement>('#where').value='';changedScope();loadValues();});
  $<HTMLSelectElement>('#corpus').addEventListener('change',()=>{const reopen=pickerState!=='closed';closePicker();++requestId;state.metadata=Object.create(null);fields=[];$('#metadata-field').replaceChildren();$('#metadata-values').replaceChildren();renderChips();if(reopen)void openPicker();clearTimeout(searchTimer);changed();search();});
  scrubNumber($<HTMLInputElement>('#context-size'),()=>{if(!$<HTMLInputElement>('#context-size').checkValidity()||$<HTMLInputElement>('#context-size').value==='')return;state.context=Number($<HTMLInputElement>('#context-size').value);viewChanged();changed();});
  scrubNumber($<HTMLInputElement>('#line-limit'),()=>{if(!$<HTMLInputElement>('#line-limit').checkValidity()||$<HTMLInputElement>('#line-limit').value==='')return;state.lines=Number($<HTMLInputElement>('#line-limit').value);viewChanged();changed();});
  renderChips();
  return {state,where:()=>scopeClause(state.metadata,$<HTMLInputElement>('#where').value),entries:()=>({metadata:JSON.stringify(state.metadata),raw:$<HTMLInputElement>('#where').value,context:state.context,lines:state.lines}),restore(params:URLSearchParams){
    clearTimeout(searchTimer);clearTimeout(valueTimer);++requestId;Object.assign(state,questionSettings(params));
    $<HTMLInputElement>('#where').value=state.raw;$<HTMLInputElement>('#context-size').value=String(state.context);$<HTMLInputElement>('#line-limit').value=String(state.lines);
    fields=[];renderChips();if(!$('#metadata-picker').hidden)loadFields();
  },localize(){for(const input of document.querySelectorAll<HTMLElement>('.number-span'))input.title=text('Drag horizontally or type a number','左右にドラッグ、または数値を入力');$<HTMLInputElement>('#line-limit').setAttribute('aria-label',text('Number of examples','表示する用例数'));$<HTMLInputElement>('#context-size').setAttribute('aria-label',text('Context tokens','前後の文脈のトークン数'));renderChips();if(fields.length){const selected=$<HTMLSelectElement>('#metadata-field').value;$<HTMLSelectElement>('#metadata-field').innerHTML=fields.map(f=>`<option value="${esc(f.key)}">${esc(label(f.key))} (${f.present.toLocaleString()})</option>`).join('');$<HTMLSelectElement>('#metadata-field').value=selected;}}};
}
