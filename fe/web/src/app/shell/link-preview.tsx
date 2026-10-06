import { createRoot } from 'react-dom/client';
import { useEffect, useMemo } from 'react';

import type { WorkspaceFilePort } from '../../../../core/domain/fs.ts';
import { trackReportLinkUrl, type TrackReport } from '../../../../core/domain/report.ts';
import { ReportDocument } from '../../features/report/document/public.tsx';
import { useState } from '../../ui/state/public.ts';
import styles from './link-preview.module.css';

function exampleNotes() { return '# 把链接变成一个可以停留的地方\n\n悬停只是快速看一眼。预览出现后就可以移入阅读和点击；移到别处时卡片自动消失。\n\n## 进入卡片之后\n\n- 在卡片里滚动长文\n- 鼠标沿着边缘移入卡片，点击里面的链接\n- 移到卡片外自动收起，也可以点击外部或按 Esc\n\n## 文件中的链接\n\n继续查看 [实现示例](./example.ts)，相对路径仍以当前文件为准。\n\n' + Array.from({ length: 12 }, (_, index) => `### 阅读段落 ${index + 1}\n\n卡片有自己的滚动区域，长内容不会把页面推开。标题始终留在顶部，内容在卡片里滚动。\n\n`).join(''); }
const code = 'type PreviewState = "closed" | "waiting" | "open";\n\n// 预览打开后即可交互，离开后自动收起。\nfunction openPreview() {\n  return "open";\n}\n';
const image = '<svg xmlns="http://www.w3.org/2000/svg" width="800" height="480" viewBox="0 0 800 480"><rect width="800" height="480" fill="#eef0f7"/><circle cx="650" cy="110" r="70" fill="#d4bfea"/><path d="M0 430L180 200L370 400L570 180L800 430V480H0Z" fill="#8a94bd"/><path d="M0 460L230 300L420 450L650 300L800 430V480H0Z" fill="#586a94"/><text x="40" y="70" font-family="sans-serif" font-size="26" fill="#344260">NEIGE · 图片预览</text></svg>';
const files: WorkspaceFilePort = Object.freeze({
  readFile: async (path: string) => {
    await new Promise((resolve) => { setTimeout(resolve, 220); });
    const text = path === 'docs/notes.md' ? exampleNotes() : path === 'docs/example.ts' ? code : null;
    if (text === null) throw new Error('Example file not found');
    return { path, text, size: text.length, truncated: false };
  },
  rawUrl: () => `data:image/svg+xml;charset=utf-8,${encodeURIComponent(image)}`,
});
function exampleReport(): TrackReport { return Object.freeze({ summary: '', body: '', blocks: Object.freeze([
  Object.freeze({ id: 'intro', kind: 'prose' as const, payload: Object.freeze({ markdown:
    '# 不离开正文，也能把内容看完\n\n把鼠标停在下面任意链接上。停留一小会即可预览。移入卡片可以阅读和点击，移到别处自动收起。\n\n## 工作区文件\n\n先试试 [长篇 Markdown](./docs/notes.md)，再看 [代码文件](./docs/example.ts)。内容可以在卡片里滚动，原来的页面留在原处。\n\n## 图片\n\n[一张风景图片](./assets/landscape.svg) 和 ![图片节点](./assets/landscape.svg) 都可以预览。\n\n## 网络链接\n\n悬停 [外部网页](https://example.com) 或 [外部图片](https://www.w3.org/Icons/w3c_home.svg)。点击卡片内的加载按钮后才访问外部地址。\n\n## 内部引用\n\n悬停 [交互说明](' + trackReportLinkUrl('link-preview') + '#guide)，查看同一份正文中的内容。\n\n## 失败也有出口\n\n[不存在的文件](./missing.txt) 会显示重试入口。外部网站拒绝嵌入时，可以直接打开原网页。'
  }) }),
  Object.freeze({ id: 'guide', kind: 'prose' as const, payload: Object.freeze({ markdown:
    '# 交互说明\n\n**打开**：悬停片刻即可查看，不需要等待或切换额外状态。\n\n**移入**：沿着卡片边缘移动进去，可以停留、阅读和点击。\n\n**阅读**：鼠标移入卡片，滚动长文；链接仍可以点击。\n\n**收起**：鼠标移到链接和卡片以外时自动消失，也可以点击外部或按 Esc。\n\n**键盘**：Tab 选中链接，按 ↓ 移入预览内容；Tab 进入卡片，移开焦点后自动收起。'
  }) }),
]) }); }

function Preview() {
  const report = useMemo(exampleReport, []);
  const [theme, setTheme] = useState<'light' | 'dark'>('light');
  const [notice, setNotice] = useState('');
  useEffect(() => { document.documentElement.dataset.theme = theme; }, [theme]);
  return <main className={styles.page}>
    <header className={styles.header}>
      <div><p className={styles.eyebrow}>NEIGE · 链接预览</p><h1>悬停，停留，继续阅读。</h1><p className={styles.subtitle}>卡片优先放在正文旁边 · 沿路径移入交互 · 移到别处自动收起</p></div>
      <button type="button" className={styles.theme} onClick={() => setTheme(theme === 'light' ? 'dark' : 'light')}>{theme === 'light' ? '深色模式' : '浅色模式'}</button>
    </header>
    <div className={styles.document}>
      <ReportDocument report={report} empty={null} fileRoot="/preview" linkPreview={{ files, trackId: 'link-preview', report }}
        onOpenFileLink={({ path }) => setNotice(`已点击打开文件：${path}。此预览页使用示例文件，正式页面会在工作区中打开。`)}
        onOpenLink={() => { document.getElementById('guide')?.scrollIntoView({ behavior: 'smooth' }); }} />
    </div>
    <p role="status" className={styles.notice}>{notice || '这是生产组件组成的独立交互预览，使用示例文件。'}</p>
  </main>;
}
createRoot(document.getElementById('root')!).render(<Preview />);
