//! カット区間とオブジェクトの範囲から、オブジェクトごとの処理を決める（仕様 3.4）。
//!
//! オブジェクトの範囲からカット区間を引いた残りが「ピース」。各ピースは、その開始より前にある
//! カットの合計だけ左へ動く。詰めた後の並びは元と同じ順で、左のものから処理すれば動かす先は必ず空いている
//! （同じレイヤーの 2 つのオブジェクトの間にあるカットは、その隙間の中にしか無いため）。

use std::ops::Range;

/// ピース（元の位置、両端を含む）と、置く先の開始フレーム
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Piece {
    pub start: usize,
    pub end: usize,
    pub new_start: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// そのまま
    Keep,
    /// 範囲はそのままで左へ動かす
    Move { to: usize },
    /// 全部がカットに入る
    Delete,
    /// 2 つ以上のピース、または端が削られる
    Split { pieces: Vec<Piece> },
}

/// `frame` より前（`frame` を含まない）にあるカットの合計
pub fn cut_before(cuts: &[Range<usize>], frame: usize) -> usize {
    cuts.iter().map(|r| r.end.min(frame).saturating_sub(r.start)).sum()
}

/// オブジェクト `[start, end]`（両端を含む）の処理
pub fn action_for(cuts: &[Range<usize>], start: usize, end: usize) -> Action {
    let mut pieces = Vec::new();
    let mut cur = start;
    for r in cuts {
        if r.end <= cur || r.start > end {
            continue;
        }
        if r.start > cur {
            pieces.push((cur, r.start - 1));
        }
        cur = r.end;
        if cur > end {
            break;
        }
    }
    if cur <= end {
        pieces.push((cur, end));
    }
    let pieces: Vec<Piece> =
        pieces.into_iter().map(|(s, e)| Piece { start: s, end: e, new_start: s - cut_before(cuts, s) }).collect();
    match pieces.as_slice() {
        [] => Action::Delete,
        [p] if p.start == start && p.end == end => {
            if p.new_start == start {
                Action::Keep
            } else {
                Action::Move { to: p.new_start }
            }
        }
        _ => Action::Split { pieces },
    }
}

/// 処理した後にオブジェクトが占める範囲（両端を含む）。`Delete` は空
pub fn target_ranges(action: &Action, start: usize, end: usize) -> Vec<(usize, usize)> {
    match action {
        Action::Keep => vec![(start, end)],
        Action::Move { to } => vec![(*to, to + end - start)],
        Action::Delete => vec![],
        Action::Split { pieces } => pieces.iter().map(|p| (p.new_start, p.new_start + p.end - p.start)).collect(),
    }
}

/// 置き場所がふさがっている（`check_layer` の失敗）
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Blocked {
    /// 処理しようとしたオブジェクトの元の範囲
    pub object: (usize, usize),
    /// 置こうとした範囲
    pub target: (usize, usize),
    /// その範囲にかかっているもの
    pub by: (usize, usize),
}

fn overlaps(a: (usize, usize), b: (usize, usize)) -> bool {
    a.0 <= b.1 && b.0 <= a.1
}

/// 1 つのレイヤーのオブジェクト（開始順、両端を含む）を左から順に処理したとき、
/// 動かす先・作り直す先が毎回空いているかを確かめる（何かを変える前に呼ぶ）。
///
/// 処理する時点で、左のものは処理後の位置、自分より右のものは元の位置にある。
/// 自分の元の範囲は空いているとみなす（動かす・消してから作り直す）。
pub fn check_layer(objects: &[(usize, usize)], cuts: &[Range<usize>]) -> Result<(), Blocked> {
    let mut placed: Vec<(usize, usize)> = Vec::new();
    for (i, &(s, e)) in objects.iter().enumerate() {
        let targets = target_ranges(&action_for(cuts, s, e), s, e);
        for (k, &t) in targets.iter().enumerate() {
            let others = placed.iter().chain(&objects[i + 1..]).chain(&targets[..k]);
            if let Some(&by) = others.into_iter().find(|&&o| overlaps(o, t)) {
                return Err(Blocked { object: (s, e), target: t, by });
            }
        }
        placed.extend(targets);
    }
    Ok(())
}

