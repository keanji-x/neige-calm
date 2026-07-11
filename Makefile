# neige-calm dev orchestration.
#
# The docker stack runs the *host-built* binary (no rust toolchain in the
# image), bind-mounts $HOME at the same path inside the container, and
# publishes a single host port (CALM_PORT, default 4041) via nginx. The
# backend lives on the compose bridge network; nginx is the only host-facing
# entrypoint.
#
#   make dev                  # build + bring stack up in background
#   make dev-fresh            # wipe this DEV_ID's /tmp state, then start
#   make prod                 # run host production process (no docker)
#   make stop                 # tear stack down
#   make logs                 # tail logs
#   make help                 # everything

SHELL          := bash
.SHELLFLAGS    := -eu -o pipefail -c
.DEFAULT_GOAL  := help

# Host UID/GID for the container to run as. Exported so docker compose can
# interpolate them. (UID is shell-readonly in bash, so we re-export via the
# environment for child processes that don't inherit make's view.)
HOST_UID := $(shell id -u)
HOST_GID := $(shell id -g)
export UID  := $(HOST_UID)
export GID  := $(HOST_GID)
export HOME
export USER

# By default, start from PORT_BASE and pick the first currently-free host
# port. 4040 is reserved for host prod. Explicit CALM_PORT still wins:
#
#   make dev-fresh                 # auto, usually 4041 or next free
#   CALM_PORT=4315 make dev-fresh  # fixed
PORT_BASE ?= 4041
CALM_PORT ?= $(shell p=$(PORT_BASE); while ss -H -ltn "sport = :$$p" 2>/dev/null | grep -q .; do p=$$((p+1)); done; echo $$p)
export PORT_BASE
export CALM_PORT

# Isolate compose resources per dev instance. Override DEV_ID when running
# multiple worktrees or PRs side-by-side:
#
#   make dev-fresh DEV_ID=pr315 CALM_PORT=4315
#
# Runtime state lives inside the server container on a tmpfs. `make dev`
# keeps it for the lifetime of that compose container; `make dev-fresh`
# runs `docker compose down -v` first, so the next boot is clean.
DEV_ID ?= $(notdir $(CURDIR))
COMPOSE_PROJECT_NAME ?= neige-calm-$(DEV_ID)
export DEV_ID
export COMPOSE_PROJECT_NAME

# Optional local overrides (.gitignored — see .gitignore). Docker compose
# reads this automatically for service env interpolation; we `-include` it
# here so the proxy-forwarder target sees the same vars.
-include .env

# Proxy forwarder — alpine/socat container bridging docker0 to a host-bound
# upstream proxy. Linux loopback (127.0.0.1) is in the host net namespace
# and unreachable from bridge containers; the forwarder listens on the
# docker0 gateway IP, runs in --network host so its 127.0.0.1 = host's,
# and forwards to the upstream. Leave CALM_HOST_PROXY_PORT empty in .env
# to skip (no proxy needed).
CALM_HOST_PROXY_HOST ?= 127.0.0.1
CALM_FORWARDER_PORT  ?= 2081
PROXY_FORWARDER_NAME ?= calm-proxy-forwarder
PROXY_FORWARDER_IMAGE ?= alpine/socat
DOCKER_BRIDGE_IP     ?= 172.17.0.1
export CALM_FORWARDER_PORT

CALM_CONTAINER_STATE_DIR ?= /var/lib/neige-calm
CALM_DATA_DIR ?= $(CALM_CONTAINER_STATE_DIR)/data
CALM_DB_URL ?= sqlite://$(CALM_CONTAINER_STATE_DIR)/calm.db?mode=rwc
CALM_PLUGINS_DATA_DIR ?= $(CALM_CONTAINER_STATE_DIR)/plugins-data
export CALM_CONTAINER_STATE_DIR
export CALM_DATA_DIR
export CALM_DB_URL
export CALM_PLUGINS_DATA_DIR

