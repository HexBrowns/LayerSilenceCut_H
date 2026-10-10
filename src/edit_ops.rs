//! 本体との読み書き。
//!
//! - 読み取りは `call_read_section`（本体の Undo に触れない）
//! - 書き込みは「カットする」を押したときの `call_edit_section` 1 回だけ（Ctrl+Z 1 回で全部戻る。プローブの P5）
//! - 音の描画の要求は `analyze.rs`（`call_edit_section` の中で要求する。ルール au2-rs-plugin）

use std::collections::{BTreeMap, BTreeSet};
use std::hash::{Hash, Hasher};
use std::ops::Range;

use aviutl2::generic::{EditSection, EditState, ObjectHandle, ReadSection};

use crate::alias::AliasDoc;
use crate::plan::{self, Action};
use crate::split;
use crate::EDIT_HANDLE;

/// 音を出す効果（`音声ファイル` などの出力。動画は `映像再生` が `音量` を持つ）
const AUDIO_OUTPUT: &str = "音声再生";
const VIDEO_OUTPUT: &str = "映像再生";
const ITEM_VOLUME: &str = "音量";

fn ready() -> Result<(), String> {
    if !EDIT_HANDLE.is_ready() {
        return Err("本体の準備ができていません".into());
    }
    match EDIT_HANDLE.get_edit_state() {
        Ok(EditState::Edit) => Ok(()),
        Ok(EditState::Preview) => Err("再生中です。止めてからもう一度押してください".into()),
        Ok(EditState::Save) => Err("ファイル出力中です。終わってからもう一度押してください".into()),
        Err(e) => Err(format!("本体の状態を取れません: {e}")),
    }
}

/// シーンのフレームレート。本体の準備ができる前は `None`
/// （ウィンドウはプラグインの登録が終わる前に描かれ始める。その間に `get_edit_info` を呼ぶと panic する。v0.1.0 で踏んだ）
pub fn fps() -> Option<f64> {
    if !EDIT_HANDLE.is_ready() {
        return None;
    }
    let f = EDIT_HANDLE.get_edit_info().fps;
    Some(*f.numer() as f64 / (*f.denom()).max(1) as f64)
}

/// レイヤーのオブジェクト（開始順）
fn objects_in(edit: &ReadSection, layer: usize) -> Vec<(ObjectHandle, usize, usize)> {
    let mut out = Vec::new();
    let mut from = 0;
    loop {
        let Ok(Some(h)) = edit.find_object_after(layer, from) else { break };
        let Ok(lf) = edit.get_object_layer_frame(h) else { break };
        out.push((h, lf.start, lf.end));
        from = lf.end + 1;
    }
    out
}

// ---------------------------------------------------------------- 一覧

#[derive(Debug, Clone)]
pub struct LayerRow {
    pub layer: usize,
    pub name: String,
    pub enabled: bool,
    pub locked: bool,
    /// 中身の参考（種類ごとの件数と最初の 1 件の名前）
    pub summary: String,
    pub has_audio: bool,
}

pub struct Scan {
    pub layers: Vec<LayerRow>,
    pub scene: String,
}

fn object_label(edit: &ReadSection, h: ObjectHandle, doc: &AliasDoc) -> String {
    edit.get_object_name(h)
        .ok()
        .flatten()
        .filter(|s| !s.is_empty())
        .or_else(|| doc.sections.iter().find(|s| s.effect_name() == Some("テキスト")).and_then(|s| s.get("テキスト")).map(|t| t.chars().take(12).collect()))
        .or_else(|| doc.sections.iter().find_map(|s| s.get("ファイル")).map(|f| f.rsplit(['\\', '/']).next().unwrap_or(f).to_string()))
        .unwrap_or_default()
}

/// 音声関連のオブジェクトか（音の出力の効果を持つ。動画は `音声付き` が 1 のときだけ）
fn audio_output(doc: &AliasDoc) -> Option<&'static str> {
    let names: BTreeSet<&str> = doc.sections.iter().filter_map(|s| s.effect_name()).collect();
    if names.contains(AUDIO_OUTPUT) {
        return Some(AUDIO_OUTPUT);
    }
    let video = doc.sections.iter().find(|s| s.effect_name() == Some("動画ファイル"));
    if names.contains(VIDEO_OUTPUT) && video.and_then(|v| v.get("音声付き")) == Some("1") {
        return Some(VIDEO_OUTPUT);
    }
    None
}

