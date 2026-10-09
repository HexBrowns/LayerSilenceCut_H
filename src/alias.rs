//! `get_object_alias` の文字列を、行の並びを保ったまま読み書きする（オブジェクト 1 つ分）。
//!
//! ```text
//! [Object]
//! frame=100,130,160,189      ← 開始・中間点・終了（シーン基準）
//! [Object.0]
//! effect.name=音声ファイル
//! 再生位置=0.000
//! [Object.1]
//! effect.name=音声再生
//! 音量=100.00
//! ```
//!
//! 分割では `frame=` と一部の値だけを書き換え、知らない行（`name=` など）はそのまま残す。

#[derive(Debug, Clone, PartialEq)]
pub struct Section {
    /// `[` `]` の中（`Object` / `Object.0` …）
    pub header: String,
    pub lines: Vec<(String, String)>,
    /// `=` の無かった行（`lines` の位置）。書き出しでも `=` を付けない
    bare: Vec<usize>,
}

impl Section {
    pub fn get(&self, key: &str) -> Option<&str> {
        self.lines.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
    }

    pub fn set(&mut self, key: &str, value: String) {
        match self.lines.iter_mut().find(|(k, _)| k == key) {
            Some((_, v)) => *v = value,
            None => self.lines.push((key.to_string(), value)),
        }
    }

    /// 効果の区画か（`Object.N`）
    pub fn is_effect(&self) -> bool {
        self.header.starts_with("Object.")
    }

    pub fn effect_name(&self) -> Option<&str> {
        self.get("effect.name")
    }
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct AliasDoc {
    /// 先頭の区画より前の行（ふつうは無い）
    pub preamble: Vec<String>,
    pub sections: Vec<Section>,
    crlf: bool,
}

impl AliasDoc {
    pub fn parse(raw: &str) -> Self {
        let mut doc = AliasDoc { crlf: raw.contains("\r\n"), ..Default::default() };
        for line in raw.lines() {
            let line = line.trim_end_matches('\r');
            if line.starts_with('[') && line.ends_with(']') && line.len() >= 2 {
                doc.sections.push(Section { header: line[1..line.len() - 1].to_string(), lines: Vec::new(), bare: Vec::new() });
                continue;
            }
            match doc.sections.last_mut() {
                Some(s) => {
                    if let Some((k, v)) = line.split_once('=') {
                        s.lines.push((k.to_string(), v.to_string()));
                    } else if !line.is_empty() {
                        // `=` の無い行はそのまま残す
                        s.bare.push(s.lines.len());
                        s.lines.push((line.to_string(), String::new()));
                    }
                }
                None => {
                    if !line.is_empty() {
                        doc.preamble.push(line.to_string());
                    }
                }
            }
        }
        doc
    }

    pub fn to_text(&self) -> String {
        let nl = if self.crlf { "\r\n" } else { "\n" };
        let mut s = String::new();
        for p in &self.preamble {
            s.push_str(p);
            s.push_str(nl);
        }
        for sec in &self.sections {
            s.push('[');
            s.push_str(&sec.header);
            s.push(']');
            s.push_str(nl);
            for (i, (k, v)) in sec.lines.iter().enumerate() {
                s.push_str(k);
                if !sec.bare.contains(&i) {
                    s.push('=');
                    s.push_str(v);
                }
                s.push_str(nl);
            }
        }
        s
    }

    pub fn object(&self) -> Option<&Section> {
        self.sections.iter().find(|s| s.header == "Object")
    }

    pub fn object_mut(&mut self) -> Option<&mut Section> {
        self.sections.iter_mut().find(|s| s.header == "Object")
    }

    /// `frame=` の値（開始, 中間点…, 終了）
    pub fn frames(&self) -> Vec<usize> {
        self.object()
            .and_then(|o| o.get("frame"))
            .map(|v| v.split(',').filter_map(|x| x.trim().parse().ok()).collect())
            .unwrap_or_default()
    }

    pub fn set_frames(&mut self, frames: &[usize]) {
        let v = frames.iter().map(|f| f.to_string()).collect::<Vec<_>>().join(",");
        if let Some(o) = self.object_mut() {
            o.set("frame", v);
        }
    }

    /// 効果の区画ごとに（区画の位置, 効果名, 同名の効果の中の順番）。順番は API の `"効果名:n"` の n
    pub fn effects(&self) -> Vec<(usize, String, usize)> {
        let mut out: Vec<(usize, String, usize)> = Vec::new();
        for (i, s) in self.sections.iter().enumerate() {
            if !s.is_effect() {
                continue;
            }
            let Some(name) = s.effect_name() else { continue };
            let ordinal = out.iter().filter(|(_, n, _)| n == name).count();
            out.push((i, name.to_string(), ordinal));
        }
        out
    }

    /// 先頭の効果の名前（オブジェクトの種類。`音声ファイル` `テキスト` など）
    pub fn kind(&self) -> Option<&str> {
        self.sections.iter().find(|s| s.is_effect()).and_then(|s| s.effect_name())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RAW: &str = "[Object]\nframe=100,130,160,189\nname=声\n[Object.0]\neffect.name=音声ファイル\n再生位置=1.500\nファイル=C:\\a\\b.wav\n\
                       [Object.1]\neffect.name=音声再生\n音量=100.00,0.00,50.00,20.00,直線移動,0\n[Object.2]\neffect.name=ぼかし\n範囲=5\n\
                       [Object.3]\neffect.name=ぼかし\n範囲=a=b\n";

    #[test]
    fn roundtrip_keeps_everything() {
        let d = AliasDoc::parse(RAW);
        assert_eq!(d.to_text(), RAW);
        let crlf = RAW.replace('\n', "\r\n");
        assert_eq!(AliasDoc::parse(&crlf).to_text(), crlf);
    }

    #[test]
    fn frames_and_effects() {
        let mut d = AliasDoc::parse(RAW);
        assert_eq!(d.frames(), vec![100, 130, 160, 189]);
        assert_eq!(d.kind(), Some("音声ファイル"));
        let e = d.effects();
        assert_eq!(e.len(), 4);
        assert_eq!((e[2].1.as_str(), e[2].2), ("ぼかし", 0));
        assert_eq!((e[3].1.as_str(), e[3].2), ("ぼかし", 1));
        assert_eq!(d.sections[e[3].0].get("範囲"), Some("a=b"));
        d.set_frames(&[0, 29]);
        assert!(d.to_text().starts_with("[Object]\nframe=0,29\nname=声\n"));
    }
}