# Host production mode: no docker, no nginx. 4040 is reserved for prod by
# default. calm-server serves web/dist itself and uses the host's HOME,
# ~/.codex, PATH, and login shell.
LOCAL_SHELL ?= $(shell command -v zsh 2>/dev/null || getent passwd $(USER) | cut -d: -f7 2>/dev/null || echo /bin/sh)
PROD_PORT ?= 4040
PROD_LISTEN ?= 127.0.0.1:$(PROD_PORT)
PROD_DATA_DIR ?= $(HOME)/.local/share/neige-calm
PROD_DB_URL ?= sqlite://$(PROD_DATA_DIR)/calm.db?mode=rwc
PROD_PLUGINS_DATA_DIR ?= $(PROD_DATA_DIR)/plugins
PROD_AUTH_USERNAME ?= owner
PROD_AUTH_PASSWORD ?= dev
PROD_DEV_AUTOLOGIN ?= false
PROD_ALLOWED_ORIGIN ?= http://localhost:$(PROD_PORT)

# Compose wrapper so every recipe picks up the same project name + file.
COMPOSE := docker compose

# XDG paths the binary expects. sqlx creates the db file but not the parent
# dir; plugin install/state dirs are touched on first install.
XDG_DIRS := \
  $(HOME)/.local/share/neige-calm \
  $(HOME)/.local/share/neige-calm/plugins \
  $(HOME)/.config/neige-calm/plugins

# Override to build + serve a different worktree's code without leaving
# the primary repo. Useful for validating a PR's branch state via docker
# without polluting the primary working tree. Default = current dir.
#
#   make dev                                # build from cwd (default)
#   make dev WORKTREE=/path/to/agent-XYZ    # build from that worktree
#
# Implementation: every source path the build touches is made absolute
# relative to $(WORKTREE), and the four docker-compose env vars
# (CALM_BIN / CALM_DAEMON_BIN / CALM_CODEX_BRIDGE_BIN / CALM_WEB_DIST)
# are exported pointing at $(WORKTREE)'s target/release + web/dist so
# the container picks up the right binaries.
#
# Caveat: two stacks can run side-by-side only when both DEV_ID and CALM_PORT
# differ. DEV_ID isolates compose resources + /tmp state; CALM_PORT isolates
# the single host-facing nginx port.
WORKTREE ?= $(CURDIR)
WORKTREE_INPUT := $(WORKTREE)
# `override` is required because command-line `WORKTREE=...` assignments
# would otherwise win over a plain `:=`, leaving WORKTREE un-resolved.
override WORKTREE := $(realpath $(WORKTREE))
ifeq ($(WORKTREE),)
$(error WORKTREE=$(WORKTREE_INPUT) does not resolve to a real path)
endif
ifeq ($(wildcard $(WORKTREE)/Cargo.toml),)
$(error WORKTREE=$(WORKTREE) is not a valid neige-calm checkout (no Cargo.toml))
endif

BIN      := $(WORKTREE)/target/release/calm-server
BRIDGE   := $(WORKTREE)/target/release/neige-codex-bridge
APP      := $(WORKTREE)/target/release/neige-app
# Issue #236 followup — kernel-as-MCP-server stdio bridge. Codex inside
# the docker container spawns this per spec/worker card (config.toml's
# `[mcp_servers.calm].command`); without it the handshake exits with
# `os error 2`. docker-compose.yml bind-mounts the built binary into
# /usr/local/bin/.
MCP_SHIM := $(WORKTREE)/target/release/neige-mcp-stdio-shim
# Issue #388 Phase 1 — fork-broker between neige-app and calm-server's
# per-terminal supervisor operations. calm-server connects to the
# control UDS at `proc_supervisor_sock` for every terminal spawn; without
# this binary the first spawn fails with `connect calm-proc-supervisor …
# No such file or directory`. neige-app peer-supervises it.
PROC_SUP := $(WORKTREE)/target/release/calm-proc-supervisor
NEIGE_CLI := $(WORKTREE)/target/release/neige
DIST     := $(WORKTREE)/web/dist
NODE_MODULES_STAMP := $(WORKTREE)/web/node_modules/.package-lock.json
LOCAL_BIN_DIR ?= $(HOME)/.local/bin
LOCAL_MCP_STDIO_SHIM ?= $(LOCAL_BIN_DIR)/neige-mcp-stdio-shim
LOCAL_NEIGE_CLI ?= $(LOCAL_BIN_DIR)/neige

