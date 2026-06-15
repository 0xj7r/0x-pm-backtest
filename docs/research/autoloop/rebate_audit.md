# Rebate audit (live account)

Source: agent audit of the public activity API for the live wallet 0x00190179D84224687aDfB93e4A499D36AAD0FF59 (confirmed br2: BTC-5m dominates). Month to date Jun 1-12, 2026.

## Measured
- 4,098 taker fills, $14,568 notional, weighted volume (wV = size x (1-price)) $9,280.
- Estimated taker fees paid: $324.38 over 4 active days, about $81.10 per active day.
- Current tier: Bronze (3%). Estimated taker rebate accrual ~$2.43 per active day ($0.84 per calendar day).
- No taker-rebate pUSD credits visible in the activity API; reconciliation of actual payments needs the rewards UI.
- Two MAKER_REBATE credits ARE visible: $4.03 and $10.27 (Jun 1-2, 00:45 UTC, from the paired-MM overnight run). Maker rebates are real and paid daily.

## Scaling
- 5x volume: Silver 8%, about $32/day rebate.
- 20x volume ($185.6k wV): still Silver, about $130/day.
- Gold 18% needs ~21.6x current volume, then about $315/day.

## P&L model correction
Multiply modeled taker fees by 0.97 at current size, 0.92 at Silver, 0.82 at Gold. Negligible today; material past 5x volume. Maker fills (passive exits) additionally earn from the 20% maker pool, already observed paying on this account.
