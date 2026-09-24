//! Resource request arithmetic (T04-C05; docs/v5/03_DOMAIN.md §命令与退出).
//!
//! Canonical native units: positive integer ms timeouts, integer bytes,
//! integer cpu_millis, integer PIDs. JSON-level boundary rejection (bool as
//! number, unknown enum, empty required) is already frozen in the T41
//! ExecSpec fixtures; THIS module owns the arithmetic: unit conversions used
//! by adapters MUST be checked — overflow, NaN/Infinity, negative and
//! sub-millisecond losses are INVALID_ARGUMENT, never silently clamped.
//!
//! Adapter trap pinned by test: "MB" is NOT assumed MiB — the caller must
//! name the unit system explicitly (03_DOMAIN.md).

use super::error::DomainError;

pub const MIB: u64 = 1_048_576;
pub const MB: u64 = 1_000_000;

/// Physical resource demand. Every present value is a positive integer in
/// canonical units; zero is meaningless and rejected at construction.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ResourceRequest {
    pub memory_bytes: Option<u64>,
    pub cpu_millis: Option<u64>,
    pub pids: Option<u64>,
    pub storage_bytes: Option<u64>,
}

fn positive(field: &str, value: u64) -> Result<u64, DomainError> {
    if value == 0 {
        return Err(DomainError::invalid(format!(
            "{field} must be a positive integer in canonical units"
        )));
    }
    Ok(value)
}

impl ResourceRequest {
    pub fn unconstrained() -> Self {
        Self::default()
    }

    pub fn with_memory_bytes(mut self, value: u64) -> Result<Self, DomainError> {
        self.memory_bytes = Some(positive("memory_bytes", value)?);
        Ok(self)
    }

    pub fn with_cpu_millis(mut self, value: u64) -> Result<Self, DomainError> {
        self.cpu_millis = Some(positive("cpu_millis", value)?);
        Ok(self)
    }

    pub fn with_pids(mut self, value: u64) -> Result<Self, DomainError> {
        self.pids = Some(positive("pids", value)?);
        Ok(self)
    }

    pub fn with_storage_bytes(mut self, value: u64) -> Result<Self, DomainError> {
        self.storage_bytes = Some(positive("storage_bytes", value)?);
        Ok(self)
    }

    /// Every present demand must fit within every present budget dimension.
    /// An absent budget dimension constrains nothing (admission may still
    /// reject unconstrained requests elsewhere — that is policy, not math).
    pub fn fits_within(&self, budget: &ResourceRequest) -> bool {
        fn fits(req: Option<u64>, cap: Option<u64>) -> bool {
            match (req, cap) {
                (Some(r), Some(c)) => r <= c,
                _ => true,
            }
        }
        fits(self.memory_bytes, budget.memory_bytes)
            && fits(self.cpu_millis, budget.cpu_millis)
            && fits(self.pids, budget.pids)
            && fits(self.storage_bytes, budget.storage_bytes)
    }
}

/// Adapter conversion: fractional SECONDS → integer milliseconds (canonical).
///
/// * NaN / Infinity / negative → INVALID_ARGUMENT
/// * result overflowing u64 → INVALID_ARGUMENT
/// * positive input that would round to 0 ms (sub-ms loss) → INVALID_ARGUMENT
pub fn seconds_to_ms(seconds: f64) -> Result<u64, DomainError> {
    if !seconds.is_finite() {
        return Err(DomainError::invalid(
            "timeout seconds must be finite (NaN/Infinity rejected)",
        ));
    }
    if seconds < 0.0 {
        return Err(DomainError::invalid("timeout seconds must not be negative"));
    }
    let ms = seconds * 1000.0;
    if ms > (u64::MAX as f64) {
        return Err(DomainError::invalid("timeout conversion overflows u64 ms"));
    }
    let rounded = ms.round();
    if seconds > 0.0 && rounded < 1.0 {
        return Err(DomainError::invalid(
            "sub-millisecond timeout cannot be represented in canonical ms",
        ));
    }
    Ok(rounded as u64)
}

