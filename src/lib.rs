// Copyright 2026 Mist Tecnologia LTDA. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//! Lens runs a real command, keeps its full output, and shows an AI coding agent
//! the view of that output worth spending context on.
//!
//! The command runs, every byte it produced goes to a content-addressed store,
//! and a filtered view of it reaches the caller. Anything left out of that view
//! is announced with a marker carrying the handle, so a reader always knows the
//! rest is a request away.
//!
//! This library exists so the benchmarks can measure the pipeline directly. The
//! binary is a thin caller over it: argument handling, process control, and the
//! decisions that need an environment.

pub mod adapters;
pub mod cli;
pub mod executor;
pub mod log;
pub mod pipeline;
pub mod platform;
pub mod render;
pub mod report;
pub mod resolve;
pub mod static_assert;
pub mod store;
pub mod tokens;