/// レイヤーを左から辿り、`target`（両端を含む）にかかる最初のオブジェクトの範囲を返す。`skip` の範囲のものは数えない
/// （同じレイヤーのオブジェクトは重ならないので、範囲でオブジェクトが決まる）。
///
/// `find(from)` は `find_object_after(layer, from)` で見つけたオブジェクトの範囲。SDK（`plugin2.h` の `find_object`）の
/// 説明は「指定のフレーム番号以降にあるオブジェクトを検索」だけで、開始が `from` より前にあって `from` にかかる
/// オブジェクトを返すかは書かれていない（aviutl2-rs 0.48 も素通し）。どちらでも漏れないように、先頭（0）から辿る。
pub fn first_overlap(
    mut find: impl FnMut(usize) -> Option<(usize, usize)>,
    target: (usize, usize),
    skip: (usize, usize),
) -> Option<(usize, usize)> {
    let mut from = 0;
    loop {
        let (s, e) = find(from)?;
        if s > target.1 {
            return None;
        }
        if (s, e) != skip && e >= target.0 {
            return Some((s, e));
        }
        if e < from {
            // 先へ進まない（起きないはず）。空いているとは言い切れないので、かかっているとみなす
            return Some((s, e));
        }
        from = e + 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn before() {
        let cuts = [10..20, 30..35];
        assert_eq!(cut_before(&cuts, 5), 0);
        assert_eq!(cut_before(&cuts, 15), 5);
        assert_eq!(cut_before(&cuts, 20), 10);
        assert_eq!(cut_before(&cuts, 100), 15);
    }

    #[test]
    fn actions() {
        let cuts = vec![10..20, 30..35];
        assert_eq!(action_for(&cuts, 0, 9), Action::Keep);
        assert_eq!(action_for(&cuts, 20, 29), Action::Move { to: 10 });
        assert_eq!(action_for(&cuts, 12, 18), Action::Delete);
        assert_eq!(action_for(&cuts, 10, 19), Action::Delete);
        assert_eq!(action_for(&cuts, 40, 50), Action::Move { to: 25 });
        // 頭が削られる
        assert_eq!(
            action_for(&cuts, 15, 25),
            Action::Split { pieces: vec![Piece { start: 20, end: 25, new_start: 10 }] }
        );
        // 2 つのカットをまたぐ
        assert_eq!(
            action_for(&cuts, 5, 40),
            Action::Split {
                pieces: vec![
                    Piece { start: 5, end: 9, new_start: 5 },
                    Piece { start: 20, end: 29, new_start: 10 },
                    Piece { start: 35, end: 40, new_start: 20 },
                ]
            }
        );
        // 終わりがカットの中
        assert_eq!(action_for(&cuts, 25, 32), Action::Split { pieces: vec![Piece { start: 25, end: 29, new_start: 15 }] });
    }

    #[test]
    fn packed_layout_never_overlaps() {
        // 同じレイヤーに隙間なく並べた・離して並べたオブジェクトを、いろいろなカットで詰めても重ならない
        let objects = [(0, 9), (10, 24), (25, 25), (30, 59), (70, 99)];
        let cut_sets = [vec![5..12], vec![0..3, 24..31, 60..75], vec![9..10, 25..26], vec![0..100]];
        for cuts in cut_sets {
            let mut placed: Vec<(usize, usize)> = Vec::new();
            for &(s, e) in &objects {
                for r in target_ranges(&action_for(&cuts, s, e), s, e) {
                    if let Some(last) = placed.last() {
                        assert!(r.0 > last.1, "{cuts:?}: {r:?} が {last:?} と重なる");
                    }
                    placed.push(r);
                }
            }
            // 左から順に処理する途中でも、置く先は毎回空いている
            assert_eq!(check_layer(&objects, &cuts), Ok(()), "{cuts:?}");
        }
    }

    #[test]
    fn check_layer_reports_blocked_target() {
        // 並びが想定と違う（右のものが先に来た）と、動かす先に元の位置のオブジェクトが残っている
        let cuts = [0..5];
        assert_eq!(
            check_layer(&[(20, 29), (5, 15)], &cuts),
            Err(Blocked { object: (20, 29), target: (15, 24), by: (5, 15) })
        );
        // 分割のピースが、まだ動かしていない右のオブジェクトにかかる
        let cuts = [10..12];
        assert_eq!(
            check_layer(&[(0, 30), (25, 40)], &cuts),
            Err(Blocked { object: (0, 30), target: (10, 28), by: (25, 40) })
        );
        assert_eq!(check_layer(&[], &cuts), Ok(()));
    }

    /// 同じレイヤーの並び（開始順）に対する `find_object_after` の 2 通りの解釈
    fn finder(objects: &'static [(usize, usize)], covering: bool) -> impl FnMut(usize) -> Option<(usize, usize)> {
        move |from| objects.iter().copied().find(|&(s, e)| if covering { e >= from } else { s >= from })
    }

    #[test]
    fn first_overlap_on_the_timeline() {
        const OBJECTS: &[(usize, usize)] = &[(0, 9), (20, 40), (50, 60)];
        const NONE: (usize, usize) = (usize::MAX, usize::MAX);
        for covering in [false, true] {
            // 開始が前にあって範囲にかかる
            assert_eq!(first_overlap(finder(OBJECTS, covering), (30, 35), NONE), Some((20, 40)), "covering={covering}");
            // 端が接するだけでも重なる（両端を含む）
            assert_eq!(first_overlap(finder(OBJECTS, covering), (45, 50), NONE), Some((50, 60)), "covering={covering}");
            // 隙間
            assert_eq!(first_overlap(finder(OBJECTS, covering), (10, 19), NONE), None, "covering={covering}");
            assert_eq!(first_overlap(finder(OBJECTS, covering), (61, 99), NONE), None, "covering={covering}");
            // 自分（これから消す元）は数えない。その先にかかるものは拾う
            assert_eq!(first_overlap(finder(OBJECTS, covering), (25, 35), (20, 40)), None, "covering={covering}");
            assert_eq!(first_overlap(finder(OBJECTS, covering), (15, 55), (20, 40)), Some((50, 60)), "covering={covering}");
        }
    }
}
