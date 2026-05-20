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

BIN := target/release/calm-server
DAEMON := target/release/calm-session-daemon
BRIDGE := target/release/neige-codex-bridge
DIST := web/dist

.PHONY: help
help: ## Show this help.
	@awk 'BEGIN {FS = ":.*##"; printf "Targets:\n"} \
	  /^[a-zA-Z_-]+:.*?##/ { printf "  \033[36m%-12s\033[0m %s\n", $$1, $$2 }' $(MAKEFILE_LIST)
	@echo ""
	@echo "Host port: $(CALM_PORT) (override: CALM_PORT=18080 make dev)"

# ---- build (on host, not in docker) -------------------------------------

.PHONY: build
build: $(BIN) $(DAEMON) $(BRIDGE) $(DIST) ## Build server, daemon, codex bridge, web bundle.

# Single cargo invocation builds all three binaries — cheaper than three
# separate calls because deps overlap. Touch every output so the rule
# re-fires only when sources change.
$(BIN) $(DAEMON) $(BRIDGE) &: $(shell find crates -name '*.rs' -o -name 'Cargo.toml' 2>/dev/null) Cargo.toml Cargo.lock
	cargo build --release -p calm-server -p calm-session -p calm-codex-bridge --bin calm-server --bin calm-session-daemon --bin neige-codex-bridge

$(DIST): $(shell find web/src -type f 2>/dev/null) web/package.json web/vite.config.ts web/index.html
	@if [ ! -d web/node_modules ]; then (cd web && npm install); fi
	cd web && npm run build

# ---- docker lifecycle ---------------------------------------------------

.PHONY: dev
dev: build dirs ## Build, then bring the stack up in the background.
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
	cargo clean
	rm -rf $(DIST)

.PHONY: clean-data
clean-data: ## Remove the sqlite db and plugin state (DANGEROUS).
	@read -p "wipe ~/.local/share/neige-calm? [y/N] " ans; \
	  [ "$$ans" = "y" ] && rm -rf $(HOME)/.local/share/neige-calm/* || echo "aborted"
