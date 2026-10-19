import '../../styles/entry.css';
import emptyStyles from '../../ui/empty-state/empty-state.module.css';
import { noop, ok, area, track, navigationActions, mount, EmptySearchComposer, mountReportDataSources, mountEmptyFileViewer, mountEmptyDirectoryBrowser, mountReportComponentStates, mountMissingReportSources, mountEmptyResolvedSeries, mountEmptyReportReference } from './normal-empty-fixtures.tsx';
import { assertNormalEmptySurface } from './normal-empty-assertions.ts';
import { captureEmptySurface, waitForEmptySurfaceRendering } from './normal-empty-capture.ts';
import { cleanup } from '@testing-library/react';
import { afterEach, expect, it } from 'vitest';
import { page, userEvent } from 'vitest/browser';
import { createUiPreferences } from '../providers/ui-preferences.tsx';
import { renderDailyFixture } from './daily-planner-fixture.tsx';
import { MobilePages } from '../shell/mobile-pages.tsx';
import { MobileTracks } from '../shell/mobile-tracks.tsx';
import { ChatEmpty } from '../../features/chat/thread/public.tsx';
import { ReportDocument } from '../../features/report/document/public.tsx';
import { ReportPreviewBlock } from '../../features/report/preview/public.tsx';
import { CalendarTasks } from '../../features/calendar/public.tsx';

it('catalog report changes without changes', async () => {
  await page.viewport(1440,900);
  renderDailyFixture({ reportBody:'', reply: request => request.path.startsWith('/api/today/report-changes') ? ok({date:'2026-10-03',timezone:'Asia/Shanghai',through_event_id:42,next_cursor:null,changes:[]}) : undefined });
  await page.getByText('Report changes',{exact:true}).click();
  await expect.element(page.getByText('No report changes recorded for visible Tracks on this day.')).toBeVisible();
  await shot('26-today-no-report-changes', ["No report changes recorded for visible Tracks on this day."]);
});
it('catalog historical day without Track', async () => {
  await page.viewport(1440,900);
  renderDailyFixture({ initial:'/?day=2026-10-18' });
  await expect.element(page.getByText('No daily Track was created for this date.')).toBeVisible();
  await shot('27-historical-day-no-track', ["No daily Track was created for this date."]);
});
it('catalog report table and chart source placeholders', async () => {
  await page.viewport(1100,850);
  mountReportDataSources();
  await expect.element(page.getByText('This chart is resolved by the kernel and this view does not carry its data.')).toBeVisible();
  await shot('29-table-and-series-placeholders', ["暂无记录", "暂无图表数据"]);
});
it('catalog missing report reference block', async () => {
  await page.viewport(800,400);
  mountEmptyReportReference(true);
  await expect.element(page.getByText('报告中没有这条引用指向的内容。')).toBeVisible();
  await shot('30-reference-missing-block', []);
});


afterEach(() => { cleanup(); document.body.replaceChildren(); });
async function shot(name: string, expectedTitles: readonly string[]) {
  await waitForEmptySurfaceRendering();
  assertNormalEmptySurface(expectedTitles, emptyStyles);
  await captureEmptySurface(name);
}

