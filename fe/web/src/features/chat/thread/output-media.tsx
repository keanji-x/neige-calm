/** Conversation-owned pixel lease. No module registry, cache, queue or URI reader. */
import { useEffect, useMemo, useRef, useSyncExternalStore } from 'react';
import { createPortal } from 'react-dom';
import { useLightbox } from '@astryxdesign/core/Lightbox';
import { preflightOutputRaster } from '../../../../../core/domain/conversation-output-image.ts';
import { useState } from '../../../ui/state/public.ts';
import styles from './reply.module.css';

export type OutputMediaOwner = ReturnType<typeof createOutputMediaOwner>;
export function createOutputMediaOwner(scope='') {
  let held:Readonly<{key:string;dispose:()=>void}>|null=null,chosen:string|null=null;
  const listeners=new Set<()=>void>();
  const changed=()=>{for(const listener of listeners)listener();};
  const clear=()=>{const previous=held;held=null;previous?.dispose();};
  return {
    scope,
    subscribe:(listener:()=>void)=>{listeners.add(listener);return ()=>{listeners.delete(listener);};},
    snapshot:()=>chosen,
    acquire(key:string,dispose:()=>void){
      if(chosen!==null && chosen!==key)return false;
      if(held!==null)return false;
      chosen=key;held={key,dispose};changed();return true;
    },
    choose(key:string){if(chosen===key)return;clear();chosen=key;changed();},
    release(key:string){if(held?.key!==key)return;clear();chosen=null;changed();},
    dispose(){clear();chosen=null;changed();},
  };
}

export function OutputImage({data,mime,identity,alt,owner}:{data:string;mime:string;identity:string;alt:string;owner:OutputMediaOwner}) {
  const [loaded,setLoaded]=useState<Readonly<{identity:string;url:string}>|null>(null);
  const [failed,setFailed]=useState<string|null>(null);
  const [suspended,setSuspended]=useState(false);
  const chosen=useSyncExternalStore(owner.subscribe,owner.snapshot);
  const eligible=chosen===null || chosen===identity;
  const url=loaded?.identity===identity ? loaded.url:null;
  const media=useMemo(()=>url===null ? []:[{src:url,alt}], [url,alt]);
  const lightbox=useLightbox({media,hasZoom:true});
  const closeRef=useRef(lightbox.close);closeRef.current=lightbox.close;
  useEffect(()=>{
    if(failed===identity || !eligible || suspended)return;
    let alive=true,objectUrl:string|null=null,image:HTMLImageElement|null=null;
    const dispose=()=>{
      alive=false;closeRef.current();
      image?.removeAttribute('src');
      if(objectUrl!==null){URL.revokeObjectURL(objectUrl);objectUrl=null;}
      setLoaded(current=>current?.identity===identity ? null:current);
    };
    if(!owner.acquire(identity,dispose))return;
    const raster=preflightOutputRaster(data,mime);
    if(raster===null){setFailed(identity);owner.release(identity);return;}
    objectUrl=URL.createObjectURL(new Blob([raster.bytes],{type:raster.mime}));
    image=new Image();image.src=objectUrl;
    const decoding=image;
    void decoding.decode().then(()=>{
      if(!alive)return;
      if(decoding.naturalWidth!==raster.width || decoding.naturalHeight!==raster.height){setFailed(identity);owner.release(identity);return;}
      setLoaded({identity,url:objectUrl!});
    }).catch(()=>{if(alive){setFailed(identity);owner.release(identity);}});
    return ()=>owner.release(identity);
    // Lease notifications allow a deferred image to retry when the open surface is released.
  },[owner,identity,data,mime,failed,eligible,suspended]);
  if(failed===identity)return <span className={styles.unavailable}>Unsupported image · invalid, animated or exceeds 4096 / 16MP limits</span>;
  if(url===null)return <button type="button" onClick={()=>{setSuspended(false);owner.choose(identity);}} className={styles.link}>Load image · deferred</button>;
  return <>
    <button type="button" className={styles.imageButton} aria-haspopup="dialog" aria-label={alt || 'Open image'} onClick={()=>lightbox.open()}>
      <img src={url} alt={alt} className={styles.image} />
    </button>
    <button type="button" className={styles.link} onClick={()=>{setSuspended(true);owner.release(identity);}}>Release image</button>
    {url!==null && createPortal(lightbox.element,document.body)}
  </>;
}
