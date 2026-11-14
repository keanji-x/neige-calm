import { configure } from "./config.mjs";
const port = process.argv[3] === undefined ? 4187 : Number(process.argv[3]);
if (!Number.isInteger(port) || port < 1024 || port > 65535)
  throw Error("Invalid preview port");
const { vite, config } = await configure(process.argv[2], port);
const server = await vite.createServer(config);
await server.listen();
console.log("Live frontend demo: http://127.0.0.1:" + port);
