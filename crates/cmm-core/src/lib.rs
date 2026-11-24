//! clean-my-mac core library. See docs/ARCHITECTURE.md for the contract.

pub mod actions;
pub mod config;
pub mod detectors;
pub mod error;
pub mod guard;
pub mod history;
pub mod model;
pub mod process;
pub mod rules;
pub mod scan;
pub mod size;
pub mod volume;

pub use config::Config;
pub use error::CoreError;
pub use model::*;
pub use scan::{ScanContext, scan};
