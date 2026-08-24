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
pub mod decide;
pub mod directional;
#[doc(hidden)]
pub mod equivalence;
#[cfg(test)]
mod decide_construction_parity;
pub mod fair_value;
pub mod fair_value_twap;
pub mod fingerprint;
pub mod harness;
pub mod model;
pub mod regime;
pub mod state;
pub mod vol;

pub use calibrator::{ExoCalibrator, ExoCalibratorSnapshot, ExoFeatures, TrainingConfig, TrainingSample};
pub use decide::{
    DecideConfig, DecisionInputs, EntryAction, EntryDecision, EntryState, EntryStateDelta,
    SessionGateState, decide_entry, frozen_fade_decide_config, session_gates_active,
    session_observe_trades,
};
pub use directional::{DIR_FEATURE_NAMES, DIR_FEATURES, DirFeatures, DirModel, clean_directional_pressure, dir_features};
pub use fair_value::{FairValueEstimate, FairValueModel, NoSignalReason};
pub use fair_value_twap::twap_digital;
pub use model::{AlphaModel, AlphaModelConfig, Belief, Evaluation};
pub use regime::Regime;
pub use state::{ExoState, MarketMeta, PerpState, Token};
pub use vol::VolEstimator;