pub fn scan() -> Result<Scan, String> {
    if !EDIT_HANDLE.is_ready() {
        return Err("本体の準備ができていません".into());
    }
    let info = EDIT_HANDLE.get_edit_info();
    EDIT_HANDLE
        .call_read_section(move |read| {
            let mut layers = Vec::new();
            for layer in 0..=info.layer_max {
                let objs = objects_in(read, layer);
                if objs.is_empty() {
                    continue;
                }
                let mut kinds: BTreeMap<String, (usize, String)> = BTreeMap::new();
                let mut has_audio = false;
                for (h, _, _) in &objs {
                    let doc = AliasDoc::parse(&read.get_object_alias(*h).unwrap_or_default());
                    has_audio |= audio_output(&doc).is_some();
                    let kind = doc.kind().unwrap_or("?").to_string();
                    let label = object_label(read, *h, &doc);
                    let e = kinds.entry(kind).or_insert((0, label));
                    e.0 += 1;
                }
                let summary = kinds
                    .iter()
                    .map(|(k, (n, label))| if label.is_empty() { format!("{k} {n}") } else { format!("{k} {n}（{label}）") })
                    .collect::<Vec<_>>()
                    .join("・");
                layers.push(LayerRow {
                    layer,
                    name: read.get_layer_name(layer).ok().flatten().unwrap_or_default(),
                    enabled: read.get_layer_enable(layer).unwrap_or(true),
                    locked: read.get_layer_lock(layer).unwrap_or(false),
                    summary,
                    has_audio,
                });
            }
            Scan { layers, scene: read.get_scene_name().unwrap_or_default() }
        })
        .map_err(|e| format!("読み取りに失敗しました: {e}"))
}

// ---------------------------------------------------------------- 解析の準備

/// 音量（%）を掛ける倍率
#[derive(Debug, Clone)]
pub enum Gain {
    Const(f64),
    /// オブジェクトの開始からのフレームごと
    PerFrame(Vec<f64>),
}

impl Gain {
    pub fn at(&self, i: usize) -> f64 {
        match self {
            Gain::Const(g) => *g,
            Gain::PerFrame(v) => v.get(i).copied().unwrap_or(0.0),
        }
    }
}

#[derive(Debug, Clone)]
pub struct AudioObject {
    pub handle: ObjectHandle,
    /// 解析対象の中の番号（`Prepared::analysis_layers` の添字）
    pub slot: usize,
    pub start: usize,
    pub end: usize,
    /// 非表示のレイヤー（全体を無音とみなす）
    pub muted: bool,
    pub gain: Gain,
}

/// カット対象のオブジェクト（レイヤー, 開始, 終了, 説明）
#[derive(Debug, Clone)]
pub struct CutObject {
    pub layer: usize,
    pub start: usize,
    pub end: usize,
    pub label: String,
}

pub struct Prepared {
    pub analysis_layers: Vec<usize>,
    pub cut_layers: Vec<usize>,
    pub audio: Vec<AudioObject>,
    pub cut_objects: Vec<CutObject>,
    /// 解析の対象にするフレーム数（シーンの最後のオブジェクトの終わりまで）
    pub frame_count: usize,
    pub fps: f64,
    pub fingerprint: u64,
    /// 確認画面に出す注意
    pub notes: Vec<String>,
}

fn fingerprint(edit: &ReadSection, layers: &BTreeSet<usize>) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    for &layer in layers {
        for (o, s, e) in objects_in(edit, layer) {
            (layer, s, e).hash(&mut h);
            edit.get_object_alias(o).unwrap_or_default().hash(&mut h);
        }
    }
    h.finish()
}

