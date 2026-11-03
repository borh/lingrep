//! Calibration evidence and comparison setup checks, separate from optimization verdicts.

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::campaign::{Campaign, calibration_effect};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MeasurementRig {
    pub hostname: String,
    pub affinity: String,
    pub backend: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CalibrationReference {
    pub path: String,
    pub manifest_digest: String,
    /// BLAKE3 of the complete calibration file, which covers the observations
    /// as well as the manifest.
    pub evidence_digest: String,
}

impl CalibrationReference {
    /// Seal a local calibration file without requiring it to have passed.
    ///
    /// # Errors
    /// Returns a diagnostic for an unreadable file, invalid JSON or invalid manifest seal.
    pub fn from_file(path: &Path) -> Result<Self, String> {
        let path = path
            .canonicalize()
            .map_err(|error| format!("{}: {error}", path.display()))?;
        let bytes = std::fs::read(&path).map_err(|error| format!("{}: {error}", path.display()))?;
        let calibration: Campaign = serde_json::from_slice(&bytes)
            .map_err(|error| format!("{}: {error}", path.display()))?;
        if calibration.manifest.digest().as_ref() != Some(&calibration.manifest_digest) {
            return Err(format!(
                "{}: calibration manifest seal is invalid",
                path.display()
            ));
        }
        if calibration
            .manifest
            .calibration_wall_tolerance_pct
            .is_none()
        {
            return Err(format!("{}: not an A/A calibration", path.display()));
        }
        Ok(Self {
            path: path.to_string_lossy().into_owned(),
            manifest_digest: calibration.manifest_digest,
            evidence_digest: format!("blake3:{}", blake3::hash(&bytes)),
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CalibrationAssessment {
    Missing,
    NotChecked,
    Unqualified(String),
    Checked {
        passed: bool,
        checks: BTreeMap<&'static str, bool>,
        declared_at_unix_seconds: Option<u64>,
    },
}

impl std::fmt::Display for CalibrationAssessment {
    fn fmt(&self, out: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Missing => write!(out, "missing (unqualified)"),
            Self::NotChecked => write!(out, "not checked (unqualified)"),
            Self::Unqualified(reason) => write!(out, "unqualified: {reason}"),
            Self::Checked { passed, checks, .. } => {
                if !passed {
                    write!(out, "failed (unqualified)")
                } else if checks.values().all(|value| *value) {
                    write!(out, "matched")
                } else {
                    write!(out, "mismatched (unqualified)")
                }
            }
        }
    }
}

/// Load the sealed reference and label qualification without changing the optimization verdict.
/// The comparison must pass [`crate::campaign::adjudicate`] before reporting its verdict.
#[must_use]
pub fn assess(comparison: &Campaign) -> CalibrationAssessment {
    let Some(reference) = &comparison.manifest.calibration else {
        return CalibrationAssessment::Missing;
    };
    let bytes = match std::fs::read(&reference.path) {
        Ok(bytes) => bytes,
        Err(error) => {
            return CalibrationAssessment::Unqualified(format!("{}: {error}", reference.path));
        }
    };
    if format!("blake3:{}", blake3::hash(&bytes)) != reference.evidence_digest {
        return CalibrationAssessment::Unqualified("calibration evidence changed".to_owned());
    }
    let calibration: Campaign = match serde_json::from_slice(&bytes) {
        Ok(campaign) => campaign,
        Err(error) => {
            return CalibrationAssessment::Unqualified(format!("invalid calibration: {error}"));
        }
    };
    if calibration.manifest_digest != reference.manifest_digest {
        return CalibrationAssessment::Unqualified(
            "calibration manifest differs from the sealed reference".to_owned(),
        );
    }
    let effect = match calibration_effect(&calibration) {
        Ok(effect) => effect,
        Err(error) => {
            return CalibrationAssessment::Unqualified(format!("invalid calibration: {error}"));
        }
    };
    let tolerance = calibration
        .manifest
        .calibration_wall_tolerance_pct
        .unwrap_or_default()
        / 100.0;
    let expected = &calibration.manifest;
    let actual = &comparison.manifest;
    let rigs = actual
        .measurement_rig
        .as_ref()
        .zip(expected.measurement_rig.as_ref());
    let checks = BTreeMap::from([
        (
            "machine",
            actual.machine == expected.machine
                && rigs.is_some_and(|(a, b)| !a.hostname.is_empty() && a.hostname == b.hostname),
        ),
        (
            "workers",
            actual.workers.is_some() && actual.workers == expected.workers,
        ),
        (
            "affinity",
            rigs.is_some_and(|(a, b)| a.affinity == b.affinity),
        ),
        (
            "warmup_policy",
            actual.warmup_runs_per_arm.is_some_and(|n| n > 0)
                && actual.warmup_runs_per_arm == expected.warmup_runs_per_arm,
        ),
        (
            "backend",
            rigs.is_some_and(|(a, b)| !a.backend.is_empty() && a.backend == b.backend),
        ),
        (
            "catalog",
            actual.primary_catalog == expected.primary_catalog,
        ),
    ]);
    CalibrationAssessment::Checked {
        passed: effect.low >= 1.0 - tolerance && effect.high <= 1.0 + tolerance,
        checks,
        declared_at_unix_seconds: expected.declared_at_unix_seconds,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::campaign::tests::{calibration, seal};

    #[test]
    fn calibration_reference_checks_all_rig_fields_and_immutable_evidence() {
        let directory =
            std::env::temp_dir().join(format!("ling-calibration-reference-{}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("calibration.json");
        let mut evidence = calibration();
        evidence.manifest.warmup_runs_per_arm = Some(1);
        evidence.manifest.measurement_rig = Some(MeasurementRig {
            hostname: "minimal".to_owned(),
            affinity: String::new(),
            backend: "gnu-time".to_owned(),
        });
        evidence.manifest.declared_at_unix_seconds = Some(100);
        evidence = seal(evidence);
        std::fs::write(&path, serde_json::to_vec(&evidence).unwrap()).unwrap();
        let mut comparison = evidence.clone();
        comparison.manifest.calibration_wall_tolerance_pct = None;
        comparison.manifest.declared_at_unix_seconds = Some(200);
        comparison.manifest.calibration = Some(CalibrationReference::from_file(&path).unwrap());
        comparison = seal(comparison);
        let status = assess(&comparison);
        assert_eq!(status.to_string(), "matched");
        assert!(matches!(
            status,
            CalibrationAssessment::Checked {
                declared_at_unix_seconds: Some(100),
                ..
            }
        ));
        let crate::campaign::Adjudication {
            calibration: unchecked,
            ..
        } = crate::campaign::adjudicate(&comparison).unwrap();
        assert_eq!(unchecked, CalibrationAssessment::NotChecked);
        let mut report = crate::campaign::adjudicate(&comparison).unwrap();
        report.calibration = status;
        let rendered = crate::report::render(&comparison, &report);
        assert!(rendered.contains("Calibration: matched"));
        assert!(rendered.contains("age at comparison declaration: 100 seconds"));
        for field in [
            "machine",
            "workers",
            "affinity",
            "warmup_policy",
            "backend",
            "catalog",
        ] {
            assert!(rendered.contains(&format!("| {field} | match |")));
        }

        for field in [
            "machine",
            "workers",
            "affinity",
            "warmup_policy",
            "backend",
            "catalog",
        ] {
            let mut changed = comparison.clone();
            match field {
                "machine" => {
                    changed.manifest.measurement_rig.as_mut().unwrap().hostname =
                        "other-host".to_owned()
                }
                "workers" => changed.manifest.workers = Some(16),
                "affinity" => {
                    changed.manifest.measurement_rig.as_mut().unwrap().affinity = "0-7".to_owned()
                }
                "warmup_policy" => changed.manifest.warmup_runs_per_arm = Some(2),
                "backend" => {
                    changed.manifest.measurement_rig.as_mut().unwrap().backend = "other".to_owned()
                }
                "catalog" => changed.manifest.primary_catalog = "other.lx".to_owned(),
                other => panic!("unexpected configuration field: {other}"),
            }
            let CalibrationAssessment::Checked { passed, checks, .. } = assess(&seal(changed))
            else {
                panic!("expected field checks");
            };
            assert!(passed);
            assert!(!checks[field], "{field}");
            assert_eq!(
                checks.values().filter(|value| !**value).count(),
                1,
                "{field}"
            );
        }
        let mut unknown = comparison.clone();
        unknown.manifest.measurement_rig = None;
        let CalibrationAssessment::Checked { checks, .. } = assess(&seal(unknown)) else {
            panic!("expected unknown rig fields");
        };
        assert!(!checks["machine"]);
        let mut missing = comparison.clone();
        missing.manifest.calibration = None;
        assert_eq!(assess(&seal(missing)), CalibrationAssessment::Missing);
        let before = comparison.manifest_digest.clone();
        let mut changed = comparison.clone();
        changed
            .manifest
            .calibration
            .as_mut()
            .unwrap()
            .manifest_digest = "changed".to_owned();
        assert_ne!(seal(changed.clone()).manifest_digest, before);
        assert!(
            assess(&seal(changed))
                .to_string()
                .contains("differs from the sealed reference")
        );

        evidence.observations[0].wall_s *= 10.0;
        std::fs::write(&path, serde_json::to_vec(&evidence).unwrap()).unwrap();
        assert!(assess(&comparison).to_string().contains("evidence changed"));
        comparison.manifest.calibration = Some(CalibrationReference::from_file(&path).unwrap());
        let CalibrationAssessment::Checked { passed, .. } = assess(&seal(comparison.clone()))
        else {
            panic!("expected failed calibration");
        };
        assert!(!passed);
        evidence.manifest_digest = "invalid".to_owned();
        std::fs::write(&path, serde_json::to_vec(&evidence).unwrap()).unwrap();
        assert!(
            CalibrationReference::from_file(&path)
                .unwrap_err()
                .contains("seal is invalid")
        );
        std::fs::remove_file(&path).unwrap();
        assert!(assess(&comparison).to_string().contains("unqualified:"));
        std::fs::remove_dir_all(directory).unwrap();
    }
}
