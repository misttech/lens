# Copyright 2026 Mist Tecnologia LTDA. All rights reserved.
# SPDX-License-Identifier: Apache-2.0

# Cargo invocation and the targets built on it.
# Included by the top-level Makefile; needs build/arch.mk for OUT and BIN.
#
# CARGO_TARGET_DIR is pinned under out/ so cargo's intermediates cannot land in
# target/ — or, in a sandbox with its own CARGO_TARGET_DIR, somewhere that
# desyncs from where `install` looks for the binary. `-u RUSTC` and the PATH
# prepend keep a system rustc from desyncing with rustup's cargo, which
# rust-toolchain.toml pins to stable.
#
# rustfmt.toml sets imports_granularity, which is nightly-only, so fmt runs
# cargo +nightly. Everything else stays on the pinned stable toolchain.

CARGO_TARGET_DIR := $(OUT)/.cargo
CARGO := env -u RUSTC CARGO_TARGET_DIR=$(CARGO_TARGET_DIR) PATH="$(HOME)/.cargo/bin:$(PATH)" cargo

.PHONY: build test check fmt clippy

build: ## release binary into out/<target>/<arch>/lens
	$(CARGO) build --release
	@mkdir -p $(BIN)
	install -m 755 $(CARGO_TARGET_DIR)/release/lens $(BIN)/lens
	@echo "==> $(BIN)/lens"

test: ## unit, integration and property tests
	$(CARGO) test --all-targets

check: fmt clippy ## format check and lints

fmt: ## rustfmt --check (nightly: rustfmt.toml uses imports_granularity)
	$(CARGO) +nightly fmt --check

clippy: ## clippy with warnings denied
	$(CARGO) clippy --all-targets -- -D warnings