pub fn prepare(analysis_layers: Vec<usize>, cut_layers: Vec<usize>) -> Result<Prepared, String> {
    ready()?;
    let info = EDIT_HANDLE.get_edit_info();
    let fps = fps().unwrap_or(30.0);
    EDIT_HANDLE
        .call_read_section(move |read| -> Result<Prepared, String> {
            let mut audio = Vec::new();
            let mut notes = Vec::new();
            for (slot, &layer) in analysis_layers.iter().enumerate() {
                let muted = !read.get_layer_enable(layer).unwrap_or(true);
                for (h, s, e) in objects_in(read, layer) {
                    let doc = AliasDoc::parse(&read.get_object_alias(h).unwrap_or_default());
                    let Some(output) = audio_output(&doc) else { continue };
                    let gain = match read.get_object_track_info(h, output, 0, ITEM_VOLUME) {
                        Ok(info) if info.mode.as_deref().is_some_and(|m| !m.is_empty()) => {
                            Gain::PerFrame((s..=e).map(|f| read.get_object_track_value(h, output, 0, ITEM_VOLUME, f as f64).unwrap_or(100.0) / 100.0).collect())
                        }
                        _ => Gain::Const(read.get_object_track_value(h, output, 0, ITEM_VOLUME, s as f64).unwrap_or(100.0) / 100.0),
                    };
                    audio.push(AudioObject { handle: h, slot, start: s, end: e, muted, gain });
                }
            }
            let mut cut_objects = Vec::new();
            let mut locked = Vec::new();
            for &layer in &cut_layers {
                if read.get_layer_lock(layer).unwrap_or(false) {
                    locked.push(layer + 1);
                }
                for (h, s, e) in objects_in(read, layer) {
                    let doc = AliasDoc::parse(&read.get_object_alias(h).unwrap_or_default());
                    let kind = doc.kind().unwrap_or("?");
                    if kind == "グループ制御" || kind == "カメラ制御" {
                        notes.push(format!("{} レイヤー目に{kind}があります（範囲だけを切ります。下のレイヤーへの効き方が変わることがあります）", layer + 1));
                    }
                    let name = object_label(read, h, &doc);
                    let label = if name.is_empty() { kind.to_string() } else { format!("{kind}「{name}」") };
                    cut_objects.push(CutObject { layer, start: s, end: e, label });
                }
            }
            if !locked.is_empty() {
                notes.push(format!("ロックされたレイヤー（{}）も切ります", locked.iter().map(|l| l.to_string()).collect::<Vec<_>>().join("・")));
            }
            let layers: BTreeSet<usize> = analysis_layers.iter().chain(&cut_layers).copied().collect();
            Ok(Prepared {
                fingerprint: fingerprint(read, &layers),
                analysis_layers,
                cut_layers,
                audio,
                cut_objects,
                frame_count: info.frame_max + 1,
                fps,
                notes,
            })
        })
        .map_err(|e| format!("読み取りに失敗しました: {e}"))?
}

// ---------------------------------------------------------------- カット

#[derive(Debug, Default, Clone)]
pub struct CutReport {
    pub moved: usize,
    pub deleted: usize,
    pub split: usize,
    pub pieces: usize,
}

struct EditSource<'a> {
    edit: &'a EditSection,
    h: ObjectHandle,
    fps: f64,
}

impl split::Source for EditSource<'_> {
    fn is_track(&self, effect: &str, ordinal: usize, item: &str) -> bool {
        self.edit.get_object_track_info(self.h, effect, ordinal, item).is_ok()
    }
    fn value_at(&self, effect: &str, ordinal: usize, item: &str, frame: usize) -> Option<f64> {
        self.edit.get_object_track_value(self.h, effect, ordinal, item, frame as f64).ok()
    }
    fn fps(&self) -> f64 {
        self.fps
    }
    fn file_length(&self, path: &str) -> Option<f64> {
        aviutl2::cache::get_audio_file_info(path).ok().flatten().map(|i| i.total_time)
    }
}

