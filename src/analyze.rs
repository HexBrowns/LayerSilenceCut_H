//! 解析: 解析対象のオブジェクトの音を 1 フレームずつ本体に描かせて、フレームごとの音量（dBFS）にする。
//!
//! 実機で確かめた形（無音カットプローブ_H の P1、ルール au2-rs-plugin「オブジェクト 1 つ分の音を本体に描かせる」）:
//! - `rendering_object_audio` を裏スレッド（egui のウィンドウも含む）から直接呼ぶと本体が落ちる。
//!   裏スレッドから `call_edit_section` を呼び、メインスレッドで動くそのコールバックの中で要求だけを出す
//! - `wait_rendering_task` は使わず、結果はチャンネルで受け取る（結果のコールバックはメインスレッド以外に届く）
//! - `音声再生` の `音量` は入らないので、ここで掛ける（`edit_ops::Gain`）
//!
//! 要求は 1 回の `call_edit_section` でまとめて出す（何も編集しない編集セクションが Undo に積まれるかは未確認なので、回数を増やさない）。
//! 裏のスレッドはプラグインが外れる前に止めて待つ（`shutdown`。ルール au2-rs-plugin「裏のスレッド」）。
//!
//! **要求を出したら DLL を pin する**（`pin_module`）。中止しても、出した要求の結果のコールバック（この DLL のコード）は
//! 本体に残る。解析中に本体を終了すると、DLL を外した後にそれが呼ばれて落ちうる。待ちが切れて裏のスレッドが残る場合も同じ
//! （公開前チェックリストの点検、`issues/20261010_publish_checklist_audit.md`）。

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{mpsc, Arc};
use std::thread::JoinHandle;
use std::time::Duration;

use parking_lot::Mutex;

use crate::edit_ops::Prepared;
use crate::EDIT_HANDLE;

/// 解析の結果。解析対象のレイヤーごとに、フレームごとの音量（dBFS）。`None` は音声関連のオブジェクトが無い
pub struct Levels {
    pub per_layer: Vec<Vec<Option<f64>>>,
}

pub struct Progress {
    pub total: AtomicUsize,
    pub done: AtomicUsize,
    pub cancel: AtomicBool,
    pub result: Mutex<Option<Result<Levels, String>>>,
}

static WORKER: Mutex<Option<(Arc<Progress>, JoinHandle<()>)>> = Mutex::new(None);

fn db(rms: f64) -> f64 {
    if rms <= 1e-9 {
        f64::NEG_INFINITY
    } else {
        20.0 * rms.log10()
    }
}

/// 左右の平均の二乗平均の平方根（モノラルなら左だけ）
pub fn rms(l: &[f32], r: &[f32]) -> f64 {
    if l.is_empty() {
        return 0.0;
    }
    let sl: f64 = l.iter().map(|x| (*x as f64).powi(2)).sum();
    let sr: f64 = if r.len() == l.len() { r.iter().map(|x| (*x as f64).powi(2)).sum() } else { sl };
    ((sl + sr) / (2.0 * l.len() as f64)).sqrt()
}

/// この DLL をプロセスが終わるまで外させない（`GetModuleHandleExW` の PIN）。何度呼んでもよい
fn pin_module() {
    static PINNED: AtomicBool = AtomicBool::new(false);
    if PINNED.swap(true, Ordering::AcqRel) {
        return;
    }
    const GET_MODULE_HANDLE_EX_FLAG_PIN: u32 = 0x1;
    const GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS: u32 = 0x4;
    #[link(name = "kernel32")]
    extern "system" {
        fn GetModuleHandleExW(flags: u32, name: *const u16, module: *mut *mut std::ffi::c_void) -> i32;
    }
    let mut module = std::ptr::null_mut();
    let addr = pin_module as *const () as *const u16;
    // SAFETY: FROM_ADDRESS では `name` をこの DLL の中のアドレスとして読むだけ。`module` は書き込み先
    let ok = unsafe {
        GetModuleHandleExW(GET_MODULE_HANDLE_EX_FLAG_PIN | GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS, addr, &mut module)
    };
    if ok == 0 {
        tracing::warn!("[LayerSilenceCut_H] DLL を pin できませんでした（解析中に終了すると落ちることがあります）");
    }
}

