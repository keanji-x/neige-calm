import { configure } from "./config.mjs";
const { vite, config } = await configure(process.argv[2]);
await vite.build(config);
console.log("Standalone frontend demo built in output/live-dist");
