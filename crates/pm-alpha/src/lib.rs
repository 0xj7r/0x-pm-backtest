//! pm-alpha — canonical exogenous signal + edge-model layer (signal SSOT).
//!
//! The belief (probability that the market resolves Up/YES) is a pure function
//! of exogenous data: CEX spot, realized vol, time. The Polymarket book never
//! enters the belief — structurally: [`state::ExoState`] contains no book data,
//! and [`model::belief`] accepts only an `ExoState`. Price is used solely
//! downstream as bet cost (`edge = p_exo - price`), inside [`harness`].
//!
//! Design: docs/superpowers/specs/2026-06-09-signal-ssot-design.md

pub mod calibrator;
pub mod fair_value;
pub mod harness;
pub mod model;
pub mod regime;
pub mod state;
pub mod vol;

pub use calibrator::{ExoCalibrator, ExoCalibratorSnapshot, ExoFeatures, TrainingConfig, TrainingSample};
pub use fair_value::{FairValueEstimate, FairValueModel, NoSignalReason};
pub use model::{AlphaModel, AlphaModelConfig, Belief, Evaluation};
pub use regime::Regime;
pub use state::{ExoState, MarketMeta, PerpState, Token};
