use crate::{Ctx, Side};

/// Model-support gate thresholds (seeded from champion late_favourite_*).
#[derive(Debug, Clone, Copy)]
pub struct SignalGate {
    pub min_confidence: f32,
    pub max_risk: f32,
    pub min_side_p: f32,
    pub min_edge: f32,
}
impl Default for SignalGate {
    fn default() -> Self {
        Self { min_confidence: 0.68, max_risk: 0.72, min_side_p: 0.62, min_edge: 0.03 }
    }
}

/// Directional conviction for the favoured outcome of one market.
/// `side_p`/`edge`/`confidence`/`risk` are reserved for Plan 4 sizing-curve tuning
/// (conviction-scaled clip sizing); only `favourite` drives v1 sizing.
#[derive(Debug, Clone, Copy)]
pub struct Conviction {
    pub favourite: Side,
    pub side_p: f32,
    pub edge: f32,
    pub confidence: f32,
    pub risk: f32,
}

/// Favourite side = market-implied (yes_mid >= 0.5 -> YES). Returns `Some` only
/// when the model SUPPORTS that side (agrees on direction and clears the gate),
/// mirroring br2's `model_support_for_side`. `fav_ask` is the real ask of the
/// favourite side (yes_ask for YES, no_ask for NO).
pub fn evaluate(ctx: &Ctx, yes_mid: f32, fav_ask: f32, gate: &SignalGate) -> Option<Conviction> {
    let model = ctx.model_output?;
    let favourite = if yes_mid >= 0.5 { Side::BuyYes } else { Side::BuyNo };
    let model_side_is_yes = model.direction_score >= 0.0;
    let fav_is_yes = matches!(favourite, Side::BuyYes);
    if model_side_is_yes != fav_is_yes {
        return None;
    }
    let side_p = model.calibrated_p;
    let edge = side_p - fav_ask;
    if model.confidence_score < gate.min_confidence
        || model.risk_score > gate.max_risk
        || side_p < gate.min_side_p
        || edge < gate.min_edge
    {
        return None;
    }
    Some(Conviction { favourite, side_p, edge, confidence: model.confidence_score, risk: model.risk_score })
}

#[cfg(test)]
mod tests {
    use super::*;
    use pm_model::ModelOutput;

    #[test]
    fn favourite_is_market_side_and_must_pass_model_gate() {
        let gate = SignalGate::default();
        // YES is favourite (yes_mid 0.8), model agrees (direction +), p high, low risk, edge ok.
        let ctx = Ctx { model_output: Some(ModelOutput {
            direction_score: 0.5, confidence_score: 0.75, calibrated_p: 0.78, risk_score: 0.3,
        }), ..Ctx::default() };
        let conv = evaluate(&ctx, 0.80, 0.74, &gate)
            .expect("supported conviction");
        assert_eq!(conv.favourite, Side::BuyYes);
        assert!((conv.side_p - 0.78).abs() < 1e-6);
        assert!((conv.edge - (0.78 - 0.74)).abs() < 1e-6);

        // Same market, but model disagrees (direction negative) -> not supported -> None.
        let ctx_bad = Ctx { model_output: Some(ModelOutput {
            direction_score: -0.5, confidence_score: 0.75, calibrated_p: 0.78, risk_score: 0.3,
        }), ..Ctx::default() };
        assert!(evaluate(&ctx_bad, 0.80, 0.74, &gate).is_none(), "model disagrees with favourite");
    }
}
