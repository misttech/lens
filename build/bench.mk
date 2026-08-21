# Copyright 2026 Mist Tecnologia LTDA. All rights reserved.
# SPDX-License-Identifier: Apache-2.0

# Benchmarks. Two kinds, and they are not interchangeable.
# Included by the top-level Makefile; needs build/cargo.mk for CARGO.
#
#   make bench      micro-benchmarks: per-stage latency and growth against
#                   tests/fixtures. Growth is the CI gate, because it compares a
#                   machine against itself; latency is reported, because absolute
#                   microseconds belong to the machine that recorded them.
#   make retention  the retention benchmark: drives a real coding agent over the
#                   task suite at every budget in the sweep. Slow, nondeterministic
#                   and it spends API credits, so it never runs per-commit — by
#                   hand and nightly only.
#
# Both land under bench/results/. The number that matters is the knee of the
# retention curve, not the compression ratio.

.PHONY: bench bench-save retention

bench: ## micro-benchmarks: gate on growth, report latency
	$(CARGO) bench --bench pipeline

bench-save: ## rewrite the committed latency baseline from this machine
	$(CARGO) bench --bench pipeline -- --save

retention: ## retention benchmark: slow, nondeterministic, spends API credits
	$(CARGO) run --release --bin lens-bench
