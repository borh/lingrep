import type { Behavior, RunSnapshot, RunningSnapshot, SearchState } from './model.js';
export function receive(state: SearchState, snapshot: RunSnapshot): SearchState {
  if (snapshot.id !== state.current?.id) return state;
  if (state.current.phase !== 'running' && snapshot.phase === 'running') return state;
  return { ...state, current: snapshot, completed: snapshot.phase === 'complete' ? snapshot : state.completed };
}
export function begin(state: SearchState, snapshot: RunningSnapshot): SearchState {
  return { ...state, previous: state.completed, current: snapshot };
}
export const exportable = (run: RunSnapshot): boolean => (run.phase === 'complete' || run.phase === 'failed' && run.search_complete === true && run.exportable === true) && Object.values(run.counts).some(total => total > 0);
export function visible(state: SearchState, behavior: Behavior): RunSnapshot | null {
  if (behavior === 'keep' && state.current?.phase !== 'complete' && state.previous) return state.previous;
  return state.current;
}

export function receiveUpdate(state: SearchState, update: import('./model.js').SearchUpdate): SearchState {
  const current=state.current;
  if(current?.id!==update.id||current.phase!=='running')return state;
  const version=update.patch.version??current.version;
  if(version<current.version||!update.reset&&version===current.version&&Object.keys(update.append).length)return state;
  for(const key of ['source','dataset','where'] as const)if(update.patch[key]!=null&&update.patch[key]!==current[key])throw new Error('Search identity changed during an update.');
  if(current.contract_run&&update.patch.contract_run&&current.contract_run.analysis_identity!==update.patch.contract_run.analysis_identity)throw new Error('Corpus identity changed during an update.');
  const rows=update.reset?update.append:Object.keys(update.append).length?{...current.rows,...Object.fromEntries(Object.entries(update.append).map(([name,added])=>[name,[...(Object.hasOwn(current.rows,name)?current.rows[name]:[]),...added]]))}:current.rows;
  return receive(state,{...current,...update.patch,counts:update.reset?update.patch.counts??{}:update.patch.counts?{...current.counts,...update.patch.counts}:current.counts,bodies:update.reset?update.patch.bodies??{}:update.patch.bodies?{...current.bodies,...update.patch.bodies}:current.bodies,rows} as RunSnapshot);
}
