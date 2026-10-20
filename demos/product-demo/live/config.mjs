import fs from "node:fs";
import path from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import { createRequire } from "node:module";
export async function configure(fePath, port = 4187) {
  if (!fePath)
    throw Error("Provide the Neige fe directory as an explicit argument");
  const fe = path.resolve(fePath),
    root = path.dirname(fileURLToPath(import.meta.url));
  const link = path.join(root, "node_modules"),
    modules = path.join(fe, "node_modules");
  if (
    fs.existsSync(link) &&
    fs.lstatSync(link).isSymbolicLink() &&
    fs.realpathSync(link) !== fs.realpathSync(modules)
  )
    fs.unlinkSync(link);
  if (!fs.existsSync(link)) fs.symlinkSync(modules, link, "dir");
  const require = createRequire(path.join(fe, "package.json"));
  const vite = await import(pathToFileURL(require.resolve("vite")).href);
  const react = (
    await import(pathToFileURL(require.resolve("@vitejs/plugin-react")).href)
  ).default;
  const { markdownRuntimeAliases } = await import(
    pathToFileURL(
      path.join(fe, "tools/architecture/markdown-runtime-resolution.ts"),
    ).href
  );
  const aliases = {
    "@neige": path.join(fe, "web/src"),
    "@core": path.join(fe, "core"),
    react: path.join(modules, "react"),
    "react-dom": path.join(modules, "react-dom"),
  };
  return {
    vite,
    config: {
      configFile: false,
      root,
      publicDir: path.join(root, "public"),
      cacheDir: path.resolve(root, "../output/live-vite-cache-" + port),
      plugins: [react()],
      resolve: {
        dedupe: ["react", "react-dom"],
        alias: [
          ...markdownRuntimeAliases(),
          ...Object.entries(aliases).map(([find, replacement]) => ({
            find,
            replacement,
          })),
        ],
      },
      define: {
        __NC_VERSION__: JSON.stringify("demo"),
        __NC_BUILD__: JSON.stringify("frontend-director"),
        __NC_BUNDLED__: "false",
      },
      server: {
        host: "127.0.0.1",
        port,
        strictPort: true,
        fs: { allow: [fe, path.resolve(root, "..")] },
      },
      build: {
        outDir: path.resolve(root, "../output/live-dist"),
        emptyOutDir: true,
        sourcemap: false,
        rollupOptions: {
          input: {
            index: path.join(root, "index.html"),
            app: path.join(root, "app.html"),
          },
        },
      },
    },
  };
}