# Plumb the same paths into docker-compose so the container bind-mounts
# the right binaries + web bundle. The `${VAR:-default}` form in
# docker-compose.yml uses these when set, falls back to ./… otherwise.
export CALM_BIN := $(BIN)
export CALM_CODEX_BRIDGE_BIN := $(BRIDGE)
export CALM_MCP_SHIM_BIN := $(MCP_SHIM)
export CALM_PROC_SUPERVISOR_BIN := $(PROC_SUP)
export CALM_WEB_DIST := $(DIST)

# Wipe the local sqlite DB (with a timestamped backup) before `up -d` so
# the new stack boots from a clean schema. Useful when switching between
# branches with different migration histories — old rows + migration
# records confuse the new binary. Default = off (preserve current state).
#
#   make dev RESET_DB=1     # back up + wipe DB, then `up`
#   make dev                # unchanged
#   make reset-db           # standalone: wipe without bringing stack up
#
# Backup location: $(CALM_DB_PATH).bak-make-dev-<unix_ts>
# Terminal sock leftovers at $(HOME)/.local/share/neige-calm/terminals/*.sock
# are removed too — they reference dead terminal_ids after a reset.
#
# Caveat: only the default host DB path is targeted. If CALM_DB_URL points
# elsewhere, this is a no-op for the custom location.
RESET_DB ?=
CALM_DB_PATH := $(HOME)/.local/share/neige-calm/calm.db
CALM_TERMINALS_DIR := $(HOME)/.local/share/neige-calm/terminals

.PHONY: help
help: ## Show this help.
	@awk 'BEGIN {FS = ":.*##"; printf "Targets:\n"} \
	  /^[a-zA-Z0-9_-]+:.*?##/ { printf "  \033[36m%-12s\033[0m %s\n", $$1, $$2 }' $(MAKEFILE_LIST)
	@echo ""
	@echo "Dev port:  $(CALM_PORT) (auto from PORT_BASE=$(PORT_BASE); override: CALM_PORT=18080 make dev)"
	@echo "Worktree:  $(WORKTREE) (override: WORKTREE=/path/to/other-worktree make dev)"
	@echo "Dev ID:    $(DEV_ID) (override: DEV_ID=pr315 make dev-fresh)"
	@echo "Dev state: container $(CALM_CONTAINER_STATE_DIR) tmpfs"
	@echo "Prod:      make prod (host process, port $(PROD_PORT), local shell: $(LOCAL_SHELL))"

# ---- build (on host, not in docker) -------------------------------------

.PHONY: build
build: $(BIN) $(BRIDGE) $(APP) $(MCP_SHIM) $(PROC_SUP) $(NEIGE_CLI) $(DIST) ## Build server, app shell, codex bridge, mcp-stdio shim, proc-supervisor, neige CLI, web bundle.

# Single cargo invocation builds all binaries — cheaper than separate
# calls because deps overlap. Touch every output so the rule re-fires
# only when sources change. Issue #236 followup added `neige-mcp-stdio-shim`;
# issue #388 Phase 1 added `calm-proc-supervisor` (peer-supervised by
# neige-app and contacted by calm-server for every terminal spawn).
$(BIN) $(BRIDGE) $(APP) $(MCP_SHIM) $(PROC_SUP) $(NEIGE_CLI) &: $(shell find $(WORKTREE)/crates -name '*.rs' -o -name 'Cargo.toml' 2>/dev/null) $(WORKTREE)/Cargo.toml $(WORKTREE)/Cargo.lock
	cargo build --manifest-path $(WORKTREE)/Cargo.toml --release -p calm-server -p calm-codex-bridge -p neige-app -p neige-mcp-stdio-shim -p calm-proc-supervisor -p neige-cli --bin calm-server --bin neige-codex-bridge --bin neige-app --bin neige-mcp-stdio-shim --bin calm-proc-supervisor --bin neige

