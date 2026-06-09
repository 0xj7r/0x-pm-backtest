//! Exogenous-only calibration layer: Beta + Isotonic + gradient-boosted
//! stumps over the raw fair-value probability.
//!
//! Machinery lifted from `pm-model`'s `OnlineMetaCalibrator`
//! (pm-model/src/lib.rs:1009) and re-instantiated with a compact
//! exogenous feature vector. The structural difference from pm-model is the
//! contract: features are built from [`ExoState`] + [`Belief`] only — there
//! is no constructor that accepts book data, so price cannot enter the
//! calibrated belief (spec section 4.4: trained against realized outcomes
//! only, never price).

use crate::model::Belief;
use crate::state::ExoState;
use serde::{Deserialize, Serialize};

pub const EXO_FEATURES: usize = 16;

pub const EXO_FEATURE_NAMES: [&str; EXO_FEATURES] = [
    "z_signed",            // BSM d = log_moneyness / sigma_remaining
    "z_abs",
    "tau_fraction",
    "sigma_bar",           // bar vol, bps / 50
    "delta_bps",           // (S-K)/K in bps / 10
    "mom_30s_sigma",       // 30s drift in remaining-bar sigma units
    "mom_120s_sigma",
    "mom_300s_sigma",
    "mom_accel",           // (r30 - r120/4) in sigma units
    "flow_imbalance_60s",  // signed CEX taker flow, [-1, 1]
    "flow_intensity",      // ln(1 + trades/s) / 5
    "large_adverse",       // large opposing prints in 60s, capped at 5, / 5
    "vol_ratio_short_long",// sigma(300s) / sigma(lookback) - 1
    "tod_sin",             // UTC time-of-day cycle
    "tod_cos",
    "base_p_centered",     // raw fair value - 0.5
];

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ExoFeatures {
    pub values: [f32; EXO_FEATURES],
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct TrainingSample {
    pub features: ExoFeatures,
    pub base_side_probability: f32,
    pub side_observed: bool,
}

/// Build the exogenous feature vector at one decision instant. Takes only
/// the exogenous state and the raw belief — book data cannot reach this.
pub fn exo_features(state: &ExoState, belief: &Belief, vol_lookback_s: u32) -> ExoFeatures {
    let est = &belief.estimate;
    let sigma_rem = if est.sigma_remaining.is_finite() && est.sigma_remaining > 1e-9 {
        est.sigma_remaining
    } else {
        1e-9
    };
    let z = (est.log_moneyness / sigma_rem).clamp(-6.0, 6.0);
    let tau = state.tau_fraction();
    let sigma_bar = belief.sigma_bar_bps;

    let spot_now = state.spot_now().unwrap_or(f64::NAN);
    let strike = state.market.strike;
    let delta_bps = if spot_now.is_finite() && strike > 0.0 {
        ((spot_now - strike) / strike * 10_000.0).clamp(-300.0, 300.0)
    } else {
        0.0
    };

    // Momentum in remaining-bar sigma units: r_w / (sigma_bar * sqrt(w/bar)).
    let bar_s = state.market.window_secs.max(1) as f64;
    let sigma_frac = (sigma_bar / 10_000.0).max(1e-9);
    let mom_sigma = |w_s: i64| -> f32 {
        let r = state
            .spot
            .trailing_return(state.now_ns, w_s * 1_000_000_000)
            .unwrap_or(0.0);
        let scale = sigma_frac * (w_s as f64 / bar_s).sqrt().max(1e-6);
        ((r / scale).clamp(-8.0, 8.0)) as f32
    };
    let m30 = mom_sigma(30);
    let m120 = mom_sigma(120);
    let m300 = mom_sigma(300);

    let flow = state
        .spot
        .signed_flow_and_adverse(state.now_ns, 60_000_000_000, true);
    let intensity = ((1.0 + flow.intensity).ln() / 5.0).clamp(0.0, 2.0) as f32;
    let large_adverse = (flow.large_adverse_count.min(5) as f32) / 5.0;

    let vol_short = crate::vol::realized_vol_bps_over_bar(
        state.spot,
        state.now_ns,
        300,
        1,
        state.market.window_secs,
    );
    let vol_ratio = match vol_short {
        Some(s) if sigma_bar > 1e-9 => ((s / sigma_bar - 1.0).clamp(-1.0, 3.0)) as f32,
        _ => 0.0,
    };
    let _ = vol_lookback_s; // sigma_bar already reflects the configured lookback

    let day_s = ((state.now_ns / 1_000_000_000).rem_euclid(86_400)) as f64;
    let phase = day_s / 86_400.0 * std::f64::consts::TAU;

    ExoFeatures {
        values: [
            z as f32,
            z.abs() as f32,
            tau as f32,
            (sigma_bar / 50.0).clamp(0.0, 10.0) as f32,
            (delta_bps / 10.0) as f32,
            m30,
            m120,
            m300,
            (m30 - m120 / 4.0).clamp(-8.0, 8.0),
            flow.imbalance.clamp(-1.0, 1.0) as f32,
            intensity,
            large_adverse,
            vol_ratio,
            phase.sin() as f32,
            phase.cos() as f32,
            (belief.p_up - 0.5) as f32,
        ],
    }
}

// Tuning constants (values carried over from pm-model).
const TREE_COUNT: usize = 10;
const TREE_LEARNING_RATE: f32 = 0.08;
const TREE_L2: f32 = 8.0;
const TREE_MIN_LEAF: usize = 128;
const TREE_MAX_TRAIN_SAMPLES: usize = 48_000;
const TREE_VALUE_CLIP: f32 = 0.65;
const BETA_MIN_SAMPLES: usize = 256;
const BETA_EPOCHS: usize = 96;
const BETA_LR: f32 = 0.03;
const BETA_L2: f32 = 0.01;
const BETA_COEFF_CLIP: f32 = 5.0;
const ISOTONIC_MIN_SAMPLES: usize = 256;
const ISOTONIC_SHRINKAGE: f32 = 500.0;
const MIN_UPDATES: u32 = 64;

fn sigmoid(x: f32) -> f32 {
    1.0 / (1.0 + (-x).exp())
}

fn logit(p: f32) -> f32 {
    let p = p.clamp(1e-6, 1.0 - 1e-6);
    (p / (1.0 - p)).ln()
}

pub fn binary_log_loss(p: f32, observed: bool) -> f32 {
    let p = p.clamp(1e-6, 1.0 - 1e-6);
    if observed { -p.ln() } else { -(1.0 - p).ln() }
}

#[derive(Debug, Clone, Copy)]
struct BetaCalibrator {
    a: f32,
    b: f32,
    c: f32,
    enabled: bool,
}

impl Default for BetaCalibrator {
    fn default() -> Self {
        Self {
            a: 1.0,
            b: -1.0,
            c: 0.0,
            enabled: false,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
pub struct BetaSnapshot {
    pub a: f32,
    pub b: f32,
    pub c: f32,
    pub enabled: bool,
}

impl Default for BetaSnapshot {
    fn default() -> Self {
        BetaCalibrator::default().snapshot()
    }
}

impl BetaCalibrator {
    fn from_snapshot(s: BetaSnapshot) -> Self {
        Self {
            a: s.a,
            b: s.b,
            c: s.c,
            enabled: s.enabled,
        }
    }

    fn snapshot(&self) -> BetaSnapshot {
        BetaSnapshot {
            a: self.a,
            b: self.b,
            c: self.c,
            enabled: self.enabled,
        }
    }

    fn predict(&self, base_probability: f32) -> f32 {
        let p = base_probability.clamp(1.0e-6, 1.0 - 1.0e-6);
        if !self.enabled {
            return p;
        }
        let z = self.a * p.ln() + self.b * (1.0 - p).ln() + self.c;
        sigmoid(z).clamp(1.0e-6, 1.0 - 1.0e-6)
    }

    fn fit(samples: &[TrainingSample]) -> Self {
        if samples.len() < BETA_MIN_SAMPLES {
            return Self::default();
        }
        let mut beta = Self {
            enabled: true,
            ..Self::default()
        };
        let mut prepared = Vec::with_capacity(samples.len());
        for sample in samples {
            let p = sample.base_side_probability.clamp(1.0e-6, 1.0 - 1.0e-6);
            prepared.push((
                p.ln(),
                (1.0 - p).ln(),
                if sample.side_observed { 1.0f32 } else { 0.0 },
            ));
        }
        let n = samples.len() as f32;
        for _ in 0..BETA_EPOCHS {
            let mut grad_a = 0.0;
            let mut grad_b = 0.0;
            let mut grad_c = 0.0;
            for (x_a, x_b, y) in &prepared {
                let pred = sigmoid(beta.a * x_a + beta.b * x_b + beta.c);
                let error = pred - *y;
                grad_a += error * *x_a;
                grad_b += error * *x_b;
                grad_c += error;
            }
            grad_a = grad_a / n + BETA_L2 * (beta.a - 1.0);
            grad_b = grad_b / n + BETA_L2 * (beta.b + 1.0);
            grad_c = grad_c / n + BETA_L2 * beta.c;
            beta.a = (beta.a - BETA_LR * grad_a).clamp(-BETA_COEFF_CLIP, BETA_COEFF_CLIP);
            beta.b = (beta.b - BETA_LR * grad_b).clamp(-BETA_COEFF_CLIP, BETA_COEFF_CLIP);
            beta.c = (beta.c - BETA_LR * grad_c).clamp(-BETA_COEFF_CLIP, BETA_COEFF_CLIP);
        }

        let raw_log_loss = samples
            .iter()
            .map(|s| binary_log_loss(s.base_side_probability, s.side_observed))
            .sum::<f32>()
            / n;
        let beta_log_loss = samples
            .iter()
            .zip(prepared.iter())
            .map(|(s, (x_a, x_b, _))| {
                binary_log_loss(sigmoid(beta.a * *x_a + beta.b * *x_b + beta.c), s.side_observed)
            })
            .sum::<f32>()
            / n;
        if beta_log_loss + 1.0e-4 < raw_log_loss {
            beta
        } else {
            Self::default()
        }
    }
}

#[derive(Debug, Clone, Default)]
struct IsotonicCalibrator {
    thresholds: Vec<f32>,
    values: Vec<f32>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct IsotonicSnapshot {
    pub thresholds: Vec<f32>,
    pub values: Vec<f32>,
}

impl IsotonicCalibrator {
    fn from_snapshot(s: IsotonicSnapshot) -> Self {
        Self {
            thresholds: s.thresholds,
            values: s.values,
        }
    }

    fn snapshot(&self) -> IsotonicSnapshot {
        IsotonicSnapshot {
            thresholds: self.thresholds.clone(),
            values: self.values.clone(),
        }
    }

    fn is_empty(&self) -> bool {
        self.thresholds.is_empty() || self.values.is_empty()
    }

    fn fit(samples: &[TrainingSample], beta: &BetaCalibrator) -> Self {
        if samples.len() < ISOTONIC_MIN_SAMPLES {
            return Self::default();
        }
        let mut pairs: Vec<(f32, f32)> = samples
            .iter()
            .map(|s| {
                (
                    beta.predict(s.base_side_probability).clamp(1.0e-6, 1.0 - 1.0e-6),
                    if s.side_observed { 1.0 } else { 0.0 },
                )
            })
            .collect();
        pairs.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));

        #[derive(Clone, Copy)]
        struct Block {
            max_x: f32,
            sum_y: f32,
            weight: f32,
        }

        let mut grouped: Vec<Block> = Vec::new();
        for (x, y) in pairs {
            if let Some(last) = grouped.last_mut()
                && (last.max_x - x).abs() <= 1.0e-7
            {
                last.sum_y += y;
                last.weight += 1.0;
                continue;
            }
            grouped.push(Block {
                max_x: x,
                sum_y: y,
                weight: 1.0,
            });
        }

        let total_weight = grouped.iter().map(|b| b.weight).sum::<f32>();
        let global_mean = (grouped.iter().map(|b| b.sum_y).sum::<f32>() / total_weight)
            .clamp(1.0e-4, 1.0 - 1.0e-4);
        let mut blocks: Vec<Block> = Vec::new();
        for group in grouped {
            blocks.push(group);
            while blocks.len() >= 2 {
                let n = blocks.len();
                let left_rate = blocks[n - 2].sum_y / blocks[n - 2].weight;
                let right_rate = blocks[n - 1].sum_y / blocks[n - 1].weight;
                if left_rate <= right_rate {
                    break;
                }
                let right = blocks.pop().expect("block exists");
                let left = blocks.pop().expect("block exists");
                blocks.push(Block {
                    max_x: right.max_x,
                    sum_y: left.sum_y + right.sum_y,
                    weight: left.weight + right.weight,
                });
            }
        }

        let mut thresholds = Vec::with_capacity(blocks.len());
        let mut values = Vec::with_capacity(blocks.len());
        for block in blocks {
            let observed = block.sum_y / block.weight;
            let shrunk = ((observed * block.weight) + (global_mean * ISOTONIC_SHRINKAGE))
                / (block.weight + ISOTONIC_SHRINKAGE);
            thresholds.push(block.max_x);
            values.push(shrunk.clamp(1.0e-4, 1.0 - 1.0e-4));
        }
        Self { thresholds, values }
    }

    fn predict(&self, base_probability: f32) -> f32 {
        if self.is_empty() {
            return base_probability;
        }
        let p = base_probability.clamp(1.0e-6, 1.0 - 1.0e-6);
        let idx = self
            .thresholds
            .partition_point(|t| *t <= p)
            .saturating_sub(1)
            .min(self.values.len().saturating_sub(1));
        self.values[idx]
    }
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq)]
pub struct BoostedTree {
    pub root_feature: usize,
    pub root_threshold: f32,
    pub left_feature: usize,
    pub left_threshold: f32,
    pub left_left_value: f32,
    pub left_right_value: f32,
    pub right_feature: usize,
    pub right_threshold: f32,
    pub right_left_value: f32,
    pub right_right_value: f32,
    pub gain: f32,
}

impl BoostedTree {
    fn predict(&self, features: &ExoFeatures) -> f32 {
        let root_value = features.values[self.root_feature.min(EXO_FEATURES - 1)];
        if root_value <= self.root_threshold {
            let value = features.values[self.left_feature.min(EXO_FEATURES - 1)];
            if value <= self.left_threshold {
                self.left_left_value
            } else {
                self.left_right_value
            }
        } else {
            let value = features.values[self.right_feature.min(EXO_FEATURES - 1)];
            if value <= self.right_threshold {
                self.right_left_value
            } else {
                self.right_right_value
            }
        }
    }
}

#[derive(Debug, Clone, Default)]
struct TreeEnsemble {
    trees: Vec<BoostedTree>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct TreeEnsembleSnapshot {
    pub trees: Vec<BoostedTree>,
}

impl TreeEnsemble {
    fn from_snapshot(s: TreeEnsembleSnapshot) -> Self {
        Self { trees: s.trees }
    }

    fn snapshot(&self) -> TreeEnsembleSnapshot {
        TreeEnsembleSnapshot {
            trees: self.trees.clone(),
        }
    }

    fn is_empty(&self) -> bool {
        self.trees.is_empty()
    }

    fn predict_logit_delta(&self, features: &ExoFeatures) -> f32 {
        self.trees.iter().map(|t| t.predict(features)).sum()
    }

    fn fit(
        samples: &[TrainingSample],
        weights: &[f32; EXO_FEATURES],
        bias: f32,
        beta: &BetaCalibrator,
        isotonic: &IsotonicCalibrator,
    ) -> Self {
        if samples.len() < TREE_MIN_LEAF * 4 {
            return Self::default();
        }
        let stride = samples.len().div_ceil(TREE_MAX_TRAIN_SAMPLES).max(1);
        let train_indices: Vec<usize> = (0..samples.len()).step_by(stride).collect();
        if train_indices.len() < TREE_MIN_LEAF * 4 {
            return Self::default();
        }
        let mut trees = Vec::with_capacity(TREE_COUNT);
        for _ in 0..TREE_COUNT {
            let tree = fit_boosted_tree(samples, &train_indices, weights, bias, beta, isotonic, &trees);
            if tree.gain <= 1.0e-6 {
                break;
            }
            trees.push(tree);
        }
        Self { trees }
    }
}

#[derive(Debug, Clone, Copy, Default)]
struct SplitCandidate {
    feature: usize,
    threshold: f32,
    gain: f32,
    left_leaf: f32,
    right_leaf: f32,
}

#[derive(Debug, Clone, Copy)]
struct RootStumpDelta {
    feature: usize,
    threshold: f32,
    left_leaf: f32,
    right_leaf: f32,
}

impl RootStumpDelta {
    fn predict(self, features: &ExoFeatures) -> f32 {
        if features.values[self.feature.min(EXO_FEATURES - 1)] <= self.threshold {
            self.left_leaf
        } else {
            self.right_leaf
        }
    }
}

fn finite_or(value: f32, fallback: f32) -> f32 {
    if value.is_finite() { value } else { fallback }
}

fn clamp_tree_leaf(value: f32) -> f32 {
    value.clamp(-TREE_VALUE_CLIP, TREE_VALUE_CLIP)
}

fn fit_boosted_tree(
    samples: &[TrainingSample],
    train_indices: &[usize],
    weights: &[f32; EXO_FEATURES],
    bias: f32,
    beta: &BetaCalibrator,
    isotonic: &IsotonicCalibrator,
    prior_trees: &[BoostedTree],
) -> BoostedTree {
    let root = best_split(
        samples,
        train_indices,
        weights,
        bias,
        beta,
        isotonic,
        prior_trees,
        None,
    )
    .unwrap_or_default();
    if root.gain <= 0.0 {
        return BoostedTree::default();
    }

    let root_delta = RootStumpDelta {
        feature: root.feature,
        threshold: root.threshold,
        left_leaf: root.left_leaf,
        right_leaf: root.right_leaf,
    };

    let (left_indices, right_indices): (Vec<usize>, Vec<usize>) = train_indices
        .iter()
        .copied()
        .partition(|idx| samples[*idx].features.values[root.feature] <= root.threshold);

    let left = best_split(
        samples,
        &left_indices,
        weights,
        bias,
        beta,
        isotonic,
        prior_trees,
        Some(root_delta),
    );
    let right = best_split(
        samples,
        &right_indices,
        weights,
        bias,
        beta,
        isotonic,
        prior_trees,
        Some(root_delta),
    );

    let (left_feature, left_threshold, left_left_value, left_right_value, left_gain) =
        if let Some(left) = left {
            (
                left.feature,
                left.threshold,
                root.left_leaf + left.left_leaf,
                root.left_leaf + left.right_leaf,
                left.gain,
            )
        } else {
            (root.feature, root.threshold, root.left_leaf, root.left_leaf, 0.0)
        };
    let (right_feature, right_threshold, right_left_value, right_right_value, right_gain) =
        if let Some(right) = right {
            (
                right.feature,
                right.threshold,
                root.right_leaf + right.left_leaf,
                root.right_leaf + right.right_leaf,
                right.gain,
            )
        } else {
            (root.feature, root.threshold, root.right_leaf, root.right_leaf, 0.0)
        };

    BoostedTree {
        root_feature: root.feature,
        root_threshold: finite_or(root.threshold, 0.0),
        left_feature,
        left_threshold: finite_or(left_threshold, root.threshold),
        left_left_value: finite_or(clamp_tree_leaf(left_left_value), 0.0),
        left_right_value: finite_or(clamp_tree_leaf(left_right_value), 0.0),
        right_feature,
        right_threshold: finite_or(right_threshold, root.threshold),
        right_left_value: finite_or(clamp_tree_leaf(right_left_value), 0.0),
        right_right_value: finite_or(clamp_tree_leaf(right_right_value), 0.0),
        gain: finite_or(root.gain + 0.5 * (left_gain + right_gain), 0.0),
    }
}

#[allow(clippy::too_many_arguments)]
fn best_split(
    samples: &[TrainingSample],
    indices: &[usize],
    weights: &[f32; EXO_FEATURES],
    bias: f32,
    beta: &BetaCalibrator,
    isotonic: &IsotonicCalibrator,
    prior_trees: &[BoostedTree],
    provisional_delta: Option<RootStumpDelta>,
) -> Option<SplitCandidate> {
    if indices.len() < TREE_MIN_LEAF * 2 {
        return None;
    }
    let parent = gradient_sums(
        samples,
        indices,
        weights,
        bias,
        beta,
        isotonic,
        prior_trees,
        provisional_delta,
        None,
    );
    let parent_gain = split_score(parent.0, parent.1);
    let mut best = SplitCandidate::default();
    for feature in 0..EXO_FEATURES {
        let thresholds = candidate_thresholds(samples, indices, feature);
        for threshold in thresholds {
            let left = gradient_sums(
                samples,
                indices,
                weights,
                bias,
                beta,
                isotonic,
                prior_trees,
                provisional_delta,
                Some((feature, threshold, true)),
            );
            let left_n = left.2;
            let right_n = indices.len().saturating_sub(left_n);
            if left_n < TREE_MIN_LEAF || right_n < TREE_MIN_LEAF {
                continue;
            }
            let right_g = parent.0 - left.0;
            let right_h = parent.1 - left.1;
            let gain = split_score(left.0, left.1) + split_score(right_g, right_h) - parent_gain;
            if gain > best.gain {
                best = SplitCandidate {
                    feature,
                    threshold,
                    gain,
                    left_leaf: leaf_value(left.0, left.1),
                    right_leaf: leaf_value(right_g, right_h),
                };
            }
        }
    }
    (best.gain > 0.0).then_some(best)
}

fn candidate_thresholds(samples: &[TrainingSample], indices: &[usize], feature: usize) -> Vec<f32> {
    if indices.len() < 2 {
        return Vec::new();
    }
    let mut values = Vec::with_capacity(indices.len());
    for idx in indices {
        values.push(samples[*idx].features.values[feature]);
    }
    values.sort_by(|a, b| a.total_cmp(b));
    values.dedup_by(|a, b| (*a - *b).abs() <= 1.0e-6);
    if values.len() < 2 {
        return Vec::new();
    }

    const QUANTILES: [f32; 11] = [
        0.02, 0.05, 0.10, 0.20, 0.35, 0.50, 0.65, 0.80, 0.90, 0.95, 0.98,
    ];
    let mut thresholds = Vec::with_capacity(QUANTILES.len());
    for q in QUANTILES {
        let left_idx = ((values.len() - 2) as f32 * q).round() as usize;
        let left_idx = left_idx.min(values.len() - 2);
        let left = values[left_idx];
        let right = values[left_idx + 1];
        if (right - left).abs() > 1.0e-6 {
            thresholds.push(0.5 * (left + right));
        }
    }
    thresholds.dedup_by(|a, b| (*a - *b).abs() <= 1.0e-6);
    thresholds
}

#[allow(clippy::too_many_arguments)]
fn gradient_sums(
    samples: &[TrainingSample],
    indices: &[usize],
    weights: &[f32; EXO_FEATURES],
    bias: f32,
    beta: &BetaCalibrator,
    isotonic: &IsotonicCalibrator,
    prior_trees: &[BoostedTree],
    provisional_delta: Option<RootStumpDelta>,
    split: Option<(usize, f32, bool)>,
) -> (f32, f32, usize) {
    let mut sum_g = 0.0f32;
    let mut sum_h = 0.0f32;
    let mut count = 0usize;
    for idx in indices {
        let sample = &samples[*idx];
        if let Some((feature, threshold, want_left)) = split {
            let is_left = sample.features.values[feature] <= threshold;
            if is_left != want_left {
                continue;
            }
        }
        let p = predict_with_components(sample, weights, bias, beta, isotonic, prior_trees, provisional_delta);
        let y = if sample.side_observed { 1.0 } else { 0.0 };
        sum_g += y - p;
        sum_h += (p * (1.0 - p)).max(1.0e-4);
        count += 1;
    }
    (sum_g, sum_h, count)
}

fn split_score(sum_g: f32, sum_h: f32) -> f32 {
    (sum_g * sum_g) / (sum_h + TREE_L2)
}

fn leaf_value(sum_g: f32, sum_h: f32) -> f32 {
    (TREE_LEARNING_RATE * sum_g / (sum_h + TREE_L2)).clamp(-TREE_VALUE_CLIP, TREE_VALUE_CLIP)
}

fn predict_with_components(
    sample: &TrainingSample,
    weights: &[f32; EXO_FEATURES],
    bias: f32,
    beta: &BetaCalibrator,
    isotonic: &IsotonicCalibrator,
    trees: &[BoostedTree],
    provisional_delta: Option<RootStumpDelta>,
) -> f32 {
    let base = sample.base_side_probability.clamp(1.0e-6, 1.0 - 1.0e-6);
    let beta_base = beta.predict(base).clamp(1.0e-6, 1.0 - 1.0e-6);
    let calibrated_base = isotonic.predict(beta_base).clamp(1.0e-6, 1.0 - 1.0e-6);
    let linear_delta = bias
        + weights
            .iter()
            .zip(sample.features.values.iter())
            .map(|(w, f)| w * f)
            .sum::<f32>();
    let tree_delta = trees.iter().map(|t| t.predict(&sample.features)).sum::<f32>();
    let provisional = provisional_delta
        .map(|d| d.predict(&sample.features))
        .unwrap_or(0.0);
    sigmoid(logit(calibrated_base) + linear_delta + tree_delta + provisional)
}

#[derive(Debug, Clone)]
pub struct ExoCalibrator {
    weights: [f32; EXO_FEATURES],
    bias: f32,
    beta: BetaCalibrator,
    isotonic: IsotonicCalibrator,
    trees: TreeEnsemble,
    updates: u32,
}

impl Default for ExoCalibrator {
    fn default() -> Self {
        Self {
            weights: [0.0; EXO_FEATURES],
            bias: 0.0,
            beta: BetaCalibrator::default(),
            isotonic: IsotonicCalibrator::default(),
            trees: TreeEnsemble::default(),
            updates: 0,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ExoCalibratorSnapshot {
    pub weights: Vec<f32>,
    pub bias: f32,
    #[serde(default)]
    pub beta: BetaSnapshot,
    #[serde(default)]
    pub isotonic: IsotonicSnapshot,
    #[serde(default)]
    pub trees: TreeEnsembleSnapshot,
    pub updates: u32,
}

#[derive(Debug, Clone, Copy)]
pub struct TrainingConfig {
    pub epochs: usize,
    pub learning_rate: f32,
    pub l2: f32,
    pub weight_clip: f32,
}

impl Default for TrainingConfig {
    fn default() -> Self {
        Self {
            epochs: 3,
            learning_rate: 0.02,
            l2: 1.0e-4,
            weight_clip: 2.0,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize)]
pub struct TrainingStats {
    pub samples: usize,
    pub epochs: usize,
    pub updates: u32,
    pub log_loss: f32,
}

impl ExoCalibrator {
    pub fn from_snapshot(s: ExoCalibratorSnapshot) -> Self {
        let mut weights = [0.0f32; EXO_FEATURES];
        for (i, w) in s.weights.iter().take(EXO_FEATURES).enumerate() {
            weights[i] = *w;
        }
        Self {
            weights,
            bias: s.bias,
            beta: BetaCalibrator::from_snapshot(s.beta),
            isotonic: IsotonicCalibrator::from_snapshot(s.isotonic),
            trees: TreeEnsemble::from_snapshot(s.trees),
            updates: s.updates,
        }
    }

    pub fn snapshot(&self) -> ExoCalibratorSnapshot {
        ExoCalibratorSnapshot {
            weights: self.weights.to_vec(),
            bias: self.bias,
            beta: self.beta.snapshot(),
            isotonic: self.isotonic.snapshot(),
            trees: self.trees.snapshot(),
            updates: self.updates,
        }
    }

    pub fn updates(&self) -> u32 {
        self.updates
    }

    fn apply_logit_delta(&self, side_probability: f32, features: &ExoFeatures) -> f32 {
        let base = side_probability.clamp(1e-6, 1.0 - 1e-6);
        let beta_base = self.beta.predict(base).clamp(1.0e-6, 1.0 - 1.0e-6);
        let calibrated_base = self.isotonic.predict(beta_base).clamp(1.0e-6, 1.0 - 1.0e-6);
        let linear = self.bias
            + self
                .weights
                .iter()
                .zip(features.values.iter())
                .map(|(w, f)| w * f)
                .sum::<f32>();
        let adjusted = logit(calibrated_base) + linear + self.trees.predict_logit_delta(features);
        sigmoid(adjusted).clamp(1e-6, 1.0 - 1e-6)
    }

    /// Calibrated probability. Falls back to the raw base until trained.
    pub fn predict(&self, base_side_probability: f32, features: &ExoFeatures) -> f32 {
        if self.updates < MIN_UPDATES
            && !self.beta.enabled
            && self.isotonic.is_empty()
            && self.trees.is_empty()
        {
            base_side_probability
        } else {
            self.apply_logit_delta(base_side_probability, features)
        }
    }

    /// Batch fit: Beta + Isotonic on the base probability, then linear SGD
    /// epochs, then the boosted-stump ensemble on residuals.
    pub fn fit_batch(&mut self, samples: &[TrainingSample], cfg: TrainingConfig) -> TrainingStats {
        if samples.is_empty() || cfg.epochs == 0 {
            return TrainingStats {
                samples: samples.len(),
                epochs: cfg.epochs,
                updates: self.updates,
                log_loss: 0.0,
            };
        }
        let lr = cfg.learning_rate.max(1.0e-6);
        let l2 = cfg.l2.max(0.0);
        let clip = cfg.weight_clip.max(0.01);
        self.beta = BetaCalibrator::fit(samples);
        self.isotonic = IsotonicCalibrator::fit(samples, &self.beta);
        for _ in 0..cfg.epochs {
            for sample in samples {
                let p_hat = self.apply_logit_delta(sample.base_side_probability, &sample.features);
                let target = if sample.side_observed { 1.0 } else { 0.0 };
                let error = target - p_hat;
                for (w, f) in self.weights.iter_mut().zip(sample.features.values.iter()) {
                    *w = (*w + lr * error * *f - l2 * *w).clamp(-clip, clip);
                }
                self.bias = (self.bias + lr * error).clamp(-clip, clip);
                self.updates = self.updates.saturating_add(1);
            }
        }
        self.trees = TreeEnsemble::fit(samples, &self.weights, self.bias, &self.beta, &self.isotonic);
        let log_loss = samples
            .iter()
            .map(|s| binary_log_loss(self.predict(s.base_side_probability, &s.features), s.side_observed))
            .sum::<f32>()
            / samples.len() as f32;
        TrainingStats {
            samples: samples.len(),
            epochs: cfg.epochs,
            updates: self.updates,
            log_loss,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(z: f32, base_p: f32, observed: bool) -> TrainingSample {
        let mut values = [0.0f32; EXO_FEATURES];
        values[0] = z;
        values[15] = base_p - 0.5;
        TrainingSample {
            features: ExoFeatures { values },
            base_side_probability: base_p,
            side_observed: observed,
        }
    }

    /// Synthetic world where the base is systematically overconfident:
    /// base says 0.8/0.2 but the truth is 0.65/0.35.
    fn overconfident_world(n: usize) -> Vec<TrainingSample> {
        let mut out = Vec::with_capacity(n);
        for i in 0..n {
            // Deterministic pseudo-random outcome stream at 65% accuracy.
            let r = ((i * 2654435761) % 100) as f32 / 100.0;
            let up_leg = i % 2 == 0;
            let base_p = if up_leg { 0.8 } else { 0.2 };
            let truth_p = if up_leg { 0.65 } else { 0.35 };
            out.push(sample(if up_leg { 1.0 } else { -1.0 }, base_p, r < truth_p));
        }
        out
    }

    #[test]
    fn untrained_calibrator_passes_base_through() {
        let cal = ExoCalibrator::default();
        let s = sample(1.0, 0.7, true);
        assert_eq!(cal.predict(0.7, &s.features), 0.7);
    }

    #[test]
    fn fit_batch_improves_log_loss_on_overconfident_base() {
        let samples = overconfident_world(4000);
        let raw_ll = samples
            .iter()
            .map(|s| binary_log_loss(s.base_side_probability, s.side_observed))
            .sum::<f32>()
            / samples.len() as f32;
        let mut cal = ExoCalibrator::default();
        let stats = cal.fit_batch(&samples, TrainingConfig::default());
        assert!(stats.log_loss < raw_ll, "fit {} vs raw {}", stats.log_loss, raw_ll);
        // Overconfident 0.8 should be pulled back toward 0.65.
        let s = sample(1.0, 0.8, true);
        let p = cal.predict(0.8, &s.features);
        assert!(p < 0.75 && p > 0.5, "expected pull-back toward truth, got {p}");
    }

    #[test]
    fn snapshot_roundtrip_preserves_predictions() {
        let samples = overconfident_world(2000);
        let mut cal = ExoCalibrator::default();
        cal.fit_batch(&samples, TrainingConfig::default());
        let snap = cal.snapshot();
        let json = serde_json::to_string(&snap).unwrap();
        let back = ExoCalibrator::from_snapshot(serde_json::from_str(&json).unwrap());
        let s = sample(0.5, 0.62, true);
        assert!((cal.predict(0.62, &s.features) - back.predict(0.62, &s.features)).abs() < 1e-6);
    }
}
