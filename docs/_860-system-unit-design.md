# #860 — neige-app 迁移为 system unit（与登录会话解耦）

## 1. 问题 & 根因（已坐实）

- neige-app + calm-server(4040) + 所有 spawn 的 claude/codex/bun worker 都活在 `user@1000.service` →
  `user-1000.slice` 下。
- 2026-06-26 18:57 UID1000 的 SSH 会话全断 → logind/PID1 销毁 `user@1000.service`（`exit.target`），
  SIGKILL 整个 user slice → 4040 + workers 一锅端死，85s 后重登才被拉起。
- `Linger=yes` 未生效：`/var/lib/systemd/linger/kenji`(mtime 6-23，疑似备份带来) 未被 logind 运行时认领。
  **判据**：linger 生效则 user@1000 在末会话关闭时不会停；实测等到重登才起 → 当时按非-linger 处理。
- 已 `loginctl disable→enable` 重新 assert（band-aid，不在本 slice 依赖）。

## 2. 决策

neige-app 由 **per-user unit** 迁移为 **system unit**（`/etc/systemd/system/neige-app.service`，`User=kenji`），
PID1 直接监管，脱离登录会话生命周期。

## 3. 关键判断：无需权限模型改动

重启/升级机制本就与 systemd scope 无关：

| 机制 | 实现 | 是否依赖 systemctl 权限 |
|---|---|---|
| `/restart`(admin) | 重启子进程 calm-server，supervisor respawn (main.rs:1162) | 否 |
| binary 自升级 | `schedule_exec_self` → execve 换新二进制 (main.rs:1193) | 否 |
| `/upgrade/full-reboot` | `std::process::exit(0)` 靠 `Restart=always` 接住 (main.rs:1234) | 否（system unit 同样有 Restart=always）|
| `systemctl --user restart` | 仅 `upgrade_next_steps` 打印字符串 (main.rs:898) | 仅文案 |

→ 自升级/重启全程不经 `systemctl`，故 system 化**不需要 sudo/polkit 白名单**。一次性 `install` 需 root（可接受）。

## 4. 代码改动（3 处 + 配置）

### 4.1 `render_systemd_unit` (main.rs:1463)
新增 scope 参数；system 分支产出：

```ini
[Unit]
Description=neige-app system service
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
User=kenji
Group=kenji
Environment="PATH=<path_env>"
Environment=HOME=/home/kenji          # 显式设：Claude adapter 不自设 HOME(claude_adapter.rs:306)，且 data_dir 解析依赖 HOME；不赌 passwd 查询
# 不设 XDG_RUNTIME_DIR / DBUS_SESSION_BUS_ADDRESS —— 它们绑 /run/user/1000，会重新耦合会话。
# 不设 XDG_DATA_HOME / XDG_CONFIG_HOME —— 改为在 config.toml 里把 data_dir/plugins_dir 钉绝对路径（见 §5）。
ExecStart=<bin> system serve --config <config_path>
Restart=always
RestartSec=2
Delegate=yes          # 让 neige-app 真正掌控自身子 cgroup，修 "Failed to kill cgroup: Operation not permitted" + 孤儿 worker
KillMode=mixed        # 与 #547 对齐：SIGTERM 只发主进程，cgroup 内 PTY/子进程交给 neige-app 自身的进程组清理，避免 restart 误杀 user PTY

[Install]
WantedBy=multi-user.target
```

user 分支保持现状（向后兼容测试/旧部署）。

### 4.2 `run_install` (main.rs:~796)
- `systemd.unit_path` 默认随 scope：system→`/etc/systemd/system/neige-app.service`。
- system scope 时 install 走 `systemctl daemon-reload` + `systemctl enable --now`（非 --user）。
- install 需以 root 运行；检测非 root 时给出明确报错提示 `sudo neige-app system install`。

### 4.3 `upgrade_next_steps` (main.rs:898)
`systemctl --user restart {unit}` → `systemctl restart {unit}`（或直接以 `/restart`/`/upgrade/full-reboot` 为主，弱化该提示）。

### 4.4 配置 schema
`config.rs` 的 `SystemdConfig` 增 `scope: SystemdScope { User | System }`（默认 `User` 保持兼容）；
TOML key `systemd.scope`。本部署在 `~/.config/neige-app/config.toml` 设 `systemd.scope = "system"`。

## 5. 环境风险 & e2e 硬门（唯一真风险）

system unit 不继承会话的 `XDG_RUNTIME_DIR` / `DBUS_SESSION_BUS_ADDRESS` / `XDG_DATA_HOME` / `XDG_CONFIG_HOME`。

