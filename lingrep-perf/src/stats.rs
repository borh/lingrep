//! The paired estimator and its critical value.
//!
//! Everything here is a pure function of the observations, with no dependency
//! on `lingrep-core`, so a campaign's arithmetic can be tested against hand-computed
//! values rather than against a measurement.
//!
//! Two decisions are load-bearing and are argued where they are implemented:
//! the unit of differencing is the **block**, not the pair, and the critical
//! value spends alpha across the declared looks by the **union bound**.

/// Two-sided alpha for a campaign. Not configurable: a campaign that wants a
/// different error rate wants a different decision rule, not a knob.
pub const ALPHA: f64 = 0.05;

/// A paired effect on the ratio scale, with `r < 1` better.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Effect {
    /// Geometric mean of the per-block ratios.
    pub ratio: f64,
    /// Lower interval bound, or `ratio` when there is only a point estimate.
    pub low: f64,
    /// Upper interval bound, or `ratio` when there is only a point estimate.
    pub high: f64,
    /// Blocks behind the estimate. `1` means a screen pair: a point estimate
    /// with no interval, which the verdict rules treat differently.
    pub blocks: usize,
}

impl Effect {
    /// Whether this is a point estimate with no spread.
    #[must_use]
    pub fn is_screen(&self) -> bool {
        self.blocks < 2
    }

    /// The effect as a percentage change, negative being an improvement.
    #[must_use]
    pub fn percent(&self) -> f64 {
        (self.ratio - 1.0) * 100.0
    }
}

/// One balanced ABBA block: the two A runs and the two B runs it contains.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Block {
    pub a: [f64; 2],
    pub b: [f64; 2],
}

impl Block {
    /// The drift-corrected log ratio for this block.
    ///
    /// `ln( mean(B) / mean(A) )`. Averaging within the block before taking the
    /// ratio is what cancels drift that is linear across the block, and that
    /// cancellation is the entire reason for the ABBA ordering. Treating the
    /// two inner pairs as two independent observations would claim degrees of
    /// freedom the balancing was spent to buy.
    fn log_ratio(&self) -> Option<f64> {
        // Every one of the four runs is checked individually, before any
        // averaging. Validating only the block means would let a `0.00s` run
        // be averaged away by its partner — which is exactly the rig failure
        // this check exists for, and is what a test of this function caught.
        if self
            .a
            .iter()
            .chain(&self.b)
            .any(|value| *value <= 0.0 || !value.is_finite())
        {
            return None;
        }
        let a = f64::midpoint(self.a[0], self.a[1]);
        let b = f64::midpoint(self.b[0], self.b[1]);
        Some((b / a).ln())
    }
}

/// Why an effect could not be estimated.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EstimateError {
    /// No blocks at all.
    NoBlocks,
    /// A measurement was zero, negative, or not finite. Never silently
    /// dropped: a run that produced a nonsense time is a rig failure, and
    /// averaging around it would hide exactly the `0.00s` row that made this
    /// tooling necessary.
    NonPositiveMeasurement,
}

/// Estimate the paired effect over `blocks`, against `decisions` simultaneous
/// decisions.
///
/// With one block the result is a point estimate and `low == high == ratio`;
/// the caller must not read that as an interval.
///
/// # Errors
///
/// [`EstimateError`] when there are no blocks, or when any measurement is not
/// a positive finite number.
pub fn paired_effect(blocks: &[Block], decisions: usize) -> Result<Effect, EstimateError> {
    if blocks.is_empty() {
        return Err(EstimateError::NoBlocks);
    }

    let mut logs = Vec::with_capacity(blocks.len());
    for block in blocks {
        logs.push(
            block
                .log_ratio()
                .ok_or(EstimateError::NonPositiveMeasurement)?,
        );
    }

    let count = logs.len();
    #[expect(
        clippy::cast_precision_loss,
        reason = "block counts are campaign-bounded and far below 2^53"
    )]
    let n = count as f64;
    let mean = logs.iter().sum::<f64>() / n;

    if count < 2 {
        let ratio = mean.exp();
        return Ok(Effect {
            ratio,
            low: ratio,
            high: ratio,
            blocks: count,
        });
    }

    let variance = logs.iter().map(|log| (log - mean).powi(2)).sum::<f64>() / (n - 1.0);
    let standard_error = (variance / n).sqrt();
    let critical = critical_value(decisions, count - 1);
    let half_width = critical * standard_error;

    Ok(Effect {
        ratio: mean.exp(),
        low: (mean - half_width).exp(),
        high: (mean + half_width).exp(),
        blocks: count,
    })
}

