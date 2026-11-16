/** Bounded static raster preflight. Browser decoding is still required after this succeeds.
 * Encoded bytes and one RGBA surface have separate limits; this is not a total RSS guarantee. */
export type OutputRaster = Readonly<{bytes:Uint8Array<ArrayBuffer>;mime:string;width:number;height:number}>;
const MAX_BYTES=8*1024*1024;
const ALPHABET='ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/';
function base64(data:string):Uint8Array<ArrayBuffer>|null {
  if(data.length === 0 || data.length>Math.ceil(MAX_BYTES/3)*4 || data.length%4 !== 0
    || !/^[A-Za-z0-9+/]*={0,2}$/.test(data))return null;
  const padding=data.endsWith('==') ? 2 : data.endsWith('=') ? 1 : 0;
  const length=data.length/4*3-padding;
  if(length>MAX_BYTES)return null;
  if(padding && (ALPHABET.indexOf(data[data.length-padding-1]) & (padding === 2 ? 15 : 3)) !== 0)return null;
  const bytes=new Uint8Array(length);
  let offset=0;
  for(let i=0;i<data.length;i+=4){
    const word=(ALPHABET.indexOf(data[i])<<18)|(ALPHABET.indexOf(data[i+1])<<12)
      |(Math.max(0,ALPHABET.indexOf(data[i+2]))<<6)|Math.max(0,ALPHABET.indexOf(data[i+3]));
    for(const shift of [16,8,0])if(offset<length)bytes[offset++]=(word>>shift)&255;
  }
  return bytes;
}
function ascii(bytes:Uint8Array,start:number,length:number):string {
  return String.fromCharCode(...bytes.subarray(start,start+length));
}
function dimensions(bytes:Uint8Array,mime:string):readonly [number,number]|null {
  const v=new DataView(bytes.buffer,bytes.byteOffset,bytes.byteLength), n=bytes.length;
  if(mime === 'image/png'){
    if(n<45 || ascii(bytes,0,8)!=='\x89PNG\r\n\x1a\n')return null;
    let p=8, size:readonly [number,number]|null=null, data=false;
    while(p+12<=n){
      const len=v.getUint32(p),type=ascii(bytes,p+4,4);
      if(len>n-p-12 || ['acTL','fcTL','fdAT'].includes(type))return null;
      if(p === 8){if(type !== 'IHDR' || len !== 13)return null;size=[v.getUint32(p+8),v.getUint32(p+12)];}
      else if(type === 'IHDR')return null;
      if(type === 'IDAT')data=true;
      p+=len+12;
      if(type === 'IEND')return len === 0 && data && p === n ? size:null;
    }
    return null;
  }
  if(mime === 'image/jpeg'){
    if(n<4 || bytes[0]!==255 || bytes[1]!==216)return null;
    let p=2,size:readonly [number,number]|null=null,scan=false;
    while(p<n){
      if(bytes[p++]!==255){if(scan)continue;return null;}
      while(bytes[p]===255)p++;
      const marker=bytes[p++];
      if(scan && (marker===0 || (marker !== undefined && marker>=208 && marker<=215)))continue;
      if(marker===217)return scan && p===n ? size:null;
      if(marker===undefined || marker===216 || p+2>n)return null;
      const len=v.getUint16(p);
      if(len<2 || p+len>n)return null;
      if((marker>=192 && marker<=195) || (marker>=197 && marker<=199) || (marker>=201 && marker<=203) || (marker>=205 && marker<=207)){
        if(size!==null || len<8)return null;
        size=[v.getUint16(p+5),v.getUint16(p+3)];
      }
      if(marker===218)scan=true;
      p+=len;
    }
    return null;
  }
  if(mime === 'image/gif'){
    if(n<14 || !['GIF87a','GIF89a'].includes(ascii(bytes,0,6)))return null;
    const size=[v.getUint16(6,true),v.getUint16(8,true)] as const;
    let p=13+((bytes[10]&128) ? 3*(1<<((bytes[10]&7)+1)):0),frames=0;
    const skip=()=>{while(p<n){const len=bytes[p++];if(len===0)return true;p+=len;}return false;};
    while(p<n){
      const tag=bytes[p++];
      if(tag===59)return p===n && frames===1 ? size:null;
      if(tag===33){const label=bytes[p++];if(label===255)return null;if(!skip())return null;}
      else if(tag===44){
        if(++frames>1 || p+9>n)return null;
        const w=v.getUint16(p+4,true),h=v.getUint16(p+6,true);
        if(v.getUint16(p,true)+w>size[0] || v.getUint16(p+2,true)+h>size[1])return null;
        const packed=bytes[p+8];p+=9+((packed&128) ? 3*(1<<((packed&7)+1)):0);
        if(p>=n)return null;p++;if(!skip())return null;
      }else return null;
    }
    return null;
  }
  if(mime === 'image/webp'){
    if(n<20 || ascii(bytes,0,4)!=='RIFF' || v.getUint32(4,true)+8!==n || ascii(bytes,8,4)!=='WEBP')return null;
    let p=12,size:readonly [number,number]|null=null,frame=false;
    while(p+8<=n){
      const type=ascii(bytes,p,4),len=v.getUint32(p+4,true),b=p+8;
      if(len>n-b || type==='ANIM' || type==='ANMF')return null;
      if(type==='VP8X'){
        if(frame || size!==null || len!==10 || (bytes[b]&2))return null;
        size=[1+(bytes[b+4]|(bytes[b+5]<<8)|(bytes[b+6]<<16)),1+(bytes[b+7]|(bytes[b+8]<<8)|(bytes[b+9]<<16))];
      }else if(type==='VP8 ' || type==='VP8L'){
        if(frame)return null;frame=true;
        let inner:readonly [number,number];
        if(type==='VP8 '){
          if(len<10 || (bytes[b]&1) || ascii(bytes,b+3,3)!=='\x9d\x01\x2a')return null;
          inner=[v.getUint16(b+6,true)&16383,v.getUint16(b+8,true)&16383];
        }else{
          if(len<5 || bytes[b]!==47)return null;
          const bits=v.getUint32(b+1,true);inner=[1+(bits&16383),1+((bits>>>14)&16383)];
        }
        if(size!==null && (size[0]!==inner[0] || size[1]!==inner[1]))return null;
        size=inner;
      }
      p=b+len+(len&1);
    }
    return p===n && frame ? size:null;
  }
  return null;
}
export function preflightOutputRaster(data:string,mime:string):OutputRaster|null {
  const bytes=base64(data);if(bytes===null)return null;
  const size=dimensions(bytes,mime);if(size===null)return null;
  const [width,height]=size;
  return width>0 && height>0 && width<=4096 && height<=4096 && width*height<=16_000_000
    ? {bytes,mime,width,height}:null;
}
