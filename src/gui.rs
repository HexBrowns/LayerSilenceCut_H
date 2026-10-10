//! レイヤー指定無音カット_H のウィンドウ（egui）
//!
//! 流れ: レイヤーを選ぶ → 「音声を解析する」（裏のスレッド。`analyze.rs`）→ 確認 → 「カットする」（`call_edit_section` 1 回）。
//! 解析の結果（フレームごとの音量）を持っているので、設定を変えたときは解析し直さずにカット区間だけを計算し直す。

use std::collections::{BTreeMap, BTreeSet};
use std::ops::Range;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use aviutl2_eframe::{eframe, egui, AviUtl2EframeHandle};

use crate::analyze::{self, Levels, Progress};
use crate::detect::{self, FrameState};
use crate::edit_ops::{self, Prepared, Scan};
use crate::settings::Settings;

const RED: egui::Color32 = egui::Color32::from_rgb(235, 110, 110);
const YELLOW: egui::Color32 = egui::Color32::from_rgb(235, 200, 90);

pub struct App {
    _handle: AviUtl2EframeHandle,
    scan: Option<Scan>,
    /// 解析対象・カット対象（レイヤー番号, 選んだときのレイヤー名）。名前が変わっていたら選択を外す
    analysis: BTreeMap<usize, String>,
    cut: BTreeMap<usize, String>,
    settings: Settings,
    /// 変えたがまだ保存していない（ドラッグ中）
    settings_dirty: bool,
    prepared: Option<Arc<Prepared>>,
    progress: Option<Arc<Progress>>,
    levels: Option<Levels>,
    /// 解析した時のレイヤーの選択（変えたら解析し直し）
    analyzed_with: Option<(Vec<usize>, Vec<usize>)>,
    status: String,
    status_is_error: bool,
    first_scan_done: bool,
}

/// 設定値の欄（ルール au2-rs-plugin「入力の確定と取り消し」）。
/// 打っている途中の値は使わず、Enter かほかをクリックしてフォーカスが外れたときに確定する。入力中の Esc は入力前の値に戻す。
/// ドラッグで変えた値はその場で使う。
///
/// egui 0.36.2 の `DragValue` は `update_while_editing(false)` でも、Esc の次のフレームで打った文字を値にしてしまう
/// （フォーカスを失ったとみなす期間が 2 フレームあり、2 フレーム目には Esc が押されていないため）。
/// Esc のフレームの値を覚えておき、次のフレームで戻す
fn setting_value(ui: &mut egui::Ui, value: &mut f64, range: (f64, f64), speed: f64, suffix: &str) -> egui::Response {
    let resp = ui.add(
        egui::DragValue::new(value)
            .range(range.0..=range.1)
            .speed(speed)
            .suffix(suffix)
            .update_while_editing(false),
    );
    let key = resp.id.with("escaped");
    let pass = ui.ctx().cumulative_pass_nr();
    if resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Escape)) {
        ui.data_mut(|d| d.insert_temp(key, (pass, *value)));
    } else if let Some((at, before)) = ui.data_mut(|d| d.remove_temp::<(u64, f64)>(key)) {
        if pass == at + 1 {
            *value = before;
        }
    }
    resp
}

