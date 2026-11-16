import { EditorView } from '@codemirror/view';
import { language } from '@codemirror/language';
import { cleanup, fireEvent, render, screen, waitFor, within } from '@testing-library/react';
import { afterEach, expect, it, vi } from 'vitest';
import { commands, page } from 'vitest/browser';

import '../../../styles/entry.css';
import type { Conversation, ConversationTurn } from '../../../../../core/domain/conversation.ts';
import { Drawer } from '../../../ui/drawer/public.tsx';
import { StrictMode } from 'react';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { GitHubPreviewProvider } from '../../../systems/github-links/public.tsx';
import { decodeOutput } from '../../../../../core/domain/conversation-output.ts';
import { githubPreviewOperation } from '../../../../../core/domain/github-preview.ts';
import { OUTPUT_ANIMATIONS, OUTPUT_RASTERS } from './output-raster-fixtures.ts';
import { Reply } from './reply.tsx';
import { ChatThread } from './public.tsx';

afterEach(async()=>{cleanup();await commands.emulateReducedMotion(false);});

it('shares the code contract with fenced code through the real chat renderer', async () => {
  render(<Reply text={'```rust\nfn main() {}\n```'} imageFiles={null} />);
  await waitFor(() => {
    const content = screen.getByRole('textbox', { name: 'Code' });
    expect(EditorView.findFromDOM(content)?.state.facet(language)?.name).toBe('rust');
  }, { timeout: 10000 });
  expect(screen.getByRole('button', { name: 'Copy code' })).toBeTruthy();
});


it('preserves readable heading typography and block spacing at every reply level', () => {
  const { container } = render(<Reply imageFiles={null} text={'# First\n\ntext\n\n## Second\n\n### Third\n\n#### Fourth'} />);
  const headings = [...container.querySelectorAll<HTMLElement>('h3, h4, h5, h6')];
  for (const [index, heading] of headings.entries()) {
    const painted = getComputedStyle(heading);
    expect(painted.fontWeight).toBe('600');
    const probe = document.createElement('span');
    probe.style.fontSize = `var(--text-heading-${index + 3}-size)`;
    probe.style.lineHeight = `var(--text-heading-${index + 3}-leading)`;
    document.body.append(probe);
    expect(painted.fontSize).toBe(getComputedStyle(probe).fontSize);
    expect(painted.lineHeight).toBe(getComputedStyle(probe).lineHeight);
    probe.remove();
    expect(Number.parseFloat(painted.lineHeight)).toBeGreaterThan(Number.parseFloat(painted.fontSize));
    expect(Number.parseFloat(painted.marginBlockStart)).toBe(index === 0 ? 0 : index < 3 ? 24 : 16);
    expect(Number.parseFloat(painted.marginBlockEnd)).toBe(index < 3 ? 12 : 8);
  }
});

it('keeps content-sized table columns readable inside their own narrow scroller', () => {
  const { container } = render(<div style={{ inlineSize: 240, overflowWrap: 'anywhere' }}><Reply imageFiles={null}
    text={'| ID | Medium label | Very long descriptive header | Extra |\n| - | - | - | - |\n| 1 | words | **long body value with spaces** | longunbrokentextvalue |'} /></div>);
  const table = container.querySelector('table')!;
  const wrap = table.parentElement!;
  const widths = [...table.querySelectorAll('th')].map(cell => cell.getBoundingClientRect().width);
  expect(widths[0]).toBeGreaterThanOrEqual(60);
  expect(widths[1]).toBeGreaterThanOrEqual(80);
  expect(widths[2]).toBeGreaterThanOrEqual(120);
  expect(widths[3]).toBeGreaterThanOrEqual(120);
  expect(wrap.scrollWidth).toBeGreaterThan(wrap.clientWidth);
  wrap.scrollLeft = 50;
  expect(wrap.scrollLeft).toBe(50);
  expect(container.firstElementChild!.scrollWidth).toBeLessThanOrEqual(240);
});

