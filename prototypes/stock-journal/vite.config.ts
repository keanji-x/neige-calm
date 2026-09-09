import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import { fileURLToPath } from "node:url";
import connection from './connection.json' with { type: 'json' };

const dependency = (name: string) => fileURLToPath(new URL(`../../fe/node_modules/${name}`, import.meta.url));

export default defineConfig({
  base: '/next/',
  plugins: [react()],
  server: { proxy: { '/api': { target: connection.backend, changeOrigin: true } } },
  preview: { proxy: { '/api': { target: connection.backend, changeOrigin: true } } },
  build: { rollupOptions: { input: {
    app: fileURLToPath(new URL('./index.html', import.meta.url)),
  } } },
  resolve: { alias: {
    react: dependency('react'),
    'react-dom': dependency('react-dom'),
    '@tanstack/react-query': dependency('@tanstack/react-query'),
    '@tanstack/react-router': dependency('@tanstack/react-router'),
  } },
  define: { __NC_VERSION__: JSON.stringify('native-report-preview'), __NC_BUILD__: JSON.stringify('demo') },
});