/// Adapter conversion: mega-units → bytes. The unit system is EXPLICIT —
/// `binary=true` for MiB, `false` for decimal MB. Callers must never guess.
pub fn mb_to_bytes(mb: u64, binary: bool) -> Result<u64, DomainError> {
    if mb == 0 {
        return Err(DomainError::invalid("mega-unit input must be positive"));
    }
    let unit = if binary { MIB } else { MB };
    mb.checked_mul(unit).ok_or_else(|| {
        DomainError::invalid(format!(
            "{mb} {} overflows bytes",
            if binary { "MiB" } else { "MB" }
        ))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- T04-C05: illegal resource/time parameters ----

    #[test]
    fn seconds_to_ms_rejects_non_finite_and_negative() {
        for bad in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert!(seconds_to_ms(bad).is_err(), "{bad} must be rejected");
        }
        assert!(seconds_to_ms(-0.5).is_err());
        assert!(seconds_to_ms(-1.0).is_err());
    }

    #[test]
    fn seconds_to_ms_checked_arithmetic() {
        assert_eq!(seconds_to_ms(0.0).unwrap(), 0);
        assert_eq!(seconds_to_ms(1.5).unwrap(), 1500);
        assert_eq!(seconds_to_ms(30.0).unwrap(), 30_000);
        // sub-millisecond positive input cannot be represented faithfully
        assert!(seconds_to_ms(0.0004).is_err());
        // overflow must be an error, never a clamp
        assert!(seconds_to_ms(f64::MAX).is_err());
        assert!(seconds_to_ms(1.0e30).is_err());
        // largest in-range seconds value saturates cleanly into u64-ms
        let v = seconds_to_ms((u64::MAX / 1000) as f64).unwrap();
        assert!(v > 0);
    }

    #[test]
    fn mb_units_are_explicit_and_overflow_checked() {
        assert_eq!(mb_to_bytes(1, true).unwrap(), MIB);
        assert_eq!(mb_to_bytes(1, false).unwrap(), MB);
        assert_ne!(mb_to_bytes(1, true).unwrap(), mb_to_bytes(1, false).unwrap());
        assert!(mb_to_bytes(0, true).is_err());
        assert!(mb_to_bytes(u64::MAX, true).is_err());
        // (MAX / MB) * MB fits; one unit more must overflow into an error
        assert_eq!(mb_to_bytes(u64::MAX / MB, false).unwrap(), (u64::MAX / MB) * MB);
        assert!(mb_to_bytes(u64::MAX / MB + 1, false).is_err());
        // largest binary conversion stays exact
        assert_eq!(mb_to_bytes(u64::MAX / MIB, true).unwrap(), (u64::MAX / MIB) * MIB);
    }

    #[test]
    fn resource_request_fields_are_positive() {
        assert!(ResourceRequest::unconstrained().with_memory_bytes(0).is_err());
        assert!(ResourceRequest::unconstrained().with_cpu_millis(0).is_err());
        assert!(ResourceRequest::unconstrained().with_pids(0).is_err());
        assert!(ResourceRequest::unconstrained().with_storage_bytes(0).is_err());
        let ok = ResourceRequest::unconstrained()
            .with_memory_bytes(8 * MIB)
            .unwrap()
            .with_cpu_millis(500)
            .unwrap();
        assert_eq!(ok.memory_bytes, Some(8 * MIB));
        assert_eq!(ok.cpu_millis, Some(500));
        assert_eq!(ok.pids, None);
    }

    #[test]
    fn budget_fit_is_dimensionwise() {
        let req = ResourceRequest::unconstrained()
            .with_memory_bytes(4 * MIB)
            .unwrap()
            .with_cpu_millis(500)
            .unwrap();
        let roomy = ResourceRequest::unconstrained()
            .with_memory_bytes(8 * MIB)
            .unwrap()
            .with_cpu_millis(1000)
            .unwrap();
        assert!(req.fits_within(&roomy));

        let tight_mem = ResourceRequest::unconstrained().with_memory_bytes(MIB).unwrap();
        assert!(!req.fits_within(&tight_mem));

        // absent budget dimension constrains nothing
        let no_cpu_budget = ResourceRequest::unconstrained().with_memory_bytes(8 * MIB).unwrap();
        assert!(req.fits_within(&no_cpu_budget));

        // unconstrained request always fits
        assert!(ResourceRequest::unconstrained().fits_within(&roomy));
    }
}
