//! 設定の保存（`Plugin/LayerSilenceCut_H/settings.json`）。レイヤーの選択はプロジェクトごとに違うので保存しない。

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::detect::Params;

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub threshold_db: f64,
    pub min_silence_ms: f64,
    pub min_voiced_ms: f64,
    pub padding_ms: f64,
    pub cut_no_audio: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self::from_params(Params::default())
    }
}

impl Settings {
    pub fn from_params(p: Params) -> Self {
        Self {
            threshold_db: p.threshold_db,
            min_silence_ms: p.min_silence_ms,
            min_voiced_ms: p.min_voiced_ms,
            padding_ms: p.padding_ms,
            cut_no_audio: p.cut_no_audio,
        }
    }

    pub fn params(&self) -> Params {
        Params {
            threshold_db: self.threshold_db,
            min_silence_ms: self.min_silence_ms,
            min_voiced_ms: self.min_voiced_ms,
            padding_ms: self.padding_ms,
            cut_no_audio: self.cut_no_audio,
        }
        .clamped()
    }

    fn path() -> PathBuf {
        aviutl2::config::app_data_path().join("Plugin").join("LayerSilenceCut_H").join("settings.json")
    }

    pub fn load() -> Self {
        let Ok(text) = std::fs::read_to_string(Self::path()) else { return Self::default() };
        match serde_json::from_str::<Settings>(&text) {
            Ok(s) => Self::from_params(s.params()),
            Err(e) => {
                tracing::warn!("[LayerSilenceCut_H] 設定を読めないので初期値にします: {e}");
                Self::default()
            }
        }
    }

    pub fn save(&self) {
        let path = Self::path();
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        match serde_json::to_string_pretty(self) {
            Ok(text) => {
                if let Err(e) = std::fs::write(&path, text) {
                    tracing::warn!("[LayerSilenceCut_H] 設定を保存できません: {}: {e}", path.display());
                }
            }
            Err(e) => tracing::warn!("[LayerSilenceCut_H] 設定を保存できません: {e}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_roundtrip_and_missing_fields() {
        let s = Settings { threshold_db: -35.0, ..Default::default() };
        let text = serde_json::to_string(&s).unwrap();
        assert_eq!(serde_json::from_str::<Settings>(&text).unwrap(), s);
        // 足りない項目は初期値
        let partial: Settings = serde_json::from_str(r#"{"threshold_db": -50}"#).unwrap();
        assert_eq!(partial.min_silence_ms, 500.0);
        // 範囲の外は収める
        let wild = Settings { threshold_db: -100.0, min_silence_ms: 10.0, ..Default::default() };
        assert_eq!(wild.params().threshold_db, -60.0);
        assert_eq!(wild.params().min_silence_ms, 100.0);
    }
}
