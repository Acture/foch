//! Test-only product acceptance support.
//!
//! This module deliberately lives beside the `foch-cli` integration test: it
//! measures the public `foch` executable and is not part of any shipped crate.

#![allow(dead_code)]

pub mod config;
pub mod corpus;
pub mod dataset;
pub mod evidence_store;
pub mod lifecycle;
pub mod orchestrate;
pub mod report;
pub mod runner;
pub mod score;
pub mod workshop_inputs;
