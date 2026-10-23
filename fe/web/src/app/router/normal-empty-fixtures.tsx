import { conversationCommands } from '../../features/chat/thread/composer-command.ts';
import { ReportSourcePanel } from '../../features/report/source/public.tsx';
import { ReportSeriesBlock } from '../../features/report/series/public.tsx';
import { InlineTable } from '../../features/report/table/inline.tsx';
import { createUnauthorizedChannel } from '../../../../core/api/unauthorized.ts';
import { ReportReferencePreview } from './report-reference.tsx';
import { TaskRecoveryDetails } from '../../features/report/task/recovery.tsx';
import { LivePlaceholder } from '../../features/report/live/placeholder.tsx';
import { RecordBrowser } from '../../features/report/native/records.tsx';
import { BarChart, MeterChart, TimeSeriesChart, DistributionChart } from '../../ui/data-visualization/public.tsx';
import { Dialog } from '../../ui/dialog/public.tsx';
import { DirectoryBrowser } from '../../ui/directory-browser/public.tsx';
import { FileViewer, type ViewerSlots } from '../../systems/fs-viewers/public.tsx';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
// Test data and component staging; no assertions or screenshot export.
import { render } from '@testing-library/react';
import type { ReactNode } from 'react';
import { ThemeProvider } from '../theme/public.tsx';
import { ChatComposer } from '../../features/chat/thread/public.tsx';
import { useMentionTrigger } from '../../features/chat/thread/mention-trigger.tsx';

export function noop() { return undefined; }
export const ok = (body: unknown) => ({ status: 200, statusText: 'OK', body });
export const area = Object.freeze({ id: 'project-area', name: 'Project', color: '#6574cd', sort: 0, kind: 'user' as const, defaultTemplateId: null, defaultCwd: null, createdAt: 1, updatedAt: 1 });
export const track = Object.freeze({ id: 'project', area_id: 'project-area', title: '新项目', sort: 0, workspace: Object.freeze({ isolation: 'managed', kind: 'managed', path: '/tmp', frozen_at: null }), cwd: '/tmp', pinned_at: null, closed_at: null, created_at: 1, updated_at: 2 });
export function navigationActions() { return { onBack: noop, onNewTrack: noop, onEditArea: noop, onOpenSettings: noop, onCreateArea: noop }; }

export function mount(node: ReactNode, padded = false) {
  const container = document.createElement('div'); container.id = 'root'; document.body.appendChild(container);
  return render(<ThemeProvider storage={{ getItem: () => null, setItem: noop }}><div style={{ height: '100dvh', padding: padded ? 24 : 0, background: 'var(--bg)', color: 'var(--text)', fontFamily: 'var(--font-sans)' }}>{node}</div></ThemeProvider>, { container });
}
export function EmptySearchComposer() {
  const trigger = useMentionTrigger(() => Promise.resolve([]));
  return <ChatComposer commands={conversationCommands({ newConversation: noop })} mentionTrigger={trigger} onSend={noop} />;
}


export function mountReportDataSources() {
  const payload = {source:'neige://plugin/example/data',series:['US:EXAMPLE'],caption:'示例数据源'};
  mount(<div style={{display:'grid',gap:28}}>
    <section><h2 style={{font:'var(--type-ui)',fontWeight:'var(--weight-medium)'}}>表格 · 无行数据</h2><InlineTable payload={{columns:[{key:'name',label:'名称'},{key:'status',label:'状态'}],rows:[]}} /></section>
    <section><h2 style={{font:'var(--type-ui)',fontWeight:'var(--weight-medium)'}}>图表引用 · 此视图未携带数据</h2><ReportSeriesBlock payload={payload} blockId="example" rev={0}/></section>
    <section><h2 style={{font:'var(--type-ui)',fontWeight:'var(--weight-medium)'}}>图表引用 · 等待数据</h2><ReportSeriesBlock payload={payload} blockId="example" rev={0} resolve={()=>({status:'pending',view:'line',field:'close',period:'day',range:'1M',reason:'等待数据源返回'})}/></section>
  </div>,true);
}

export function mountEmptyFileViewer(tab: 'code' | 'diff') {
  const values = new Map<string,unknown>();
  values.set('fs-viewer-nav',{tab,folderPath:'/repo',selectedPath:null,diffSelected:null});
  const slots: ViewerSlots = { get: <T,>(key:string,initial:T|(()=>T)) => values.has(key) ? values.get(key) as T : typeof initial === 'function' ? (initial as ()=>T)() : initial, set: (key,value)=> {values.set(key,value);} };
  const files = { listDirectory: ()=>Promise.resolve({path:'/repo',parent:'/',entries:[]}),readFile:()=>Promise.resolve({path:'/repo',size:0,text:'',truncated:false}),gitStatus:()=>Promise.resolve({repo_root:'/repo',files:[]}),gitDiff:()=>Promise.resolve({path:'',status:'modified',head_text:'',working_text:'',truncated:false}),rawUrl:()=>'' };
  mount(<div style={{height:500}}><FileViewer path="/repo" files={files} slots={slots} theme="light" /></div>,true);
}

export function mountEmptyDirectoryBrowser(filtered: boolean) {
  mount(<Dialog origin={{ kind: 'surface-center' }} open title="选择文件夹" onClose={noop}><DirectoryBrowser initialPath="/work" listDirectory={()=>Promise.resolve({path:'/work',parent:'/',entries:filtered ? [{name:'src',path:'/work/src',isDirectory:true}] : []})} onCancel={noop} onSelect={noop}/></Dialog>);
}