### 5.0 【最强隐患，review 补】数据目录解析的会话耦合 — 必须钉绝对路径
`calm-server/src/config.rs:152-189`：`data_dir_resolved` / `plugins_dir_resolved` / `plugins_data_dir_resolved`
经 `XDG_DATA_HOME` / `XDG_CONFIG_HOME` → `HOME/.local/share|.config` fallback；`proc_supervisor_sock_resolved`
(:162) 及 codex socket 全挂在 `data_dir` 上。system unit 下 `XDG_DATA_HOME/XDG_CONFIG_HOME` **未设**、
`HOME` 取自 passwd。若 `config.toml` 不钉绝对 `data_dir`/`plugins_dir`，可能静默解析到**不同目录** →
丢 proc-supervisor socket / codex home / plugins / DB。
**动作**：cutover 前在 `~/.config/neige-app/config.toml` 显式钉 `data_dir = "/home/kenji/.local/share/neige-calm"`
（及 plugins 路径）为绝对值；unit 里显式 `Environment=HOME=/home/kenji`；cutover 时核对 passwd-HOME == 当前 login-HOME。

### 5.1 worker 环境（纯继承，e2e 硬门）
- calm-server 源码：无 DBUS 使用；`XDG_RUNTIME_DIR` 仅 `new_stub()`（带 `/tmp` fallback），生产 `new()` 走 HOME 路径。✅
- worker/PTY env 纯继承父进程（`terminal_adapter.rs:851`、`calm-proc-supervisor/src/lib.rs:785/866` 不 env_clear）；
  Claude adapter 不自设 HOME(`claude_adapter.rs:306`) → 依赖 unit 的 `Environment=HOME`。
- `bun` worker 为构建期，运行期无 spawn → 非风险（review 确认）。
- **未知数**：spawn 的外部 worker（claude / codex / `codex app-server` / bun）在无 runtime dir 下能否正常起。
  - `codex app-server` 已知会对 listen socket 父目录 `chmod 0700`——已挂在 HOME 下 `data_dir`（state_clients.rs:54-69），不依赖 /run/user。✅（需 e2e 复核）
- **e2e（worktree，硬门）**：在剥离 `XDG_RUNTIME_DIR`/`DBUS_SESSION_BUS_ADDRESS` 的环境起 neige-app，跑一个 wave，验证：
  1. codex worker spawn + 跑通；
  2. claude worker spawn + 跑通；
  3. 终端 PTY 正常。
- 若某 worker 确需 runtime dir：用 systemd `RuntimeDirectory=neige`（→ `/run/neige`，非会话绑定）注入稳定 `XDG_RUNTIME_DIR`，**不要**指回 `/run/user/1000`。

## 6. Cutover（生产，需用户显式 OK + sudo）

1. worktree 实现，gates 全绿（fmt / clippy -D / 全工作区 test / OpenAPI 若漂移 / web build / e2e §5）。
2. 经正常 upgrade 管线 stage release。
3. 在 `config.toml` 设 `systemd.scope = "system"`。
4. 切换（手动、分步、可回滚）：
   - `systemctl --user stop neige-app && systemctl --user disable neige-app`
   - `sudo neige-app system install --force`（写 system unit + daemon-reload + enable --now）
   - 验 `ss -ltnp | grep 4040`、worker spawn、`/upgrade` dry-run。
5. linger 可保留（无害）或 `loginctl disable-linger kenji`。
6. 回滚：`sudo systemctl disable --now neige-app`（system）→ 复原 user unit + `systemctl --user enable --now`。

## 7. 范围边界 / 开放问题

- **范围外**：`neige.service`（3333 终端管理器）同源漏洞、独立 unit → 单独 follow-up issue。
- **已定（review 后）**：
  - `KillMode=mixed` + `Delegate=yes` 一并进本 slice（§4.1），与 #547 对齐；impl 时复核 `/restart`/proc-supervisor
    的进程组清理边界(`main.rs:594/1661`、`calm-proc-supervisor/src/lib.rs:169/636`)在 mixed 下不被绕过。
  - `systemd.scope` 默认保持 `User`（兼容）；**必须新增 scope-aware 测试**钉住：system 分支产出含 `User=kenji`/
    `WantedBy=multi-user.target`/`Delegate=yes`/`KillMode=mixed`，install/upgrade nextSteps 不含 `--user`
    （现有断言 `main.rs:1757/2131/2151` 只覆盖 user 分支，要扩）。
- **开放**：是否给 system unit 配 `OOMPolicy`/`MemoryMax` 护栏（本次 125Gi 无 OOM，倾向不加）。

## 8. Cutover 原子性（review 补）

为缩短 4040 离线窗口 + 保证可回滚：
1. 先 `sudo neige-app system install`（**不 --now**，只写 unit + daemon-reload），此步不停旧服务、可反复。
2. 一个短窗口里串行：`systemctl --user stop neige-app` → `sudo systemctl start neige-app` → `ss -ltnp|grep 4040` 确认。
   （旧 user unit 先 `disable` 防止重登把它又拉起来与 system unit 抢 4040。）
3. 失败回滚：`sudo systemctl disable --now neige-app` → `systemctl --user enable --now neige-app`，旧 unit 文件全程保留未删。
4. 验证通过、稳定运行 24h 后，再清理旧 user unit 文件。
