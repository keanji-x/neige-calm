/** Two explicit prerecorded outcomes. No user input becomes HTML or executable code. */
export function demoPortfolio(revised: boolean): string {
  const title = revised ? '我的第一个作品' : '我的作品集';
  return `<!doctype html><html lang="zh-CN"><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1">
<title>${title} · 演示页面</title><style>
*{box-sizing:border-box}body{margin:0;background:#f5f6f1;color:#29413c;font:16px/1.6 system-ui,sans-serif}main{max-width:980px;margin:auto;padding:40px 28px}small{letter-spacing:.12em;color:#61756d}h1{font-size:36px;font-weight:500;margin:12px 0}p{color:#61756d}.grid{display:grid;grid-template-columns:repeat(auto-fit,minmax(180px,1fr));gap:20px;margin-top:28px}article{background:white;border-radius:14px;overflow:hidden}svg{width:100%;display:block}article h2,article p{padding:0 16px}h2{font-size:18px;font-weight:500}footer{font-size:12px;color:#61756d;margin-top:24px}
</style><main><small>NEIGE / YOUR FIRST PROJECT</small><h1>${title}</h1><p>从一句想法开始，做出看得见的成果。</p><div class="grid">
${[['山间记录', '#dce8d7', '#86a58c'], ['周末去海边', '#f2dfc9', '#d0a57e'], ['城市漫游', '#dce5ed', '#8a9fac']].map(([name, sky, shape]) => `<article><svg viewBox="0 0 280 160" role="img" aria-label="${name}封面"><rect width="280" height="160" fill="${sky}"/><circle cx="210" cy="42" r="22" fill="#fff2c9"/><path d="M0 160L90 55L155 125L205 84L280 160Z" fill="${shape}"/></svg><h2>${name}</h2><p>为生活中的小灵感留一个位置。</p></article>`).join('')}
</div><footer>演示成果 · 这是预置页面，没有调用 AI，也没有写入你的文件。</footer></main></html>`;
}