it('aligns Markdown tables with surrounding reply prose on both inline edges while keeping inter-column padding', () => {
  const conversation: Conversation = { provider: 'codex' as const, id: 'c1', trackId: 'w1', title: 'Planner', kind: 'codex', state: 'idle', updatedAt: 0 };
  const prose = 'A long introductory paragraph that wraps across the full width of the conversation container so its inline-start and inline-end edges establish the reading measure. '.repeat(3).trim();
  const markdown = [
    '## Section summary',
    prose,
    ['| Index | Topic | Action |', '| :--- | :--- | ---: |', '| #1 | Area creation flow | Navigate to new area |'].join('\n'),
  ].join('\n\n');
  render(
    <div style={{ inlineSize: 780 }}>
      <ChatThread
        hasPendingMessages={false}
        canContinue={false}
        cards={{}}
        stalled={false}
        conversation={conversation}
        turns={[{ id: 'reply-1', author: 'agent', text: markdown, atMs: 1 }]}
      />
    </div>,
  );

  const heading = screen.getByRole('heading', { level: 4, name: 'Section summary' });
  const paragraph = screen.getByText(prose);
  const table = screen.getByRole('table');
  const [firstHeader, middleHeader, lastHeader] = screen.getAllByRole('columnheader');
  const [firstCell, middleCell, lastCell] = screen.getAllByRole('cell');

  const headingBox = heading.getBoundingClientRect();
  const paragraphBox = paragraph.getBoundingClientRect();
  const tableBox = table.getBoundingClientRect();
  const firstHeaderBox = firstHeader.getBoundingClientRect();
  const firstCellBox = firstCell.getBoundingClientRect();
  const lastHeaderBox = lastHeader.getBoundingClientRect();
  const lastCellBox = lastCell.getBoundingClientRect();

  // Prose is not capped at Astryx's 680px default inside the 740px reply column.
  expect(paragraphBox.width).toBeGreaterThan(700);
  expect(Math.abs(paragraphBox.left - headingBox.left)).toBeLessThanOrEqual(1);
  expect(Math.abs(tableBox.left - paragraphBox.left)).toBeLessThanOrEqual(1);
  expect(Math.abs(tableBox.right - paragraphBox.right)).toBeLessThanOrEqual(1);

  // Outer column edges sit flush with the prose and row rules; inner column edges keep their spacing.
  expect(getComputedStyle(firstHeader).paddingInlineStart).toBe('0px');
  expect(getComputedStyle(firstCell).paddingInlineStart).toBe('0px');
  expect(getComputedStyle(lastHeader).paddingInlineEnd).toBe('0px');
  expect(getComputedStyle(lastCell).paddingInlineEnd).toBe('0px');
  expect(Number.parseFloat(getComputedStyle(firstCell).paddingInlineEnd)).toBeGreaterThan(0);
  expect(Number.parseFloat(getComputedStyle(middleHeader).paddingInlineStart)).toBeGreaterThan(0);
  expect(Number.parseFloat(getComputedStyle(middleCell).paddingInlineStart)).toBeGreaterThan(0);
  expect(Number.parseFloat(getComputedStyle(lastCell).paddingInlineStart)).toBeGreaterThan(0);

  expect(Math.abs(firstHeaderBox.left - paragraphBox.left)).toBeLessThanOrEqual(1);
  expect(Math.abs(firstCellBox.left - paragraphBox.left)).toBeLessThanOrEqual(1);
  expect(Math.abs(lastHeaderBox.right - paragraphBox.right)).toBeLessThanOrEqual(1);
  expect(Math.abs(lastCellBox.right - paragraphBox.right)).toBeLessThanOrEqual(1);
});

