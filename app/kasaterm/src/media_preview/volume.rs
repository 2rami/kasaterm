#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Volume {
    gain: f32,
    pub(super) muted: bool,
}

impl Default for Volume {
    fn default() -> Self { Self { gain: 1.0, muted: false } }
}

impl Volume {
    pub(super) fn load(settings: &serde_json::Value) -> Self {
        let value = &settings["media_volume"];
        let mut volume = Self::default();
        if let Some(gain) = value["gain"].as_f64().or_else(|| value.as_f64()) {
            volume.set(gain as f32);
        }
        volume.muted |= value["muted"].as_bool().unwrap_or(false);
        volume
    }

    pub(super) fn value(self) -> f32 { if self.muted { 0.0 } else { self.gain } }

    pub(super) fn set(&mut self, value: f32) {
        if !value.is_finite() { return; }
        let value = value.clamp(0.0, 1.0);
        self.muted = value == 0.0;
        // Dragging to zero must not discard the level restored by unmuting.
        if value > 0.0 { self.gain = value; }
    }

    pub(super) fn toggle(&mut self) { self.muted = !self.muted; }

    pub(super) fn settings(self) -> serde_json::Value {
        serde_json::json!({"gain": self.gain, "muted": self.muted})
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mute_and_zero_restore_the_chosen_level() {
        let mut v = Volume::default();
        v.set(0.35);
        v.toggle();
        assert_eq!(v.value(), 0.0);
        v.toggle();
        assert_eq!(v.value(), 0.35);
        v.set(0.0);
        v.toggle();
        assert_eq!(v.value(), 0.35);
    }

    #[test]
    fn settings_preserve_mute_and_reject_invalid_values() {
        let mut v = Volume::default();
        v.set(0.6);
        v.toggle();
        let mut restored = Volume::load(&serde_json::json!({"media_volume":v.settings()}));
        assert_eq!(restored, v);
        restored.toggle();
        assert_eq!(restored.value(), 0.6);
        restored.set(f32::NAN);
        assert_eq!(restored.value(), 0.6);
        restored.set(5.0);
        assert_eq!(restored.value(), 1.0);
        restored.set(-1.0);
        assert_eq!(restored.value(), 0.0);
    }
}
