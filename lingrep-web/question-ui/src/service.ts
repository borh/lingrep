export const serviceURL = (path:string,base:string=document.baseURI):string => new URL(path.replace(/^\/+/,''),base).href;
export class ServiceError extends Error {
  constructor(readonly status:number,message:string){super(message);}
}
export const expiredSearch = (error:unknown):boolean => error instanceof ServiceError && (error.status===404||error.status===410);
