//! Global (per-user) application settings, persisted as `settings.json` in the
//! app data home alongside `models/` and `registry.json`. Holds the global
//! defaults layer for feature flags. Best-effort like `ModelSelection`:
//! missing or corrupt file loads as defaults, and unknown keys round-trip so
//! older and newer Kens can share the file.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::{Error, Result};

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct AppSettings {
    /// Global feature-flag defaults; bool values keyed by flag name.
    #[serde(default)]
    pub features: Map<String, Value>,
    /// The one background throttle (knowledge-core decision 34): its pace
    /// and whether background work is paused. None until a person sets it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub background: Option<crate::throttle::Setting>,
    /// Forward-compat passthrough for keys this build doesn't know about.
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

pub fn settings_path(base_dir: &Path) -> PathBuf {
    base_dir.join("settings.json")
}

impl AppSettings {
    /// The throttle setting: what a person set, else Gentle, paused when an
    /// older `backgroundExtraction: false` switched background work off.
    pub fn throttle(&self) -> crate::throttle::Setting {
        self.background.unwrap_or_else(|| crate::throttle::Setting {
            paused: self.features.get("backgroundExtraction").and_then(Value::as_bool) == Some(false),
            ..Default::default()
        })
    }

    pub fn load(base_dir: &Path) -> AppSettings {
        match std::fs::read_to_string(settings_path(base_dir)) {
            Ok(raw) => serde_json::from_str(&raw).unwrap_or_default(),
            Err(_) => AppSettings::default(),
        }
    }

    pub fn save(&self, base_dir: &Path) -> Result<()> {
        let path = settings_path(base_dir);
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| Error::io(dir, e))?;
        }
        let json = serde_json::to_string_pretty(self).map_err(|e| Error::Other(e.to_string()))?;
        // Atomic write: write to a sibling temp file, then rename over the
        // target so a crash mid-write can't leave a truncated settings.json.
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, json + "\n").map_err(|e| Error::io(&tmp, e))?;
        std::fs::rename(&tmp, &path).map_err(|e| Error::io(&path, e))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn missing_file_loads_defaults() {
        let dir = tempdir().unwrap();
        let settings = AppSettings::load(dir.path());
        assert_eq!(settings, AppSettings::default());
        assert!(settings.features.is_empty());
    }

    #[test]
    fn corrupt_file_loads_defaults() {
        let dir = tempdir().unwrap();
        std::fs::write(settings_path(dir.path()), "{ not valid json").unwrap();
        let settings = AppSettings::load(dir.path());
        assert_eq!(settings, AppSettings::default());
    }

    #[test]
    fn save_load_roundtrip() {
        let dir = tempdir().unwrap();
        let mut settings = AppSettings::default();
        settings.features.insert("semanticIndex".into(), true.into());
        settings.save(dir.path()).unwrap();

        let loaded = AppSettings::load(dir.path());
        assert_eq!(
            loaded.features.get("semanticIndex").and_then(Value::as_bool),
            Some(true)
        );
    }

    #[test]
    fn unknown_keys_survive_roundtrip() {
        let dir = tempdir().unwrap();
        // A newer Ken wrote a key this build doesn't model.
        let raw = r#"{"features":{"semanticIndex":true},"futureKnob":{"nested":42}}"#;
        std::fs::write(settings_path(dir.path()), raw).unwrap();

        let loaded = AppSettings::load(dir.path());
        assert!(loaded.extra.contains_key("futureKnob"));

        // Round-trips through save unchanged.
        loaded.save(dir.path()).unwrap();
        let reloaded = AppSettings::load(dir.path());
        assert_eq!(reloaded.extra.get("futureKnob"), loaded.extra.get("futureKnob"));
        assert_eq!(
            reloaded.features.get("semanticIndex").and_then(Value::as_bool),
            Some(true)
        );
    }

    #[test]
    fn the_throttle_setting_round_trips_and_an_old_switch_reads_as_pause() {
        use crate::throttle::{Pace, Setting};
        let dir = tempdir().unwrap();
        let mut settings = AppSettings::default();
        assert_eq!(settings.throttle(), Setting::default());
        settings.features.insert("backgroundExtraction".into(), false.into());
        assert!(settings.throttle().paused, "background reading switched off is a pause");
        settings.background = Some(Setting { pace: Pace::Gentle, paused: false });
        settings.save(dir.path()).unwrap();
        assert_eq!(AppSettings::load(dir.path()).throttle(), Setting { pace: Pace::Gentle, paused: false }, "a set throttle wins");
    }

    #[test]
    fn no_temp_file_left_after_save() {
        let dir = tempdir().unwrap();
        AppSettings::default().save(dir.path()).unwrap();
        assert!(!settings_path(dir.path()).with_extension("json.tmp").exists());
        assert!(settings_path(dir.path()).exists());
    }
}