/// The equal-weight geometric mean of several catalogs' effects.
///
/// Geometric because the per-catalog quantities are ratios, and the same
/// operation as the per-catalog estimate because both live in log space —
/// which is why log space is the representation rather than a convenience.
#[must_use]
pub fn geometric_mean(ratios: &[f64]) -> Option<f64> {
    if ratios.is_empty() || ratios.iter().any(|ratio| *ratio <= 0.0) {
        return None;
    }
    #[expect(
        clippy::cast_precision_loss,
        reason = "catalog counts are small integers"
    )]
    let count = ratios.len() as f64;
    Some((ratios.iter().map(|ratio| ratio.ln()).sum::<f64>() / count).exp())
}

/// The critical value for one of `decisions` simultaneous decisions at `df`
/// degrees of freedom.
///
/// Alpha is split across them by the **union bound**: `P(∪ᵢ Eᵢ) ≤ Σᵢ P(Eᵢ)`
/// holds whatever the dependence between them, so this is valid by proof
/// rather than by simulation, and it stays valid when catalogs escalate
/// independently and therefore sit at different looks.
///
/// `decisions` is `ladder x |VERDICT_BEARING|`, not the ladder alone. The
/// ladder prices escalation — each rung is another look at the same data —
/// and the metric family prices selection, because every verdict-bearing
/// metric is computed and reported on every run whether or not the campaign
/// declared it. See [`crate::campaign::Campaign::decisions`].
///
/// Bonferroni is chosen over a true group-sequential boundary deliberately. A
/// Pocock boundary is not determined by the look count alone — it depends on
/// the correlation between cumulative looks, normally parameterized by
/// information fractions — so "the Pocock constant for K" is not a
/// specification an implementer can follow, and no amount of monotonicity
/// testing would establish its coverage. The cost is width: at three
/// decisions this uses a 98.33% interval where Pocock would use about 97.8%.
///
/// The remaining approximation is the `t` quantile's own normality assumption
/// about the per-block log ratios. That is an assumption about the data, not
/// about the sequential procedure, and escalation does not affect it.
#[must_use]
pub fn critical_value(decisions: usize, df: usize) -> f64 {
    #[expect(
        clippy::cast_precision_loss,
        reason = "decision counts are small integers"
    )]
    let decisions = decisions.max(1) as f64;
    student_t_quantile(1.0 - ALPHA / (2.0 * decisions), df)
}

/// The `p`-quantile of Student's t with `df` degrees of freedom.
///
/// Computed rather than tabulated so any `(looks, df)` a ladder can reach is
/// covered; pinned reference values guard the implementation.
#[must_use]
pub fn student_t_quantile(p: f64, df: usize) -> f64 {
    assert!(df >= 1, "a t quantile needs at least one degree of freedom");
    assert!(
        p > 0.5 && p < 1.0,
        "this quantile is used for upper tails only"
    );

    // Bisection on a monotone CDF. The upper bracket is generous: at df = 1
    // and p = 0.9999 the quantile is already in the thousands.
    let (mut low, mut high) = (0.0_f64, 1.0e7_f64);
    for _ in 0..200 {
        let mid = f64::midpoint(low, high);
        if student_t_cdf(mid, df) < p {
            low = mid;
        } else {
            high = mid;
        }
    }
    f64::midpoint(low, high)
}

/// `P(T <= t)` for Student's t with `df` degrees of freedom.
fn student_t_cdf(t: f64, df: usize) -> f64 {
    #[expect(
        clippy::cast_precision_loss,
        reason = "degrees of freedom are small integers"
    )]
    let v = df as f64;
    let x = v / (v + t * t);
    let tail = 0.5 * regularized_incomplete_beta(x, v / 2.0, 0.5);
    if t > 0.0 { 1.0 - tail } else { tail }
}

/// Regularized incomplete beta `I_x(a, b)`, by the standard continued fraction
/// with the symmetry reflection that keeps it converging.
fn regularized_incomplete_beta(x: f64, a: f64, b: f64) -> f64 {
    if x <= 0.0 {
        return 0.0;
    }
    if x >= 1.0 {
        return 1.0;
    }

    let front =
        (ln_gamma(a + b) - ln_gamma(a) - ln_gamma(b) + a * x.ln() + b * (1.0 - x).ln()).exp();

    if x < (a + 1.0) / (a + b + 2.0) {
        front * beta_continued_fraction(x, a, b) / a
    } else {
        1.0 - front * beta_continued_fraction(1.0 - x, b, a) / b
    }
}

