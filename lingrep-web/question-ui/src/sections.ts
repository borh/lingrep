import {validVectorDigest} from './model.js';
import {parseView} from './location.js';
import type {ViewLocation} from './location.js';
export interface SectionOrigin { readonly parent:string; readonly view:ViewLocation; readonly label:string; }
export interface QueryRecipe {readonly source:string;readonly dataset:string;readonly where:string;readonly analysis:string|null;readonly vectors_blake3?:string;}
export interface SectionLink { readonly id:string; readonly origin:SectionOrigin|null; readonly recipe?:QueryRecipe; }
export function parseSections(text:string|null):readonly SectionLink[]{
  if(!text)return [];
  let values:unknown;
  try{values=JSON.parse(text);}catch{throw new Error('The linked searches could not be read. Open an individual search.');}
  if(!Array.isArray(values))throw new Error('The linked searches could not be read. Open an individual search.');
  const ids=new Set<string>();
  return values.map((value:unknown)=>{
    if(!value||typeof value!=='object'||!('id' in value)||typeof value.id!=='string'||!value.id||ids.has(value.id)||!('origin' in value))throw new Error('The linked searches contain an invalid search reference.');
    let origin:SectionOrigin|null=null;
    if(value.origin!==null){
      const item=value.origin;
      if(!item||typeof item!=='object'||!('parent' in item)||typeof item.parent!=='string'||!ids.has(item.parent)||!('label' in item)||typeof item.label!=='string'||!('view' in item))throw new Error('The linked searches contain an invalid parent reference.');
      origin={parent:item.parent,label:item.label,view:parseView(JSON.stringify(item.view))};
    }
    let recipe:QueryRecipe|undefined;
    if('recipe' in value){
      const item=value.recipe;
      if(!item||typeof item!=='object'||!('source' in item)||typeof item.source!=='string'||!('dataset' in item)||typeof item.dataset!=='string'||!('where' in item)||typeof item.where!=='string'||!('analysis' in item)||(item.analysis!==null&&typeof item.analysis!=='string'))throw new Error('A linked search contains an invalid saved query.');
      if('vectors_blake3' in item&&!validVectorDigest(item.vectors_blake3))throw new Error('A linked search contains an invalid word-vector digest.');
      recipe={...('vectors_blake3' in item?{vectors_blake3:item.vectors_blake3 as string}:{}),source:item.source,dataset:item.dataset,where:item.where,analysis:item.analysis};
    }
    ids.add(value.id);return {id:value.id,origin,...(recipe?{recipe}:{})};
  });
}
export async function restoreSections(links:readonly SectionLink[],available:ReadonlySet<string>,execute:(link:SectionLink&{recipe:QueryRecipe})=>Promise<string>,progress:(links:readonly SectionLink[])=>void):Promise<readonly SectionLink[]>{
  let manifest=links;
  for(let index=0;index<manifest.length;index++){
    const link=manifest[index];
    if(available.has(link.id))continue;
    if(!link.recipe)continue;
    const id=await execute({...link,recipe:link.recipe});
    manifest=manifest.map(item=>({...item,id:item.id===link.id?id:item.id,origin:item.origin?.parent===link.id?{...item.origin,parent:id}:item.origin}));
    progress(manifest);
  }
  return manifest;
}