# npm rewrites node_modules/.package-lock.json after npm ci/install, so use
# it as the dependency stamp for lockfile-driven web installs. Match CI's
# --legacy-peer-deps incantation; see ci.yml note / TODO(#2).
$(NODE_MODULES_STAMP): $(WORKTREE)/web/package-lock.json
	cd $(WORKTREE)/web && npm ci --legacy-peer-deps

$(DIST): $(shell find $(WORKTREE)/web/src -type f 2>/dev/null) $(WORKTREE)/web/package.json $(WORKTREE)/web/vite.config.ts $(WORKTREE)/web/index.html $(NODE_MODULES_STAMP)
	cd $(WORKTREE)/web && npm run build

# ---- docker lifecycle ---------------------------------------------------

.PHONY: proxy-forwarder-up
proxy-forwarder-up: ## Ensure the host-loopback → docker0 proxy forwarder container is running.
	@if [ -z "$(CALM_HOST_PROXY_PORT)" ]; then \
	    echo "  Proxy forwarder skipped (CALM_HOST_PROXY_PORT not set in .env)"; \
	    exit 0; \
	fi; \
	spec="$(CALM_HOST_PROXY_HOST):$(CALM_HOST_PROXY_PORT)->$(DOCKER_BRIDGE_IP):$(CALM_FORWARDER_PORT)"; \
	if docker inspect $(PROXY_FORWARDER_NAME) >/dev/null 2>&1; then \
	    existing=$$(docker inspect -f '{{index .Config.Labels "calm.proxy.spec"}}' $(PROXY_FORWARDER_NAME) 2>/dev/null || echo ""); \
	    running=$$(docker inspect -f '{{.State.Running}}' $(PROXY_FORWARDER_NAME) 2>/dev/null || echo "false"); \
	    if [ "$$existing" != "$$spec" ]; then \
	        echo "  Forwarder spec changed ($$existing → $$spec); recreating"; \
	        docker rm -f $(PROXY_FORWARDER_NAME) >/dev/null; \
	    elif [ "$$running" != "true" ]; then \
	        docker start $(PROXY_FORWARDER_NAME) >/dev/null; \
	        echo "  Forwarder restarted: $$spec"; \
	        exit 0; \
	    else \
	        echo "  Forwarder already up: $$spec"; \
	        exit 0; \
	    fi; \
	fi; \
	docker run -d --network host \
	    --name $(PROXY_FORWARDER_NAME) \
	    --label "calm.proxy.spec=$$spec" \
	    --restart unless-stopped \
	    $(PROXY_FORWARDER_IMAGE) \
	    tcp-listen:$(CALM_FORWARDER_PORT),bind=$(DOCKER_BRIDGE_IP),fork,reuseaddr \
	    tcp:$(CALM_HOST_PROXY_HOST):$(CALM_HOST_PROXY_PORT) >/dev/null; \
	echo "  Forwarder created: $$spec"

.PHONY: proxy-forwarder-down
proxy-forwarder-down: ## Remove the proxy forwarder container.
	-@docker rm -f $(PROXY_FORWARDER_NAME) >/dev/null 2>&1 && echo "  Forwarder removed" || echo "  Forwarder not present"

