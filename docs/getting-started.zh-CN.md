# 从一个仓库开始

[English](getting-started.md)

这份指南面向已经使用 Codex 或 Claude 的开发者。先完成一次仓库分析，得到可继续
维护的报告，再尝试小修改或 PR 交付。

## 安装 Linux 发行包

准备经过审查、适配你的 CPU 架构的 Linux Alpha 归档及校验文件。运行发行包不需要
Rust、Node.js、Go、Docker 或 systemd。运行时需要 Git、ripgrep（`rg`）、`zstd`、
浏览器及已登录的编程 CLI；Linux CLI 沙箱还需要 bubblewrap（`bwrap`）。在
Debian/Ubuntu 中，对应的软件包名为 `git ripgrep zstd bubblewrap`。登录 CLI 和启动
Neige 应使用同一个操作系统用户。Codex 的配套可执行文件也
应保留在它的安装目录中。目前发行包面向 Linux；macOS 的有限源码构建能力见
[README](../README.md#macos-local-builds-limited)。

用随包提供的 `.sha256` 文件检查归档，解压后进入解压目录，运行：

```bash
./release/bin/neige-app system setup --package ./release
```

按提示设置 Neige 本地登录密码。它与编程 CLI 的登录是两回事。安装程序检查包的
完整性，创建私有安装目录，并打印准确的前台启动命令和浏览器地址。执行该命令，
保持终端运行；打开打印的地址，以 `owner` 和刚设置的密码登录。

默认安装目录为 `~/.local/share/neige-app`，配置、发行版本、数据、插件存储和辅助
程序都在其中。需要其他位置时，添加 `--root /绝对路径/neige`，目标目录必须尚未
存在。已有安装请使用[升级指南](deploy-and-upgrade.md)，不要重新执行首次安装。
安装不会覆盖旧工作，也不会注册 systemd 服务。

自动化场景可用 `--password-file /绝对路径/私有文件` 读取密码，避免放到命令行中；
文件只能由本人读取。没有归档时，由维护者按 [Alpha 打包流程](alpha-release.md)
准备；贡献者也可使用 [源码预览](../README.zh-CN.md#源码预览)。

## 先用 Demo 了解流程

打开页面后，可以从头像菜单的 **入门引导** 进入 Demo；空工作区也有 **先体验 Demo**
入口。也可以在浏览器地址的 `/next/` 后加上 `?demo=onboarding`。
Demo 使用当前界面和预置结果，带你建演示项目、查看页面、再改一次标题；不需要先登录
AI 工具，不调用后端 API，也不读写你的项目文件。顶部始终显示演示标识。
演示数据只保留在本次页面内，刷新或点 **重新开始** 会清空；点 **退出演示** 回到正常使用。

## 准备连接并确认 Agent 可以工作

若机器访问模型需要代理，先填写 **Settings → Network → HTTP proxy / HTTPS
proxy**，离开各输入框或按 Enter 保存。代理地址必须能从运行 Neige 的机器访问。
首次配置完成后，用 Ctrl-C 停止前台服务，等待原进程完全退出，再执行保存的
启动命令，让共享 Agent 进程采用已保存的网络配置；无需重新安装，数据也会保留。

请在开始工作前配置网络。以后需要修改时，先等当前工作结束，保存后再用原命令重启。
运行中改代理可能让旧 Planner 会话保持等待；保留原 Track 和已保存成果，重启服务
后在原 Track 继续。这个恢复限制记录在 [#2693](https://github.com/keanji-x/neige-calm/issues/2693)。

然后打开 **Settings → Planners**，至少有一个服务显示 **Ready** 后再发送首次请求。
解决登录、可执行文件或配置问题后，点击 **Recheck** 发起新检查。网页能打开说明
本地服务已启动；模型连接还需要单独确认。

Codex 使用服务运行用户的 CLI 和登录。Claude 需要显式的
[Planner 配置及专用目录登录](neige-app-config.md#enable-a-claude-planner)，仅安装
CLI 还不够。在新建 Track 的模型选择器中选择已经就绪的服务；初始默认是 Codex。

登录检查超时不等于没有登录。先确认同一个操作系统用户能运行 CLI 且已登录。
若刚修改了代理，Recheck 仍超时，用保存的前台启动命令重启，再检查一次。持续
失败时保留错误信息用于排查，反复创建 Track 无法修复连接。后台服务不一定继承
交互终端里的代理变量。

## 得到第一份报告

1. 点击 **Create your first area**，用项目名称命名并保存。**Area** 将相关工作归在
   一起，初次使用可按一个项目一个 Area 组织。
2. 点击 Area 旁的 **＋** 新建 **Track**。一个 Track 承载一项调查、修改或持续工作。
   通过 **Folder** 选择本机 Git 仓库。这里是 Neige 服务所在机器的目录，不是上传
   浏览器所在电脑的文件。
3. 在 **Template** 中选择 **Investigation**，并选择已经就绪的模型。模板提供工作
   方法和报告初始结构，你发送的文字决定这次具体做什么。
4. 输入需求，点击创建箭头一次：

   > 请阅读这个仓库，说明它解决什么问题、主要目录和一条关键执行流程。在报告中
   > 给出相关文件链接，并列出三个值得继续调查的问题。本次只分析，不修改项目
   > 代码，也不创建 PR。

5. 查看 **Report** 中的结论和文件依据。在同一个 Track 的对话里追问或补充要求。
   创建时已经提交了第一条需求，无需再发送一次。

完成标志：得到一份有依据的报告，刷新后仍能找到，并能在原 Track 中继续追问。
Neige 会准备这个 Track 的工作目录，不需要你手动建立 worktree。

## 选择下一项工作

| 你想做什么 | 选择 | 预期产出 |
| --- | --- | --- |
| 理解代码、调查问题 | Investigation | 调查结论和证据 |
| 完成一个范围明确的小修改 | Small change | 修改结果和实际运行的验证 |
| 把需求或 Issue 推进到 PR | Development | 实现、验证、评审和按授权交付 |
| 暂不指定工作方法 | No template | 按你的要求推进 |

**Small change**：选择仓库，说清要改什么、如何确认完成。例如：“修正 README
中的用法示例，对照实现检查示例，并在报告中说明修改。结果保留在本地，不发布
PR。”仓库自身的开发规则依然适用。

**Development**：先在 **Settings → Plugins** 启用 **development**。选择器会提示
缺失的能力，启用后回到保留的草稿。发布 PR 还需要服务端 PATH 上已登录的 `gh`
和 `jq`，以及正确的远端仓库和发布权限。

Development 会把 worker 成果保存为 Git 提交。Planner 在修改前检查 Git 作者和
提交者身份。缺失时，以服务运行用户配置自己的 Git 姓名和邮箱，或明确授权 Planner
为当前仓库配置一个身份。在该仓库执行 `git var GIT_AUTHOR_IDENT` 和
`git var GIT_COMMITTER_IDENT` 可检查现有配置。Investigation 不要求提交身份。

选择仓库，描述目标与验收标准。**Issue URL** 可留空；填写时应为同一仓库的 GitHub
Issue。保持 **Merge automatically** 关闭，Neige 会等待你批准合并。模板按目标仓库
的 `AGENTS.md` 和 `CONTRIBUTING.md` 安排开发、评审与验证。

### 以后可能要发 PR，应该怎样开始

预计需要 PR 交付时，从一开始就选 **Development**，Issue 仍可留空。可以先要求
本地实现与验证，准备好后再要求发布。**Small change** 适合本地小修改，结果可能
仍以未提交改动保存在该 Track 的独立目录里。

已有本地成果时，保留原 Track，先查看它的工作目录和 Git diff，按仓库的正常 Git
流程保存经过检查的改动，再交接给其他 Track。新 Track 使用仓库中已提交的内容，
不会自动带入未提交改动。工作指令和插件绑定在创建时确定。更直接的交付
交接引导记录在 [#2680](https://github.com/keanji-x/neige-calm/issues/2680)。

## 什么时候需要你操作，之后怎样继续

报告维护当前成果，对话记录需求、反馈和决定。任务及 worker 卡片提供执行和验证
细节。通过 **Waiting on you** 或通知中心找到需要你处理的事项，阅读证据后再回答。
worker 完成、验证通过、PR 发布和 PR 合并是不同节点，以报告中的实际结果为准。

完成后可关闭 Track，Planner 也可能自行关闭已完成的工作；安排新工作前先
**Reopen**。若关闭后在侧栏找不到，在 Area 菜单选择 **Show closed**。重启服务时执行原来的启动命令，打开原有 Track 即可，
不需要重做首次安装。

经常做同类工作时，可打开 **Template → Manage recipes… → New recipe**，用普通
Markdown 保存工作要求和希望报告包含的章节。Recipe 不会预先排出固定任务，也不
会启用插件；Planner 按实际需求创建任务。修改 Recipe 只影响以后新建的 Track。
更多示例见 [Recipe 指南](recipe-body-format.md)。

## 卡住时怎么办

| 现象 | 处理方式 |
| --- | --- |
| 安装提示目标已存在 | 保留现有内容，使用原启动命令或升级流程；另建实例时才指定新的 `--root`。 |
| 缺失动态加载器或共享库 | 对照 `BUILD.json` 检查 CPU 架构与 Linux/libc 基线。 |
| 安装拒绝路径或提示 socket 路径过长 | 选择更短、不含 `%`、`?` 或 `:` 的绝对安装目录；此时尚未写入安装。 |
| 端口被占用 | 首次安装前，用 `--listen` 和 `--admin-listen` 指定两个空闲回环地址，不要停止其他实例。 |
| Planner 显示 Unavailable 或 Not configured | 阅读 Settings → Planners 中的原因，以服务用户修复后 Recheck。 |
| 登录检查超时或请求一直等待 | 检查 CLI 登录与 Network；刚修改代理时，用原命令重启并 Recheck。持续失败时保留错误，避免反复创建相同工作。 |
| Development 提示需要插件 | 在 Settings → Plugins 启用 development，然后回到草稿。 |
| worker 拒绝脏工作目录 | 先检查该目录的改动，提交或明确撤回后再继续。 |

需要报告引用（`@`）、实时预览、更多插件或详细任务操作时，继续阅读
[使用指南](using-neige-calm.md)。预览需要事先配置，目前要求 HTTP 访问。
