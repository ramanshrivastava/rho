//! Small formatting helpers shared across the coding layer.

/// Format a float like Python's `%g` (default precision 6), used for the `bash`
/// timeout status text (`Command timed out after {timeout:g} seconds`) and the
/// transcript's `(timeout {g}s)` label.
///
/// `%g` picks fixed vs scientific notation by decimal exponent: exponent `< -4`
/// or `>= 6` → scientific (lowercase `e`, sign, ≥2 exponent digits), else fixed;
/// either way trailing zeros and a trailing `.` are stripped. Rust's plain
/// `Display` diverges from this below `1e-4` (`0.00001` vs tau's `1e-05`), which
/// is what this reproduces.
#[must_use]
pub(crate) fn format_g(value: f64) -> String {
    const PRECISION: i32 = 6;

    if value == 0.0 {
        return "0".to_string();
    }
    if value.is_nan() {
        return "nan".to_string();
    }
    if value.is_infinite() {
        return if value < 0.0 { "-inf" } else { "inf" }.to_string();
    }

    let neg = value.is_sign_negative();
    let a = value.abs();

    // Exact decimal exponent via `{:e}` (avoids `log10` rounding at powers of ten).
    let exponent: i32 = format!("{a:e}")
        .split_once('e')
        .and_then(|(_, e)| e.parse().ok())
        .unwrap_or(0);

    let body = if (-4..PRECISION).contains(&exponent) {
        let frac = usize::try_from((PRECISION - 1 - exponent).max(0)).unwrap_or(0);
        strip_trailing_zeros(&format!("{a:.frac$}"))
    } else {
        let mantissa_prec = usize::try_from(PRECISION - 1).unwrap_or(0);
        normalize_scientific(&format!("{a:.mantissa_prec$e}"))
    };

    if neg { format!("-{body}") } else { body }
}

fn strip_trailing_zeros(s: &str) -> String {
    if s.contains('.') {
        s.trim_end_matches('0').trim_end_matches('.').to_string()
    } else {
        s.to_string()
    }
}

/// Reformat Rust's `{:e}` scientific output (`1.50000e-5`) to Python's `%g`
/// scientific shape (`1.5e-05`): strip mantissa trailing zeros, sign the
/// exponent, zero-pad it to at least two digits.
fn normalize_scientific(s: &str) -> String {
    let Some((mantissa, exp)) = s.split_once('e') else {
        return s.to_string();
    };
    let mantissa = strip_trailing_zeros(mantissa);
    let exp: i32 = exp.parse().unwrap_or(0);
    let sign = if exp < 0 { '-' } else { '+' };
    format!("{mantissa}e{sign}{:02}", exp.abs())
}

#[cfg(test)]
mod tests {
    use super::format_g;

    #[test]
    fn matches_python_g_for_common_values() {
        assert_eq!(format_g(0.0), "0");
        assert_eq!(format_g(1.0), "1");
        assert_eq!(format_g(1.5), "1.5");
        assert_eq!(format_g(0.5), "0.5");
        assert_eq!(format_g(30.0), "30");
        assert_eq!(format_g(0.01), "0.01");
        assert_eq!(format_g(0.001), "0.001");
        assert_eq!(format_g(0.0001), "0.0001");
    }

    #[test]
    fn uses_scientific_below_1e_minus_4() {
        assert_eq!(format_g(0.000_01), "1e-05");
        assert_eq!(format_g(0.000_015), "1.5e-05");
    }

    #[test]
    fn uses_scientific_at_1e6() {
        assert_eq!(format_g(1_000_000.0), "1e+06");
        assert_eq!(format_g(100_000.0), "100000");
    }

    #[test]
    fn negative_values() {
        assert_eq!(format_g(-2.5), "-2.5");
        assert_eq!(format_g(-0.000_01), "-1e-05");
    }
}
