# Copyright 2026 Mist Tecnologia LTDA. All rights reserved.
# SPDX-License-Identifier: Apache-2.0

# Benchmarks. Two kinds, and they are not interchangeable (LENS.md §13).
# Included by the top-level Makefile; needs build/cargo.mk for CARGO.
#
#   make bench      micro-benchmarks: per-stage latency and throughput against
#                   tests/fixtures, gated in CI on a >20% p50 regression.
#   make retention  the retention benchmark: drives a real coding agent over the
#                   task suite at every budget in the sweep. Slow, nondeterministic
#                   and it spends API credits, so it never runs per-commit — by
#                   hand and nightly only.
#
# Both land under bench/results/. The number that matters is the knee of the
# retention curve, not the compression ratio.

.PHONY: bench retention

bench: ## micro-benchmarks against the committed baseline
	$(CARGO) bench

retention: ## retention benchmark: slow, nondeterministic, spends API credits
	$(CARGO) run --release --bin lens-bench
