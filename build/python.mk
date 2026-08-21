# Copyright 2026 Mist Tecnologia LTDA. All rights reserved.
# SPDX-License-Identifier: Apache-2.0

# Repository tooling is written in Python and formatted with ruff, which is
# black-compatible — one tool replaces black, isort and flake8. See ruff.toml.
#
# The targets are no-ops until a tool exists, so `make check` is safe to run on
# a tree with no Python in it. They exist now so the first script added is
# already covered rather than retrofitted.

PY_DIRS := tools bench
PY_SOURCES := $(shell find $(PY_DIRS) -name '*.py' 2>/dev/null)

# A ruff on PATH wins: it needs no trust prompt and is what CI installs. mise is
# the fallback, so a machine that pins tools through mise.toml still gets the
# pinned version without installing anything globally.
RUFF := $(shell command -v ruff 2>/dev/null)
ifeq ($(RUFF),)
  MISE := $(shell command -v mise 2>/dev/null)
  ifneq ($(MISE),)
    RUFF := $(MISE) exec -- ruff
  endif
endif

.PHONY: fmt-py fmt-py-check lint-py

# Every target is skipped when there is no Python to check, and fails loudly
# when there is Python but no ruff — a silent skip is how a lint gate stops
# being one.
fmt-py: ## format the Python tooling
ifneq ($(PY_SOURCES),)
	@test -n "$(RUFF)" || { echo "ruff not found: pip install ruff, or install mise"; exit 1; }
	$(RUFF) format $(PY_SOURCES)
	$(RUFF) check --fix-only $(PY_SOURCES)
endif

fmt-py-check: ## verify Python formatting without rewriting
ifneq ($(PY_SOURCES),)
	@test -n "$(RUFF)" || { echo "ruff not found: pip install ruff, or install mise"; exit 1; }
	$(RUFF) format --check $(PY_SOURCES)
endif

lint-py: ## lint the Python tooling
ifneq ($(PY_SOURCES),)
	@test -n "$(RUFF)" || { echo "ruff not found: pip install ruff, or install mise"; exit 1; }
	$(RUFF) check $(PY_SOURCES)
endif
