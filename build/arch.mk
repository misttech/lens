# Copyright 2026 Mist Tecnologia LTDA. All rights reserved.
# SPDX-License-Identifier: Apache-2.0

# Arch / output-path detection for Lens builds.
# Included by the top-level Makefile.
#
# Layout: out/<target>/<arch>/lens, where <target> is the OS (linux, darwin)
# and <arch> is amd64 or arm64. out/.cargo holds cargo's intermediates, so the
# whole build is one ignorable directory.
#
# Override either half when cross-building: make build TARGET=darwin ARCH=arm64

OUT := $(CURDIR)/out

TARGET ?= $(shell uname -s | tr 'A-Z' 'a-z')
ARCH ?= $(shell uname -m | sed -e 's/^x86_64$$/amd64/' -e 's/^aarch64$$/arm64/')

BIN := $(OUT)/$(TARGET)/$(ARCH)

ifeq ($(filter linux darwin,$(TARGET)),)
$(error unsupported TARGET $(TARGET) — Lens builds on linux (verified) and darwin (placeholder))
endif
ifeq ($(filter amd64 arm64,$(ARCH)),)
$(error unsupported ARCH $(ARCH) — expected amd64 or arm64)
endif
