//! Perceptual spring from [Rune](https://github.com/anomalyco/rune)
//! (`crates/rune-motion`, MIT). `Spring::new(duration, bounce)` matches SwiftUI:
//! ω = 2π / duration, ζ = 1 − bounce when bounce ≥ 0.
//!
//! FluxPlay only samples it. The window, the layout and the controls stay iced.

/// `duration` is seconds. `bounce` is in (−1, 1); 0 is critically damped.
#[derive(Clone, Copy, Debug)]
pub struct Spring {
    duration: f32,
    bounce: f32,
}

impl Spring {
    pub fn new(duration: f32, bounce: f32) -> Self {
        Self {
            duration: duration.max(1.0e-3),
            bounce: bounce.clamp(-0.9, 0.9),
        }
    }

    /// Position and velocity `dt` seconds after leaving `x` with velocity `v`
    /// toward `target`. A new target is sampled from the current pair, so the
    /// motion does not jump.
    pub fn sample(self, x: f32, v: f32, target: f32, dt: f32) -> (f32, f32) {
        let dt = dt.max(0.0);
        let omega = std::f32::consts::TAU / self.duration;
        let zeta = if self.bounce >= 0.0 {
            1.0 - self.bounce
        } else {
            1.0 / (1.0 + self.bounce)
        };
        let a = x - target;
        if a == 0.0 && v == 0.0 {
            return (target, 0.0);
        }
        if (zeta - 1.0).abs() < 1.0e-3 {
            let b = v + omega * a;
            let e = (-omega * dt).exp();
            let off = (a + b * dt) * e;
            let vel = ((b - omega * a) - omega * b * dt) * e;
            return (target + off, vel);
        }
        if zeta < 1.0 {
            let sigma = zeta * omega;
            let wd = omega * (1.0 - zeta * zeta).sqrt();
            let b = (v + sigma * a) / wd;
            let e = (-sigma * dt).exp();
            let (s, co) = (wd * dt).sin_cos();
            let off = e * (a * co + b * s);
            let vel = e * ((b * wd - sigma * a) * co + (-a * wd - sigma * b) * s);
            return (target + off, vel);
        }
        let root = (zeta * zeta - 1.0).sqrt();
        let r1 = -omega * (zeta - root);
        let r2 = -omega * (zeta + root);
        let c1 = (v - r2 * a) / (r1 - r2);
        let c2 = a - c1;
        let e1 = (r1 * dt).exp();
        let e2 = (r2 * dt).exp();
        (target + c1 * e1 + c2 * e2, c1 * r1 * e1 + c2 * r2 * e2)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn critical_spring_settles_without_a_jump() {
        let spring = Spring::new(0.35, 0.0);
        let (x, v) = spring.sample(0.0, 0.0, 1.0, 0.0);
        assert!((x - 0.0).abs() < 1e-5 && v.abs() < 1e-4);
        let mut x = 0.0;
        let mut v = 0.0;
        for _ in 0..90 {
            let next = spring.sample(x, v, 1.0, 1.0 / 60.0);
            x = next.0;
            v = next.1;
        }
        assert!((x - 1.0).abs() < 0.02, "{x}");
    }
}