it.each([1440, 390])('catalog main report and sidebar %i', async width => {
  await page.viewport(width, 900);
  renderDailyFixture({ initial: '/track/project', reply: request => request.path === '/api/tracks/project' ? ok({ track, can_reopen: false, can_admin: false, can_close: true, cards: [], overlays: [] }) : undefined });
  await expect.element(page.getByRole('heading', { name: '进展会整理在这里' })).toBeVisible();
  await shot('01-report-' + width, ["进展会整理在这里"]);
  if (width === 390) {
    await page.getByRole('button', { name: 'Open conversation history', exact: true }).click();
    await expect.element(page.getByText('当前 Track 还没有对话。')).toBeVisible();
    await shot('12-history-current', ["当前 Track 还没有对话。"]);
    for (const [label, name, message] of [['已置顶','13-history-pinned','还没有已置顶的 Track。'], ['未读','14-history-unread','没有未读的 Track。'], ['未处理','15-history-waiting','没有需要处理的 Track。']]) {
      await page.getByRole('button', { name: label, exact: true }).click();
      await expect.element(page.getByText(message)).toBeVisible();
      await shot(name, [message]);
    }
  }
});
it.each([1440, 390])('catalog Today and conversation %i', async width => {
  await page.viewport(width, 900);
  renderDailyFixture({ reportBody: '', emptyWorkspace: true });
  await expect.element(page.getByRole('heading', { name: '今天还没有记录' })).toBeVisible();
  await shot('02-today-' + width, ["今天还没有记录"]);
  if (width === 1440) await page.getByRole('button', { name: /Conversation Daily Planner conversation/ }).click();
  else {
    await page.getByRole('button', { name: 'Open conversation history', exact: true }).click();
    await page.getByRole('button', { name: '2026-10-04 Daily Planner conversation', exact: true }).click();
  }
  await expect.element(page.getByRole('heading', { name: '聊聊你想做什么' })).toBeVisible();
  await shot('03-conversation-' + width, ["聊聊你想做什么"]);
});
it.each([1440,390])('catalog recipes and plugins %i', async width => {
  await page.viewport(width,900);
  const fixture = renderDailyFixture({ initial: '/recipes', reply: request => request.path === '/api/plugins' ? ok([]) : undefined });
  await expect.element(page.getByRole('heading',{name:'还没有保存的模板'})).toBeVisible();
  await shot('04-recipes-' + width, ["还没有保存的模板"]);
  await fixture.router.navigate({ to: '/settings/plugins' });
  await expect.element(page.getByRole('heading',{name:'还没有添加插件'})).toBeVisible();
  await shot('05-plugins-' + width, ["还没有添加插件"]);
});
it('catalog heading-only report mobile', async () => {
  await page.viewport(390,844);
  mount(<ReportDocument report={{ summary:'', body:'# 概要\n\n# 今日计划', blocks:null }} empty={null} />, true);
  await expect.element(page.getByRole('heading',{name:'进展会整理在这里'})).toBeVisible();
  await shot('06-heading-only-report', ["进展会整理在这里"]);
});
it.each(['areas','tracks'] as const)('catalog mobile navigation %s', async view => {
  await page.viewport(390,844);
  mount(<MobileTracks {...navigationActions()} view={view} areas={view === 'areas' ? [] : [area]} tracksByArea={new Map()} areaId={view === 'areas' ? undefined : area.id} currentTrackId={undefined} onSelectArea={noop} onOpenTrack={noop} isUnread={() => false} readError={null} readLoading={false} onRetryRead={noop} />);
  await expect.element(page.getByText(view === 'areas' ? '还没有 Area' : '这个 Area 还没有 Track')).toBeVisible();
  await shot(view === 'areas' ? '07-mobile-no-areas' : '08-mobile-no-tracks', [view === 'areas' ? '还没有 Area' : '这个 Area 还没有 Track']);
});
it('catalog mobile Pages groups', async () => {
  await page.viewport(390,844);
  mount(<MobilePages {...navigationActions()} areas={[area]} areaId={area.id} tracks={[]} onOpenTrack={noop} />);
  await expect.element(page.getByText('还没有最近浏览的 Page')).toBeVisible(); await shot('09-pages-recent', ["还没有最近浏览的 Page"]);
  await page.getByRole('radio',{name:'Pinned',exact:true}).click();
  await expect.element(page.getByText('还没有置顶的 Page')).toBeVisible(); await shot('10-pages-pinned', ["还没有置顶的 Page"]);
});
it('catalog calendar without plugin', async () => {
  await page.viewport(900,650);
  mount(<CalendarTasks date="2026-10-19" timezone="Asia/Shanghai" month={{ entries:[],loading:false,error:null }} day={{ entries:[],loading:false,error:null }} enabled={false} pending={false} onDateChange={noop} onWindowChange={noop} onRetry={noop} onReload={noop} onSettings={noop} onOpenTrack={noop} onSave={()=>Promise.resolve()} />, true);
  await expect.element(page.getByText('Calendar is temporarily unavailable.')).toBeVisible();
  await shot('16-calendar-unavailable', []);
});
it.each(['code','diff'] as const)('catalog file viewer %s', async tab => {
  await page.viewport(1000,650);
  mountEmptyFileViewer(tab);
  await expect.element(page.getByText(tab === 'code' ? '选择一个文件以查看内容' : '工作区没有未提交的更改')).toBeVisible();
  await expect.element(page.getByRole('heading', { name: '这个文件夹是空的', exact: true })).toBeVisible();
  await shot(tab === 'code' ? '17-files-empty-folder-and-unselected' : '18-files-no-changes', tab === 'code' ? ['这个文件夹是空的','选择一个文件以查看内容'] : ['工作区没有未提交的更改','Select a changed file']);
});
it.each([false,true])('catalog directory browser filtered %s', async filtered => {
  await page.viewport(900,650);
  mountEmptyDirectoryBrowser(filtered);
  if (filtered) {
    await expect.element(page.getByRole('option',{name:'src'})).toBeVisible();
    await page.getByRole('combobox').fill('/work/nonexistent');
  }
  await expect.element(page.getByText(filtered ? 'No matches' : '这个文件夹是空的')).toBeVisible();
  await shot(filtered ? '20-folder-search-no-matches' : '19-folder-picker-empty', [filtered ? 'No matches' : '这个文件夹是空的']);
});
it.each(['mention','command'])('catalog composer search %s', async type => {
  await page.viewport(1000,700);
  mount(<div style={{position:'absolute',left:120,top:430,width:480}}><EmptySearchComposer /></div>);
  const field = page.getByRole('combobox',{name:'Message'});
  await field.click();
  await userEvent.keyboard(type === 'mention' ? '@nonexistent' : '/nonexistent');
  await expect.element(page.getByText(type === 'mention' ? 'No matches' : 'No command by that name')).toBeVisible();
  expect(page.getByRole('option').elements()).toHaveLength(0);
  expect(field.element().getAttribute('aria-activedescendant')).toBeNull();
  await userEvent.keyboard('{ArrowDown}');
  expect(field.element().textContent).toBe(type === 'mention' ? '@nonexistent' : '/nonexistent');
  await shot(type === 'mention' ? '21-mention-no-matches' : '22-command-no-matches', [type === 'mention' ? 'No matches' : 'No command by that name']);
});
it('catalog report data and history component states', async () => {
  await page.viewport(1100,950);
  mountReportComponentStates();
  await expect.element(page.getByText('No attempts yet')).toBeVisible();
  await shot('23-report-component-states', ["暂无数据", "暂无记录", "No attempts yet", "暂无实时数据"]);
});
it('catalog waiting conversation', async () => {
  await page.viewport(650,500);
  mount(<div style={{display:'flex',height:420,background:'var(--paper)',borderRadius:12}}><ChatEmpty working /></div>,true);
  await expect.element(page.getByRole('heading',{name:'正在整理你的想法'})).toBeVisible();
  await shot('24-conversation-working', []);
});
it('catalog report reference without document', async () => {
  await page.viewport(800,400);
  mountEmptyReportReference(false);
  await expect.element(page.getByText('这份报告暂无内容。')).toBeVisible();
  await shot('25-reference-empty', ["这份报告暂无内容。"]);
});


