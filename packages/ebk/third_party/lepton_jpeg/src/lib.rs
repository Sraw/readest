/*---------------------------------------------------------------------------------------------
 *  Copyright (c) Microsoft Corporation. All rights reserved.
 *  Licensed under the Apache License, Version 2.0. See LICENSE.txt in the project root for license information.
 *  This software incorporates material from third parties. See NOTICE.txt for details.
 *--------------------------------------------------------------------------------------------*/
// Modified for the EBK project: see EBK-CHANGES.md at the root of this crate.

//! A lossless JPEG compressor with precise bit-for-bit recovery, supporting both baseline and progressive JPEGs.
//! Achieves compression savings of around 22%, making it suitable for cold cloud storage use cases.
//!
//! This crate is a Rust port of Dropbox’s original [lepton](https://github.com/dropbox/lepton) JPEG compression tool.
//! It retains the performance characteristics of the C++ version while benefiting from Rust’s memory safety guarantees.
//! All JPEG content—including metadata and even malformed segments—is preserved accurately.
//!
//! The original C++ codebase has been deprecated by Dropbox. This Rust implementation incorporates
//! an exhaustive security review of the original, making it a safer and more maintainable alternative.

// Don't allow any unsafe code by default. Since this code has to potentially deal with
// badly/maliciously formatted images, we want this extra level of safety.
#![forbid(unsafe_code)]
#![forbid(trivial_casts)]
#![forbid(trivial_numeric_casts)]
#![forbid(non_ascii_idents)]
#![forbid(unused_extern_crates)]
#![forbid(unused_import_braces)]
#![forbid(redundant_lifetimes)]
#![forbid(single_use_lifetimes)]
#![forbid(unused_extern_crates)]
#![forbid(unused_lifetimes)]
#![forbid(unused_macro_rules)]
#![forbid(macro_use_extern_crate)]
#![forbid(missing_unsafe_on_extern)]
#![deny(missing_docs)]

mod consts;
mod helpers;
mod jpeg;
mod metrics;
mod structs;

mod enabled_features;
mod lepton_error;

pub use enabled_features::EnabledFeatures;
pub use helpers::catch_unwind_result;
pub use lepton_error::{ExitCode, LeptonError};
pub use metrics::{CpuTimeMeasure, Metrics};

use crate::lepton_error::Result;
pub use crate::structs::simple_threadpool::{
    DEFAULT_THREAD_POOL, LeptonThreadPool, LeptonThreadPriority, SimpleThreadPool,
    SingleThreadPool, ThreadPoolHolder,
};

/// Trait for types that can provide the current position in a stream. This
/// is intentionally a subset of the Seek trait, as it only requires remembering
/// the current position without allowing seeking to arbitrary positions.
///
/// This is useful for callers for which it would be complex to provide seek capabilities, but can
/// count the number of bytes read or written so far.
///
/// We provide a blanket implementation for any type that implements `std::io::Seek`.
pub trait StreamPosition {
    /// Returns the current position in the stream.
    fn position(&mut self) -> u64;
}

impl<T: std::io::Seek> StreamPosition for T {
    fn position(&mut self) -> u64 {
        self.stream_position().unwrap()
    }
}

pub use structs::lepton_file_reader::decode_lepton;

pub use structs::lepton_file_reader::LeptonFileReader;