it('opens user message attachments in the Lightbox gallery, preserves unclipped button focus geometry, and navigates across images', async () => {
  const firstUrl = URL.createObjectURL(new Blob([
    '<svg xmlns="http://www.w3.org/2000/svg" width="800" height="600"><rect width="800" height="600" fill="#dbeafe"/></svg>',
  ], { type: 'image/svg+xml' }));
  const secondUrl = URL.createObjectURL(new Blob([
    '<svg xmlns="http://www.w3.org/2000/svg" width="900" height="600"><rect width="900" height="600" fill="#dcfce7"/></svg>',
  ], { type: 'image/svg+xml' }));
  try {
    const conversation: Conversation = {
      provider: 'codex' as const, id: 'c1', trackId: 'w1', title: 'Planner', kind: 'codex', state: 'idle', updatedAt: 0,
    };
    render(
      <ChatThread
        hasPendingMessages={false}
        canContinue={false}
        cards={{}}
        stalled={false}
        conversation={conversation}
        turns={[{
          id: 'u1',
          author: 'you',
          text: 'Compare these two captures',
          atMs: 1,
          attachments: [
            { id: 'att-1', url: firstUrl, contentType: 'image/svg+xml', size: 100 },
            { id: 'att-2', url: secondUrl, contentType: 'image/svg+xml', size: 100 },
          ],
        }]}
      />,
    );

    expect(document.querySelector('dialog')).toBeNull();
    const secondTrigger = screen.getByRole('button', { name: 'Image 2 of 2' });
    expect(getComputedStyle(secondTrigger.parentElement!).overflow).toBe('visible');
    expect(getComputedStyle(secondTrigger).overflow).toBe('hidden');
    expect(getComputedStyle(secondTrigger).borderRadius).toBe('6px');
    secondTrigger.focus();
    fireEvent.click(secondTrigger);

    const dialog = await screen.findByRole('dialog', { name: 'Media viewer' });
    expect(within(dialog).getByText('2 / 2')).toBeTruthy();
    expect(dialog.querySelector('img')?.getAttribute('src')).toBe(secondUrl);

    fireEvent.click(within(dialog).getByRole('button', { name: 'Previous' }));
    expect(within(dialog).getByText('1 / 2')).toBeTruthy();
    expect(dialog.querySelector('img')?.getAttribute('src')).toBe(firstUrl);

    fireEvent.keyDown(dialog, { key: 'ArrowRight' });
    expect(within(dialog).getByText('2 / 2')).toBeTruthy();
    expect(dialog.querySelector('img')?.getAttribute('src')).toBe(secondUrl);

    fireEvent.click(within(dialog).getByRole('button', { name: 'Close' }));
    await waitFor(() => expect(screen.queryByRole('dialog', { name: 'Media viewer' })).toBeNull());
    expect(document.activeElement).toBe(secondTrigger);
  } finally {
    URL.revokeObjectURL(firstUrl);
    URL.revokeObjectURL(secondUrl);
  }
});


it.each(OUTPUT_RASTERS)('decodes a bounded static $mime blob and opens Lightbox in a narrow reply',async raster=>{
  await commands.emulateReducedMotion(raster.mime==='image/webp');
  const output=decodeOutput({version:1,blocks:[{type:'text',text:'before',fields:{}},{type:'image',data:raster.data,mimeType:raster.mime,fields:{name:raster.mime}},{type:'text',text:'after',fields:{}}]},raster.mime);
  const close=vi.fn();
  const view=render(<Drawer open title="Planner" onClose={close}><div style={{inlineSize:280}}><Reply text="" output={output} imageFiles={null} /></div></Drawer>);
  const image=await screen.findByRole('img',{name:raster.mime});
  await waitFor(()=>expect((image as HTMLImageElement).naturalWidth).toBe(20));
  expect(image.getBoundingClientRect().width).toBeGreaterThan(0);
  expect(image.getBoundingClientRect().width).toBeLessThanOrEqual(280);
  const trigger=screen.getByRole('button',{name:raster.mime});trigger.focus();fireEvent.click(trigger);
  const dialog=await screen.findByRole('dialog',{name:raster.mime});
  expect(within(dialog).getByRole('img').getAttribute('src')).toMatch(/^blob:/);
  fireEvent.keyDown(document,{key:'Escape',bubbles:true,cancelable:true});
  await waitFor(()=>expect(screen.queryByRole('dialog',{name:raster.mime})).toBeNull());
  expect(document.activeElement).toBe(trigger);expect(close).not.toHaveBeenCalled();
  view.unmount();
});

