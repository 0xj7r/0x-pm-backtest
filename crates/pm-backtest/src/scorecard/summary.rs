//! Post-hoc summarization of a `market-results.jsonl` file into a
//! `ResultSummary` report. Separate submodule (rather than flattened into
//! `scorecard`) because it has its own module-private `FillTagAccumulator`
//! with a different field set than `scorecard`'s own; see
//! `docs/superpowers/plans/2026-08-23-phase2-extraction-map.md` (tranche 5).

use std::collections::{BTreeMap, HashMap};
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

const MARKETS_PER_DAY: f64 = 288.0;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResultSummary {
    pub strategy: String,
    pub markets: usize,
    pub elapsed_days: f64,
    pub first_start_equity_usdc: f64,
    pub last_end_equity_usdc: f64,
    pub total_pnl_usdc: f64,
    pub compounded_return_pct: f64,
    pub compounded_daily_return_pct: f64,
    pub path_max_drawdown_pct: f64,
    pub orders_submitted: usize,
    pub orders_filled: usize,
    pub orders_filled_taker: usize,
    pub orders_filled_maker: usize,
    pub maker_fill_rate: f64,
    pub requested_shares: f64,
    pub filled_shares: f64,
    pub fill_shares_ratio: f64,
    pub requested_notional_usdc: f64,
    pub filled_notional_usdc: f64,
    pub fill_notional_ratio: f64,
    pub avg_slippage_bps: f64,
    pub markets_with_fills: usize,
    pub winning_markets: usize,
    pub losing_markets: usize,
    pub hit_rate_filled_markets: f64,
    pub worst_market_slug: Option<String>,
    pub worst_market_pnl_usdc: f64,
    pub best_market_slug: Option<String>,
    pub best_market_pnl_usdc: f64,
    pub by_fill_tag: HashMap<String, FillTagSummary>,
    pub by_day: Vec<DailyResultSummary>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DailyResultSummary {
    pub day: String,
    pub markets: usize,
    pub first_start_equity_usdc: f64,
    pub last_end_equity_usdc: f64,
    pub pnl_usdc: f64,
    pub return_pct: f64,
    pub max_drawdown_pct: f64,
    pub orders_filled: usize,
    pub markets_with_fills: usize,
    pub winning_markets: usize,
    pub losing_markets: usize,
    pub worst_market_slug: Option<String>,
    pub worst_market_pnl_usdc: f64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FillTagSummary {
    pub fills: usize,
    pub maker_fills: usize,
    pub taker_fills: usize,
    pub maker_fill_rate: f64,
    pub total_notional_usdc: f64,
    pub avg_fill_price: f64,
    pub avg_slippage_bps: f64,
    pub avg_side_edge_vs_fill: f64,
    pub avg_market_yes_range_so_far: f64,
    pub avg_regime_whipsaw_score: f64,
    pub avg_regime_path_efficiency: f64,
    pub avg_regime_reversal_pressure: f64,
    pub avg_regime_sign_flip_rate: f64,
    pub avg_regime_realized_vol_180s_bps: f64,
    pub avg_post_fill_adverse_excursion: f64,
    pub avg_post_fill_favourable_excursion: f64,
    pub post_fill_cross_mid_rate: f64,
}

#[derive(Debug, Default)]
struct FillTagAccumulator {
    fills: usize,
    maker_fills: usize,
    taker_fills: usize,
    total_notional_usdc: f64,
    sum_fill_price: f64,
    slippage_notional: f64,
    sum_side_edge_vs_fill: f64,
    side_edge_samples: usize,
    sum_market_yes_range_so_far: f64,
    market_range_samples: usize,
    sum_regime_whipsaw_score: f64,
    sum_regime_path_efficiency: f64,
    sum_regime_reversal_pressure: f64,
    sum_regime_sign_flip_rate: f64,
    sum_regime_realized_vol_180s_bps: f64,
    regime_samples: usize,
    sum_post_fill_adverse_excursion: f64,
    sum_post_fill_favourable_excursion: f64,
    post_fill_cross_mid_count: usize,
    post_fill_path_samples: usize,
}

impl FillTagAccumulator {
    fn push(&mut self, fill: &FillRow) {
        self.fills += 1;
        if fill.maker {
            self.maker_fills += 1;
        } else {
            self.taker_fills += 1;
        }
        self.total_notional_usdc += fill.notional;
        self.sum_fill_price += fill.price;
        self.slippage_notional += fill.slippage_bps * fill.notional;
        if let Some(edge) = fill.side_edge_vs_fill {
            self.sum_side_edge_vs_fill += edge;
            self.side_edge_samples += 1;
        }
        if let Some(range) = fill.market_yes_range_so_far {
            self.sum_market_yes_range_so_far += range;
            self.market_range_samples += 1;
        }
        if let (
            Some(whipsaw),
            Some(path_efficiency),
            Some(reversal_pressure),
            Some(sign_flip_rate),
            Some(realized_vol),
        ) = (
            fill.regime_whipsaw_score,
            fill.regime_path_efficiency,
            fill.regime_reversal_pressure,
            fill.regime_sign_flip_rate,
            fill.regime_realized_vol_180s_bps,
        ) {
            self.sum_regime_whipsaw_score += whipsaw;
            self.sum_regime_path_efficiency += path_efficiency;
            self.sum_regime_reversal_pressure += reversal_pressure;
            self.sum_regime_sign_flip_rate += sign_flip_rate;
            self.sum_regime_realized_vol_180s_bps += realized_vol;
            self.regime_samples += 1;
        }
        if let Some(path) = fill.post_fill_path {
            self.sum_post_fill_adverse_excursion += path.adverse_excursion;
            self.sum_post_fill_favourable_excursion += path.favourable_excursion;
            self.post_fill_cross_mid_count += usize::from(path.crossed_mid_after_fill);
            self.post_fill_path_samples += 1;
        }
    }

    fn into_summary(self) -> FillTagSummary {
        FillTagSummary {
            fills: self.fills,
            maker_fills: self.maker_fills,
            taker_fills: self.taker_fills,
            maker_fill_rate: if self.fills > 0 {
                self.maker_fills as f64 / self.fills as f64
            } else {
                0.0
            },
            total_notional_usdc: self.total_notional_usdc,
            avg_fill_price: if self.fills > 0 {
                self.sum_fill_price / self.fills as f64
            } else {
                0.0
            },
            avg_slippage_bps: if self.total_notional_usdc > 0.0 {
                self.slippage_notional / self.total_notional_usdc
            } else {
                0.0
            },
            avg_side_edge_vs_fill: if self.side_edge_samples > 0 {
                self.sum_side_edge_vs_fill / self.side_edge_samples as f64
            } else {
                0.0
            },
            avg_market_yes_range_so_far: if self.market_range_samples > 0 {
                self.sum_market_yes_range_so_far / self.market_range_samples as f64
            } else {
                0.0
            },
            avg_regime_whipsaw_score: if self.regime_samples > 0 {
                self.sum_regime_whipsaw_score / self.regime_samples as f64
            } else {
                0.0
            },
            avg_regime_path_efficiency: if self.regime_samples > 0 {
                self.sum_regime_path_efficiency / self.regime_samples as f64
            } else {
                0.0
            },
            avg_regime_reversal_pressure: if self.regime_samples > 0 {
                self.sum_regime_reversal_pressure / self.regime_samples as f64
            } else {
                0.0
            },
            avg_regime_sign_flip_rate: if self.regime_samples > 0 {
                self.sum_regime_sign_flip_rate / self.regime_samples as f64
            } else {
                0.0
            },
            avg_regime_realized_vol_180s_bps: if self.regime_samples > 0 {
                self.sum_regime_realized_vol_180s_bps / self.regime_samples as f64
            } else {
                0.0
            },
            avg_post_fill_adverse_excursion: if self.post_fill_path_samples > 0 {
                self.sum_post_fill_adverse_excursion / self.post_fill_path_samples as f64
            } else {
                0.0
            },
            avg_post_fill_favourable_excursion: if self.post_fill_path_samples > 0 {
                self.sum_post_fill_favourable_excursion / self.post_fill_path_samples as f64
            } else {
                0.0
            },
            post_fill_cross_mid_rate: if self.post_fill_path_samples > 0 {
                self.post_fill_cross_mid_count as f64 / self.post_fill_path_samples as f64
            } else {
                0.0
            },
        }
    }
}

#[derive(Debug, Deserialize)]
struct MarketResultRow {
    slug: String,
    #[serde(default)]
    close_ts: Option<i64>,
    per_strategy: HashMap<String, StrategyResultRow>,
}

#[derive(Debug, Deserialize)]
struct StrategyResultRow {
    #[serde(default)]
    orders_submitted: usize,
    #[serde(default)]
    orders_filled: usize,
    #[serde(default)]
    orders_filled_taker: usize,
    #[serde(default)]
    orders_filled_maker: usize,
    #[serde(default)]
    requested_shares: f64,
    #[serde(default)]
    filled_shares: f64,
    #[serde(default)]
    requested_notional_usdc: f64,
    #[serde(default)]
    filled_notional_usdc: f64,
    #[serde(default)]
    avg_slippage_bps: f64,
    #[serde(default)]
    pnl_usdc: f64,
    #[serde(default)]
    start_equity_usdc: f64,
    #[serde(default)]
    end_equity_usdc: f64,
    #[serde(default)]
    fills_detail: Vec<FillRow>,
}

#[derive(Debug, Deserialize)]
struct FillRow {
    #[serde(default)]
    shares: f64,
    #[serde(default)]
    price: f64,
    #[serde(default)]
    notional: f64,
    #[serde(default)]
    maker: bool,
    #[serde(default)]
    slippage_bps: f64,
    #[serde(default)]
    tag: String,
    #[serde(default)]
    side_edge_vs_fill: Option<f64>,
    #[serde(default)]
    market_yes_range_so_far: Option<f64>,
    #[serde(default)]
    regime_whipsaw_score: Option<f64>,
    #[serde(default)]
    regime_path_efficiency: Option<f64>,
    #[serde(default)]
    regime_reversal_pressure: Option<f64>,
    #[serde(default)]
    regime_sign_flip_rate: Option<f64>,
    #[serde(default)]
    regime_realized_vol_180s_bps: Option<f64>,
    #[serde(default)]
    post_fill_path: Option<PostFillPathRow>,
}

#[derive(Debug, Clone, Copy, Deserialize)]
struct PostFillPathRow {
    adverse_excursion: f64,
    favourable_excursion: f64,
    crossed_mid_after_fill: bool,
}

#[derive(Debug, Default)]
struct DailyAccumulator {
    markets: usize,
    first_start_equity: Option<f64>,
    last_end_equity: f64,
    peak_equity: f64,
    max_drawdown: f64,
    pnl: f64,
    orders_filled: usize,
    markets_with_fills: usize,
    winning_markets: usize,
    losing_markets: usize,
    worst_market_slug: Option<String>,
    worst_market_pnl: f64,
}

impl DailyAccumulator {
    fn push(&mut self, slug: &str, strategy_row: &StrategyResultRow) {
        self.markets += 1;
        self.first_start_equity
            .get_or_insert(strategy_row.start_equity_usdc);
        self.last_end_equity = strategy_row.end_equity_usdc;
        self.peak_equity = self.peak_equity.max(strategy_row.end_equity_usdc);
        if self.peak_equity > 0.0 {
            self.max_drawdown = self
                .max_drawdown
                .max((self.peak_equity - strategy_row.end_equity_usdc) / self.peak_equity);
        }
        self.pnl += strategy_row.pnl_usdc;
        self.orders_filled += strategy_row.orders_filled;
        if strategy_row.orders_filled > 0 {
            self.markets_with_fills += 1;
        }
        if strategy_row.pnl_usdc > 0.0 {
            self.winning_markets += 1;
        } else if strategy_row.pnl_usdc < 0.0 {
            self.losing_markets += 1;
        }
        if strategy_row.pnl_usdc < self.worst_market_pnl {
            self.worst_market_pnl = strategy_row.pnl_usdc;
            self.worst_market_slug = Some(slug.to_string());
        }
    }

    fn into_summary(self, day: String) -> DailyResultSummary {
        let first_start_equity = self.first_start_equity.unwrap_or(0.0);
        let return_pct = if first_start_equity > 0.0 {
            (self.last_end_equity / first_start_equity - 1.0) * 100.0
        } else {
            0.0
        };
        DailyResultSummary {
            day,
            markets: self.markets,
            first_start_equity_usdc: first_start_equity,
            last_end_equity_usdc: self.last_end_equity,
            pnl_usdc: self.pnl,
            return_pct,
            max_drawdown_pct: self.max_drawdown * 100.0,
            orders_filled: self.orders_filled,
            markets_with_fills: self.markets_with_fills,
            winning_markets: self.winning_markets,
            losing_markets: self.losing_markets,
            worst_market_slug: self.worst_market_slug,
            worst_market_pnl_usdc: self.worst_market_pnl,
        }
    }
}

fn close_day_key(close_ts: Option<i64>, market_index: usize) -> String {
    close_ts
        .and_then(|ts| DateTime::<Utc>::from_timestamp(ts, 0))
        .map(|dt| dt.format("%Y-%m-%d").to_string())
        .unwrap_or_else(|| format!("day_{:04}", market_index / MARKETS_PER_DAY as usize + 1))
}

pub fn summarize_markets_jsonl(path: &Path, strategy: &str) -> Result<ResultSummary> {
    let file = File::open(path).with_context(|| format!("open {}", path.display()))?;
    let reader = BufReader::new(file);

    let mut markets = 0usize;
    let mut first_start_equity = None::<f64>;
    let mut last_end_equity = 0.0f64;
    let mut peak_equity = 0.0f64;
    let mut path_max_drawdown = 0.0f64;
    let mut total_pnl = 0.0f64;
    let mut orders_submitted = 0usize;
    let mut orders_filled = 0usize;
    let mut orders_filled_taker = 0usize;
    let mut orders_filled_maker = 0usize;
    let mut requested_notional = 0.0f64;
    let mut filled_notional = 0.0f64;
    let mut requested_shares = 0.0f64;
    let mut filled_shares = 0.0f64;
    let mut slippage_notional = 0.0f64;
    let mut markets_with_fills = 0usize;
    let mut winning_markets = 0usize;
    let mut losing_markets = 0usize;
    let mut worst_market_slug = None::<String>;
    let mut worst_market_pnl = 0.0f64;
    let mut best_market_slug = None::<String>;
    let mut best_market_pnl = 0.0f64;
    let mut tag_acc: HashMap<String, FillTagAccumulator> = HashMap::new();
    let mut day_acc: BTreeMap<String, DailyAccumulator> = BTreeMap::new();

    for (line_no, line) in reader.lines().enumerate() {
        let line = line.with_context(|| format!("read line {}", line_no + 1))?;
        if line.trim().is_empty() {
            continue;
        }
        let row: MarketResultRow = serde_json::from_str(&line)
            .with_context(|| format!("parse {} line {}", path.display(), line_no + 1))?;
        let Some(strategy_row) = row.per_strategy.get(strategy) else {
            continue;
        };

        let day_key = close_day_key(row.close_ts, markets);
        day_acc
            .entry(day_key)
            .or_default()
            .push(&row.slug, strategy_row);

        markets += 1;
        first_start_equity.get_or_insert(strategy_row.start_equity_usdc);
        last_end_equity = strategy_row.end_equity_usdc;
        peak_equity = peak_equity.max(last_end_equity);
        if peak_equity > 0.0 {
            path_max_drawdown =
                path_max_drawdown.max((peak_equity - last_end_equity) / peak_equity);
        }
        total_pnl += strategy_row.pnl_usdc;
        orders_submitted += strategy_row.orders_submitted;
        orders_filled += strategy_row.orders_filled;
        let row_maker_fills =
            if strategy_row.orders_filled_maker > 0 || strategy_row.orders_filled_taker > 0 {
                strategy_row.orders_filled_maker
            } else {
                strategy_row
                    .fills_detail
                    .iter()
                    .filter(|fill| fill.maker)
                    .count()
            };
        let row_taker_fills =
            if strategy_row.orders_filled_maker > 0 || strategy_row.orders_filled_taker > 0 {
                strategy_row.orders_filled_taker
            } else {
                strategy_row
                    .fills_detail
                    .len()
                    .saturating_sub(row_maker_fills)
            };
        orders_filled_taker += row_taker_fills;
        orders_filled_maker += row_maker_fills;
        requested_shares += strategy_row.requested_shares;
        let row_filled_shares = if strategy_row.filled_shares > 0.0 {
            strategy_row.filled_shares
        } else {
            strategy_row
                .fills_detail
                .iter()
                .map(|fill| fill.shares)
                .sum()
        };
        filled_shares += row_filled_shares;
        requested_notional += strategy_row.requested_notional_usdc;
        let row_filled_notional = if strategy_row.filled_notional_usdc > 0.0 {
            strategy_row.filled_notional_usdc
        } else {
            strategy_row
                .fills_detail
                .iter()
                .map(|fill| fill.notional)
                .sum()
        };
        filled_notional += row_filled_notional;
        let row_avg_slippage = if strategy_row.avg_slippage_bps > 0.0 {
            strategy_row.avg_slippage_bps
        } else if row_filled_notional > 0.0 {
            strategy_row
                .fills_detail
                .iter()
                .map(|fill| fill.slippage_bps * fill.notional)
                .sum::<f64>()
                / row_filled_notional
        } else {
            0.0
        };
        slippage_notional += row_avg_slippage * row_filled_notional;
        if strategy_row.orders_filled > 0 {
            markets_with_fills += 1;
        }
        if strategy_row.pnl_usdc > 0.0 {
            winning_markets += 1;
        } else if strategy_row.pnl_usdc < 0.0 {
            losing_markets += 1;
        }
        if strategy_row.pnl_usdc < worst_market_pnl {
            worst_market_pnl = strategy_row.pnl_usdc;
            worst_market_slug = Some(row.slug.clone());
        }
        if strategy_row.pnl_usdc > best_market_pnl {
            best_market_pnl = strategy_row.pnl_usdc;
            best_market_slug = Some(row.slug);
        }
        for fill in &strategy_row.fills_detail {
            let tag = if fill.tag.is_empty() {
                "unknown".to_string()
            } else {
                fill.tag.clone()
            };
            tag_acc.entry(tag).or_default().push(fill);
        }
    }

    let first_start_equity = first_start_equity.unwrap_or(0.0);
    let elapsed_days = markets as f64 / MARKETS_PER_DAY;
    let compounded_return_pct = if first_start_equity > 0.0 {
        (last_end_equity / first_start_equity - 1.0) * 100.0
    } else {
        0.0
    };
    let compounded_daily_return_pct =
        if elapsed_days > 0.0 && first_start_equity > 0.0 && last_end_equity > 0.0 {
            ((last_end_equity / first_start_equity).powf(1.0 / elapsed_days) - 1.0) * 100.0
        } else {
            0.0
        };
    let hit_rate_filled_markets = if markets_with_fills > 0 {
        winning_markets as f64 / markets_with_fills as f64
    } else {
        0.0
    };
    let by_fill_tag = tag_acc
        .into_iter()
        .map(|(tag, acc)| (tag, acc.into_summary()))
        .collect();
    let by_day = day_acc
        .into_iter()
        .map(|(day, acc)| acc.into_summary(day))
        .collect();

    Ok(ResultSummary {
        strategy: strategy.to_string(),
        markets,
        elapsed_days,
        first_start_equity_usdc: first_start_equity,
        last_end_equity_usdc: last_end_equity,
        total_pnl_usdc: total_pnl,
        compounded_return_pct,
        compounded_daily_return_pct,
        path_max_drawdown_pct: path_max_drawdown * 100.0,
        orders_submitted,
        orders_filled,
        orders_filled_taker,
        orders_filled_maker,
        maker_fill_rate: if orders_filled > 0 {
            orders_filled_maker as f64 / orders_filled as f64
        } else {
            0.0
        },
        requested_shares,
        filled_shares,
        fill_shares_ratio: if requested_shares > 0.0 {
            filled_shares / requested_shares
        } else {
            0.0
        },
        requested_notional_usdc: requested_notional,
        filled_notional_usdc: filled_notional,
        fill_notional_ratio: if requested_notional > 0.0 {
            filled_notional / requested_notional
        } else {
            0.0
        },
        avg_slippage_bps: if filled_notional > 0.0 {
            slippage_notional / filled_notional
        } else {
            0.0
        },
        markets_with_fills,
        winning_markets,
        losing_markets,
        hit_rate_filled_markets,
        worst_market_slug,
        worst_market_pnl_usdc: worst_market_pnl,
        best_market_slug,
        best_market_pnl_usdc: best_market_pnl,
        by_fill_tag,
        by_day,
    })
}

pub fn write_result_summary_json(path: &Path, summary: &ResultSummary) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create output directory {}", parent.display()))?;
    }
    std::fs::write(path, serde_json::to_string_pretty(summary)?)
        .with_context(|| format!("write {}", path.display()))
}