/// Lentz's algorithm for the beta continued fraction.
#[expect(
    clippy::many_single_char_names,
    reason = "x, a, b, c and d are the published names in Lentz's formulation; \
              renaming them makes the implementation harder to check against it"
)]
fn beta_continued_fraction(x: f64, a: f64, b: f64) -> f64 {
    const TINY: f64 = 1.0e-30;
    const EPSILON: f64 = 1.0e-14;

    let qab = a + b;
    let qap = a + 1.0;
    let qam = a - 1.0;

    let mut c = 1.0_f64;
    let mut d = 1.0 - qab * x / qap;
    if d.abs() < TINY {
        d = TINY;
    }
    d = 1.0 / d;
    let mut result = d;

    for iteration in 1..=300_u32 {
        let m = f64::from(iteration);
        let two_m = 2.0 * m;

        // Even step.
        let numerator = m * (b - m) * x / ((qam + two_m) * (a + two_m));
        d = 1.0 + numerator * d;
        if d.abs() < TINY {
            d = TINY;
        }
        c = 1.0 + numerator / c;
        if c.abs() < TINY {
            c = TINY;
        }
        d = 1.0 / d;
        result *= d * c;

        // Odd step.
        let numerator = -(a + m) * (qab + m) * x / ((a + two_m) * (qap + two_m));
        d = 1.0 + numerator * d;
        if d.abs() < TINY {
            d = TINY;
        }
        c = 1.0 + numerator / c;
        if c.abs() < TINY {
            c = TINY;
        }
        d = 1.0 / d;
        let step = d * c;
        result *= step;

        if (step - 1.0).abs() < EPSILON {
            break;
        }
    }

    result
}

