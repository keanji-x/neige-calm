import { build, type Plugin } from 'vite';
import { fileURLToPath } from 'node:url';

// A classic bundle works inside the existing opaque-origin Report sandbox.
// No sandbox permissions or development-server CORS policy need changing.
export function portfolioChartBundle(): Plugin {
  let bundle: Promise<string> | undefined;
  const root = fileURLToPath(new URL('.', import.meta.url));
  function compile() {
    return bundle ??= build({
      configFile: false, root, mode: 'production', logLevel: 'silent',
      oxc: { jsx: { development: false } },
      define: { 'process.env.NODE_ENV': JSON.stringify('production') },
      build: { write: false, minify: true, sourcemap: false,
        lib: { entry: `${root}src/charts/main.tsx`, name: 'PortfolioCharts', formats: ['iife'] },
      },
    }).then(result => {
      const output = (Array.isArray(result) ? result[0] : result);
      if (!('output' in output)) throw new Error('Expected chart bundle output');
      const chunk = output.output.find(item => item.type === 'chunk');
      if (!chunk || chunk.type !== 'chunk') throw new Error('Missing chart bundle');
      return chunk.code;
    }).catch(error => { bundle = undefined; throw error; });
  }
  return {
    name: 'portfolio-chart-bundle',
    transformIndexHtml: {
      order: 'post',
      handler(html, context) {
        if (!context.filename.endsWith('/portfolio-demo.html')) return;
        // The sandbox runs the classic bundle only; development HMR modules
        // cannot load from its opaque origin and are unnecessary for this frame.
        // Vite adds crossorigin to built stylesheets. This iframe has an
        // opaque origin, so that turns an ordinary CSS load into a failing
        // CORS request. Remove it only from this figure's stylesheet links;
        // keep the actual application HTML and iframe sandbox unchanged.
        const figureHtml = html.replace(/<script\b[^>]*>[\s\S]*?<\/script>/g, '')
          .replace(/<link\b[^>]*>/g, tag => /\brel=["']stylesheet["']/.test(tag)
            ? tag.replace(/\s+crossorigin(?:=(?:"[^"]*"|'[^']*'|[^\s>]+))?/g, '') : tag);
        return { html: figureHtml,
          tags: [{ tag: 'script', attrs: { defer: true, src: '/next/portfolio-charts.js' }, injectTo: 'head' }],
        };
      },
    },
    configureServer(server) {
      server.middlewares.use((request, response, next) => {
        if (!['/next/portfolio-charts.js', '/portfolio-charts.js'].includes(request.url?.split('?')[0] ?? '')) { next(); return; }
        void compile().then(code => { response.setHeader('Content-Type', 'application/javascript'); response.end(code); })
          .catch(next);
      });
      server.watcher.on('change', path => {
        if (path.includes('/src/charts/') || /\/src\/portfolio-[^/]+\.(ts|json)$/.test(path)) {
          bundle = undefined;
          server.ws.send({ type: 'full-reload' });
        }
      });
    },
    async generateBundle() {
      this.emitFile({ type: 'asset', fileName: 'portfolio-charts.js', source: await compile() });
    },
  };
}