pub fn print_result_summary(summary: &ResultSummary) {
    println!("== market-results summary ==");
    println!("strategy              : {}", summary.strategy);
    println!(
        "markets / days        : {} / {:.1}",
        summary.markets, summary.elapsed_days
    );
    println!(
        "equity / pnl          : {:.2} / {:+.2}",
        summary.last_end_equity_usdc, summary.total_pnl_usdc
    );
    println!(
        "return / daily        : {:+.2}% / {:+.3}%",
        summary.compounded_return_pct, summary.compounded_daily_return_pct
    );
    println!(
        "max drawdown          : {:.2}%",
        summary.path_max_drawdown_pct
    );
    println!(
        "orders / fills        : {} / {}",
        summary.orders_submitted, summary.orders_filled
    );
    println!(
        "fill quality          : shares={:.1}% notional={:.1}% maker={:.1}% slip={:.1}bps requested={:.2} filled={:.2}",
        summary.fill_shares_ratio * 100.0,
        summary.fill_notional_ratio * 100.0,
        summary.maker_fill_rate * 100.0,
        summary.avg_slippage_bps,
        summary.requested_notional_usdc,
        summary.filled_notional_usdc
    );
    println!(
        "fill markets W/L      : {} / {} / {} ({:.1}% hit)",
        summary.markets_with_fills,
        summary.winning_markets,
        summary.losing_markets,
        summary.hit_rate_filled_markets * 100.0
    );
    println!(
        "worst market          : {:+.2} {}",
        summary.worst_market_pnl_usdc,
        summary.worst_market_slug.as_deref().unwrap_or("-")
    );
    println!(
        "best market           : {:+.2} {}",
        summary.best_market_pnl_usdc,
        summary.best_market_slug.as_deref().unwrap_or("-")
    );
    if !summary.by_fill_tag.is_empty() {
        println!("fills by tag:");
        let mut tags: Vec<_> = summary.by_fill_tag.iter().collect();
        tags.sort_by(|a, b| a.0.cmp(b.0));
        for (tag, agg) in tags {
            println!(
                "  {:28} fills={:<6} notional={:.2} avg_px={:.4} maker={:.1}% slip={:.1}bps edge_fill={:+.4} range={:.3} whip={:.3} path={:.3} rev={:.3}",
                tag,
                agg.fills,
                agg.total_notional_usdc,
                agg.avg_fill_price,
                agg.maker_fill_rate * 100.0,
                agg.avg_slippage_bps,
                agg.avg_side_edge_vs_fill,
                agg.avg_market_yes_range_so_far,
                agg.avg_regime_whipsaw_score,
                agg.avg_regime_path_efficiency,
                agg.avg_regime_reversal_pressure
            );
        }
    }
    if !summary.by_day.is_empty() {
        println!("daily slices:");
        for day in &summary.by_day {
            println!(
                "  {} markets={:<4} pnl={:+.2} ret={:+.2}% dd={:.2}% fills={} W/L={}/{} worst={:+.2}",
                day.day,
                day.markets,
                day.pnl_usdc,
                day.return_pct,
                day.max_drawdown_pct,
                day.orders_filled,
                day.winning_markets,
                day.losing_markets,
                day.worst_market_pnl_usdc
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn summarizes_compounded_portfolio_results() {
        let path = std::env::temp_dir().join(format!(
            "pm-result-summary-test-{}.jsonl",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let mut file = File::create(&path).unwrap();
        writeln!(
            file,
            r#"{{"slug":"m1","per_strategy":{{"test_strat":{{"orders_submitted":1,"orders_filled":1,"orders_filled_taker":1,"orders_filled_maker":0,"requested_shares":10.0,"filled_shares":10.0,"requested_notional_usdc":10.0,"filled_notional_usdc":8.0,"avg_slippage_bps":20.0,"pnl_usdc":10.0,"start_equity_usdc":1000.0,"end_equity_usdc":1010.0,"fills_detail":[{{"shares":10.0,"price":0.8,"notional":8.0,"tag":"fav","maker":false,"slippage_bps":20.0}}]}}}}}}"#
        )
        .unwrap();
        writeln!(
            file,
            r#"{{"slug":"m2","per_strategy":{{"test_strat":{{"orders_submitted":1,"orders_filled":1,"orders_filled_taker":0,"orders_filled_maker":1,"requested_shares":10.0,"filled_shares":10.0,"requested_notional_usdc":9.0,"filled_notional_usdc":9.0,"avg_slippage_bps":0.0,"pnl_usdc":-20.0,"start_equity_usdc":1010.0,"end_equity_usdc":990.0,"fills_detail":[{{"shares":10.0,"price":0.9,"notional":9.0,"tag":"fav","maker":true,"slippage_bps":0.0}}]}}}}}}"#
        )
        .unwrap();
        drop(file);

        let summary = summarize_markets_jsonl(&path, "test_strat").unwrap();
        let _ = std::fs::remove_file(&path);
        assert_eq!(summary.markets, 2);
        assert_eq!(summary.orders_filled, 2);
        assert_eq!(summary.orders_filled_taker, 1);
        assert_eq!(summary.orders_filled_maker, 1);
        assert!((summary.maker_fill_rate - 0.5).abs() < 1e-9);
        assert!((summary.requested_shares - 20.0).abs() < 1e-9);
        assert!((summary.filled_shares - 20.0).abs() < 1e-9);
        assert!((summary.fill_shares_ratio - 1.0).abs() < 1e-9);
        assert!((summary.requested_notional_usdc - 19.0).abs() < 1e-9);
        assert!((summary.filled_notional_usdc - 17.0).abs() < 1e-9);
        assert!((summary.fill_notional_ratio - 17.0 / 19.0).abs() < 1e-9);
        assert!((summary.avg_slippage_bps - (160.0 / 17.0)).abs() < 1e-9);
        assert_eq!(summary.markets_with_fills, 2);
        assert_eq!(summary.winning_markets, 1);
        assert_eq!(summary.losing_markets, 1);
        assert_eq!(summary.worst_market_slug.as_deref(), Some("m2"));
        assert!((summary.path_max_drawdown_pct - 1.9801980198019802).abs() < 1e-9);
        let fav = summary.by_fill_tag.get("fav").unwrap();
        assert_eq!(fav.fills, 2);
        assert_eq!(fav.taker_fills, 1);
        assert_eq!(fav.maker_fills, 1);
        assert!((fav.total_notional_usdc - 17.0).abs() < 1e-9);
        assert!((fav.avg_fill_price - 0.85).abs() < 1e-9);
        assert!((fav.avg_slippage_bps - (160.0 / 17.0)).abs() < 1e-9);
    }
}
