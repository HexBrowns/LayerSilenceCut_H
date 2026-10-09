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
                let ranges: Vec<(usize, usize)> = match action_for(&cuts, s, e) {
                    Action::Keep => vec![(s, e)],
                    Action::Move { to } => vec![(to, to + e - s)],
                    Action::Delete => vec![],
                    Action::Split { pieces } => pieces.iter().map(|p| (p.new_start, p.new_start + p.end - p.start)).collect(),
                };
                for r in ranges {
                    if let Some(last) = placed.last() {
                        assert!(r.0 > last.1, "{cuts:?}: {r:?} が {last:?} と重なる");
                    }
                    placed.push(r);
                }
            }
        }
    }
}