it('catalog missing report sources', async () => {
  await page.viewport(1100,1100);
  mountMissingReportSources();
  await expect.element(page.getByRole('heading',{name:'引用锚点缺失',exact:true})).toBeVisible();
  await shot('31-source-missing-states', []);
});

it('catalog confirmed empty series in every chart presentation', async () => {
  await page.viewport(1100,1050);
  mountEmptyResolvedSeries();
  await expect.element(page.getByRole('heading',{name:'暂无图表数据'}).first()).toBeVisible();
  await shot('32-series-no-points', ["暂无图表数据"]);
});

it('catalog body patch without net changes', async () => {
  await page.viewport(1440,900);
  renderDailyFixture({reply:request=>request.path.startsWith('/api/today/report-changes') ? ok({date:'2026-10-03',timezone:'Asia/Shanghai',through_event_id:2,next_cursor:null,changes:[{track_id:'project',track_title:'新项目',area_id:'project-area',area_name:'Project',edit_count:2,first_event_id:1,last_event_id:2,summary_before:'之前摘要',summary_after:'当前摘要',patch:'',patch_truncated:false}]}) : undefined});
  await page.getByText('Report changes',{exact:true}).click();
  await page.getByRole('heading',{name:/新项目.*2 report edits/}).click();
  await page.getByRole('heading',{name:'Body changes',exact:true}).click();
  await expect.element(page.getByRole('heading',{name:'正文没有净变化',exact:true})).toBeVisible();
  await shot('33-body-no-changes', ["正文没有净变化"]);
});

it('catalog confirmed empty desktop shortcut groups', async () => {
  await page.viewport(1440,1000);
  const preferences = createUiPreferences();
  for (const id of ['pinned','unread','waiting','running'] as const) {
    preferences.setSidebarGroupVisible(id,true);
    preferences.setSidebarGroupExpanded(id,true);
  }
  renderDailyFixture({emptyWorkspace:true,reportBody:'',uiPreferences:preferences});
  await expect.element(page.getByRole('heading',{name:'没有运行中的 Track。',exact:true})).toBeVisible();
  await shot('34-desktop-empty-shortcuts', ["没有未读的 Track。", "没有运行中的 Track。"]);
});

it('catalog a surface without live previews', async () => {
  await page.viewport(1000,650);
  mount(<ReportPreviewBlock payload={{key:'example',title:'示例预览',height:300}}/>,true);
  await expect.element(page.getByRole('heading',{name:'暂无预览内容',exact:true})).toBeVisible();
  await shot('35-preview-no-content', ["暂无预览内容"]);
});