it('revokes every output URL across replacement, empty blocks, card switch and StrictMode unmount',async()=>{
  const create=vi.spyOn(URL,'createObjectURL'),revoke=vi.spyOn(URL,'revokeObjectURL');
  const raster=OUTPUT_RASTERS[0];
  const output=(id:string)=>decodeOutput({version:1,blocks:[{type:'image',mimeType:raster.mime,data:raster.data,fields:{}}]},id);
  const conversation:Conversation={provider:'codex',id:'one',trackId:'track',title:null,kind:'codex',state:'running',updatedAt:0};
  const paint=(id:string,card=conversation,empty=false)=><StrictMode><ChatThread hasPendingMessages={false} canContinue={false} stalled={false} cards={{}} conversation={card} turns={[{id:'row',author:'agent',text:'',atMs:0,output:empty ? decodeOutput({version:1,blocks:[]},id):output(id)}]} /></StrictMode>;
  try{
    const view=render(paint('r1'));await screen.findByRole('img',{name:'Output image'});
    fireEvent.click(screen.getByRole('button',{name:'Output image'}));await screen.findByRole('dialog');
    view.rerender(paint('r2'));await waitFor(()=>expect(screen.queryByRole('dialog')).toBeNull());
    await screen.findByRole('img',{name:'Output image'});
    view.rerender(paint('r3',conversation,true));await waitFor(()=>expect(screen.queryByRole('img')).toBeNull());
    view.rerender(paint('r4'));await screen.findByRole('img',{name:'Output image'});
    view.rerender(paint('r5',{...conversation,id:'two'}));await screen.findByRole('img',{name:'Output image'});
    view.unmount();
    expect(create.mock.results.length).toBeGreaterThan(3);
    expect(new Set(revoke.mock.calls.map(([url])=>url))).toEqual(new Set(create.mock.results.map(result=>String(result.value))));
  }finally{create.mockRestore();revoke.mockRestore();}
});

it('shares one conversation pixel lease and lets a deferred image switch and release',async()=>{
  const raster=OUTPUT_RASTERS[0],output=decodeOutput({version:1,blocks:[0,1].map(()=>({type:'image',mimeType:raster.mime,data:raster.data,fields:{}}))},'lease');
  const conversation:Conversation={provider:'codex',id:'lease',trackId:'track',title:null,kind:'codex',state:'idle',updatedAt:0};
  render(<ChatThread hasPendingMessages={false} canContinue={false} stalled={false} cards={{}} conversation={conversation} turns={[{id:'row',author:'agent',text:'',atMs:0,output}]} />);
  await screen.findByRole('img');expect(screen.getAllByRole('img')).toHaveLength(1);
  const first=screen.getByRole('img').getAttribute('src');fireEvent.click(screen.getByRole('button',{name:'Load image · deferred'}));
  await waitFor(()=>expect(screen.getByRole('img').getAttribute('src')).not.toBe(first));
  expect(screen.getAllByRole('img')).toHaveLength(1);
  fireEvent.click(screen.getByRole('button',{name:'Release image'}));
  await waitFor(()=>expect(screen.getAllByRole('img')).toHaveLength(1));
});

