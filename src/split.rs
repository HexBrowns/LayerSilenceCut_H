//! オブジェクトの一部分（ピース）を、元のエイリアスから作る（仕様 3.5）。
//!
//! SDK に「分割」の関数が無いので、元のエイリアスを読んでピースごとのエイリアスを組み立て、
//! `create_object_from_alias` で置き直す。値は元のオブジェクトから `get_object_track_value` で読む
//! （`Source`。シーン基準のフレーム）。
//!
//! | 項目 | ピースでの値 |
//! |---|---|
//! | `frame=` | 0、ピースの中に残る中間点（ピースの開始からの相対）、ピースの長さ - 1 |
//! | 点の数だけ値があるトラック | ピースの両端での元の値 ＋ 残った中間点の元の値 |
//! | 値が 2 つのトラック（中間点無視・再生範囲） | ピースの両端での元の値 |
//! | 移動無しのトラック | そのまま。メディアの `再生位置` だけはピースの開始の時点の位置にする |
//! | 区間ごとのチェック（`0,1,0`） | ピースに残った区間の分 |
//! | それ以外 | そのまま |
//!
//! 直線の移動なら、ピースの各フレームの値は元と一致する。加速・減速・イージングの区間を途中で切ると、
//! その区間の動きの形は変わる（区間の長さに対する割合で動くため）。
//! **正解は本体の「分割」。** 無音カットプローブ_H（`AI/devenv/probes/silence_cut_probe/`）で突き合わせる。

use crate::alias::AliasDoc;
use crate::track;

/// 元のオブジェクトの値を読む口（実機では `EditSection`、テストでは作り物）
pub trait Source {
    /// トラックバー項目か（`get_object_track_info` が成功するか）
    fn is_track(&self, effect: &str, ordinal: usize, item: &str) -> bool;
    /// シーン基準のフレームでの値（`get_object_track_value`）
    fn value_at(&self, effect: &str, ordinal: usize, item: &str, frame: usize) -> Option<f64>;
    /// シーンのフレームレート
    fn fps(&self) -> f64;
    /// メディアファイルの長さ（秒）。分からなければ `None`
    fn file_length(&self, path: &str) -> Option<f64>;
}

/// 区間ごとの直線補間（本体と同じ形。区間 i は点 i から点 i+1 へ、最後の区間は終了フレームで終点の値になる。
/// 値が 2 つなら全体を 1 本の直線）。実機で確かめた形（無音カットプローブ_H の P2・P3）
pub fn linear_at(points: &[usize], values: &[f64], frame: usize) -> f64 {
    let pts: Vec<usize> = if values.len() == 2 { vec![points[0], *points.last().unwrap()] } else { points.to_vec() };
    if values.len() < 2 || pts.len() < 2 {
        return values.first().copied().unwrap_or(0.0);
    }
    let i = section_of(&pts, frame).min(values.len() - 2);
    let (p0, p1) = (pts[i] as f64, pts[i + 1] as f64);
    let r = if p1 > p0 { (frame as f64 - p0) / (p1 - p0) } else { 0.0 };
    values[i] + (values[i + 1] - values[i]) * r
}

/// 再生位置を持つメディアの効果（本体はこれらの分割で再生位置を調整する。`aviutl2.txt` の「動画ファイル」「音声ファイル」）
pub const MEDIA_EFFECTS: [&str; 3] = ["動画ファイル", "音声ファイル", "シーン"];
pub const ITEM_POSITION: &str = "再生位置";
pub const ITEM_SPEED: &str = "再生速度";
pub const ITEM_LOOP: &str = "ループ再生";
pub const MODE_RANGE: &str = "再生範囲";
/// 設定の `|` の前のビット 8（時間制御。`直線移動(時間制御),8|`）
pub const FLAG_TIMECONTROL: u32 = 8;

/// 区間ごとのチェックの値か（`0,1,0` で、数が区間の数と同じ）
fn as_section_checks(value: &str, sections: usize) -> Option<Vec<&str>> {
    if sections < 2 {
        return None;
    }
    let parts: Vec<&str> = value.split(',').collect();
    (parts.len() == sections && parts.iter().all(|p| *p == "0" || *p == "1")).then_some(parts)
}

