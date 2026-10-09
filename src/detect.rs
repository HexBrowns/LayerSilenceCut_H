//! フレームごとの有音・無音から、カットする区間を決める（仕様 3.3）。
//!
//! 1. 解析対象のどれか 1 つのレイヤーで有音なら有音。どのレイヤーにも音声関連のオブジェクトが無いフレームは「対象外」
//! 2. 対象外は、「音声関連アイテムがない区間もカット」が OFF なら残す（有音とも無音とも数えない）、ON なら無音
//! 3. 最短有音時間: これ未満の有音の連なりで、前後の無音がどちらも最短無音時間以上のものを無音にする
//! 4. 最短無音時間: これ以上の無音の連なりだけをカット候補にする
//! 5. 有音区間拡張: 候補をシーンの端以外の側から縮める。0 以下になったら外す

use std::ops::Range;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameState {
    Voiced,
    Silent,
    /// 音声関連のオブジェクトが無い
    NoAudio,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Params {
    /// 無音判定基準（dB）。これ以下を無音
    pub threshold_db: f64,
    /// 最短無音時間（ms）
    pub min_silence_ms: f64,
    /// 最短有音時間（ms）。0 で無効
    pub min_voiced_ms: f64,
    /// 有音区間拡張（ms）
    pub padding_ms: f64,
    /// 音声関連アイテムがない区間もカット
    pub cut_no_audio: bool,
}

impl Default for Params {
    fn default() -> Self {
        Self { threshold_db: -40.0, min_silence_ms: 500.0, min_voiced_ms: 100.0, padding_ms: 100.0, cut_no_audio: false }
    }
}

/// 設定の範囲（原作と同じ）
pub const THRESHOLD_RANGE: (f64, f64) = (-60.0, 0.0);
pub const MIN_SILENCE_RANGE: (f64, f64) = (100.0, 5000.0);
pub const MIN_VOICED_RANGE: (f64, f64) = (0.0, 500.0);
pub const PADDING_RANGE: (f64, f64) = (0.0, 1000.0);

impl Params {
    pub fn clamped(self) -> Self {
        Self {
            threshold_db: self.threshold_db.clamp(THRESHOLD_RANGE.0, THRESHOLD_RANGE.1),
            min_silence_ms: self.min_silence_ms.clamp(MIN_SILENCE_RANGE.0, MIN_SILENCE_RANGE.1),
            min_voiced_ms: self.min_voiced_ms.clamp(MIN_VOICED_RANGE.0, MIN_VOICED_RANGE.1),
            padding_ms: self.padding_ms.clamp(PADDING_RANGE.0, PADDING_RANGE.1),
            cut_no_audio: self.cut_no_audio,
        }
    }
}

/// ms をフレーム数にする（四捨五入）
pub fn ms_to_frames(ms: f64, fps: f64) -> usize {
    (ms * fps / 1000.0).round().max(0.0) as usize
}

/// フレームの音量（dBFS）を有音・無音にする。`None` は音声関連のオブジェクトが無い
pub fn classify(level_db: Option<f64>, threshold_db: f64) -> FrameState {
    match level_db {
        None => FrameState::NoAudio,
        Some(db) if db > threshold_db => FrameState::Voiced,
        Some(_) => FrameState::Silent,
    }
}

/// レイヤーごとの結果をまとめる（どれか 1 つで有音なら有音、どれかで無音なら無音、全部対象外なら対象外）
pub fn combine(layers: &[Vec<FrameState>], len: usize) -> Vec<FrameState> {
    (0..len)
        .map(|i| {
            let mut out = FrameState::NoAudio;
            for l in layers {
                match l.get(i).copied().unwrap_or(FrameState::NoAudio) {
                    FrameState::Voiced => return FrameState::Voiced,
                    FrameState::Silent => out = FrameState::Silent,
                    FrameState::NoAudio => {}
                }
            }
            out
        })
        .collect()
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Class {
    Voiced,
    Silent,
    /// 残す（対象外で、カットしない設定）
    Keep,
}

/// 同じ値の連なり（値, 範囲）
fn runs<T: Copy + PartialEq>(v: &[T]) -> Vec<(T, Range<usize>)> {
    let mut out: Vec<(T, Range<usize>)> = Vec::new();
    for (i, x) in v.iter().enumerate() {
        match out.last_mut() {
            Some((y, r)) if y == x => r.end = i + 1,
            _ => out.push((*x, i..i + 1)),
        }
    }
    out
}

/// カットする区間（半開区間、シーン基準のフレーム）。昇順で重ならない
pub fn cut_ranges(states: &[FrameState], fps: f64, params: &Params) -> Vec<Range<usize>> {
    let p = params.clamped();
    let min_silence = ms_to_frames(p.min_silence_ms, fps).max(1);
    let min_voiced = ms_to_frames(p.min_voiced_ms, fps);
    let pad = ms_to_frames(p.padding_ms, fps);
    let len = states.len();

    let mut class: Vec<Class> = states
        .iter()
        .map(|s| match s {
            FrameState::Voiced => Class::Voiced,
            FrameState::Silent => Class::Silent,
            FrameState::NoAudio if p.cut_no_audio => Class::Silent,
            FrameState::NoAudio => Class::Keep,
        })
        .collect();

    // 3. 短い有音を無音に（判定は元の連なりで行う）
    if min_voiced > 0 {
        let r = runs(&class);
        let long_silence = |k: Option<usize>| match k {
            None => true, // シーンの端
            Some(k) => r[k].0 == Class::Silent && r[k].1.len() >= min_silence,
        };
        let mut to_silence = Vec::new();
        for (k, (c, range)) in r.iter().enumerate() {
            if *c == Class::Voiced
                && range.len() < min_voiced
                && long_silence(k.checked_sub(1))
                && long_silence((k + 1 < r.len()).then_some(k + 1))
                // 両端ともシーンの端（全体が短い音だけ）は消さない
                && r.len() > 1
            {
                to_silence.push(range.clone());
            }
        }
        for range in to_silence {
            class[range].fill(Class::Silent);
        }
    }

    // 4・5. 長い無音を候補にして、端以外の側から縮める
    let mut out = Vec::new();
    for (c, range) in runs(&class) {
        if c != Class::Silent || range.len() < min_silence {
            continue;
        }
        let s = if range.start == 0 { 0 } else { range.start + pad };
        let e = if range.end == len { len } else { range.end.saturating_sub(pad) };
        if s < e {
            out.push(s..e);
        }
    }
    out
}

/// カットの合計フレーム数
pub fn total_len(cuts: &[Range<usize>]) -> usize {
    cuts.iter().map(|r| r.len()).sum()
}

#[cfg(test)]
mod tests {
    use super::*;
    use FrameState::*;

    fn states(spec: &str) -> Vec<FrameState> {
        spec.chars()
            .filter(|c| !c.is_whitespace())
            .map(|c| match c {
                'v' => Voiced,
                's' => Silent,
                _ => NoAudio,
            })
            .collect()
    }

    /// 30fps 相当で扱いやすいように、1 フレーム = 100ms にする（fps 10）
    fn p(min_silence: f64, min_voiced: f64, pad: f64) -> Params {
        Params { threshold_db: -40.0, min_silence_ms: min_silence, min_voiced_ms: min_voiced, padding_ms: pad, cut_no_audio: false }
    }

    #[test]
    fn ms_rounding_30fps() {
        assert_eq!(ms_to_frames(500.0, 30.0), 15);
        assert_eq!(ms_to_frames(100.0, 30.0), 3);
        assert_eq!(ms_to_frames(1000.0, 30.0), 30);
        assert_eq!(ms_to_frames(0.0, 30.0), 0);
    }

    #[test]
    fn classify_threshold_is_inclusive_silence() {
        assert_eq!(classify(Some(-40.0), -40.0), Silent);
        assert_eq!(classify(Some(-39.9), -40.0), Voiced);
        assert_eq!(classify(None, -40.0), NoAudio);
        assert_eq!(classify(Some(f64::NEG_INFINITY), -40.0), Silent);
    }

    #[test]
    fn short_silence_is_kept_long_silence_is_cut_with_padding() {
        // v3 s3 v3 s8 v3（fps 10、最短無音 500ms = 5 フレーム、拡張 100ms = 1）
        let st = states("vvv sss vvv ssssssss vvv");
        let cuts = cut_ranges(&st, 10.0, &p(500.0, 0.0, 100.0));
        assert_eq!(cuts, vec![10..16]);
    }

    #[test]
    fn edges_are_not_padded() {
        let st = states("ssssss vvv ssssss");
        let cuts = cut_ranges(&st, 10.0, &p(500.0, 0.0, 100.0));
        assert_eq!(cuts, vec![0..5, 10..15]);
    }

    #[test]
    fn padding_can_remove_a_candidate() {
        let st = states("vv sssss vv");
        assert!(cut_ranges(&st, 10.0, &p(500.0, 0.0, 300.0)).is_empty());
        assert_eq!(cut_ranges(&st, 10.0, &p(500.0, 0.0, 200.0)), vec![4..5]);
    }

    #[test]
    fn min_voiced_removes_isolated_clicks_only() {
        // 2 フレームの音（200ms）が長い無音に挟まれる → 無音になって 1 つの長い候補に
        let st = states("vvvv sssss vv sssss vvvv");
        let cuts = cut_ranges(&st, 10.0, &p(500.0, 300.0, 0.0));
        assert_eq!(cuts, vec![4..16]);
        // 片側の無音が短ければ残す
        let st = states("vvvv ss vv sssss vvvv");
        let cuts = cut_ranges(&st, 10.0, &p(500.0, 300.0, 0.0));
        assert_eq!(cuts, vec![8..13]);
        // 0 で無効
        let st = states("vvvv sssss vv sssss vvvv");
        assert_eq!(cut_ranges(&st, 10.0, &p(500.0, 0.0, 0.0)), vec![4..9, 11..16]);
    }

    #[test]
    fn no_audio_is_kept_unless_enabled() {
        let st = states("vvv ...... vvv ssssss vvv");
        assert_eq!(cut_ranges(&st, 10.0, &p(500.0, 0.0, 0.0)), vec![12..18]);
        let mut q = p(500.0, 0.0, 0.0);
        q.cut_no_audio = true;
        assert_eq!(cut_ranges(&st, 10.0, &q), vec![3..9, 12..18]);
        // 対象外は短い有音の判定で「無音」に数えない
        let st = states("...... vv ssssss vvv");
        assert_eq!(cut_ranges(&st, 10.0, &p(500.0, 300.0, 0.0)), vec![8..14]);
    }

    #[test]
    fn combine_layers() {
        let a = states("vss..");
        let b = states("s.vs.");
        assert_eq!(combine(&[a, b], 6), states("vsvs.."));
    }
}
