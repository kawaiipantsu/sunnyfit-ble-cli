.DEFAULT_GOAL := help
SHELL := /bin/bash
CARGO ?= cargo
PREFIX ?= /usr/local
DESTDIR ?=
FEATURES ?=
CARGO_FEATURES = $(if $(FEATURES),--features $(FEATURES),)

.PHONY: help build release check test integration fmt lint deb install uninstall clean run demo doctor deps deploy
help: ## Show build targets
	@awk 'BEGIN {FS = ":.*## "} /^[a-zA-Z_-]+:.*## / {printf "  %-12s %s\n", $$1, $$2}' $(MAKEFILE_LIST)
build: ## Build the development binary
	$(CARGO) build --locked $(CARGO_FEATURES)
release: ## Build the optimized binary
	$(CARGO) build --release --locked $(CARGO_FEATURES)
fmt: ## Format Rust sources
	$(CARGO) fmt
lint: ## Check formatting and Clippy warnings
	$(CARGO) fmt --check
	$(CARGO) clippy --locked --all-targets $(CARGO_FEATURES) -- -D warnings
test: ## Run decoder and application tests
	$(CARGO) test --locked $(CARGO_FEATURES)
integration: build ## Exercise CLI, TUI, signals, replay and isolated Redis
	python3 tests/integration.py
	python3 tests/features.py
	python3 tests/live-stream.py
check: lint test integration ## Run required source checks

deb: release ## Build a Debian package in dist/
	@python3 scripts/build-deb.py
install: release ## Install locally (use sudo or DESTDIR for staging)
	install -Dm755 target/release/sunnyfit-ble-cli $(DESTDIR)$(PREFIX)/bin/sunnyfit-ble-cli
uninstall: ## Remove the locally installed executable
	rm -f $(DESTDIR)$(PREFIX)/bin/sunnyfit-ble-cli
run: build ## Start using saved configuration
	./target/debug/sunnyfit-ble-cli $(ARGS)
demo: build ## Preview the workout TUI without Bluetooth
	./target/debug/sunnyfit-ble-cli --demo --workout --met 3 $(ARGS)
doctor: build ## Check Bluetooth and firmware
	./target/debug/sunnyfit-ble-cli doctor
deps: ## Install Debian build/runtime prerequisites (run as root)
	apt-get install build-essential pkg-config libdbus-1-dev bluez firmware-realtek dpkg-dev python3 redis-server redis-tools
clean: ## Remove build outputs
	$(CARGO) clean
	rm -rf dist/stage
deploy: deb ## Show private APT deployment readiness; no implicit publication
	@python3 scripts/deploy.py --check