/// `frame` を含む区間の番号。区間 i は `points[i]` から次の点の手前まで（最後の区間は終了を含む）
fn section_of(points: &[usize], frame: usize) -> usize {
    let sections = points.len().saturating_sub(1).max(1);
    let i = points.iter().rposition(|p| *p <= frame).unwrap_or(0);
    i.min(sections - 1)
}

/// ピース `[a, b]`（シーン基準、両端を含む）のエイリアスを作る
pub fn piece_alias(raw: &str, a: usize, b: usize, src: &dyn Source) -> Result<String, String> {
    let mut doc = AliasDoc::parse(raw);
    let points = doc.frames();
    if points.len() < 2 {
        return Err(format!("オブジェクトの範囲が読めません（frame={points:?}）"));
    }
    let (start, end) = (points[0], *points.last().unwrap());
    if a > b || a < start || b > end {
        return Err(format!("ピース {a}〜{b} がオブジェクト {start}〜{end} の外です"));
    }
    let sections = points.len() - 1;
    // ピースの中に残る中間点（点の位置で）
    let kept: Vec<usize> = (1..points.len() - 1).filter(|&i| points[i] > a && points[i] < b).collect();

    let effects = doc.effects();
    for (sec_index, effect, ordinal) in effects {
        let is_media = MEDIA_EFFECTS.contains(&effect.as_str());
        let looped = doc.sections[sec_index].get(ITEM_LOOP) == Some("1");
        let file = doc.sections[sec_index].get("ファイル").map(str::to_string);
        // メディアの再生位置が進む量（秒）。オブジェクトの開始からフレーム f の手前まで（再生速度は % で、移動していてもよい）
        let advanced = |f: usize| -> f64 {
            (start..f).map(|x| src.value_at(&effect, ordinal, ITEM_SPEED, x).unwrap_or(100.0) / 100.0 / src.fps()).sum()
        };
        let keys: Vec<(String, String)> = doc.sections[sec_index]
            .lines
            .iter()
            .filter(|(k, _)| k != "effect.name" && !k.contains('.'))
            .cloned()
            .collect();
        for (key, value) in keys {
            let new_value = if src.is_track(&effect, ordinal, &key) {
                let mut t = track::parse(&value).map_err(|e| format!("{effect} の {key}: {e}"))?;
                let at = |f: usize| {
                    src.value_at(&effect, ordinal, &key, f)
                        .ok_or_else(|| format!("{effect} の {key} のフレーム {f} の値が読めません"))
                };
                if is_media && key == ITEM_POSITION && looped {
                    // ループ再生は本体も調整しない（P3 の c05）
                    None
                } else if t.mode.is_none() {
                    if is_media && key == ITEM_POSITION && a > start {
                        // 移動無しの再生位置: ピースの開始までに進んだ分を足す（P3 の c02 で本体と一致）
                        t.values[0] += advanced(a);
                        Some(t.to_text())
                    } else {
                        None
                    }
                } else if is_media && key == ITEM_POSITION && t.mode.as_deref() == Some(MODE_RANGE) && t.values.len() == 2 {
                    // 再生範囲 `始め,終わり`: 始めから等速で再生し、終わりで止める。終わりが 0 なら止めない（P2）。
                    // 本体の分割は、ピースの両端の時点の位置を書く。最後のピースの終わりは、元の終わり（0 ならファイルの長さ）で止める
                    // （P3 の c01・c03・c04 で 0,0 → 0,3.8 / 3.8,10 、1,6 → 1,4.8 / 4.8,6 、150% → 0,5.7 / 5.7,10）
                    let (p0, p1) = (t.values[0], t.values[1]);
                    let limit = if p1 > 0.0 { Some(p1) } else { file.as_deref().and_then(|f| src.file_length(f)) };
                    let clamp = |v: f64| limit.map_or(v, |l| v.min(l));
                    t.values = vec![clamp(p0 + advanced(a)), clamp(p0 + advanced(b + 1))];
                    Some(t.to_text())
                } else if t.flags() & FLAG_TIMECONTROL != 0 {
                    // 時間制御: get_object_track_value が 0 しか返さない（P2）。本体の分割は、ピースの端に
                    // 区間ごとの直線補間の値を書く（P3 の c12 で 889.09 / 894.55）
                    let lin = |f: usize| linear_at(&points, &t.values, f);
                    t.values = if t.values.len() == points.len() && points.len() > 2 {
                        let mut v = vec![lin(a)];
                        v.extend(kept.iter().map(|&i| t.values[i]));
                        v.push(lin(b));
                        v
                    } else {
                        vec![lin(a), lin(b)]
                    };
                    Some(t.to_text())
                } else if t.values.len() == points.len() && t.flags() & track::FLAG_TWOPOINT == 0 && points.len() > 2 {
                    let mut v = vec![at(a)?];
                    v.extend(kept.iter().map(|&i| t.values[i]));
                    v.push(at(b)?);
                    t.values = v;
                    Some(t.to_text())
                } else if t.values.len() == 2 || t.values.len() == points.len() {
                    // 値が 2 つ（中間点無視・再生範囲・中間点の無いオブジェクト）
                    t.values = vec![at(a)?, at(b)?];
                    Some(t.to_text())
                } else {
                    return Err(format!(
                        "{effect} の {key}: 値の数 {} が点の数 {} と合いません（{value}）",
                        t.values.len(),
                        points.len()
                    ));
                }
            } else if let Some(checks) = as_section_checks(&value, sections) {
                let mut v = vec![checks[section_of(&points, a)]];
                v.extend(kept.iter().map(|&i| checks[i]));
                Some(v.join(","))
            } else {
                None
            };
            if let Some(nv) = new_value {
                doc.sections[sec_index].set(&key, nv);
            }
        }
    }

    let mut frames = vec![0];
    frames.extend(kept.iter().map(|&i| points[i] - a));
    frames.push(b - a);
    doc.set_frames(&frames);
    Ok(doc.to_text())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::collections::HashMap;

    /// 直線移動だけを知っている作り物。点 `points` と各トラックの値から、本体と同じ形（とみられる）で補間する
    pub struct Fake {
        pub points: Vec<usize>,
        /// (効果名, 項目) → 値の文字列
        pub tracks: HashMap<(String, String), String>,
        pub fps: f64,
    }

    impl Source for Fake {
        fn is_track(&self, effect: &str, _ordinal: usize, item: &str) -> bool {
            self.tracks.contains_key(&(effect.to_string(), item.to_string()))
        }
        fn value_at(&self, effect: &str, _ordinal: usize, item: &str, frame: usize) -> Option<f64> {
            let t = track::parse(self.tracks.get(&(effect.to_string(), item.to_string()))?).ok()?;
            if t.mode.is_none() {
                return Some(t.values[0]);
            }
            let (pts, vals): (Vec<usize>, Vec<f64>) = if t.values.len() == 2 {
                (vec![self.points[0], *self.points.last().unwrap()], t.values.clone())
            } else {
                (self.points.clone(), t.values.clone())
            };
            let i = section_of(&pts, frame);
            let (p0, p1) = (pts[i] as f64, pts[i + 1] as f64);
            let r = ((frame as f64 - p0) / (p1 - p0)).clamp(0.0, 1.0);
            Some(vals[i] + (vals[i + 1] - vals[i]) * r)
        }
        fn fps(&self) -> f64 {
            self.fps
        }
        fn file_length(&self, _path: &str) -> Option<f64> {
            Some(10.0)
        }
    }

    fn fake(points: &[usize], tracks: &[(&str, &str, &str)]) -> Fake {
        Fake {
            points: points.to_vec(),
            tracks: tracks.iter().map(|(e, k, v)| ((e.to_string(), k.to_string()), v.to_string())).collect(),
            fps: 30.0,
        }
    }

    const TEXT: &str = "[Object]\nframe=30,90,200,329\n[Object.0]\neffect.name=テキスト\nテキスト=あ\n[Object.1]\neffect.name=標準描画\n\
                        X=0.00,600.00,1200.00,1800.00,直線移動,0\nY=0.00,300.00,直線移動,4\nZ=5.00\n[Object.2]\neffect.name=反転\n反転=0,1,0\n";

    fn src_text() -> Fake {
        fake(
            &[30, 90, 200, 329],
            &[
                ("標準描画", "X", "0.00,600.00,1200.00,1800.00,直線移動,0"),
                ("標準描画", "Y", "0.00,300.00,直線移動,4"),
                ("標準描画", "Z", "5.00"),
            ],
        )
    }

    #[test]
    fn splits_midpoints_linear() {
        let s = src_text();
        // 中間点 90 の後、200 の前で切る
        let left = AliasDoc::parse(&piece_alias(TEXT, 30, 129, &s).unwrap());
        assert_eq!(left.frames(), vec![0, 60, 99]);
        let x = left.sections[2].get("X").unwrap();
        assert_eq!(x, format!("0.00,600.00,{},直線移動,0", track::format_value(s.value_at("標準描画", 0, "X", 129).unwrap(), 2)));
        // 中間点無視は値 2 つのまま、両端の値
        let y = track::parse(left.sections[2].get("Y").unwrap()).unwrap();
        assert_eq!(y.values.len(), 2);
        assert_eq!(y.tail.as_deref(), Some("4"));
        assert_eq!(left.sections[2].get("Z"), Some("5.00"));
        // 区間ごとのチェックは残った 2 区間
        assert_eq!(left.sections[3].get("反転"), Some("0,1"));

        let right = AliasDoc::parse(&piece_alias(TEXT, 130, 329, &s).unwrap());
        assert_eq!(right.frames(), vec![0, 70, 199]);
        assert_eq!(right.sections[3].get("反転"), Some("1,0"));
        assert!(right.sections[2].get("X").unwrap().ends_with("1200.00,1800.00,直線移動,0"));
    }

    #[test]
    fn piece_values_match_original_for_linear() {
        // ピースの各フレームの値が元と一致する（作り物の補間の上で）
        let s = src_text();
        for (a, b) in [(30, 129), (130, 329), (60, 250), (95, 96)] {
            let doc = AliasDoc::parse(&piece_alias(TEXT, a, b, &s).unwrap());
            let pts: Vec<usize> = doc.frames().iter().map(|f| f + a).collect();
            let piece = fake(&pts, &[("標準描画", "X", doc.sections[2].get("X").unwrap()), ("標準描画", "Y", doc.sections[2].get("Y").unwrap())]);
            for f in a..=b {
                for k in ["X", "Y"] {
                    let want = s.value_at("標準描画", 0, k, f).unwrap();
                    let got = piece.value_at("標準描画", 0, k, f).unwrap();
                    assert!((want - got).abs() < 0.006, "{k} {a}-{b} フレーム {f}: 元 {want} / ピース {got}");
                }
            }
        }
    }

    #[test]
    fn media_position_advances() {
        let raw = "[Object]\nframe=30,329\n[Object.0]\neffect.name=音声ファイル\n再生位置=1.000\n再生速度=150.00\nループ再生=0\nファイル=a.wav\n";
        let s = fake(&[30, 329], &[("音声ファイル", "再生位置", "1.000"), ("音声ファイル", "再生速度", "150.00")]);
        let right = AliasDoc::parse(&piece_alias(raw, 130, 329, &s).unwrap());
        // 100 フレーム × 1.5 / 30fps = 5 秒進む
        assert_eq!(right.sections[1].get("再生位置"), Some("6.000"));
        assert_eq!(right.frames(), vec![0, 199]);
        // 左のピースは開始が同じなので変わらない
        let left = AliasDoc::parse(&piece_alias(raw, 30, 129, &s).unwrap());
        assert_eq!(left.sections[1].get("再生位置"), Some("1.000"));
        // ループ再生は調整しない（本体の説明「ループ再生しない場合は…調整されます」）
        let looped = raw.replace("ループ再生=0", "ループ再生=1");
        let r = AliasDoc::parse(&piece_alias(&looped, 130, 329, &s).unwrap());
        assert_eq!(r.sections[1].get("再生位置"), Some("1.000"));
    }

    /// 本体の分割の実機の値（無音カットプローブ_H v0.4.0 の P3、devenv 2.1.11a、2026-10-09）。
    /// オブジェクト 30〜329（10 秒の WAV）を 144 で分割した
    fn audio(position: &str, speed: &str) -> (String, Fake) {
        let raw = format!(
            "[Object]\nframe=30,329\n[Object.0]\neffect.name=音声ファイル\n再生位置={position}\n再生速度={speed}\nファイル=a.wav\nトラック=0\nループ再生=0\n\
             [Object.1]\neffect.name=音声再生\n音量=100.00\n左右=0.00\n"
        );
        let s = fake(&[30, 329], &[("音声ファイル", "再生位置", position), ("音声ファイル", "再生速度", speed), ("音声再生", "音量", "100.00")]);
        (raw, s)
    }

    fn position_of(alias: &str) -> String {
        AliasDoc::parse(alias).sections[1].get("再生位置").unwrap().to_string()
    }

    #[test]
    fn range_matches_host_split() {
        for (pos, speed, left, right) in [
            ("0.000,0.000,再生範囲,0", "100.00", "0.000,3.800,再生範囲,0", "3.800,10.000,再生範囲,0"),
            ("1.000,6.000,再生範囲,0", "100.00", "1.000,4.800,再生範囲,0", "4.800,6.000,再生範囲,0"),
            ("0.000,0.000,再生範囲,0", "150.00", "0.000,5.700,再生範囲,0", "5.700,10.000,再生範囲,0"),
        ] {
            let (raw, s) = audio(pos, speed);
            assert_eq!(position_of(&piece_alias(&raw, 30, 143, &s).unwrap()), left, "{pos} {speed}");
            assert_eq!(position_of(&piece_alias(&raw, 144, 329, &s).unwrap()), right, "{pos} {speed}");
        }
        // 移動無しは 1.000 → 4.800（c02）
        let (raw, s) = audio("1.000", "100.00");
        assert_eq!(position_of(&piece_alias(&raw, 144, 329, &s).unwrap()), "4.800");
    }

    #[test]
    fn timecontrol_matches_host_split() {
        // c12: 直線移動(時間制御) は get_object_track_value が 0 を返す。本体は区間ごとの直線補間の値を書いた
        let raw = "[Object]\nframe=30,90,200,329\n[Object.0]\neffect.name=標準描画\nX=0.00,600.00,1200.00,1800.00,直線移動(時間制御),8|\n";
        let mut s = fake(&[30, 90, 200, 329], &[]);
        s.tracks.insert(("標準描画".into(), "X".into()), "0.00,0.00,直線移動,0".into());
        let left = AliasDoc::parse(&piece_alias(raw, 30, 143, &s).unwrap());
        assert_eq!(left.sections[1].get("X"), Some("0.00,600.00,889.09,直線移動(時間制御),8|"));
        let right = AliasDoc::parse(&piece_alias(raw, 144, 329, &s).unwrap());
        assert_eq!(right.sections[1].get("X"), Some("894.55,1200.00,1800.00,直線移動(時間制御),8|"));
    }

    #[test]
    fn rejects_out_of_range_and_bad_counts() {
        let s = src_text();
        assert!(piece_alias(TEXT, 10, 50, &s).is_err());
        assert!(piece_alias(TEXT, 100, 50, &s).is_err());
        let bad = TEXT.replace("X=0.00,600.00,1200.00,1800.00", "X=0.00,600.00,1200.00");
        let s2 = fake(&[30, 90, 200, 329], &[("標準描画", "X", "0.00,600.00,1200.00,直線移動,0")]);
        assert!(piece_alias(&bad, 30, 129, &s2).is_err());
    }
}
