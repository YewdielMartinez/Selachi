//! Motion tokens: cubic-bezier easings and durations (PLAN section 11).

/// CSS-style cubic bezier with implicit endpoints (0,0) and (1,1).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CubicBezier {
    pub x1: f32,
    pub y1: f32,
    pub x2: f32,
    pub y2: f32,
}

impl CubicBezier {
    pub const fn new(x1: f32, y1: f32, x2: f32, y2: f32) -> Self {
        Self { x1, y1, x2, y2 }
    }

    #[inline]
    fn coord(a: f32, b: f32, s: f32) -> f32 {
        // B(s) = 3(1-s)^2 s a + 3(1-s) s^2 b + s^3
        let inv = 1.0 - s;
        3.0 * inv * inv * s * a + 3.0 * inv * s * s * b + s * s * s
    }

    #[inline]
    fn coord_derivative(a: f32, b: f32, s: f32) -> f32 {
        let inv = 1.0 - s;
        3.0 * inv * inv * a + 6.0 * inv * s * (b - a) + 3.0 * s * s * (1.0 - b)
    }

    /// Maps progress `t` (the x axis, clamped to 0..=1) to the eased value (y).
    /// y may leave 0..=1 for overshooting curves.
    pub fn solve(&self, t: f32) -> f32 {
        if t <= 0.0 {
            return 0.0;
        }
        if t >= 1.0 {
            return 1.0;
        }
        let (x1, y1, x2, y2) = (self.x1, self.y1, self.x2, self.y2);
        const EPS: f32 = 1e-6;

        // Newton-Raphson on x(s) = t, starting from s = t.
        let mut s = t;
        let mut converged = false;
        for _ in 0..8 {
            let err = Self::coord(x1, x2, s) - t;
            if err.abs() < EPS {
                converged = true;
                break;
            }
            let d = Self::coord_derivative(x1, x2, s);
            if d.abs() < 1e-6 {
                break;
            }
            s -= err / d;
            if !(0.0..=1.0).contains(&s) {
                break;
            }
        }

        if !converged {
            // Bisection fallback; x(s) is monotonic for x1, x2 in 0..=1.
            let (mut lo, mut hi) = (0.0_f32, 1.0_f32);
            s = t;
            for _ in 0..40 {
                let x = Self::coord(x1, x2, s);
                if (x - t).abs() < EPS {
                    break;
                }
                if x < t {
                    lo = s;
                } else {
                    hi = s;
                }
                s = 0.5 * (lo + hi);
            }
        }

        Self::coord(y1, y2, s)
    }

    /// Returns a closure usable as a GPUI animation easing.
    pub fn easing(self) -> impl Fn(f32) -> f32 + Clone + 'static {
        move |t| self.solve(t)
    }
}

pub const CLICK: CubicBezier = CubicBezier::new(0.37, 1.42, 0.37, 1.0);
pub const POP: CubicBezier = CubicBezier::new(0.1, 0.8, 0.0, 1.1);
pub const SHEET: CubicBezier = CubicBezier::new(0.16, 1.0, 0.3, 1.0);
pub const WIPE: CubicBezier = CubicBezier::new(0.38, 0.49, 0.0, 1.0);
pub const SHAKE: CubicBezier = CubicBezier::new(0.6, 0.6, 0.12, 1.26);

pub mod duration {
    use std::time::Duration;

    pub const FAST: Duration = Duration::from_millis(120);
    pub const BASE: Duration = Duration::from_millis(200);
    pub const SLOW: Duration = Duration::from_millis(320);
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: [CubicBezier; 5] = [CLICK, POP, SHEET, WIPE, SHAKE];

    #[test]
    fn endpoints() {
        for c in ALL {
            assert_eq!(c.solve(0.0), 0.0);
            assert_eq!(c.solve(1.0), 1.0);
            assert_eq!(c.solve(-0.5), 0.0);
            assert_eq!(c.solve(1.5), 1.0);
        }
    }

    #[test]
    fn linear_is_identity() {
        let l = CubicBezier::new(1.0 / 3.0, 1.0 / 3.0, 2.0 / 3.0, 2.0 / 3.0);
        for i in 0..=20 {
            let t = i as f32 / 20.0;
            assert!((l.solve(t) - t).abs() < 1e-4, "t={t}");
        }
    }

    #[test]
    fn ease_known_sample() {
        // CSS `ease` = (0.25, 0.1, 0.25, 1.0).
        let ease = CubicBezier::new(0.25, 0.1, 0.25, 1.0);
        assert!((ease.solve(0.5) - 0.8024).abs() < 1e-3);
        assert!((ease.solve(0.25) - 0.4085).abs() < 1e-3);
    }

    /// solve() must agree with a brute-force dense parametric sweep.
    #[test]
    fn matches_dense_sweep() {
        for c in ALL {
            for i in 1..40 {
                let t = i as f32 / 40.0;
                let mut best = (f32::MAX, 0.0_f32);
                for k in 0..=20000 {
                    let s = k as f32 / 20000.0;
                    let e = (CubicBezier::coord(c.x1, c.x2, s) - t).abs();
                    if e < best.0 {
                        best = (e, s);
                    }
                }
                let y = CubicBezier::coord(c.y1, c.y2, best.1);
                assert!((c.solve(t) - y).abs() < 2e-3, "{c:?} t={t}");
            }
        }
    }

    #[test]
    fn overshoot_curves_overshoot_and_settle() {
        for c in [CLICK, POP, SHAKE] {
            let max = (0..=100)
                .map(|i| c.solve(i as f32 / 100.0))
                .fold(f32::MIN, f32::max);
            assert!(max > 1.0, "{c:?} should overshoot");
            assert!((c.solve(0.999) - 1.0).abs() < 0.05);
        }
    }

    #[test]
    fn non_overshoot_curves_are_monotonic() {
        for c in [SHEET, WIPE] {
            let mut prev = 0.0;
            for i in 0..=200 {
                let y = c.solve(i as f32 / 200.0);
                assert!(y >= prev - 1e-5, "{c:?}");
                assert!(y <= 1.0 + 1e-4);
                prev = y;
            }
        }
    }

    #[test]
    fn easing_closure_works() {
        let f = SHEET.easing();
        let g = f.clone();
        assert_eq!(f(0.0), 0.0);
        assert_eq!(g(1.0), 1.0);
    }

    #[test]
    fn durations() {
        assert_eq!(duration::FAST.as_millis(), 120);
        assert_eq!(duration::BASE.as_millis(), 200);
        assert_eq!(duration::SLOW.as_millis(), 320);
    }
}