export function mountReportComponentStates() {
  const box = (title:string, node:ReactNode) => <section style={{padding:20,background:'var(--paper)',borderRadius:12,minHeight:115}}><h2 style={{font:'var(--type-ui)',fontWeight:'var(--weight-medium)',marginBottom:16}}>{title}</h2>{node}</section>;
  mount(<div style={{display:'grid',gridTemplateColumns:'1fr 1fr',gap:20}}>
    {box('柱状图 · 无数据',<BarChart label="统计" unit="" points={[]} emptyText="暂无图表数据（示例文案）" />)}
    {box('分布图 · 无数据',<DistributionChart label="分布" unit="" slices={[]} emptyText="暂无图表数据（示例文案）" selected={null} onSelect={noop}/>)}
    {box('时间序列 · 无数据集',<TimeSeriesChart label="趋势" datasets={[{id:'default',label:'趋势',unit:'',style:'line',series:[{id:'value',label:'值',palette:0}],points:[]}]} emptyText="暂无图表数据（示例文案）" selection={{datasetId:'',selected:null,sample:null,readoutOpen:false}} onSelection={noop}/>)}
    {box('额度图 · 无数据',<MeterChart label="额度" unit="" used={null} limit={null} usedLabel="已用" limitLabel="总额" detail="" emptyText="暂无额度数据（示例文案）" tone="neutral"/>)}
    {box('记录视图 · 空记录',<RecordBrowser component={{kind:'records',id:'records',title:'记录',emptyText:'暂无记录（示例文案）',datasets:[{id:'default',label:'默认',items:[]}]}} selection={{datasetId:'default',mode:'list',selectedId:null,disclosures:[]}} onSelection={noop}/>)}
    {box('执行历史 · 无尝试',<TaskRecoveryDetails view={{key:'task',current:null,attempts:[]}} current={undefined} loading={false} loadError={null} onRefresh={noop} openWorker={undefined} openableWorkerIds={new Set()}/>)}
    {box('实时内容 · 等待数据',<LivePlaceholder state="pending" source="数据源" />)}
    {box('实时内容 · 未携带数据',<LivePlaceholder state="detached" source="数据源" />)}
  </div>,true);
}

export function mountMissingReportSources() {
  const source={source_id:'src_2c9e0a1b',provenance:'web_page' as const,origin:{kind:'plugin' as const,plugin_id:'example',tool:'data',args_sha256:'ab',args_canon:'v1',content_id:'example'},title:'示例来源',published_at:'2026-10-19',content_id:'example',body_bytes:12,body_sha256:'cd',captured_at:'2026-10-19T08:00:00Z',quotes:[],body:'示例来源正文。'};
  mount(<div style={{display:'grid',gap:30}}>
    <section><h2 style={{font:'var(--type-ui)',fontWeight:'var(--weight-medium)'}}>来源引用 · 来源不存在</h2><ReportSourcePanel target={{destination:'neige://source/src_2c9e0a1b',sourceId:'src_2c9e0a1b',quoteId:null}} resolution={{status:'missing'}} onRetry={noop}/></section>
    <section><h2 style={{font:'var(--type-ui)',fontWeight:'var(--weight-medium)'}}>来源引用 · 链接无法定位</h2><ReportSourcePanel target={{destination:'neige://source/invalid',sourceId:null,quoteId:null}} resolution={{status:'missing'}} onRetry={noop}/></section>
    <section><h2 style={{font:'var(--type-ui)',fontWeight:'var(--weight-medium)'}}>来源引用 · 引用锚点缺失</h2><ReportSourcePanel target={{destination:'neige://source/src_2c9e0a1b#q1',sourceId:'src_2c9e0a1b',quoteId:'q1'}} resolution={{status:'ok',source}} onRetry={noop} scrollToQuote={false}/></section>
  </div>,true);
}

export function mountEmptyResolvedSeries() {
  const entry = { asset:'US:EXAMPLE',status:'ok' as const,currency:'USD',complete_through:'2026-10-19',n:0,first:['2026-10-19',0] as [string,number],last:['2026-10-19',0] as [string,number],change_pct:null,high:0,low:0,points:[] };
  mount(<div style={{display:'grid',gridTemplateColumns:'1fr 1fr',gap:24}}>{(['line','normalized','bar','candles'] as const).map(view => <section key={view}>
    <h2 style={{font:'var(--type-ui)',fontWeight:'var(--weight-medium)'}}>{view} · 已读取，无数据点</h2>
    <ReportSeriesBlock payload={{source:'neige://plugin/example/data',series:['US:EXAMPLE'],view}} blockId={view} rev={0}
      resolve={()=>({status:'ok',view,field:'close',period:'day',range:'1M',as_of:'2026-10-19',resolved_at:'2026-10-19T08:00:00Z',pinned:false,series:[entry]})}/>
  </section>)}</div>,true);
}

export function mountEmptyReportReference(missingBlock: boolean) {
  const card = {id:'report',track_id:'project',kind:'track-report',title:null,sort:-1,payload:{schemaVersion:3,docRev:0,summary:'',body:'# 报告'},deletable:false,created_at:1,updated_at:2};
  const transport = {send: ()=>Promise.resolve(ok({track,planner_revision: 0, can_reopen:false,can_admin:false,can_close:true,cards:missingBlock ? [card] : [],overlays:[]}))};
  const unauthorized=createUnauthorizedChannel({enqueue:task=>task()});
  mount(<QueryClientProvider client={new QueryClient({defaultOptions:{queries:{retry:false}}})}><ReportReferencePreview transport={transport} unauthorized={unauthorized} target={{trackId:'project',blockId:missingBlock ? 'missing' : null}} onOpenLink={noop} onOpenSource={noop} onOpenFile={noop}/></QueryClientProvider>,true);
}