# ---- docker-isolated codex-e2e tier (#863) ------------------------------
# Runs the real-codex codex_forge_e2e suite fully contained in Docker
# (--network none + PID namespace + cgroup rails) so agents can never touch
# host prod. See scripts/e2e-isolated/run.sh header for the full model and
# the smoke protocol. The e2e forwarder is a shared singleton (unix socket,
# NO published ports) mirroring proxy-forwarder-up/down above.
#
# E2E_PROXY_FORWARDER_IMAGE is pinned BY DIGEST (the forwarder runs
# --network host; a mutable tag there is a supply-chain hole). It is now just a
# glibc runtime shell for our host-compiled `e2e-egress-proxy` gate binary
# (bind-mounted in) — debian:bookworm-slim, the SAME base as Dockerfile.e2e. To
# bump: `docker pull debian:bookworm-slim`, then `docker images --digests
# debian`, update the digest here AND the default in scripts/e2e-isolated/run.sh,
# then `make e2e-proxy-forwarder-down` so the next run recreates it.
E2E_PROXY_FORWARDER_NAME ?= calm-e2e-proxy-forwarder
E2E_PROXY_FORWARDER_IMAGE ?= debian:bookworm-slim@sha256:60eac759739651111db372c07be67863818726f754804b8707c90979bda511df
E2E_PROXY_SOCK_DIR ?= /tmp/calm-e2e-proxy
E2E_RUNNER_ENV = CALM_HOST_PROXY_HOST="$(CALM_HOST_PROXY_HOST)" \
	CALM_HOST_PROXY_PORT="$(CALM_HOST_PROXY_PORT)" \
	PROXY_FORWARDER_IMAGE="$(E2E_PROXY_FORWARDER_IMAGE)" \
	E2E_PROXY_FORWARDER_NAME="$(E2E_PROXY_FORWARDER_NAME)" \
	E2E_PROXY_SOCK_DIR="$(E2E_PROXY_SOCK_DIR)"

# TEST travels via the environment (target-specific export) and is expanded
# by the SHELL as "$$E2E_TEST" — never spliced into the command line by make
# — so names with spaces/metacharacters cannot split or inject.
.PHONY: e2e-codex-isolated
e2e-codex-isolated: export E2E_TEST := $(TEST)
e2e-codex-isolated: ## Run codex_forge_e2e docker-isolated (TEST=name for one test; DECOYS=1 for decoy telemetry).
	$(E2E_RUNNER_ENV) scripts/e2e-isolated/run.sh $(if $(TEST),--test "$$E2E_TEST")

.PHONY: e2e-proxy-forwarder-up
e2e-proxy-forwarder-up: ## Ensure the e2e-tier unix-socket proxy forwarder is running.
	$(E2E_RUNNER_ENV) scripts/e2e-isolated/run.sh --forwarder-only

.PHONY: e2e-proxy-forwarder-down
e2e-proxy-forwarder-down: ## Remove the e2e-tier proxy forwarder container + its socket dir (guarded teardown in run.sh).
	$(E2E_RUNNER_ENV) scripts/e2e-isolated/run.sh --forwarder-down

SHELLCHECK ?= shellcheck
.PHONY: e2e-codex-isolated-check
e2e-codex-isolated-check: ## shellcheck + dry-run golden + fence fail-closed regression for the isolated tier (no docker daemon needed).
	$(SHELLCHECK) -x scripts/e2e-isolated/run.sh scripts/e2e-isolated/entry.sh scripts/e2e-isolated/check_dry_run.sh scripts/e2e-isolated/check_fence.sh
	scripts/e2e-isolated/check_dry_run.sh
	scripts/e2e-isolated/check_fence.sh

.PHONY: dev
dev: proxy-forwarder-up build dirs ## Build, then bring the stack up in the background (FRESH=1 wipes this DEV_ID first).
ifeq ($(FRESH),1)
	@echo "  FRESH=1 — stopping stack, removing container state, then bringing up"
	-$(COMPOSE) down -v --remove-orphans
endif
ifeq ($(RESET_DB),1)
	@echo "  RESET_DB=1 — legacy reset of the shared XDG DB; prefer make dev-fresh"
	-$(COMPOSE) down --remove-orphans
	$(MAKE) reset-db
endif
	$(COMPOSE) up -d --build
	@echo ""
	@echo "  → http://localhost:$(CALM_PORT)/"
	@echo "  → API: http://localhost:$(CALM_PORT)/api/coves"
	@echo "  dev id: $(DEV_ID)"
	@echo "  state:  container $(CALM_CONTAINER_STATE_DIR) tmpfs"
	@echo "  logs: make logs DEV_ID=$(DEV_ID)"
	@echo "  health: make health DEV_ID=$(DEV_ID) CALM_PORT=$(CALM_PORT)"