it('makes zero implicit image or GitHub preview requests for present and malformed typed output',async()=>{
  await page.viewport(1280,800);
  expect(matchMedia('(hover: hover) and (pointer: fine)').matches).toBe(true);
  const probe='/acp-output-network-probe.png';
  performance.clearResourceTimings();
  const rawUrl=vi.fn(()=>probe),read=vi.fn(async(target:Parameters<typeof githubPreviewOperation>[0])=>{await fetch(githubPreviewOperation(target).path);throw new Error('unexpected preview');});
  const client=new QueryClient({defaultOptions:{queries:{retry:false}}});
  const markdown=`![local](${probe}) ![remote](https://attack.invalid/output.png) ![data](data:image/png;base64,AAAA) ![blob](blob:https://attack.invalid/id) [Issue](https://github.com/keanji-x/neige-calm/issues/3699)`;
  const paint=(output?:ReturnType<typeof decodeOutput>)=><QueryClientProvider client={client}><GitHubPreviewProvider port={{read}}><Reply text={markdown} output={output} imageFiles={{root:'/',files:{rawUrl}}} /></GitHubPreviewProvider></QueryClientProvider>;
  const view=render(paint(decodeOutput({version:1,blocks:[{type:'text',text:markdown,fields:{}}]},'typed')));
  const issue=screen.getByRole('link',{name:'Issue'});issue.focus();await page.getByRole('link',{name:'Issue'}).hover();
  await new Promise(resolve=>setTimeout(resolve,800));
  expect(performance.getEntriesByType('resource').filter(entry=>entry.name.includes('acp-output-network-probe') || entry.name.includes('attack.invalid') || entry.name.includes('/api/github/preview'))).toEqual([]);
  expect(read).not.toHaveBeenCalled();expect(rawUrl).not.toHaveBeenCalled();expect(screen.queryByRole('img')).toBeNull();
  for(const value of [null,{version:2,blocks:[]},{version:1,blocks:[{type:'image'}]}]){view.rerender(paint(decodeOutput(value,'bad')));expect(screen.getByText('Output unavailable')).toBeTruthy();}
  expect(performance.getEntriesByType('resource').filter(entry=>entry.name.includes('acp-output-network-probe') || entry.name.includes('attack.invalid') || entry.name.includes('/api/github/preview'))).toEqual([]);
  view.unmount();client.clear();
});

it('does not revive an old asynchronous decode after replacement or erase',async()=>{
  let release:()=>void=()=>undefined;
  const pending=new Promise<void>(resolve=>{release=resolve;});
  const decode=vi.spyOn(HTMLImageElement.prototype,'decode').mockImplementationOnce(()=>pending);
  const raster=OUTPUT_RASTERS[0];
  const output=(id:string,blocks:unknown[])=>decodeOutput({version:1,blocks},id);
  const image={type:'image',mimeType:raster.mime,data:raster.data,fields:{}};
  try{
    const view=render(<Reply text="" imageFiles={null} output={output('old',[image])} />);
    await waitFor(()=>expect(decode).toHaveBeenCalledTimes(1));
    decode.mockRestore();
    view.rerender(<Reply text="" imageFiles={null} output={output('new',[image])} />);
    await screen.findByRole('img');const current=screen.getByRole('img').getAttribute('src');
    release();await new Promise(resolve=>setTimeout(resolve,20));
    expect(screen.getByRole('img').getAttribute('src')).toBe(current);
    view.rerender(<Reply text="" imageFiles={null} output={output('erase',[])} />);
    await waitFor(()=>expect(screen.queryByRole('img')).toBeNull());
  }finally{release();decode.mockRestore();}
});

it('rejects unsafe raster headers, animation, MIME mismatch and truncated bytes before browser decode',async()=>{
  const decode=vi.spyOn(HTMLImageElement.prototype,'decode');
  const png=Uint8Array.from(atob(OUTPUT_RASTERS[0].data),c=>c.charCodeAt(0));
  const huge=png.slice();new DataView(huge.buffer).setUint32(16,4097);
  const area=png.slice();new DataView(area.buffer).setUint32(16,4001);new DataView(area.buffer).setUint32(20,4000);
  const encode=(bytes:Uint8Array)=>btoa(String.fromCharCode(...bytes));
  const animated=new Uint8Array(png.length+20);animated.set(png.subarray(0,33));
  new DataView(animated.buffer).setUint32(33,8);animated.set([97,99,84,76],37);animated.set(png.subarray(33),53);
  const cases=[...OUTPUT_ANIMATIONS,{data:encode(area),mimeType:'image/png'},{data:encode(huge),mimeType:'image/png'},{data:encode(animated),mimeType:'image/png'},
    {data:OUTPUT_RASTERS[0].data,mimeType:'image/jpeg'},{data:encode(png.subarray(0,30)),mimeType:'image/png'},
    {data:'AB==',mimeType:'image/png'},{data:'PHN2Zy8+',mimeType:'image/svg+xml'}];
  try{
    const view=render(<Reply text="" imageFiles={null} output={decodeOutput({version:1,blocks:[]},'empty')} />);
    for(const [index,value] of cases.entries()){
      view.rerender(<Reply text="" imageFiles={null} output={decodeOutput({version:1,blocks:[{type:'image',...value,fields:{}}]},String(index))} />);
      await screen.findByText(/Unsupported image/);expect(screen.queryByRole('img')).toBeNull();
    }
    expect(decode).not.toHaveBeenCalled();
  }finally{decode.mockRestore();}
});

