# Frontend end-to-end tests

These tests require the real backend stack at `http://127.0.0.1:4041` and Node.js 22. Keep
`CALM_CODEX_HOST_BIN=/bin/true` in the stack environment. Start the stack outside this command,
then run `npm run e2e` from `fe/`; Playwright starts the Vite frontend on port 5180. Override the
backend with `FE_API_PROXY_TARGET` or the frontend port with `FE_DEV_PORT` when necessary.
