# neige-calm dev orchestration.
#
# The docker stack runs the *host-built* binary (no rust toolchain in the
# image), bind-mounts $HOME at the same path inside the container, and
# publishes a single host port (CALM_PORT, default 4040) via nginx.
#
#   make dev     # build + bring stack up in background
#   make stop    # tear stack down
#   make logs    # tail logs
#   make help    # everything

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

CALM_PORT ?= 4040
export CALM_PORT

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
# Caveat: the docker stack uses fixed container names
# (neige-calm-server-1 etc.) and the host port CALM_PORT — running two
# worktrees' stacks simultaneously will collide. Stop one before starting
# the other, or override CALM_PORT + COMPOSE_PROJECT_NAME.
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
DIST     := $(WORKTREE)/web/dist

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
	@echo "Host port: $(CALM_PORT) (override: CALM_PORT=18080 make dev)"
	@echo "Worktree:  $(WORKTREE) (override: WORKTREE=/path/to/other-worktree make dev)"

# ---- build (on host, not in docker) -------------------------------------

.PHONY: build
build: $(BIN) $(DAEMON) $(BRIDGE) $(MCP_SHIM) $(DIST) ## Build server, daemon, codex bridge, mcp-stdio shim, web bundle.

# Single cargo invocation builds all four binaries — cheaper than four
# separate calls because deps overlap. Touch every output so the rule
# re-fires only when sources change. Issue #236 followup added the
# `neige-mcp-stdio-shim` binary to the list; the docker-compose stack
# bind-mounts it into /usr/local/bin so codex can spawn it per-card.
$(BIN) $(DAEMON) $(BRIDGE) $(MCP_SHIM) &: $(shell find $(WORKTREE)/crates -name '*.rs' -o -name 'Cargo.toml' 2>/dev/null) $(WORKTREE)/Cargo.toml $(WORKTREE)/Cargo.lock
	cargo build --manifest-path $(WORKTREE)/Cargo.toml --release -p calm-server -p calm-session -p calm-codex-bridge -p neige-mcp-stdio-shim --bin calm-server --bin calm-session-daemon --bin neige-codex-bridge --bin neige-mcp-stdio-shim

$(DIST): $(shell find $(WORKTREE)/web/src -type f 2>/dev/null) $(WORKTREE)/web/package.json $(WORKTREE)/web/vite.config.ts $(WORKTREE)/web/index.html
	@if [ ! -d $(WORKTREE)/web/node_modules ]; then (cd $(WORKTREE)/web && npm install); fi
	cd $(WORKTREE)/web && npm run build

# ---- docker lifecycle ---------------------------------------------------

.PHONY: dev
dev: build dirs ## Build, then bring the stack up in the background (RESET_DB=1 wipes db first).
ifeq ($(RESET_DB),1)
	@echo "  RESET_DB=1 — stopping stack, wiping DB, then bringing up"
	-$(COMPOSE) down --remove-orphans
	$(MAKE) reset-db
endif
	$(COMPOSE) up -d
	@echo ""
	@echo "  → http://localhost:$(CALM_PORT)/"
	@echo "  → API: http://localhost:$(CALM_PORT)/api/coves"
	@echo "  logs: make logs"

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

# ---- housekeeping ------------------------------------------------------

.PHONY: dirs
dirs:
	@mkdir -p $(XDG_DIRS)

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
