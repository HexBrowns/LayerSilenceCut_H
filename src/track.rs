//! トラックバー項目の値の文字列を分けて組み立て直す。
//!
//! 形式（実機で確認。`AI/docs/20261008_パラメータ編集_SDK実機確認.md`）:
//! - 移動なし: `55.00`（値 1 つ）
//! - 移動あり: `値1,値2,…,値N,移動方法,設定`。値は中間点を含む各点に順に対応する（点の数 = 区間の数 + 1）
//! - 中間点無視（設定の `|` の前のビット 4）と `再生範囲` は値が 2 つ（始点と終点）
//! - 設定は省略できない（省くと本体が例外 `not found movement` を出す）。設定はカンマを含むことがあるので、
//!   移動方法より後ろは丸ごと 1 つの文字列として持つ

#[derive(Debug, Clone, PartialEq)]
pub struct Track {
    pub values: Vec<f64>,
    /// 移動方法。移動なしは `None`
    pub mode: Option<String>,
    /// 移動方法の後ろ（`0` や `0|2`、`8|`）
    pub tail: Option<String>,
    /// 小数点以下の桁数（読んだ値の中で最も多いもの）
    pub decimals: usize,
}

pub const FLAG_TWOPOINT: u32 = 4;

impl Track {
    /// 設定の `|` の前の数値（1 = 加速、2 = 減速、4 = 中間点無視、8 = 時間制御とみられる）
    pub fn flags(&self) -> u32 {
        self.tail
            .as_deref()
            .and_then(|t| t.split('|').next())
            .and_then(|f| f.trim().parse().ok())
            .unwrap_or(0)
    }

    pub fn to_text(&self) -> String {
        let mut s = self.values.iter().map(|v| format_value(*v, self.decimals)).collect::<Vec<_>>().join(",");
        if let Some(m) = &self.mode {
            s.push(',');
            s.push_str(m);
            s.push(',');
            s.push_str(self.tail.as_deref().filter(|t| !t.is_empty()).unwrap_or("0"));
        }
        s
    }
}

fn decimals_of(s: &str) -> usize {
    s.split_once('.').map(|(_, d)| d.len()).unwrap_or(0)
}

/// 値の文字列を分ける。数値として読めない最初の要素を移動方法とみなす
pub fn parse(raw: &str) -> Result<Track, String> {
    let parts: Vec<&str> = raw.split(',').collect();
    let mut values = Vec::new();
    let mut decimals = 0;
    let mut i = 0;
    while i < parts.len() {
        let p = parts[i].trim();
        match p.parse::<f64>() {
            Ok(v) if v.is_finite() => {
                values.push(v);
                decimals = decimals.max(decimals_of(p));
                i += 1;
            }
            _ => break,
        }
    }
    if values.is_empty() {
        return Err(format!("値が読めません: {raw}"));
    }
    let (mode, tail) = if i < parts.len() {
        let mode = parts[i].to_string();
        if mode.is_empty() {
            return Err(format!("移動方法が空です: {raw}"));
        }
        if i + 1 >= parts.len() {
            return Err(format!("移動方法の後ろに設定がありません: {raw}"));
        }
        (Some(mode), Some(parts[i + 1..].join(",")))
    } else {
        (None, None)
    };
    if mode.is_none() && values.len() != 1 {
        return Err(format!("移動方法が無いのに値が {} 個あります: {raw}", values.len()));
    }
    Ok(Track { values, mode, tail, decimals })
}

/// 値を書式に合わせて文字列にする（`-0.00` にはしない）
pub fn format_value(v: f64, decimals: usize) -> String {
    let s = format!("{:.*}", decimals, v);
    if s.starts_with('-') && s[1..].chars().all(|c| c == '0' || c == '.') {
        s[1..].to_string()
    } else {
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roundtrip(raw: &str) {
        assert_eq!(parse(raw).unwrap().to_text(), raw);
    }

    #[test]
    fn roundtrips() {
        roundtrip("0.00");
        roundtrip("1.500");
        roundtrip("100.00,200.00,300.00,400.00,直線移動,0");
        roundtrip("0.000,0.000,再生範囲,0");
        roundtrip("100.00,200.00,直線移動,4");
        roundtrip("100.00,200.00,300.00,400.00,直線移動,8|");
        roundtrip("0.00,1.00,移動無し,8|10^@min(音量/20-3,2)");
        roundtrip("100.00,200.00,プローブ移動_H,0|9,0,1");
    }

    #[test]
    fn fields() {
        let t = parse("1.0,2.0,何か@X,4|a,b").unwrap();
        assert_eq!(t.values, vec![1.0, 2.0]);
        assert_eq!(t.tail.as_deref(), Some("4|a,b"));
        assert_eq!(t.flags(), 4);
        assert_eq!(parse("5").unwrap().flags(), 0);
        assert!(parse("1.00,2.00").is_err());
        assert!(parse("1.00,2.00,直線移動").is_err());
        assert!(parse("abc").is_err());
        assert_eq!(format_value(-0.0001, 3), "0.000");
    }
}
