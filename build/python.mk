# Copyright 2026 Mist Tecnologia LTDA. All rights reserved.
# SPDX-License-Identifier: Apache-2.0

# Repository tooling is written in Python and formatted with ruff, which is
# black-compatible — one tool replaces black, isort and flake8. See ruff.toml.
#
# The targets are no-ops until a tool exists, so `make check` is safe to run on
# a tree with no Python in it. They exist now so the first script added is
# already covered rather than retrofitted.

TOOLS := tools
PY_SOURCES := $(shell find $(TOOLS) -name '*.py' 2>/dev/null)

# mise pins python and ruff; without it, fall back to whatever is on PATH so a
# machine without mise can still run these.
MISE := $(shell command -v mise 2>/dev/null)
ifeq ($(MISE),)
  PY_RUN :=
else
  PY_RUN := $(MISE) exec --
endif
RUFF := $(PY_RUN) ruff

.PHONY: fmt-py fmt-py-check lint-py

fmt-py: ## format the Python tooling
ifneq ($(PY_SOURCES),)
	$(RUFF) format $(PY_SOURCES)
	$(RUFF) check --fix-only $(PY_SOURCES)
endif

fmt-py-check: ## verify Python formatting without rewriting
ifneq ($(PY_SOURCES),)
	$(RUFF) format --check $(PY_SOURCES)
endif

lint-py: ## lint the Python tooling
ifneq ($(PY_SOURCES),)
	$(RUFF) check $(PY_SOURCES)
endif
