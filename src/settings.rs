//! 設定の保存（`Plugin/LayerSilenceCut_H/settings.json`）。レイヤーの選択はプロジェクトごとに違うので保存しない。

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};

use serde::{Deserialize, Serialize};

use crate::detect::Params;

/// 設定ファイルが読めず、退避もできなかった。その間は保存しない（読めなかったファイルを初期値で上書きしない）
static SAVE_BLOCKED: AtomicBool = AtomicBool::new(false);

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

    /// 読めないとき（解析の失敗だけでなく、I/O エラーも）は `settings.json.broken-{秒}` へ退避して初期値にする。
    /// 退避できなければ、その間は保存しない（ルール `au2-plugin-checklist`「設定ファイルの下位互換」）
    pub fn load() -> Self {
        let path = Self::path();
        let parsed = match std::fs::read_to_string(&path) {
            Ok(text) => serde_json::from_str::<Settings>(&text).map_err(|e| e.to_string()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Self::default(),
            Err(e) => Err(e.to_string()),
        };
        match parsed {
            Ok(s) => Self::from_params(s.params()),
            Err(e) => {
                match backup_broken(&path) {
                    Ok(dest) => tracing::warn!(
                        "[LayerSilenceCut_H] 設定を読めないので {} へ退避し、初期値にします: {e}",
                        dest.display()
                    ),
                    Err(be) => {
                        SAVE_BLOCKED.store(true, Ordering::Release);
                        tracing::warn!("[LayerSilenceCut_H] 設定を読めず、退避もできないので、初期値で動き保存しません: {e} / {be}");
                    }
                }
                Self::default()
            }
        }
    }

    /// 一時ファイルに書いてから置き換える
    pub fn save(&self) {
        if SAVE_BLOCKED.load(Ordering::Acquire) {
            return;
        }
        let path = Self::path();
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        match serde_json::to_string_pretty(self) {
            Ok(text) => {
                let tmp = path.with_extension("json.tmp");
                if let Err(e) = std::fs::write(&tmp, text).and_then(|_| std::fs::rename(&tmp, &path)) {
                    tracing::warn!("[LayerSilenceCut_H] 設定を保存できません: {}: {e}", path.display());
                }
            }
            Err(e) => tracing::warn!("[LayerSilenceCut_H] 設定を保存できません: {e}"),
        }
    }
}

/// 読めなかった設定を `{名前}.broken-{秒}` へ移す
fn backup_broken(path: &std::path::Path) -> std::io::Result<PathBuf> {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let mut name = path.as_os_str().to_owned();
    name.push(format!(".broken-{secs}"));
    let dest = PathBuf::from(name);
    std::fs::rename(path, &dest)?;
    Ok(dest)
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
        // 知らない項目（新しい版が足したもの）は無視する
        let newer: Settings = serde_json::from_str(r#"{"threshold_db": -40, "future": true}"#).unwrap();
        assert_eq!(newer.threshold_db, -40.0);
    }

    #[test]
    fn broken_file_is_moved_aside() {
        let dir = std::env::temp_dir().join(format!("lsc_settings_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("settings.json");
        std::fs::write(&p, "{ broken").unwrap();
        let dest = backup_broken(&p).unwrap();
        assert!(!p.exists());
        assert_eq!(std::fs::read_to_string(dest).unwrap(), "{ broken");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