/// フレーム数を「分:秒.小数」にする
fn time_of(frames: usize, fps: f64) -> String {
    let s = frames as f64 / fps.max(1.0);
    format!("{}:{:04.1}", (s / 60.0).floor() as u64, s % 60.0)
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>, handle: AviUtl2EframeHandle) -> Self {
        cc.egui_ctx.all_styles_mut(|style| {
            style.visuals = aviutl2_eframe::aviutl2_visuals();
        });
        cc.egui_ctx.set_fonts(aviutl2_eframe::aviutl2_fonts());
        Self {
            _handle: handle,
            scan: None,
            analysis: BTreeMap::new(),
            cut: BTreeMap::new(),
            settings: Settings::load(),
            settings_dirty: false,
            prepared: None,
            progress: None,
            levels: None,
            analyzed_with: None,
            status: String::new(),
            status_is_error: false,
            first_scan_done: false,
        }
    }

    fn set_status(&mut self, s: impl Into<String>, error: bool) {
        self.status = s.into();
        self.status_is_error = error;
    }

    fn rescan(&mut self) {
        match edit_ops::scan() {
            Ok(scan) => {
                // 名前が変わったレイヤー・無くなったレイヤーの選択を外す（取り違え防止）
                let now: BTreeMap<usize, String> = scan.layers.iter().map(|l| (l.layer, l.name.clone())).collect();
                let before = self.analysis.len() + self.cut.len();
                self.analysis.retain(|l, n| now.get(l) == Some(n));
                self.cut.retain(|l, n| now.get(l) == Some(n));
                if self.analysis.len() + self.cut.len() != before {
                    self.set_status("名前が変わったか、オブジェクトが無くなったレイヤーの選択を外しました", false);
                }
                self.scan = Some(scan);
            }
            Err(e) => self.set_status(e, true),
        }
    }

    /// 解析対象（カット対象にも入る）とカット対象の一覧
    fn selected(&self) -> (Vec<usize>, Vec<usize>) {
        let analysis: Vec<usize> = self.analysis.keys().copied().collect();
        let cut: BTreeSet<usize> = self.cut.keys().chain(self.analysis.keys()).copied().collect();
        (analysis, cut.into_iter().collect())
    }

    fn analysis_is_current(&self) -> bool {
        self.levels.is_some() && self.analyzed_with.as_ref() == Some(&self.selected())
    }

    fn start_analysis(&mut self) {
        let (analysis, cut) = self.selected();
        if analysis.is_empty() {
            self.set_status("解析対象のレイヤーを 1 つ以上選んでください", true);
            return;
        }
        match edit_ops::prepare(analysis.clone(), cut.clone()) {
            Ok(p) => {
                if p.audio.is_empty() {
                    self.set_status("解析対象のレイヤーに音声関連のオブジェクト（音声ファイル・音声付きの動画など）がありません", true);
                    return;
                }
                let p = Arc::new(p);
                self.progress = Some(analyze::start(p.clone()));
                self.prepared = Some(p);
                self.levels = None;
                self.analyzed_with = Some((analysis, cut));
                self.set_status("解析中…", false);
            }
            Err(e) => self.set_status(e, true),
        }
    }

    fn poll_analysis(&mut self) {
        let Some(p) = &self.progress else { return };
        let Some(r) = p.result.lock().take() else { return };
        self.progress = None;
        match r {
            Ok(levels) => {
                self.levels = Some(levels);
                self.set_status("解析が終わりました。カットの確認を見てから「カットする」を押してください", false);
            }
            Err(e) => {
                self.analyzed_with = None;
                self.set_status(e, true);
            }
        }
    }

    /// 今の設定でのカット区間
    fn cuts(&self) -> Option<Vec<Range<usize>>> {
        let (levels, p) = (self.levels.as_ref()?, self.prepared.as_ref()?);
        let params = self.settings.params();
        let states: Vec<Vec<FrameState>> =
            levels.per_layer.iter().map(|l| l.iter().map(|v| detect::classify(*v, params.threshold_db)).collect()).collect();
        let combined = detect::combine(&states, p.frame_count);
        Some(detect::cut_ranges(&combined, p.fps, &params))
    }

    fn do_cut(&mut self, cuts: Vec<Range<usize>>) {
        let Some(p) = self.prepared.clone() else { return };
        match edit_ops::cut(p.fingerprint, p.analysis_layers.clone(), p.cut_layers.clone(), cuts.clone()) {
            Ok(rep) => {
                self.set_status(
                    format!(
                        "{} 区間（{}）を切りました。分割 {}（{} 個に）・削除 {}・移動 {}。元に戻すは Ctrl+Z 1 回です",
                        cuts.len(),
                        time_of(detect::total_len(&cuts), p.fps),
                        rep.split,
                        rep.pieces,
                        rep.deleted,
                        rep.moved
                    ),
                    false,
                );
                tracing::info!("[LayerSilenceCut_H] カット: {} 区間 / 分割 {} / 削除 {} / 移動 {}", cuts.len(), rep.split, rep.deleted, rep.moved);
                // タイムラインが変わったので解析の結果は使えない
                self.levels = None;
                self.prepared = None;
                self.analyzed_with = None;
                self.rescan();
            }
            Err(e) => {
                tracing::warn!("[LayerSilenceCut_H] {e}");
                self.set_status(e, true);
            }
        }
    }

    fn render_layers(&mut self, ui: &mut egui::Ui) {
        let Some(scan) = &self.scan else {
            ui.label("「一覧を更新」でレイヤーを読み込みます");
            return;
        };
        if scan.layers.is_empty() {
            ui.label("このシーンにオブジェクトがありません");
            return;
        }
        let rows = scan.layers.clone();
        let mut changed = false;
        egui::Grid::new("layers").striped(true).spacing([10.0, 4.0]).show(ui, |ui| {
            ui.strong("層");
            ui.strong("名前");
            ui.strong("解析").on_hover_text("この層の音声だけで無音を判定します。解析対象は自動でカット対象にもなります");
            ui.strong("カット").on_hover_text("無音の区間で切って詰める層。画面録画や字幕など、音声と一緒に切りたい層を選びます");
            ui.strong("中身");
            ui.end_row();
            for row in &rows {
                let mut label = format!("{}", row.layer + 1);
                if !row.enabled {
                    label += " 非表示";
                }
                if row.locked {
                    label += " ロック";
                }
                ui.label(label);
                ui.label(&row.name);
                let mut a = self.analysis.contains_key(&row.layer);
                let resp = ui.add_enabled(row.has_audio || a, egui::Checkbox::without_text(&mut a));
                let resp = if row.has_audio { resp } else { resp.on_disabled_hover_text("音声関連のオブジェクトがありません") };
                if resp.changed() {
                    changed = true;
                    if a {
                        self.analysis.insert(row.layer, row.name.clone());
                    } else {
                        self.analysis.remove(&row.layer);
                    }
                }
                let mut c = a || self.cut.contains_key(&row.layer);
                if ui.add_enabled(!a, egui::Checkbox::without_text(&mut c)).changed() {
                    changed = true;
                    if c {
                        self.cut.insert(row.layer, row.name.clone());
                    } else {
                        self.cut.remove(&row.layer);
                    }
                }
                ui.label(&row.summary);
                ui.end_row();
            }
        });
        if changed && self.levels.is_some() && !self.analysis_is_current() {
            self.set_status("レイヤーの選択を変えたので、解析し直してください", false);
        }
    }

    fn render_settings(&mut self, ui: &mut egui::Ui) {
        let before = self.settings;
        let mut dragging = false;
        egui::Grid::new("settings").spacing([10.0, 4.0]).show(ui, |ui| {
            let s = &mut self.settings;
            ui.label("① 無音判定基準").on_hover_text("この音量以下を無音、これを超える音を有音とみなします。小さい声が切れるときは小さくします");
            dragging |= setting_value(ui, &mut s.threshold_db, detect::THRESHOLD_RANGE, 0.5, " dB").dragged();
            ui.end_row();
            ui.label("② 最短無音時間").on_hover_text("これより短い無音は切らずに残します（自然な間を保つため）");
            dragging |= setting_value(ui, &mut s.min_silence_ms, detect::MIN_SILENCE_RANGE, 10.0, " ms").dragged();
            ui.end_row();
            ui.label("③ 最短有音時間").on_hover_text(
                "前後が最短無音時間以上の無音に挟まれた、これより短い音を無音とみなします（リップノイズやクリック音を残さないため）。0 で無効",
            );
            dragging |= setting_value(ui, &mut s.min_voiced_ms, detect::MIN_VOICED_RANGE, 5.0, " ms").dragged();
            ui.end_row();
            ui.label("④ 有音区間拡張").on_hover_text("有音の前後に残す無音の長さ。話し始め・話し終わりが切れるときは長くします");
            dragging |= setting_value(ui, &mut s.padding_ms, detect::PADDING_RANGE, 5.0, " ms").dragged();
            ui.end_row();
        });
        ui.horizontal(|ui| {
            ui.checkbox(&mut self.settings.cut_no_audio, "音声関連アイテムがない区間もカット")
                .on_hover_text("ON にすると、解析対象の層に音声関連のオブジェクトが無い区間も切ります");
            if ui.button("初期値に戻す").clicked() {
                self.settings = Settings::default();
            }
        });
        if let Some(fps) = edit_ops::fps() {
            ui.label(format!("判定はフレーム単位です（{fps} fps で 1 フレーム約 {:.0} ms）", 1000.0 / fps.max(1.0)))
                .on_hover_text("ms はフレーム数に四捨五入します");
        }
        if self.settings != before {
            self.settings = Settings::from_params(self.settings.params());
            self.settings_dirty = true;
        }
        // ドラッグで値を変えている間は書き込まない（離したフレームで 1 回だけ保存する）。
        // カットの確認はドラッグ中も今の値で描き直す
        if self.settings_dirty && !dragging {
            self.settings.save();
            self.settings_dirty = false;
        }
    }

    fn render_run(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            let analyzing = self.progress.is_some();
            if ui.add_enabled(!analyzing, egui::Button::new("音声を解析する")).clicked() {
                self.start_analysis();
            }
            if analyzing {
                if ui.button("中止").clicked() {
                    if let Some(p) = &self.progress {
                        p.cancel.store(true, Ordering::Release);
                    }
                }
                if let Some(p) = &self.progress {
                    let (done, total) = (p.done.load(Ordering::Acquire), p.total.load(Ordering::Acquire).max(1));
                    ui.add(egui::ProgressBar::new(done as f32 / total as f32).text(format!("{done} / {total} フレーム")).desired_width(220.0));
                }
            }
        });
        if !self.analysis_is_current() {
            return;
        }
        let (Some(cuts), Some(p)) = (self.cuts(), self.prepared.clone()) else { return };
        let total = detect::total_len(&cuts);
        let counts = edit_ops::count_actions(&p.cut_objects, &cuts);
        let mut del_by_layer: BTreeMap<usize, usize> = BTreeMap::new();
        for (l, _, _) in &counts.deleted {
            *del_by_layer.entry(*l).or_insert(0) += 1;
        }
        ui.separator();
        ui.strong("カットの確認");
        ui.label(format!(
            "カット区間 {} / 合計 {} / 長さ {} → {}",
            cuts.len(),
            time_of(total, p.fps),
            time_of(p.frame_count, p.fps),
            time_of(p.frame_count.saturating_sub(total), p.fps)
        ));
        let dels = del_by_layer.iter().map(|(l, n)| format!("{} 層 {n}", l + 1)).collect::<Vec<_>>().join("・");
        ui.label(format!(
            "分割 {}・削除 {}{}・移動 {}",
            counts.split,
            counts.deleted.len(),
            if dels.is_empty() { String::new() } else { format!("（{dels}）") },
            counts.moved
        ));
        if !counts.deleted.is_empty() {
            egui::CollapsingHeader::new(format!("削除するオブジェクト（{}）", counts.deleted.len())).show(ui, |ui| {
                egui::ScrollArea::vertical().id_salt("deleted").max_height(100.0).show(ui, |ui| {
                    for (l, s, label) in &counts.deleted {
                        ui.label(format!("{} 層 {}（フレーム {s}）{label}", l + 1, time_of(*s, p.fps)));
                    }
                });
            });
        }
        for n in &p.notes {
            ui.colored_label(YELLOW, format!("⚠ {n}"));
        }
        ui.colored_label(YELLOW, "設定を変えてやり直すときは、先に Ctrl+Z でカット前に戻してください。プロジェクトの保存はしません");
        egui::CollapsingHeader::new(format!("区間の一覧（{}）", cuts.len())).show(ui, |ui| {
            egui::ScrollArea::vertical().id_salt("cuts").max_height(140.0).show(ui, |ui| {
                for (i, r) in cuts.iter().enumerate() {
                    ui.label(format!(
                        "{:>3}. {} 〜 {}（{}、フレーム {}〜{}）",
                        i + 1,
                        time_of(r.start, p.fps),
                        time_of(r.end, p.fps),
                        time_of(r.len(), p.fps),
                        r.start,
                        r.end - 1
                    ));
                }
            });
        });
        if ui.add_enabled(!cuts.is_empty(), egui::Button::new("カットする")).clicked() {
            self.do_cut(cuts);
        }
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        if !self.first_scan_done && crate::EDIT_HANDLE.is_ready() {
            self.first_scan_done = true;
            self.rescan();
        }
        self.poll_analysis();
        if self.progress.is_some() {
            ui.ctx().request_repaint_after(Duration::from_millis(100));
        }
        egui::Panel::top("top").show(ui, |ui| {
            ui.horizontal(|ui| {
                if ui.add_enabled(self.progress.is_none(), egui::Button::new("一覧を更新")).clicked() {
                    self.rescan();
                }
                if let Some(s) = &self.scan {
                    ui.label(format!("シーン: {}", s.scene));
                }
            });
        });
        // 下の欄（設定・解析・確認）も、ウィンドウが低いとはみ出すのでスクロールできるようにし、
        // 上の一覧との境目をドラッグして高さを変えられるようにする（ユーザーの要望、v0.1.0 の実機確認）
        egui::Panel::bottom("bottom").resizable(true).default_size(320.0).min_size(80.0).show(ui, |ui| {
            egui::ScrollArea::vertical().id_salt("bottom_scroll").auto_shrink([false, false]).show(ui, |ui| {
                self.render_settings(ui);
                ui.separator();
                self.render_run(ui);
                if !self.status.is_empty() {
                    ui.separator();
                    if self.status_is_error {
                        ui.colored_label(RED, &self.status);
                    } else {
                        ui.label(&self.status);
                    }
                }
            });
        });
        // 中央パネルは最後に追加する（ルール au2-rs-plugin「egui レイアウト」）
        egui::CentralPanel::default().show(ui, |ui| {
            egui::ScrollArea::both().auto_shrink([false, false]).show(ui, |ui| self.render_layers(ui));
        });
    }
}