.PHONY: dev-fresh
dev-fresh: proxy-forwarder-up build ## Remove this DEV_ID's containers/state, then start a fresh stack.
	-$(COMPOSE) down -v --remove-orphans
	$(MAKE) dirs
	$(COMPOSE) up -d --build
	@echo ""
	@echo "  → http://localhost:$(CALM_PORT)/"
	@echo "  → API: http://localhost:$(CALM_PORT)/api/coves"
	@echo "  dev id: $(DEV_ID)"
	@echo "  state:  container $(CALM_CONTAINER_STATE_DIR) tmpfs"
	@echo "  logs: make logs DEV_ID=$(DEV_ID)"
	@echo "  health: make health DEV_ID=$(DEV_ID) CALM_PORT=$(CALM_PORT)"

.PHONY: up
up: proxy-forwarder-up dirs ## Bring the stack up without rebuilding.
	$(COMPOSE) up -d

.PHONY: stop
stop: ## Bring the stack down (keeps volumes / data).
	$(COMPOSE) down

.PHONY: restart
restart: ## Restart both containers in place.
	$(COMPOSE) restart

.PHONY: logs
logs: ## Tail logs from both services.
	$(COMPOSE) logs -f

.PHONY: ps
ps: ## Show container status.
	$(COMPOSE) ps

.PHONY: shell
shell: ## Drop into a shell in the server container (already at $HOME).
	$(COMPOSE) exec server bash || $(COMPOSE) exec server sh

.PHONY: health
health: ## Smoke-test the API end-to-end through nginx.
	@curl -fsS -w "  HTTP %{http_code}\n" http://localhost:$(CALM_PORT)/api/coves \
	  && echo "ok" || (echo "down — try: make logs"; exit 1)

# ---- host production lifecycle -----------------------------------------

.PHONY: prod-local-bin
prod-local-bin: $(MCP_SHIM) $(NEIGE_CLI) ## Link host prod MCP shim and neige CLI into ~/.local/bin.
	@mkdir -p "$(LOCAL_BIN_DIR)"
	@ln -sfn "$(MCP_SHIM)" "$(LOCAL_MCP_STDIO_SHIM)"
	@ln -sfn "$(NEIGE_CLI)" "$(LOCAL_NEIGE_CLI)"
	@echo "  mcp shim: $(LOCAL_MCP_STDIO_SHIM) -> $(MCP_SHIM)"
	@echo "  neige cli: $(LOCAL_NEIGE_CLI) -> $(NEIGE_CLI)"

.PHONY: prod-repair-codex-homes
prod-repair-codex-homes: prod-local-bin ## Rewrite stale docker shim paths in existing prod codex homes.
	@if [ -d "$(PROD_DATA_DIR)/codex-homes" ]; then \
	  find "$(PROD_DATA_DIR)/codex-homes" -name config.toml -exec sed -i 's|/usr/local/bin/neige-mcp-stdio-shim|$(LOCAL_MCP_STDIO_SHIM)|g' {} +; \
	  echo "  repaired stale MCP shim paths under $(PROD_DATA_DIR)/codex-homes"; \
	fi