pub fn start(prepared: Arc<Prepared>) -> Arc<Progress> {
    shutdown();
    pin_module();
    let progress = Arc::new(Progress {
        total: AtomicUsize::new(0),
        done: AtomicUsize::new(0),
        cancel: AtomicBool::new(false),
        result: Mutex::new(None),
    });
    let p = progress.clone();
    let handle = std::thread::spawn(move || {
        let r = run(&prepared, &p);
        *p.result.lock() = Some(r);
    });
    *WORKER.lock() = Some((progress.clone(), handle));
    progress
}

/// 解析を止めて、スレッドの終わりを待つ（上限 5 秒）
pub fn shutdown() {
    let Some((p, h)) = WORKER.lock().take() else { return };
    p.cancel.store(true, Ordering::Release);
    for _ in 0..100 {
        if h.is_finished() {
            let _ = h.join();
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    tracing::warn!("[LayerSilenceCut_H] 解析のスレッドが 5 秒で止まりませんでした（DLL は pin してあるので落ちない）");
}

fn run(prepared: &Prepared, p: &Progress) -> Result<Levels, String> {
    let n = prepared.frame_count;
    let mut per_layer: Vec<Vec<Option<f64>>> = vec![vec![None; n]; prepared.analysis_layers.len()];

    // 描かせるもの（オブジェクトの番号, フレーム）。非表示のレイヤーは描かずに無音
    let mut requests = Vec::new();
    for (i, o) in prepared.audio.iter().enumerate() {
        for f in o.start..=o.end.min(n.saturating_sub(1)) {
            if o.muted {
                per_layer[o.slot][f] = Some(f64::NEG_INFINITY);
            } else {
                requests.push((i, f));
            }
        }
    }
    p.total.store(requests.len(), Ordering::Release);
    if requests.is_empty() {
        return Ok(Levels { per_layer });
    }

    let (tx, rx) = mpsc::channel::<(usize, usize, f64)>();
    let audio: Vec<_> = prepared.audio.iter().map(|o| o.handle).collect();
    let reqs = requests.clone();
    let sent = EDIT_HANDLE
        .call_edit_section(move |edit| {
            let mut failed = 0;
            for (i, f) in reqs {
                // 解析のボタンを押してからここまでの間に消えたオブジェクトには要求を出さない
                if !edit.object(audio[i]).exists() {
                    failed += 1;
                    continue;
                }
                let tx = tx.clone();
                if EDIT_HANDLE
                    .rendering_object_audio(audio[i], f as u32, true, move |a| {
                        let _ = tx.send((i, f, rms(a.buffer0, a.buffer1)));
                    })
                    .is_err()
                {
                    failed += 1;
                }
            }
            failed
        })
        .map_err(|e| format!("音の描画を要求できませんでした: {e}"))?;
    if sent > 0 {
        return Err(format!("{sent} フレームの音の描画を要求できませんでした"));
    }

    let mut got = 0;
    // 本体が結果を返さずにコールバックを捨てると、チャンネルが閉じずに待ち続けるので、届かない時間で打ち切る
    let mut idle = 0;
    while got < requests.len() {
        if p.cancel.load(Ordering::Acquire) {
            return Err("中止しました".into());
        }
        match rx.recv_timeout(Duration::from_millis(200)) {
            Ok((i, f, r)) => {
                idle = 0;
                let o = &prepared.audio[i];
                let level = db(r * o.gain.at(f - o.start));
                let cell = &mut per_layer[o.slot][f];
                // 同じレイヤーで重なることは無いが、念のため大きい方
                *cell = Some(cell.map_or(level, |c| c.max(level)));
                got += 1;
                p.done.store(got, Ordering::Release);
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                idle += 1;
                if idle >= 150 {
                    return Err(format!("音の描画が 30 秒届きません（{got} / {} フレーム）", requests.len()));
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                return Err(format!("音の描画が {got} / {} フレームで止まりました", requests.len()));
            }
        }
    }
    Ok(Levels { per_layer })
}