/// Lanczos approximation to `ln Γ(x)` for `x > 0`.
fn ln_gamma(x: f64) -> f64 {
    const COEFFICIENTS: [f64; 6] = [
        76.180_091_729_471_46,
        -86.505_320_329_416_77,
        24.014_098_240_830_91,
        -1.231_739_572_450_155,
        0.120_865_097_386_617_7e-2,
        -0.539_523_938_495_3e-5,
    ];

    let mut y = x;
    let tmp = x + 5.5;
    let tmp = tmp - (x + 0.5) * tmp.ln();
    let mut series = 1.000_000_000_190_015;
    for coefficient in COEFFICIENTS {
        y += 1.0;
        series += coefficient / y;
    }
    -tmp + (2.506_628_274_631_000_5 * series / x).ln()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(actual: f64, expected: f64, tolerance: f64) -> bool {
        (actual - expected).abs() < tolerance
    }

    #[test]
    fn the_t_quantile_matches_published_table_values() {
        // Standard two-sided 95% and 99% points. If this drifts, every
        // interval the adjudicator prints is wrong by the same factor.
        for (p, df, expected) in [
            (0.975, 1, 12.7062),
            (0.975, 2, 4.302_65),
            (0.975, 5, 2.570_58),
            (0.975, 10, 2.228_14),
            (0.975, 30, 2.042_27),
            (0.995, 10, 3.169_27),
            (0.995, 2, 9.924_84),
        ] {
            let actual = student_t_quantile(p, df);
            assert!(
                close(actual, expected, 1.0e-3),
                "t({p}, {df}) = {actual}, expected {expected}"
            );
        }
    }

    #[test]
    fn the_critical_value_widens_with_looks_and_narrows_with_blocks() {
        // Monotonicity does not establish coverage — the union bound does —
        // but it catches an implementation that silently narrows an interval.
        for df in [2_usize, 5, 20] {
            let one = critical_value(1, df);
            let three = critical_value(3, df);
            let four = critical_value(4, df);
            assert!(one < three, "more looks must not narrow the interval");
            assert!(three < four, "more looks must not narrow the interval");
        }
        for looks in [1_usize, 3] {
            assert!(critical_value(looks, 2) > critical_value(looks, 20));
        }
    }

    #[test]
    fn a_single_look_ladder_is_the_ordinary_95_percent_interval() {
        assert!(close(critical_value(1, 10), 2.228_14, 1.0e-3));
    }

    #[test]
    fn three_looks_use_the_bonferroni_split_not_the_nominal_level() {
        // alpha/K = 0.05/3, so each look is a 98.33% interval.
        assert!(close(
            critical_value(3, 10),
            student_t_quantile(1.0 - 0.05 / 6.0, 10),
            1.0e-9
        ));
        assert!(critical_value(3, 10) > critical_value(1, 10));
    }

    #[test]
    fn a_block_differences_the_means_not_the_pairs() {
        // Drift rising across the block: A=10, B=11, B=12, A=13. Pairing
        // (B-A) naively would give +10% and -8%; the drift-corrected block
        // ratio is 11.5/11.5 = 1.0 exactly.
        let block = Block {
            a: [10.0, 13.0],
            b: [11.0, 12.0],
        };
        let effect = paired_effect(&[block], 1).expect("a positive block estimates");
        assert!(close(effect.ratio, 1.0, 1.0e-12));
    }

    #[test]
    fn one_block_is_a_point_estimate_with_no_interval() {
        let effect = paired_effect(
            &[Block {
                a: [10.0, 10.0],
                b: [9.0, 9.0],
            }],
            3,
        )
        .expect("a positive block estimates");

        assert!(effect.is_screen());
        assert!(close(effect.ratio, 0.9, 1.0e-12));
        assert!(close(effect.low, effect.ratio, 1.0e-12));
        assert!(close(effect.high, effect.ratio, 1.0e-12));
    }

    #[test]
    fn a_hand_computed_three_block_effect_matches() {
        // Ratios 0.90, 0.95, 1.00; mean log = (ln .9 + ln .95 + ln 1)/3.
        let blocks = [
            Block {
                a: [10.0, 10.0],
                b: [9.0, 9.0],
            },
            Block {
                a: [10.0, 10.0],
                b: [9.5, 9.5],
            },
            Block {
                a: [10.0, 10.0],
                b: [10.0, 10.0],
            },
        ];
        let effect = paired_effect(&blocks, 1).expect("positive blocks estimate");

        let expected_mean = ((0.90_f64).ln() + (0.95_f64).ln() + (1.00_f64).ln()) / 3.0;
        assert!(close(effect.ratio, expected_mean.exp(), 1.0e-12));
        assert_eq!(effect.blocks, 3);

        // Interval brackets the point estimate and, at three blocks, is wide.
        assert!(effect.low < effect.ratio && effect.ratio < effect.high);
        assert!(
            effect.high - effect.low > 0.05,
            "three blocks are imprecise"
        );
    }

    #[test]
    fn identical_arms_give_a_ratio_of_one_and_a_zero_width_interval() {
        let blocks = [
            Block {
                a: [5.0, 5.0],
                b: [5.0, 5.0],
            },
            Block {
                a: [7.0, 7.0],
                b: [7.0, 7.0],
            },
        ];
        let effect = paired_effect(&blocks, 1).expect("positive blocks estimate");
        assert!(close(effect.ratio, 1.0, 1.0e-12));
        assert!(close(effect.low, 1.0, 1.0e-12));
        assert!(close(effect.high, 1.0, 1.0e-12));
    }

    #[test]
    fn a_nonsense_measurement_is_refused_rather_than_averaged_around() {
        // The `0.00s` row that made this tooling necessary. Note the partner
        // run is healthy: validating the block mean would let 0.0 and 10.0
        // average to a perfectly plausible 5.0, so the check has to be
        // per-run. This assertion is what found that bug.
        for bad in [0.0, -1.0, f64::NAN, f64::INFINITY] {
            assert_eq!(
                paired_effect(
                    &[Block {
                        a: [bad, 10.0],
                        b: [9.0, 9.0],
                    }],
                    1
                ),
                Err(EstimateError::NonPositiveMeasurement),
                "a {bad} run must not be averaged away by its partner"
            );
        }

        assert_eq!(paired_effect(&[], 1), Err(EstimateError::NoBlocks));
    }

    #[test]
    fn the_aggregate_is_geometric_not_arithmetic() {
        // 0.5 and 2.0 cancel on the ratio scale; an arithmetic mean would
        // report 1.25 and claim a regression that is not there.
        assert!(close(
            geometric_mean(&[0.5, 2.0]).expect("positive ratios"),
            1.0,
            1.0e-12
        ));
        assert_eq!(geometric_mean(&[]), None);
        assert_eq!(geometric_mean(&[1.0, 0.0]), None);
    }
}
