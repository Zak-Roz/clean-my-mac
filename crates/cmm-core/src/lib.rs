//! clean-my-mac core library. See docs/ARCHITECTURE.md for the design.

pub mod actions;
pub mod agent;
pub mod config;
pub mod detectors;
pub mod error;
pub mod guard;
pub mod history;
pub mod model;
pub mod monitor;
pub mod process;
pub mod rules;
pub mod scan;
pub mod size;
pub mod storage;
pub mod volume;

pub use config::Config;
pub use error::CoreError;
pub use model::*;
pub use scan::{ScanContext, scan, scan_at};
