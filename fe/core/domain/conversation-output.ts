/** Output-only decoding. Presence is a contract: only an absent envelope is legacy. */
import { z } from 'zod';
import type { OutputContentV1 } from '../api/generated/wire.js';

const bag = z.record(z.string(), z.unknown());
function outputFields() { return { fields: bag, chunk: bag.optional() }; }
const resource = z.union([
  z.object({ uri:z.string(), text:z.string(), fields:bag }).strict(),
  z.object({ uri:z.string(), blob:z.string(), fields:bag }).strict(),
]);
export const outputContentSchema: z.ZodType<OutputContentV1> = z.object({version:z.literal(1),blocks:z.array(z.discriminatedUnion('type',[
  z.object({type:z.literal('text'),text:z.string(),...outputFields()}).strict(),
  z.object({type:z.literal('image'),data:z.string(),mimeType:z.string(),...outputFields()}).strict(),
  z.object({type:z.literal('audio'),data:z.string(),mimeType:z.string(),...outputFields()}).strict(),
  z.object({type:z.literal('resource_link'),uri:z.string(),name:z.string(),...outputFields()}).strict(),
  z.object({type:z.literal('resource'),resource,...outputFields()}).strict(),
  z.object({type:z.literal('tool_diff'),path:z.string(),newText:z.string(),...outputFields()}).strict(),
  z.object({type:z.literal('terminal_reference'),terminalId:z.string(),...outputFields()}).strict(),
  z.object({type:z.literal('unknown_extension'),raw:z.unknown().refine(value=>value!==undefined),chunk:bag.optional()}).strict(),
]))}).strict();

export type OutputProjection = Readonly<
  {kind:'available';content:OutputContentV1;identity:string} | {kind:'unavailable';identity:string}
>;
export function decodeOutput(value:unknown, identity:string):OutputProjection {
  const parsed=outputContentSchema.safeParse(value);
  return parsed.success ? {kind:'available',content:parsed.data,identity} : {kind:'unavailable',identity};
}
export function outputText(output:OutputProjection):string {
  return output.kind === 'unavailable' ? 'Output unavailable' : output.content.blocks.flatMap(block =>
    block.type === 'text' ? [block.text] : block.type === 'resource' && 'text' in block.resource ? [block.resource.text] : []).join('');
}
export function decimalU64(value:unknown):value is string {
  return typeof value === 'string' && /^(0|[1-9]\d{0,19})$/.test(value) && BigInt(value)<=18446744073709551615n;
}
export function outputRecord(value:unknown):Record<string,unknown>|null {
  return typeof value === 'object' && value !== null && !Array.isArray(value) ? value as Record<string,unknown> : null;
}
/** Persisted ordinals are JSON numbers, so unsafe values must never acquire an invented identity. */
export function persistedOutput(payload:Record<string,unknown>,identity:string,requiredSegment=false):OutputProjection|undefined {
  if(!Object.hasOwn(payload,'outputContent'))return undefined;
  const segment=outputRecord(payload.outputSegment);
  if((requiredSegment || Object.hasOwn(payload,'outputSegment')) && (segment === null || !Number.isSafeInteger(segment.ordinal) || Number(segment.ordinal)<0
    || (segment.channel !== 'message' && segment.channel !== 'thought')))return {kind:'unavailable',identity};
  return decodeOutput(payload.outputContent,identity);
}