.PHONY: prod
prod: build prod-dirs prod-repair-codex-homes ## Run production locally without docker (uses host ~/.codex and local shell).
	@echo "  → http://localhost:$(PROD_PORT)/"
	@echo "  → API: http://localhost:$(PROD_PORT)/api/coves"
	@echo "  data:  $(PROD_DATA_DIR)"
	@echo "  shell: $(LOCAL_SHELL)"
	@# Issue #388 Phase 1 — calm-server now talks to calm-proc-supervisor
	@# over a control UDS for every terminal spawn. Production has neige-app
	@# peer-supervising both; for `make prod` we background the supervisor
	@# on the same sock path calm-server resolves
	@# (CALM_PROC_SUPERVISOR_SOCK env, falling back to
	@# `CALM_DATA_DIR/proc-supervisor.sock`), wait for it to listen, then
	@# exec calm-server. A trap on EXIT reaps the supervisor when the
	@# foreground calm-server stops, so Ctrl-C doesn't leave it dangling.
	env \
	  CALM_LISTEN="$(PROD_LISTEN)" \
	  CALM_ALLOWED_ORIGIN="$(PROD_ALLOWED_ORIGIN)" \
	  CALM_DB_URL="$(PROD_DB_URL)" \
	  CALM_DATA_DIR="$(PROD_DATA_DIR)" \
	  CALM_PLUGINS_DATA_DIR="$(PROD_PLUGINS_DATA_DIR)" \
	  CALM_WEB_DIST="$(DIST)" \
	  CALM_MCP_STDIO_SHIM_BIN="$(LOCAL_MCP_STDIO_SHIM)" \
	  CALM_AUTH_USERNAME="$(PROD_AUTH_USERNAME)" \
	  CALM_AUTH_PASSWORD="$(PROD_AUTH_PASSWORD)" \
	  CALM_DEV_AUTOLOGIN="$(PROD_DEV_AUTOLOGIN)" \
	  SHELL="$(LOCAL_SHELL)" \
	  PROC_SUPERVISOR_BIN="$(PROC_SUP)" \
	  CALM_SERVER_BIN="$(BIN)" \
	  PROD_DATA_DIR="$(PROD_DATA_DIR)" \
	  sh -c '\
	    SOCK="$${CALM_PROC_SUPERVISOR_SOCK:-$$PROD_DATA_DIR/proc-supervisor.sock}"; \
	    rm -f "$$SOCK"; \
	    "$$PROC_SUPERVISOR_BIN" --control-sock "$$SOCK" & \
	    sup_pid=$$!; \
	    trap "kill -TERM $$sup_pid 2>/dev/null; wait $$sup_pid 2>/dev/null" EXIT INT TERM; \
	    until [ -S "$$SOCK" ]; do sleep 0.1; done; \
	    CALM_PROC_SUPERVISOR_SOCK="$$SOCK" exec "$$CALM_SERVER_BIN" \
	  '

# ---- housekeeping ------------------------------------------------------

.PHONY: dirs
dirs:
	@mkdir -p $(XDG_DIRS)

.PHONY: prod-dirs
prod-dirs:
	@mkdir -p "$(PROD_DATA_DIR)" "$(PROD_PLUGINS_DATA_DIR)"

.PHONY: clean
clean: ## Remove build artifacts (target/, web/dist).
	cargo clean --manifest-path $(WORKTREE)/Cargo.toml
	rm -rf $(DIST)

.PHONY: clean-data
clean-data: ## Remove the sqlite db and plugin state (DANGEROUS).
	@read -p "wipe ~/.local/share/neige-calm? [y/N] " ans; \
	  [ "$$ans" = "y" ] && rm -rf $(HOME)/.local/share/neige-calm/* || echo "aborted"

.PHONY: reset-db
reset-db: ## Back up + remove the local sqlite DB (called by `make dev RESET_DB=1`).
	@if [ -f "$(CALM_DB_PATH)" ]; then \
	  ts=$$(date +%s); \
	  cp "$(CALM_DB_PATH)" "$(CALM_DB_PATH).bak-make-dev-$$ts"; \
	  rm "$(CALM_DB_PATH)"; \
	  echo "  reset-db: backed up + removed $(CALM_DB_PATH) (backup: $(CALM_DB_PATH).bak-make-dev-$$ts)"; \
	else \
	  echo "  reset-db: $(CALM_DB_PATH) not present, nothing to back up"; \
	fi
	@if [ -d "$(CALM_TERMINALS_DIR)" ]; then \
	  rm -f "$(CALM_TERMINALS_DIR)"/*.sock 2>/dev/null || true; \
	  echo "  reset-db: removed stale terminal sock files in $(CALM_TERMINALS_DIR)/"; \
	fi
