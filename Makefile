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
DAEMON   := $(WORKTREE)/target/release/calm-session-daemon
BRIDGE   := $(WORKTREE)/target/release/neige-codex-bridge
# Issue #236 followup — kernel-as-MCP-server stdio bridge. Codex inside
# the docker container spawns this per spec/worker card (config.toml's
# `[mcp_servers.calm].command`); without it the handshake exits with
# `os error 2`. docker-compose.yml bind-mounts the built binary into
# /usr/local/bin/.
MCP_SHIM := $(WORKTREE)/target/release/neige-mcp-stdio-shim
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
export CALM_DAEMON_BIN := $(DAEMON)
export CALM_CODEX_BRIDGE_BIN := $(BRIDGE)
export CALM_MCP_SHIM_BIN := $(MCP_SHIM)
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
	  /^[a-zA-Z_-]+:.*?##/ { printf "  \033[36m%-12s\033[0m %s\n", $$1, $$2 }' $(MAKEFILE_LIST)
	@echo ""
	@echo "Dev port:  $(CALM_PORT) (auto from PORT_BASE=$(PORT_BASE); override: CALM_PORT=18080 make dev)"
	@echo "Worktree:  $(WORKTREE) (override: WORKTREE=/path/to/other-worktree make dev)"
	@echo "Dev ID:    $(DEV_ID) (override: DEV_ID=pr315 make dev-fresh)"
	@echo "Dev state: container $(CALM_CONTAINER_STATE_DIR) tmpfs"
	@echo "Prod:      make prod (host process, port $(PROD_PORT), local shell: $(LOCAL_SHELL))"

# ---- build (on host, not in docker) -------------------------------------

.PHONY: build
build: $(BIN) $(DAEMON) $(BRIDGE) $(MCP_SHIM) $(NEIGE_CLI) $(DIST) ## Build server, daemon, codex bridge, mcp-stdio shim, neige CLI, web bundle.

# Single cargo invocation builds all four binaries — cheaper than four
# separate calls because deps overlap. Touch every output so the rule
# re-fires only when sources change. Issue #236 followup added the
# `neige-mcp-stdio-shim` binary to the list; the docker-compose stack
# bind-mounts it into /usr/local/bin so codex can spawn it per-card.
$(BIN) $(DAEMON) $(BRIDGE) $(MCP_SHIM) $(NEIGE_CLI) &: $(shell find $(WORKTREE)/crates -name '*.rs' -o -name 'Cargo.toml' 2>/dev/null) $(WORKTREE)/Cargo.toml $(WORKTREE)/Cargo.lock
	cargo build --manifest-path $(WORKTREE)/Cargo.toml --release -p calm-server -p calm-session -p calm-codex-bridge -p neige-mcp-stdio-shim -p neige-cli --bin calm-server --bin calm-session-daemon --bin neige-codex-bridge --bin neige-mcp-stdio-shim --bin neige

# npm rewrites node_modules/.package-lock.json after npm ci/install, so use
# it as the dependency stamp for lockfile-driven web installs. Match CI's
# --legacy-peer-deps incantation; see ci.yml note / TODO(#2).
$(NODE_MODULES_STAMP): $(WORKTREE)/web/package-lock.json
	cd $(WORKTREE)/web && npm ci --legacy-peer-deps

$(DIST): $(shell find $(WORKTREE)/web/src -type f 2>/dev/null) $(WORKTREE)/web/package.json $(WORKTREE)/web/vite.config.ts $(WORKTREE)/web/index.html $(NODE_MODULES_STAMP)
	cd $(WORKTREE)/web && npm run build

# ---- docker lifecycle ---------------------------------------------------

.PHONY: dev
dev: build dirs ## Build, then bring the stack up in the background (FRESH=1 wipes this DEV_ID first).
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
dev-fresh: build ## Remove this DEV_ID's containers/state, then start a fresh stack.
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
up: dirs ## Bring the stack up without rebuilding.
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
	env \
	  CALM_LISTEN="$(PROD_LISTEN)" \
	  CALM_ALLOWED_ORIGIN="http://localhost:$(PROD_PORT)" \
	  CALM_DB_URL="$(PROD_DB_URL)" \
	  CALM_DATA_DIR="$(PROD_DATA_DIR)" \
	  CALM_PLUGINS_DATA_DIR="$(PROD_PLUGINS_DATA_DIR)" \
	  CALM_WEB_DIST="$(DIST)" \
	  CALM_MCP_STDIO_SHIM_BIN="$(LOCAL_MCP_STDIO_SHIM)" \
	  CALM_AUTH_USERNAME="$(PROD_AUTH_USERNAME)" \
	  CALM_AUTH_PASSWORD="$(PROD_AUTH_PASSWORD)" \
	  CALM_DEV_AUTOLOGIN="$(PROD_DEV_AUTOLOGIN)" \
	  SHELL="$(LOCAL_SHELL)" \
	  "$(BIN)"

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