it('keeps its own in-flight decode when the loading image is clicked again',async()=>{
  const candidate:unknown=Object.getOwnPropertyDescriptor(HTMLImageElement.prototype,'decode')?.value;
  if(typeof candidate!=='function')throw new Error('Browser image decode is required');
  const nativeDecode=candidate as (this:HTMLImageElement)=>Promise<void>;
  let release:()=>void=()=>undefined;
  const gate=new Promise<void>(resolve=>{release=resolve;});
  const decode=vi.spyOn(HTMLImageElement.prototype,'decode').mockImplementation(function(this:HTMLImageElement){return gate.then(()=>nativeDecode.call(this));});
  const raster=OUTPUT_RASTERS[0];
  try{
    render(<Reply text="" imageFiles={null} output={decodeOutput({version:1,blocks:[{type:'image',mimeType:raster.mime,data:raster.data,fields:{}}]},'self-selection')} />);
    await waitFor(()=>expect(decode).toHaveBeenCalledTimes(1));
    fireEvent.click(screen.getByRole('button',{name:'Load image · deferred'}));
    release();
    await screen.findByRole('img',{name:'Output image'});
  }finally{release();decode.mockRestore();}
});

it('loads a local screenshot in streamed and stored replies and fits a narrow conversation', async () => {
  const screenshot = URL.createObjectURL(new Blob([
    '<svg xmlns="http://www.w3.org/2000/svg" width="1200" height="800">'
    + '<rect width="1200" height="800" fill="#eef2f6"/>'
    + '<rect width="1200" height="70" fill="#25344b"/>'
    + '<text x="60" y="160" font-size="40">Screenshot preview</text></svg>',
  ], { type: 'image/svg+xml' }));
  try {
    const conversation: Conversation = { provider: 'codex' as const, id: 'c1', trackId: 'w1', title: 'Planner', kind: 'codex', state: 'running', updatedAt: 0 };
    const imageFiles = { root: '/work/track', files: { rawUrl: () => screenshot } };
    const reply = (id: string, text: string): ConversationTurn => ({ id, author: 'agent', text, atMs: 1 });
    const props = { conversation, imageFiles, canContinue: false, cards: {}, stalled: false };
    const { container, rerender } = render(<div style={{ inlineSize: 320 }}>
      <ChatThread hasPendingMessages={false} {...props} turns={[reply('live', 'Here is the screenshot.')]} />
    </div>);
    const text = 'Here is the screenshot.\n\n![Screenshot](/work/track/screenshots/page.svg)';
    rerender(<div style={{ inlineSize: 320 }}>
      <ChatThread hasPendingMessages={false} {...props} turns={[reply('live', text)]} />
    </div>);
    await waitFor(() => expect(container.querySelector<HTMLImageElement>('img')?.naturalWidth).toBe(1200));
    const image = screen.getByRole('img', { name: 'Screenshot' });
    expect(image.getBoundingClientRect().width).toBeGreaterThan(0);
    expect(image.getBoundingClientRect().width).toBeLessThanOrEqual(320);
    expect(container.firstElementChild?.scrollWidth).toBeLessThanOrEqual(320);
    rerender(<div style={{ inlineSize: 320 }}>
      <ChatThread hasPendingMessages={false} {...props} turns={[reply('stored', text)]} />
    </div>);
    await waitFor(() => expect(container.querySelector<HTMLImageElement>('img')?.naturalWidth).toBe(1200));
  } finally {
    URL.revokeObjectURL(screenshot);
  }
});