/// カットする。解析した後にタイムラインが変わっていたら何もしない
pub fn cut(expected: u64, analysis_layers: Vec<usize>, cut_layers: Vec<usize>, cuts: Vec<Range<usize>>) -> Result<CutReport, String> {
    ready()?;
    let fps = fps().unwrap_or(30.0);
    EDIT_HANDLE
        .call_edit_section(move |edit| -> Result<CutReport, String> {
            let layers: BTreeSet<usize> = analysis_layers.iter().chain(&cut_layers).copied().collect();
            if fingerprint(edit, &layers) != expected {
                return Err("解析した後にタイムラインが変わりました。解析し直してください（何も変えていません）".into());
            }
            // 何かを変える前に、全部のレイヤーで、動かす先・作り直す先が処理する時点で空いているかを確かめる
            // （分割は元を消してから作り直すので、置けないと分かってから止めても元が消えている）
            let mut planned = Vec::new();
            for &layer in &cut_layers {
                let objects = objects_in(edit, layer);
                let ranges: Vec<(usize, usize)> = objects.iter().map(|&(_, s, e)| (s, e)).collect();
                if let Err(b) = plan::check_layer(&ranges, &cuts) {
                    return Err(format!(
                        "{} レイヤー目の {}〜{} を {}〜{} に置けません（{}〜{} に別のオブジェクトがあります）。何も変えていません",
                        layer + 1,
                        b.object.0,
                        b.object.1,
                        b.target.0,
                        b.target.1,
                        b.by.0,
                        b.by.1
                    ));
                }
                planned.push((layer, objects));
            }
            let mut rep = CutReport::default();
            for (layer, objects) in planned {
                // 左から順に処理する（plan.rs。動かす先は必ず空いている）
                for (h, s, e) in objects {
                    let fail = |what: &str, err: String| {
                        format!("{} レイヤー目の {s}〜{e} を{what}できませんでした: {err}。Ctrl+Z で切る前に戻せます", layer + 1)
                    };
                    match plan::action_for(&cuts, s, e) {
                        Action::Keep => {}
                        Action::Move { to } => {
                            edit.move_object(h, layer, to).map_err(|x| fail("動か", x.to_string()))?;
                            rep.moved += 1;
                        }
                        Action::Delete => {
                            edit.delete_object(h).map_err(|x| fail("削除", x.to_string()))?;
                            rep.deleted += 1;
                        }
                        Action::Split { pieces } => {
                            let raw = edit.get_object_alias(h).map_err(|x| fail("読み取り", x.to_string()))?;
                            let src = EditSource { edit, h, fps };
                            let aliases = pieces
                                .iter()
                                .map(|p| split::piece_alias(&raw, p.start, p.end, &src).map(|a| (a, *p)))
                                .collect::<Result<Vec<_>, _>>()
                                .map_err(|x| fail("分割", x))?;
                            // 元を消す前に、ピースを置く場所が今のタイムラインで空いているかを確かめる（元の自分は数えない）
                            for (_, p) in &aliases {
                                let target = (p.new_start, p.new_start + p.end - p.start);
                                let by = plan::first_overlap(
                                    |from| {
                                        let o = edit.find_object_after(layer, from).ok().flatten()?;
                                        edit.get_object_layer_frame(o).ok().map(|f| (f.start, f.end))
                                    },
                                    target,
                                    (s, e),
                                );
                                if let Some((bs, be)) = by {
                                    return Err(fail(
                                        "分割",
                                        format!("置く先の {}〜{} に別のオブジェクト（{bs}〜{be}）があります（このオブジェクトは消していません）", target.0, target.1),
                                    ));
                                }
                            }
                            edit.delete_object(h).map_err(|x| fail("分割（元を消す）", x.to_string()))?;
                            let total = aliases.len();
                            for (k, (alias, p)) in aliases.into_iter().enumerate() {
                                let len = p.end - p.start + 1;
                                let made = edit.create_object_from_alias(&alias, layer, p.new_start, len).map_err(|x| {
                                    format!(
                                        "{} レイヤー目の {s}〜{e} を分割する途中で、{}/{total} 個目のピース（{}〜{}）を作れませんでした: {x}。元のオブジェクトは消えていて、作れたピースは {k} 個です。Ctrl+Z で切る前に戻せます",
                                        layer + 1,
                                        k + 1,
                                        p.new_start,
                                        p.new_start + len - 1
                                    )
                                })?;
                                if let Ok(lf) = edit.get_object_layer_frame(made) {
                                    if (lf.start, lf.end) != (p.new_start, p.new_start + len - 1) {
                                        tracing::warn!(
                                            "[LayerSilenceCut_H] {} レイヤー目: {}〜{} に置いたつもりが {}〜{}",
                                            layer + 1,
                                            p.new_start,
                                            p.new_start + len - 1,
                                            lf.start,
                                            lf.end
                                        );
                                    }
                                }
                                rep.pieces += 1;
                            }
                            rep.split += 1;
                        }
                    }
                }
            }
            Ok(rep)
        })
        .map_err(|e| format!("書き込みを始められませんでした: {e}"))?
}

/// 確認画面に出す数
#[derive(Debug, Default)]
pub struct Counts {
    pub split: usize,
    pub moved: usize,
    /// 削除するもの（レイヤー, 開始, 説明）
    pub deleted: Vec<(usize, usize, String)>,
}

pub fn count_actions(objects: &[CutObject], cuts: &[Range<usize>]) -> Counts {
    let mut c = Counts::default();
    for o in objects {
        match plan::action_for(cuts, o.start, o.end) {
            Action::Keep => {}
            Action::Move { .. } => c.moved += 1,
            Action::Delete => c.deleted.push((o.layer, o.start, o.label.clone())),
            Action::Split { .. } => c.split += 1,
        }
    }
    c
}
