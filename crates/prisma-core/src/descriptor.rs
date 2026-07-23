//! Parameter descriptors: the metadata that makes the tree self-describing
//! (CLAUDE.md §4.5).
//!
//! Every leaf carries a [`ParamDescriptor`]. It is what lets the UI build
//! MIDI-learn menus by walking the tree instead of a hardcoded list, and what
//! maps the normalized `0.0..=1.0` external representation to a real engine
//! range through a response [`Curve`] (dimmers are deliberately non-linear —
//! §4.5).
//!
//! Labels are **i18n keys**, never literal user-facing strings (§2.1). The UI
//! resolves them against the active locale.

/// What kind of value a parameter holds. This drives read semantics and, for
/// [`ParamKind::Trigger`], the fire-counter fan-out (see `trigger.rs`).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ParamKind {
    /// A continuous level in `0.0..=1.0` (dimmer, opacity, speed).
    Level,
    /// A latching on/off. Stored as a level (`>= 0.5` is on) so it flows through
    /// the same machinery; exposed as a bool by convenience accessors.
    Toggle,
    /// A momentary event (scene trigger, tap). Has no held value: each write is
    /// one "fire", counted monotonically so edges survive coalescing.
    Trigger,
}

/// Maps the normalized `0.0..=1.0` external value onto a real range. Dimmers
/// want an exponential curve so the low end has usable resolution (§4.5).
#[derive(Clone, Copy, Debug)]
pub enum Curve {
    /// `real = min + norm * (max - min)`.
    Linear,
    /// Perceptual: `shape(norm) = norm^gamma`. `gamma > 1` pushes resolution to
    /// the low end (typical dimmer feel around `gamma = 2.0..2.4`).
    Exponential { gamma: f32 },
    /// Inverse of [`Curve::Exponential`]: `shape(norm) = norm^(1/gamma)`.
    Logarithmic { gamma: f32 },
}

impl Curve {
    /// Map a normalized input `0..=1` through the curve to a normalized output.
    fn shape(&self, x: f32) -> f32 {
        match self {
            Curve::Linear => x,
            Curve::Exponential { gamma } => x.powf(*gamma),
            Curve::Logarithmic { gamma } => x.powf(1.0 / *gamma),
        }
    }

    /// The inverse of [`shape`](Self::shape): real-normalized back to external.
    fn unshape(&self, y: f32) -> f32 {
        match self {
            Curve::Linear => y,
            Curve::Exponential { gamma } => y.powf(1.0 / *gamma),
            Curve::Logarithmic { gamma } => y.powf(*gamma),
        }
    }
}

/// The real-world range a normalized parameter maps onto.
#[derive(Clone, Copy, Debug)]
pub struct RealRange {
    pub min: f32,
    pub max: f32,
}

/// Self-describing metadata for one parameter. Cloneable and cheap to hand to
/// the UI (`Arc`-wrapped inside the tree).
#[derive(Clone, Debug)]
pub struct ParamDescriptor {
    pub kind: ParamKind,
    pub range: RealRange,
    pub curve: Curve,
    /// Default value in **normalized** `0.0..=1.0`.
    pub default_norm: f32,
    /// i18n key for the human-facing label (§2.1). Never a literal string.
    pub label_key: String,
    /// Optional unit i18n key (e.g. "unit.percent", "unit.hz").
    pub unit: Option<String>,
}

impl ParamDescriptor {
    /// A continuous level parameter.
    pub fn level(
        label_key: impl Into<String>,
        range: RealRange,
        curve: Curve,
        default_norm: f32,
    ) -> Self {
        Self {
            kind: ParamKind::Level,
            range,
            curve,
            default_norm: default_norm.clamp(0.0, 1.0),
            label_key: label_key.into(),
            unit: None,
        }
    }

    /// A unit-range linear level in `0..=1` (opacity, normalized dimmer).
    pub fn unit_level(label_key: impl Into<String>, default_norm: f32) -> Self {
        Self::level(
            label_key,
            RealRange { min: 0.0, max: 1.0 },
            Curve::Linear,
            default_norm,
        )
    }

    /// A latching toggle.
    pub fn toggle(label_key: impl Into<String>, default_on: bool) -> Self {
        Self {
            kind: ParamKind::Toggle,
            range: RealRange { min: 0.0, max: 1.0 },
            curve: Curve::Linear,
            default_norm: if default_on { 1.0 } else { 0.0 },
            label_key: label_key.into(),
            unit: None,
        }
    }

    /// A momentary trigger. Range/curve are irrelevant (the value is a fire
    /// count), so they are set to a harmless unit-linear default.
    pub fn trigger(label_key: impl Into<String>) -> Self {
        Self {
            kind: ParamKind::Trigger,
            range: RealRange { min: 0.0, max: 1.0 },
            curve: Curve::Linear,
            default_norm: 0.0,
            label_key: label_key.into(),
            unit: None,
        }
    }

    pub fn with_unit(mut self, unit_key: impl Into<String>) -> Self {
        self.unit = Some(unit_key.into());
        self
    }

    /// Normalized `0..=1` → real value through the curve.
    pub fn to_real(&self, norm: f32) -> f32 {
        let n = norm.clamp(0.0, 1.0);
        self.range.min + self.curve.shape(n) * (self.range.max - self.range.min)
    }

    /// Real value → normalized `0..=1` (inverse of [`to_real`](Self::to_real)).
    pub fn to_norm(&self, real: f32) -> f32 {
        let span = self.range.max - self.range.min;
        if span == 0.0 {
            return 0.0;
        }
        let y = ((real - self.range.min) / span).clamp(0.0, 1.0);
        self.curve.unshape(y)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn linear_roundtrips() {
        let d = ParamDescriptor::level(
            "l",
            RealRange {
                min: 0.0,
                max: 255.0,
            },
            Curve::Linear,
            0.0,
        );
        assert!((d.to_real(0.0) - 0.0).abs() < 1e-4);
        assert!((d.to_real(1.0) - 255.0).abs() < 1e-3);
        assert!((d.to_real(0.5) - 127.5).abs() < 1e-3);
        assert!((d.to_norm(127.5) - 0.5).abs() < 1e-4);
    }

    #[test]
    fn exponential_pushes_resolution_low_and_roundtrips() {
        let d = ParamDescriptor::level(
            "d",
            RealRange { min: 0.0, max: 1.0 },
            Curve::Exponential { gamma: 2.2 },
            0.0,
        );
        // Midpoint fader → dimmer well below 50% brightness (perceptual feel).
        assert!(d.to_real(0.5) < 0.3);
        // Endpoints are exact.
        assert!((d.to_real(0.0)).abs() < 1e-6);
        assert!((d.to_real(1.0) - 1.0).abs() < 1e-4);
        // Roundtrip.
        let n = 0.37_f32;
        assert!((d.to_norm(d.to_real(n)) - n).abs() < 1e-4);
    }

    #[test]
    fn out_of_range_inputs_clamp() {
        let d = ParamDescriptor::unit_level("u", 0.0);
        assert_eq!(d.to_real(2.0), 1.0);
        assert_eq!(d.to_real(-1.0), 0.0);
        assert_eq!(d.to_norm(9.0), 1.0);
    }
}
