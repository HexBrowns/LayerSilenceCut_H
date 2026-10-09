//! LayerSilenceCut_H（表示名「レイヤー指定無音カット_H」）
//!
//! 指定したレイヤーの音声だけで無音の区間を見つけ、指定したレイヤーだけをその区間で切って後ろを詰める。
//! 仕様は `AI/specifications/20261009_LayerSilenceCut_H_spec.md`。根拠にした実機の結果は同じ仕様の 6 章
//! （無音カットプローブ_H、`AI/devenv/probes/silence_cut_probe/`）。
//!
//! 裏のスレッドは解析の 1 本だけ。プラグイン本体の `Drop` で止めて待つ（ルール au2-rs-plugin「裏のスレッド」）。

pub mod alias;
mod analyze;
pub mod detect;
mod edit_ops;
mod gui;
pub mod plan;
mod settings;
pub mod split;
pub mod track;

use aviutl2::generic::GlobalEditHandle;
use aviutl2::AnyResult;
use parking_lot::Mutex;

/// 表示名（ユーザーの指示、2026-10-09。公開時に「_H」を付けた）。ID・フォルダ・リポジトリは LayerSilenceCut_H のまま
pub const WINDOW_NAME: &str = "レイヤー指定無音カット_H";

pub(crate) static EDIT_HANDLE: GlobalEditHandle = GlobalEditHandle::new();

fn init_logging() {
    use aviutl2::tracing::Level;
    let level = if cfg!(debug_assertions) { Level::DEBUG } else { Level::INFO };
    let _ = aviutl2::tracing_subscriber::fmt()
        .with_max_level(level)
        .event_format(aviutl2::logger::AviUtl2Formatter)
        .with_writer(aviutl2::logger::AviUtl2LogWriter)
        .try_init();
}

#[aviutl2::plugin(GenericPlugin)]
pub struct LayerSilenceCutPlugin {
    window: Mutex<Option<aviutl2_eframe::EframeWindow>>,
}

impl aviutl2::generic::GenericPlugin for LayerSilenceCutPlugin {
    fn new(_info: aviutl2::AviUtl2Info) -> AnyResult<Self> {
        init_logging();
        tracing::info!("LayerSilenceCut_H v{} 初期化", env!("CARGO_PKG_VERSION"));
        Ok(Self { window: Mutex::new(None) })
    }

    fn plugin_info(&self) -> aviutl2::generic::GenericPluginTable {
        aviutl2::generic::GenericPluginTable {
            name: WINDOW_NAME.to_string(),
            information: format!("{WINDOW_NAME} v{} by HexBrowns", env!("CARGO_PKG_VERSION")),
        }
    }

    fn register(&mut self, registry: &mut aviutl2::generic::HostAppHandle) {
        EDIT_HANDLE.init(registry.create_edit_handle());
        let window = match aviutl2_eframe::EframeWindow::new(WINDOW_NAME, move |cc, handle| Ok(Box::new(gui::App::new(cc, handle)))) {
            Ok(w) => w,
            Err(e) => {
                tracing::error!("[LayerSilenceCut_H] ウィンドウを作れませんでした: {e:#}");
                return;
            }
        };
        // ウィンドウは表示メニューから開く。編集メニューやレイヤーの右クリックに「開く」を足しても、
        // 本体に登録したウィンドウは ShowWindow では前に出ない（v0.1.0 で足したが何も起きなかった。ユーザーの指摘で外した）
        match window.handle() {
            Ok(handle) => {
                if let Err(e) = registry.register_window_client(WINDOW_NAME, &handle) {
                    tracing::error!("[LayerSilenceCut_H] register_window_client 失敗: {e}");
                }
            }
            Err(e) => tracing::error!("[LayerSilenceCut_H] ウィンドウのハンドルを取れませんでした: {e:#}"),
        }
        *self.window.lock() = Some(window);
    }
}

impl Drop for LayerSilenceCutPlugin {
    fn drop(&mut self) {
        // 本体が DLL を外す前に、解析のスレッドを止めて待つ
        analyze::shutdown();
    }
}

// 本体が呼ぶ関数（GetCommonPluginTable など）を書き出す（ルール au2-rs-plugin「ビルド」）
aviutl2::register_generic_plugin!(LayerSilenceCutPlugin);