#[cfg(test)]
mod tests {
    //! 設定値の欄を egui だけで動かし、打っている途中の値が使われないこと・Esc で戻ることを確かめる

    use super::*;

    /// テストの 1 フレームの出力を捨てる。テクスチャの差分を片付けずに捨てると、デバッグビルドで epaint の debug_assert
    /// （Dropped TexturesDelta with N unapplied deltas）が落ちる（au2 release の prebuild の cargo test はデバッグビルド）
    fn discard_frame(mut out: egui::FullOutput) {
        out.textures_delta.clear();
    }

    fn key(key: egui::Key) -> egui::Event {
        egui::Event::Key { key, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::NONE }
    }

    /// 1 フレーム描く。`focus` なら描く前に欄へフォーカスを移す（クリックしたときと同じく全選択で編集に入る）
    fn frame(ctx: &egui::Context, value: &mut f64, id: Option<egui::Id>, focus: bool, events: Vec<egui::Event>) -> egui::Id {
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(400.0, 300.0))),
            events,
            ..Default::default()
        };
        let mut out = egui::Id::NULL;
        discard_frame(ctx.run_ui(input, |ui| {
            if let (true, Some(id)) = (focus, id) {
                ui.memory_mut(|m| m.request_focus(id));
            }
            out = setting_value(ui, value, detect::PADDING_RANGE, 5.0, " ms").id;
        }));
        out
    }

    /// 100 の欄に "250" を打ったところまで進める
    fn typed(ctx: &egui::Context, v: &mut f64) -> egui::Id {
        let id = frame(ctx, v, None, false, vec![]);
        frame(ctx, v, Some(id), true, vec![]);
        frame(ctx, v, Some(id), false, vec![egui::Event::Text("250".into())]);
        id
    }

    #[test]
    fn typed_value_is_used_only_after_enter() {
        let ctx = egui::Context::default();
        let mut v = 100.0;
        let id = typed(&ctx, &mut v);
        assert_eq!(v, 100.0, "打っている途中の値を使っている");
        frame(&ctx, &mut v, Some(id), false, vec![key(egui::Key::Enter)]);
        frame(&ctx, &mut v, Some(id), false, vec![]);
        assert_eq!(v, 250.0);
    }

    #[test]
    fn value_is_committed_when_focus_moves_away() {
        let ctx = egui::Context::default();
        let mut v = 100.0;
        let id = typed(&ctx, &mut v);
        ctx.memory_mut(|m| m.surrender_focus(id));
        for _ in 0..3 {
            frame(&ctx, &mut v, Some(id), false, vec![]);
        }
        assert_eq!(v, 250.0);
    }

    #[test]
    fn escape_restores_value_before_typing() {
        let ctx = egui::Context::default();
        let mut v = 100.0;
        let id = typed(&ctx, &mut v);
        frame(&ctx, &mut v, Some(id), false, vec![key(egui::Key::Escape)]);
        // egui はフォーカスを失った後の 2 フレームを「失った」とみなすので、続けて数フレーム描いても戻らないこと
        for _ in 0..3 {
            frame(&ctx, &mut v, Some(id), false, vec![]);
        }
        assert_eq!(v, 100.0);
    }
}
