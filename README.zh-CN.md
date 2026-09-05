<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="docs/assets/neige-mark-dark.svg">
    <img src="fe/web/src/ui/brand/neige-mark.svg" width="104" alt="">
  </picture>
</p>

<h1 align="center">Neige Calm</h1>

<p align="center"><strong>让 Agent 持续工作，同时让工作本身始终清晰、可信、可接续。</strong></p>

<p align="center">
  <a href="README.md">English</a> · 简体中文
</p>

Neige Calm 是一个本地优先、以 Agent 为原生执行主体的工作空间，面向那些不会随着一次聊天结束而结束的工作。你可以用 **Area** 组织长期上下文，围绕一项意图创建 **Track**，让规划 Agent 协调 Worker，并把当前成果持续沉淀在可检查的 **Report** 中。

> [!IMPORTANT]
> Neige Calm 仍处于早期、以源码为主的快速开发阶段，尚无稳定版本；API、界面流程、存储契约和术语仍可能变化。目前的开发路径主要面向 Linux。

## 为什么是 Neige Calm？

多数 AI 工具围绕文件夹、编辑器或聊天会话组织工作；Neige Calm 围绕工作本身组织系统。

它并不试图替代编辑器。需要亲自操作时继续使用编辑器；需要把目标交出去、监督多个 Agent 或 Workspace，并在稍后回来时无需重新翻聊天记录还原状态时，使用 Neige Calm。

```text
Area —— 长期存在的上下文
└── Track —— 一条聚焦或持续演进的工作主线
    ├── Conversation —— 意图、反馈与决策
    ├── Tasks —— 可执行计划
    ├── Workers —— Codex、Claude 或终端执行
    ├── Workspace —— 内核托管或用户附加的文件空间
    └── Report —— 持久保存的当前成果
```

Track 可以只完成一次，例如修复一个 issue；也可以经历多轮活动，例如长期维护一家公司投资观点。Session 和 Worker 可以更替，但 Track 会保留自己的身份、历史、证据与结果。

## 当前已经具备的能力

- **Area 与 Track**：把长期上下文和具体工作主线分开。
- **规划与执行**：根 Agent 规划任务、调度 Worker、响应执行结果，并推动从 draft 到 review 的类型化生命周期。
- **持久 Report**：使用稳定 block ID 与 revision 的文档，支持正文、任务、表格、K 线图、沙箱 App 视图，以及在 Track 内打开 Workspace 文件的链接。
- **隔离 Workspace**：可附加已有目录，也可由内核为 Track 创建并管理工作空间。
- **受治理的执行**：由内核强制实施 Agent 写入和副作用的角色、作用域、生命周期、评审与 Gate 边界。
- **桌面任务监督**：查看任务计数和状态详情，从通知中心进入需要处理的对话或 Worker，并在卡片上分别查看 Worker 与验证命令的工作目录。Settings 支持调整每个 Track 的默认任务并发数。
- **Today**：跨 Track 展示等待/运行状态，通过独立对话维护每日进展 Report。
- **可扩展工具**：在 Settings 添加远程 MCP 服务器或服务器本地目录中的插件，并启停、配置、重启或删除。插件可提供工具、Template、Connector、Overlay 和沙箱 UI 资源。
- **Track Recipe**：创建、编辑和删除可复用的 Markdown 计划，在新建 Track 时选择使用；内置 Template 仍为只读。
- **可恢复执行**：持久化 Event、Session、Operation 与 Supervisor 状态，面向重试和进程替换设计。

本地可执行插件以服务进程的操作系统身份运行，属于可信代码；浏览器 UI 沙箱不会隔离原生插件代码。安装前请阅读[插件宿主安全说明](docs/plugin-security.md)。

具体界面入口与限制见英文版[使用指南](docs/using-neige-calm.md)，任务块的编写方式见 [Recipe 正文格式](docs/recipe-body-format.md)。

## 快速开始

最简单的源码预览方式是直接在宿主机运行，目前假定具备：

