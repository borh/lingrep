export const serviceURL = (path:string,base:string=document.baseURI):string => new URL(path.replace(/^\/+/,''),base).href;
export class ServiceError extends Error {
  constructor(readonly status:number,message:string){super(message);}
}
export const expiredSearch = (error:unknown):boolean => error instanceof ServiceError && (error.status===404||error.status===410);
/** Builds a Workbench permalink request with compound derivation disabled. */
export const workbenchLinkRequest = (source:string,corpus:string):string => new URLSearchParams({pattern:source,corpus,derive_compounds:'false'}).toString();
/** Returns null unless `payload.path` starts with `?`. */
export const workbenchHref = (payload:unknown,base:string=document.baseURI):string|null => {
  const path=(payload as {path?:unknown}|null)?.path;
  return typeof path==='string'&&path.startsWith('?')?serviceURL('workbench'+path,base):null;
};
