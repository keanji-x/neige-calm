// OpenCode v2.1.3 native plugin: no package installation or provider process.env mutation.
import { execFile } from "node:child_process";
import { writeFile, rm } from "node:fs/promises";

export default {
  id: "neige.workspace-environment",
  async setup(context) {
    const { executable, args, ready } = context.options;
    const hook = await context.shell.hook("create.before", async (invocation) => {
      try {
        const text = await new Promise((resolve, reject) => {
          execFile(executable, args, {
            cwd: invocation.cwd,
            env: invocation.env,
            encoding: "utf8",
            maxBuffer: 4 * 1024 * 1024,
          }, (error, stdout) => error ? reject(error) : resolve(stdout));
        });
        const environment = JSON.parse(text);
        if (environment === null || Array.isArray(environment) || typeof environment !== "object" ||
            Object.values(environment).some((value) => typeof value !== "string")) {
          throw new Error("Invalid workspace environment");
        }
        invocation.env = environment;
      } catch {
        // Child errors may contain captured stdout/stderr, including project secrets.
        throw new Error("Workspace shell environment loading failed");
      }
    });
    // Native catalog activation awaits setup. Do not claim readiness before registration.
    await writeFile(ready, "ready", { mode: 0o600 });
    return async () => {
      await rm(ready, { force: true });
      await hook.dispose();
    };
  },
};