- Linux
- Git、GNU Make 与 Bash
- [rustup](https://rustup.rs/)（仓库通过 `rust-toolchain.toml` 固定 Rust 版本）
- Node.js 22.12 或更高版本与 npm
- 已安装并完成认证的 OpenAI Codex CLI

克隆并准备环境：

```bash
git clone https://github.com/keanji-x/neige-calm.git
cd neige-calm

rustup toolchain install
codex login
```

以前台宿主机模式启动 Neige Calm：

```bash
PROD_AUTH_PASSWORD=choose-a-local-password make prod
```

打开 <http://localhost:4040/next/>，以用户名 `owner` 和刚才选择的密码登录。该模式默认只监听回环地址，数据保存在 `~/.local/share/neige-calm`，并会把两个辅助二进制链接到 `~/.local/bin`。

新建 Track 时，合成器中的第一条消息会随创建请求一起提交，无需再次发送。准备 Alpha 安装包或从安装包部署，请按[Linux Alpha 打包与安装流程](docs/alpha-release.md)操作。

`make prod` 是前台宿主机运行模式。需要进程监管安装与升级流程时，请参阅[部署与升级指南](docs/deploy-and-upgrade.md)。

### 容器化开发

Docker Engine、Docker Compose v2、`curl` 与 `ss` 是容器化开发路径的可选依赖。使用前请复制并检查环境文件：

```bash
cp .env.example .env
```

- 将 `CALM_EXTRA_MOUNT=/mnt/data2` 改成主机上真实存在的路径；如果不需要额外挂载，可以改成 `/tmp`。
- `make dev` 会从 `PATH` 自动解析当前 Codex 安装；如果使用非标准包布局，请把
  `CALM_CODEX_HOST_BIN` 和 `CALM_CODEX_CODE_MODE_HOST_BIN` 都指向同一版本中的可执行文件。
- 修改 `CALM_AUTH_PASSWORD`；仓库内置的开发默认值是 `dev`。
- 容器的出站网络目前依赖 `.env.example` 所述的主机代理/转发器配置。如果你不使用主机代理，建议采用上面的宿主机运行路径。

启动开发栈：

```bash
make dev
```

`make dev` 会同时构建并部署两个前端。记下输出端口，然后打开 `http://localhost:<printed-port>/next/`；Docker 开发环境的根路径也会跳转到这里。切换期间，旧前端仍可通过 `http://localhost:<printed-port>/calm/` 访问。

如果需要前端 HMR，而不是使用构建后的 bundle，请在另一终端执行以下命令，并替换 `<printed-port>`：

```bash
FE_API_PROXY_TARGET=http://127.0.0.1:<printed-port> make fe-dev
```

然后打开 <http://localhost:5180/next/>。无需登录 cookie 即可检查公开版本端点：

```bash
curl -fsS http://localhost:<printed-port>/api/version
```

常用命令：

```bash
make logs     # 持续查看服务端和代理日志
make stop     # 停止开发栈
make help     # 列出全部 Make target
```

## 安全提示

Neige Calm 能够执行 Agent 生成的命令并修改附加的仓库。请把它视为可信、单用户的环境，而不是经过加固的多租户服务；在允许它接触敏感代码或密钥前，应审阅 Agent 操作。

Docker 开发端口默认发布到主机的**所有网络接口**。容器还会挂载主机路径，并为支持 Codex 自身的沙箱机制而获得较宽的 Linux capability。不要把默认 Docker 配置暴露到不可信网络；应使用防火墙或显式限制为回环端口，并务必先修改默认凭据。

## 开发与验证

日常 Rust 开发只运行覆盖受影响 package 和行为的测试（请将占位符替换为实际选择器）：

```bash
env -u NEIGE_CODEX_BIN RUSTC_WRAPPER= CARGO_BUILD_JOBS=6 \
  cargo nextest run --locked \
  -p <package> <test-name-filter> --test-threads 8
```

仅当受影响测试确实需要时才添加 `--features calm-server/codex-e2e`；需要进一步缩小
范围时，可添加 `--lib` 或 `--test <test-target>`。如果改动还需要 Rust 编译、lint 和
OpenAPI 预检，运行 `scripts/local-rust-gates.sh --quick`。完整的
`scripts/local-rust-gates.sh` 不是日常开发命令：全 workspace 测试由 CI 运行。只有在
明确要求，或维护 gate/nextest 配置本身时，才在本地运行完整门禁。

运行新前端门禁：

```bash
(cd fe && npm ci && npm run lint && npm run build && npm test)
```

浏览器测试还需要安装 Playwright Chromium：

```bash
(cd fe && npx playwright install --with-deps chromium && npm run test:browser)
```

默认端到端测试层不会消耗模型 Token：

```bash
./e2e/run.sh
```

Tier 2 使用真实 Codex 凭据，并可能产生模型用量：

```bash
./e2e/run.sh --tier 2
```

该直接入口只能在专用主机运行；Tier 2 stack E2E 没有共享主机安全入口。共享生产主机
可以运行另一套隔离的 `codex_forge_e2e`：

```bash
make e2e-codex-isolated
```

该目标不能替代 Tier 2 stack 覆盖。

## 仓库结构

```text
crates/    Rust 内核、持久化、执行、Provider、CLI 与进程监管
fe/        新一代前端及不依赖框架的领域核心
web/       前端切换期间暂时保留的旧前端
plugins/   内置插件 manifest 与实现
e2e/       完整技术栈与真实 Agent 端到端测试
docs/      架构、运维、设计记录与可执行 Oracle 文档
docker/    开发栈镜像与 nginx 配置
scripts/   本地门禁、诊断、生成与发布辅助脚本
```

## 产品方向

Neige Calm 正在收敛到四个面向用户的核心概念：

1. **Area**：长期上下文归属在哪里。
2. **Track**：工作经历一轮或多轮活动时，什么对象始终保持连贯。
3. **Report**：当前可检查的成果，而不是埋在聊天记录中的摘要。
4. **Recipe**：某一类工作可以怎样被重复执行和交付。

新前端已是默认入口，切换期间仍保留旧前端。插件安装与 Recipe 编辑已经可用；结构化检查点，以及携带恢复证据接续失败尝试，仍属于[后续工作](docs/architecture/long-task-reliability.md#delivery-scope)。

## 文档导航

- [使用指南](docs/using-neige-calm.md)：Recipe、任务监督、Report 文件与插件设置（英文）。
- [Linux Alpha 打包与安装](docs/alpha-release.md)：构建与安装发行归档（英文）。
- [部署与升级指南](docs/deploy-and-upgrade.md)：进程监管部署、备份与升级（英文）。
- [文档索引](docs/README.md)：配置、保留策略、架构与设计记录。

## 参与贡献

项目仍在快速演进。准备较大改动前，请先通过 issue 说明用户可见结果、涉及的权限边界，以及如何验证行为。Pull request 应保持范围清晰，保留既有 migration 与持久化契约，并在提交前运行相关本地门禁。PR 格式、验证清单与仅限 Squash Merge 的合并规则请参阅英文版 [CONTRIBUTING.md](CONTRIBUTING.md)。

## 许可证

除非另有说明，本仓库代码采用 [Apache License 2.0](LICENSE)。第三方依赖与独立分发的插件继续适用各自的许可证。
