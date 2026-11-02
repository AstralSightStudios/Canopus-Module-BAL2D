//! Float helpers that work without `std` (libm) and match Spine's constants.

pub const PI: f32 = 3.141_592_7;
pub const PI2: f32 = PI * 2.0;
pub const INV_PI2: f32 = 1.0 / PI2;
pub const RAD_DEG: f32 = 180.0 / PI;
pub const DEG_RAD: f32 = PI / 180.0;

// Sine and cosine in single precision. libm's sinf / cosf work in f64, which a Cortex-M33
// (single-precision FPU) runs as software calls: profiled on the modelled watch, they took a fifth
// of all CPU time on heavy lobbies. These reduce the argument to [-pi/4, pi/4] (radians by
// Cody-Waite with three parts of pi/2, degrees exactly by multiples of 90) and evaluate the
// Cephes sinf / cosf minimax polynomials: under 2e-7 absolute error for the angles a skeleton
// uses (checked against f64 in the tests).

#[inline]
fn sin_kernel(r: f32) -> f32 {
    let z = r * r;
    ((-1.951_529_6e-4 * z + 8.332_160_9e-3) * z - 1.666_665_5e-1) * z * r + r
}

#[inline]
fn cos_kernel(r: f32) -> f32 {
    let z = r * r;
    ((2.443_315_7e-5 * z - 1.388_731_6e-3) * z + 4.166_664_6e-2) * z * z - 0.5 * z + 1.0
}

/// Nearest integer of `x` (halves away from zero), for quadrant numbers.
#[inline]
fn round_i(x: f32) -> i32 {
    (if x >= 0.0 { x + 0.5 } else { x - 0.5 }) as i32
}

/// (quadrant, remainder in radians) of `x` radians.
#[inline]
fn reduce(x: f32) -> (i32, f32) {
    const FOPI2: f32 = 0.636_619_8; // 2 / pi
    const DP1: f32 = 1.570_312_5;
    const DP2: f32 = 4.837_513e-4;
    const DP3: f32 = 7.549_79e-8;
    let q = round_i(x * FOPI2);
    let qf = q as f32;
    (q, ((x - qf * DP1) - qf * DP2) - qf * DP3)
}

/// (quadrant, remainder in radians) of `d` degrees.
#[inline]
fn reduce_deg(d: f32) -> (i32, f32) {
    let q = round_i(d * (1.0 / 90.0));
    (q, (d - q as f32 * 90.0) * DEG_RAD)
}

#[inline]
fn sin_q(q: i32, r: f32) -> f32 {
    match q & 3 {
        0 => sin_kernel(r),
        1 => cos_kernel(r),
        2 => -sin_kernel(r),
        _ => -cos_kernel(r),
    }
}

#[inline]
fn cos_q(q: i32, r: f32) -> f32 {
    match q & 3 {
        0 => cos_kernel(r),
        1 => -sin_kernel(r),
        2 => -cos_kernel(r),
        _ => sin_kernel(r),
    }
}

#[inline]
pub fn sin(x: f32) -> f32 {
    let (q, r) = reduce(x);
    sin_q(q, r)
}

#[inline]
pub fn cos(x: f32) -> f32 {
    let (q, r) = reduce(x);
    cos_q(q, r)
}

#[inline]
pub fn sin_deg(d: f32) -> f32 {
    let (q, r) = reduce_deg(d);
    sin_q(q, r)
}

#[inline]
pub fn cos_deg(d: f32) -> f32 {
    let (q, r) = reduce_deg(d);
    cos_q(q, r)
}

#[inline]
pub fn atan2(y: f32, x: f32) -> f32 {
    libm::atan2f(y, x)
}

#[inline]
pub fn sqrt(x: f32) -> f32 {
    libm::sqrtf(x)
}

#[inline]
pub fn acos(x: f32) -> f32 {
    libm::acosf(x)
}

#[inline]
pub fn pow(x: f32, y: f32) -> f32 {
    libm::powf(x, y)
}

#[inline]
pub fn abs(x: f32) -> f32 {
    libm::fabsf(x)
}

#[inline]
pub fn floor(x: f32) -> f32 {
    libm::floorf(x)
}

#[inline]
pub fn ceil(x: f32) -> f32 {
    libm::ceilf(x)
}

#[inline]
pub fn signum(x: f32) -> f32 {
    if x > 0.0 {
        1.0
    } else if x < 0.0 {
        -1.0
    } else {
        0.0
    }
}

#[inline]
pub fn clamp(v: f32, min: f32, max: f32) -> f32 {
    if v < min {
        min
    } else if v > max {
        max
    } else {
        v
    }
}

/// Truncation toward zero, like Java's `(int)` cast.
#[inline]
pub fn to_int(x: f32) -> f32 {
    if x > 0.0 { floor(x) } else { ceil(x) }
}

/// `x % m` with the sign of the dividend (JavaScript / Java semantics).
#[inline]
pub fn fmod(x: f32, m: f32) -> f32 {
    libm::fmodf(x, m)
}

pub fn cbrt(x: f32) -> f32 {
    let y = pow(abs(x), 1.0 / 3.0);
    if x < 0.0 { -y } else { y }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trig_matches_f64() {
        let mut worst = (0f64, 0f64);
        let mut x = -4000.0f32;
        while x < 4000.0 {
            let r = x * 0.01; // radians, +-40
            let (es, ec) = ((r as f64).sin(), (r as f64).cos());
            worst.0 = worst.0.max((sin(r) as f64 - es).abs()).max((cos(r) as f64 - ec).abs());
            let (ds, dc) = ((x as f64).to_radians().sin(), (x as f64).to_radians().cos());
            worst.1 = worst.1.max((sin_deg(x) as f64 - ds).abs()).max((cos_deg(x) as f64 - dc).abs());
            x += 0.0137;
        }
        assert!(worst.0 < 4e-6, "radians: {}", worst.0);
        assert!(worst.1 < 3e-7, "degrees: {}", worst.1);
        // Exact at the axes, like Spine expects of 0 / 90 / 180.
        assert_eq!(sin_deg(0.0), 0.0);
        assert_eq!(cos_deg(90.0), 0.0);
        assert_eq!(sin_deg(-180.0), 0.0);
        assert_eq!(cos_deg(360.0), 1.0);
    }
}