it('opens a reply image in the Lightbox on click, dismisses only the Lightbox on Escape inside a Drawer, and leaves linked images as links', async () => {
  const screenshot = URL.createObjectURL(new Blob([
    '<svg xmlns="http://www.w3.org/2000/svg" width="1200" height="800">'
    + '<rect width="1200" height="800" fill="#eef2f6"/>'
    + '<text x="60" y="160" font-size="40">Architecture diagram</text></svg>',
  ], { type: 'image/svg+xml' }));
  const onCloseDrawer = vi.fn();
  try {
    const imageFiles = { root: '/work/track', files: { rawUrl: () => screenshot } };
    render(
      <Drawer open title="Planner" onClose={onCloseDrawer}>
        <Reply
          text={'![Architecture diagram](/work/track/diagram.svg)\n\n![](/work/track/unnamed.svg)\n\n[![Linked badge](/work/track/diagram.svg)](https://example.com/docs)'}
          imageFiles={imageFiles}
        />
      </Drawer>,
    );

    const linked = screen.getByRole('link', { name: 'Linked badge' });
    expect(linked.getAttribute('href')).toBe('https://example.com/docs');
    expect(linked.querySelector('button')).toBeNull();
    expect(document.querySelector('dialog')).toBeNull();

    const unnamedTrigger = screen.getByRole('button', { name: 'Open Thumbnail' });
    expect(unnamedTrigger.getAttribute('aria-haspopup')).toBe('dialog');

    const trigger = screen.getByRole('button', { name: 'Architecture diagram' });
    expect(trigger.getAttribute('aria-label')).toBeNull();
    expect(trigger.getAttribute('role')).toBeNull();
    expect(trigger.getAttribute('tabindex')).toBeNull();
    trigger.focus();
    fireEvent.click(trigger);

    const dialog = await screen.findByRole('dialog', { name: 'Architecture diagram' });
    expect(within(dialog).getByRole('img', { name: 'Architecture diagram' }).getAttribute('src')).toBe(screenshot);
    expect(within(dialog).getByRole('button', { name: 'Zoom' }).getAttribute('aria-pressed')).toBe('false');

    fireEvent.keyDown(document, { key: 'Escape', bubbles: true, cancelable: true });
    await waitFor(() => expect(screen.queryByRole('dialog', { name: 'Architecture diagram' })).toBeNull());
    expect(onCloseDrawer).not.toHaveBeenCalled();
    expect(document.activeElement).toBe(trigger);
  } finally {
    URL.revokeObjectURL(screenshot);
  }
});


it('keeps continuous text runs and repeated images in their original presentation order', async () => {
  const raster = OUTPUT_RASTERS[0];
  const output = decodeOutput({version:1,blocks:[
    {type:'text',text:'**hel',fields:{},chunk:{meta:{seq:1}}},
    {type:'text',text:'lo**',fields:{},chunk:{meta:{seq:2}}},
    {type:'image',mimeType:raster.mime,data:raster.data,fields:{name:'first'}},
    {type:'text',text:'he',fields:{},chunk:{meta:{seq:3}}},
    {type:'text',text:'llo',fields:{},chunk:{meta:{seq:4}}},
    {type:'image',mimeType:raster.mime,data:raster.data,fields:{name:'second'}},
    {type:'text',text:'tail',fields:{}},
  ]}, 'ordered-runs');
  const {container} = render(<Reply text="compatibility" imageFiles={null} output={output} />);
  await screen.findByRole('img', {name:'first'});
  const paragraphs = [...container.querySelectorAll('p')];
  expect(paragraphs.map(element => element.textContent)).toEqual(['hello', 'hello', 'tail']);
  expect(paragraphs[0].querySelector('strong')?.textContent).toBe('hello');
  const order = [paragraphs[0], screen.getByRole('button', {name:'first'}), paragraphs[1], screen.getByRole('button', {name:'Load image · deferred'}), paragraphs[2]];
  for (let index = 1; index < order.length; index++) {
    expect(order[index - 1].compareDocumentPosition(order[index]) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
  }
  fireEvent.click(screen.getByRole('button', {name:'Load image · deferred'}));
  await screen.findByRole('img', {name:'second'});
  expect(container.querySelectorAll('p')).toHaveLength(3);
});
