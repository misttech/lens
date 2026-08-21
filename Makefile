# Copyright 2026 Mist Tecnologia LTDA. All rights reserved.
# SPDX-License-Identifier: Apache-2.0

# Lens — dev convenience. Single Rust binary, everything into out/.

include build/arch.mk
include build/cargo.mk
include build/bench.mk

.PHONY: all help clean

all: check test build ## check, test, then build

help: ## list targets
	@grep -hE '^[a-z-]+:.*##' $(MAKEFILE_LIST) | sed 's/:.*##/\t/' | sort | expand -t18

clean: ## remove out/
	rm -rf $(OUT)
