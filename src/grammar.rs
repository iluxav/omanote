//! Grammar, checked as you write, by a small neural model that lives on this
//! machine: GECToR (a RoBERTa encoder that tags each word with the edit it
//! needs). It knows what Harper's dictionary cannot: that "ho are you" wants
//! "how", "tree nights" wants "three", "we has" wants "have".
//!
//! The model is not in the binary. It is one 135 MB file, fetched once into
//! `~/.cache/omanote/models`, and loaded on the checker's thread the first
//! time a note is checked. Everything it says arrives as `spell::Problem`s,
//! so it shares the underlines and the F7 list with the dictionary check.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::io::{Read, Write};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};

use candle_core::quantized::{QMatMul, gguf_file};
use candle_core::{Device, Module, Tensor};
use candle_nn::LayerNorm;

use crate::spell::{Fix, Problem};

pub const MODEL_FILE: &str = "gector-roberta-base-q4.gguf";
/// Where the model is published: a release of its own, apart from omanote's binaries.
pub const MODEL_URL: &str = "https://github.com/iluxav/omanote/releases/download/grammar-model-v1/gector-roberta-base-q4.gguf";
pub const MODEL_BYTES: u64 = 78_164_416;
const MODEL_SHA256: &str = "d1ee12655c537e3c8c4c7df7aa9d55968c0b616381db949bcea825bea0d42050";

/// The model was trained on sequences this long; anything longer is split.
const MAX_SUBWORDS: usize = 80;
const HEADS: usize = 12;
const HEAD_DIM: usize = 64;
const HIDDEN: usize = 768;
const START_ID: u32 = 50265;
const BOS: u32 = 0;
const EOS: u32 = 2;
/// A bias towards leaving words alone, and how sure the model has to be that
/// a sentence has anything wrong in it. Low, because a suggestion costs
/// nothing until you take it: better to offer "Let's" and have it ignored.
const KEEP_CONFIDENCE: f32 = 0.1;
const MIN_ERROR_PROB: f32 = 0.3;

fn thresholds() -> (f32, f32) {
    let get = |name: &str, default: f32| std::env::var(name).ok().and_then(|v| v.parse().ok()).unwrap_or(default);
    (get("OMANOTE_GRAMMAR_KEEP", KEEP_CONFIDENCE), get("OMANOTE_GRAMMAR_MIN", MIN_ERROR_PROB))
}
/// Lines whose answer is remembered, so that a keystroke elsewhere costs nothing.
const CACHE_LINES: usize = 4000;

pub fn models_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("OMANOTE_MODELS_DIR").filter(|d| !d.is_empty()) {
        return PathBuf::from(dir);
    }
    let base = std::env::var_os("XDG_CACHE_HOME").filter(|v| !v.is_empty()).map(PathBuf::from);
    let base = base.or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache"))).unwrap_or_else(|| PathBuf::from("."));
    base.join("omanote").join("models")
}

pub fn model_path() -> PathBuf {
    match std::env::var_os("OMANOTE_GRAMMAR_MODEL").filter(|p| !p.is_empty()) {
        Some(path) => PathBuf::from(path),
        None => models_dir().join(MODEL_FILE),
    }
}

// ---- the tokenizer: RoBERTa's byte-level BPE, from the vocabulary and merges in the model file

struct Bpe {
    vocab: HashMap<String, u32>,
    ranks: HashMap<(String, String), usize>,
    byte_to_char: [char; 256],
}

/// GPT-2's map from bytes to printable characters, so every byte is a token character.
fn byte_to_char() -> [char; 256] {
    let mut out = ['\0'; 256];
    let mut next = 256u32;
    for b in 0..=255u8 {
        let printable = (b'!'..=b'~').contains(&b) || (0xa1..=0xac).contains(&b) || (0xae..=0xff).contains(&b);
        out[b as usize] = if printable {
            char::from_u32(b as u32).unwrap_or('?')
        } else {
            next += 1;
            char::from_u32(next - 1).unwrap_or('?')
        };
    }
    out
}

/// `{"token": id, ...}`: vocab.json, without pulling in a JSON crate for one file.
fn parse_vocab(json: &str) -> HashMap<String, u32> {
    let mut out = HashMap::new();
    let chars: Vec<char> = json.chars().collect();
    let mut i = 0;
    let mut key: Option<String> = None;
    while i < chars.len() {
        match chars[i] {
            '"' => {
                i += 1;
                let mut s = String::new();
                while i < chars.len() && chars[i] != '"' {
                    if chars[i] == '\\' {
                        i += 1;
                        match chars.get(i) {
                            Some('u') => {
                                let hex: String = chars[i + 1..(i + 5).min(chars.len())].iter().collect();
                                s.push(u32::from_str_radix(&hex, 16).ok().and_then(char::from_u32).unwrap_or('?'));
                                i += 4;
                            }
                            Some('n') => s.push('\n'),
                            Some('t') => s.push('\t'),
                            Some(&c) => s.push(c),
                            None => {}
                        }
                    } else {
                        s.push(chars[i]);
                    }
                    i += 1;
                }
                key = Some(s);
            }
            c if c.is_ascii_digit() => {
                let start = i;
                while i < chars.len() && chars[i].is_ascii_digit() {
                    i += 1;
                }
                if let (Some(k), Ok(id)) = (key.take(), chars[start..i].iter().collect::<String>().parse::<u32>()) {
                    out.insert(k, id);
                }
                continue;
            }
            _ => {}
        }
        i += 1;
    }
    out
}

impl Bpe {
    fn new(vocab_json: &str, merges: &str) -> Self {
        let ranks = merges
            .lines()
            .filter(|l| !l.starts_with('#') && !l.trim().is_empty())
            .enumerate()
            .filter_map(|(rank, l)| l.split_once(' ').map(|(a, b)| ((a.to_string(), b.to_string()), rank)))
            .collect();
        Bpe { vocab: parse_vocab(vocab_json), ranks, byte_to_char: byte_to_char() }
    }

    /// GPT-2's pre-tokenization of one word (given with its leading space):
    /// contractions, runs of letters, of digits, of other characters.
    fn pieces(word: &str) -> Vec<String> {
        let chars: Vec<char> = word.chars().collect();
        let mut out = Vec::new();
        let mut i = 0;
        while i < chars.len() {
            let rest: String = chars[i..].iter().collect();
            if let Some(clitic) = ["'s", "'t", "'re", "'ve", "'m", "'ll", "'d"].iter().find(|c| rest.starts_with(*c)) {
                out.push(clitic.to_string());
                i += clitic.chars().count();
                continue;
            }
            let mut j = i;
            let space = chars[j] == ' ';
            if space {
                j += 1;
            }
            let class = |c: char| if c.is_alphabetic() { 1 } else if c.is_numeric() { 2 } else if c.is_whitespace() { 0 } else { 3 };
            let Some(&first) = chars.get(j) else {
                out.push(chars[i..].iter().collect());
                break;
            };
            let kind = class(first);
            if kind == 0 {
                out.push(chars[i..j].iter().collect());
                i = j;
                continue;
            }
            while j < chars.len() && class(chars[j]) == kind {
                j += 1;
            }
            out.push(chars[i..j].iter().collect());
            i = j;
        }
        out
    }

    fn encode_piece(&self, piece: &str) -> Vec<u32> {
        let mut parts: Vec<String> = piece.bytes().map(|b| self.byte_to_char[b as usize].to_string()).collect();
        loop {
            let mut best: Option<(usize, usize)> = None;
            for k in 0..parts.len().saturating_sub(1) {
                if let Some(&rank) = self.ranks.get(&(parts[k].clone(), parts[k + 1].clone())) {
                    if best.is_none_or(|(r, _)| rank < r) {
                        best = Some((rank, k));
                    }
                }
            }
            let Some((_, k)) = best else { break };
            let merged = format!("{}{}", parts[k], parts[k + 1]);
            parts.splice(k..k + 2, [merged]);
        }
        parts.iter().map(|p| self.vocab.get(p).copied().unwrap_or(3)).collect()
    }

    /// The subword ids of one word, with the space RoBERTa puts in front of every word.
    fn encode_word(&self, word: &str) -> Vec<u32> {
        if word == "$START" {
            return vec![START_ID];
        }
        Self::pieces(&format!(" {word}")).iter().flat_map(|p| self.encode_piece(p)).collect()
    }
}

// ---- the model

struct Layer {
    q: QMatMul,
    k: QMatMul,
    v: QMatMul,
    qb: Tensor,
    kb: Tensor,
    vb: Tensor,
    out: QMatMul,
    out_b: Tensor,
    ln1: LayerNorm,
    up: QMatMul,
    up_b: Tensor,
    down: QMatMul,
    down_b: Tensor,
    ln2: LayerNorm,
}

/// An embedding table kept as it is stored (4- or 8-bit blocks), with the
/// rows a sentence needs unpacked on the spot.
struct Table {
    kind: candle_core::quantized::GgmlDType,
    bytes: Vec<u8>,
    width: usize,
}

fn half(bits: u16) -> f32 {
    let (sign, exp, frac) = ((bits >> 15) as u32, ((bits >> 10) & 0x1f) as u32, (bits & 0x3ff) as u32);
    let value = match exp {
        0 => (frac as f32) * 2f32.powi(-24),
        31 => if frac == 0 { f32::INFINITY } else { f32::NAN },
        _ => f32::from_bits(((exp + 112) << 23) | (frac << 13)),
    };
    if sign == 1 { -value } else { value }
}

impl Table {
    fn new(t: &candle_core::quantized::QTensor) -> candle_core::Result<Self> {
        Ok(Table { kind: t.dtype(), bytes: t.data()?.into_owned(), width: t.shape().dims()[1] })
    }

    fn row(&self, id: usize, out: &mut Vec<f32>) {
        use candle_core::quantized::GgmlDType as G;
        let (block, per) = match self.kind {
            G::Q4_0 => (18, 32),
            G::Q8_0 => (34, 32),
            G::F32 => (4, 1),
            G::F16 => (2, 1),
            _ => (0, 1),
        };
        let row_bytes = self.width / per * block;
        let Some(row) = self.bytes.get(id * row_bytes..(id + 1) * row_bytes) else {
            out.extend(std::iter::repeat_n(0.0, self.width));
            return;
        };
        match self.kind {
            G::Q4_0 => {
                for b in row.chunks(18) {
                    let d = half(u16::from_le_bytes([b[0], b[1]]));
                    let qs = &b[2..18];
                    out.extend(qs.iter().map(|q| ((q & 0x0f) as i32 - 8) as f32 * d));
                    out.extend(qs.iter().map(|q| ((q >> 4) as i32 - 8) as f32 * d));
                }
            }
            G::Q8_0 => {
                for b in row.chunks(34) {
                    let d = half(u16::from_le_bytes([b[0], b[1]]));
                    out.extend(b[2..34].iter().map(|q| (*q as i8) as f32 * d));
                }
            }
            G::F32 => out.extend(row.chunks(4).map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))),
            G::F16 => out.extend(row.chunks(2).map(|b| half(u16::from_le_bytes([b[0], b[1]])))),
            _ => out.extend(std::iter::repeat_n(0.0, self.width)),
        }
    }

    fn rows(&self, ids: &[u32]) -> candle_core::Result<Tensor> {
        let mut out = Vec::with_capacity(ids.len() * self.width);
        for &id in ids {
            self.row(id as usize, &mut out);
        }
        Tensor::from_vec(out, (ids.len(), self.width), &Device::Cpu)
    }
}

pub struct Model {
    bpe: Bpe,
    labels: Vec<String>,
    keep: usize,
    /// `word_tag1_tag2 -> word`: the verb forms `$TRANSFORM_VERB_*` turns a word into.
    verbs: HashMap<String, String>,
    words: Table,
    positions: Tensor,
    types: Tensor,
    ln: LayerNorm,
    layers: Vec<Layer>,
    label_head: QMatMul,
    label_b: Tensor,
    detect_head: QMatMul,
    detect_b: Tensor,
}

fn text_meta(content: &gguf_file::Content, key: &str) -> Result<String, String> {
    match content.metadata.get(key) {
        Some(gguf_file::Value::String(s)) => Ok(s.clone()),
        _ => Err(format!("the model file has no {key}")),
    }
}

impl Model {
    pub fn load(path: &Path) -> Result<Self, String> {
        let dev = Device::Cpu;
        let mut file = std::fs::File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let content = gguf_file::Content::read(&mut file).map_err(|e| format!("{}: not a model file omanote can read ({e})", path.display()))?;
        let labels: Vec<String> = match content.metadata.get("gector.labels") {
            Some(gguf_file::Value::Array(items)) => items.iter().filter_map(|v| if let gguf_file::Value::String(s) = v { Some(s.clone()) } else { None }).collect(),
            _ => return Err("the model file has no labels".into()),
        };
        let keep = labels.iter().position(|l| l == "$KEEP").ok_or("the model has no $KEEP label")?;
        let bpe = Bpe::new(&text_meta(&content, "gector.vocab")?, &text_meta(&content, "gector.merges")?);
        let mut verbs = HashMap::new();
        for line in text_meta(&content, "gector.verbs")?.lines() {
            if let Some((words, tags)) = line.split_once(':') {
                if let Some((from, to)) = words.split_once('_') {
                    verbs.entry(format!("{from}_{}", tags.trim())).or_insert_with(|| to.to_string());
                }
            }
        }
        let err = |e: candle_core::Error| format!("{}: {e}", path.display());
        // One tensor at a time, straight from the file into its final form:
        // the file as a whole is never in memory, and packed weights stay packed.
        let mut take = |name: &str| content.tensor(&mut file, name, &dev).map_err(err);
        let dense = |t: candle_core::quantized::QTensor| t.dequantize(&dev).map_err(err);
        let lin = |t: candle_core::quantized::QTensor| QMatMul::from_qtensor(t).map_err(err);
        let ln = |w: Tensor, b: Tensor| LayerNorm::new(w, b, 1e-5);
        let mut layers = Vec::new();
        for i in 0..12 {
            let p = format!("bert.encoder.layer.{i}");
            let mut t = |name: &str| take(&format!("{p}.{name}"));
            layers.push(Layer {
                q: lin(t("attention.self.query.weight")?)?,
                k: lin(t("attention.self.key.weight")?)?,
                v: lin(t("attention.self.value.weight")?)?,
                qb: dense(t("attention.self.query.bias")?)?,
                kb: dense(t("attention.self.key.bias")?)?,
                vb: dense(t("attention.self.value.bias")?)?,
                out: lin(t("attention.output.dense.weight")?)?,
                out_b: dense(t("attention.output.dense.bias")?)?,
                ln1: ln(dense(t("attention.output.LayerNorm.weight")?)?, dense(t("attention.output.LayerNorm.bias")?)?),
                up: lin(t("intermediate.dense.weight")?)?,
                up_b: dense(t("intermediate.dense.bias")?)?,
                down: lin(t("output.dense.weight")?)?,
                down_b: dense(t("output.dense.bias")?)?,
                ln2: ln(dense(t("output.LayerNorm.weight")?)?, dense(t("output.LayerNorm.bias")?)?),
            });
        }
        // The word table is a third of the model. It stays packed; the rows a
        // sentence needs are unpacked when it is checked.
        let table = take("bert.embeddings.word_embeddings.weight")?;
        let words = Table::new(&table).map_err(err)?;
        let model = Model {
            positions: dense(take("bert.embeddings.position_embeddings.weight")?)?,
            types: dense(take("bert.embeddings.token_type_embeddings.weight")?)?,
            ln: ln(dense(take("bert.embeddings.LayerNorm.weight")?)?, dense(take("bert.embeddings.LayerNorm.bias")?)?),
            label_b: dense(take("label_proj_layer.bias")?)?,
            detect_b: dense(take("d_proj_layer.bias")?)?,
            label_head: lin(take("label_proj_layer.weight")?)?,
            detect_head: lin(take("d_proj_layer.weight")?)?,
            words,
            layers,
            bpe,
            labels,
            keep,
            verbs,
        };
        Ok(model)
    }

    /// The encoder: hidden states for a sequence of subword ids.
    fn encode(&self, ids: &[u32]) -> candle_core::Result<Tensor> {
        let dev = Device::Cpu;
        let n = ids.len();
        let ids_list = ids.to_vec();
        // RoBERTa counts positions from 2: 0 and 1 are its padding.
        let pos: Vec<u32> = (2..2 + n as u32).collect();
        let x = self.words.rows(&ids_list)?;
        let x = (x + self.positions.index_select(&Tensor::new(pos.as_slice(), &dev)?, 0)?)?;
        let x = x.broadcast_add(&self.types.get(0)?)?;
        let mut x = self.ln.forward(&x)?;
        for l in &self.layers {
            let heads = |t: Tensor| -> candle_core::Result<Tensor> { t.reshape((n, HEADS, HEAD_DIM))?.transpose(0, 1)?.contiguous() };
            let q = heads(l.q.forward(&x)?.broadcast_add(&l.qb)?)?;
            let k = heads(l.k.forward(&x)?.broadcast_add(&l.kb)?)?;
            let v = heads(l.v.forward(&x)?.broadcast_add(&l.vb)?)?;
            let scores = (q.matmul(&k.transpose(1, 2)?.contiguous()?)? / (HEAD_DIM as f64).sqrt())?;
            let attn = candle_nn::ops::softmax_last_dim(&scores)?;
            let ctx = attn.matmul(&v)?.transpose(0, 1)?.contiguous()?.reshape((n, HIDDEN))?;
            let a = l.out.forward(&ctx)?.broadcast_add(&l.out_b)?;
            let x1 = l.ln1.forward(&(a + &x)?)?;
            let h = l.up.forward(&x1)?.broadcast_add(&l.up_b)?.gelu_erf()?;
            let o = l.down.forward(&h)?.broadcast_add(&l.down_b)?;
            x = l.ln2.forward(&(o + &x1)?)?;
        }
        Ok(x)
    }

    /// The tag for each word (`$KEEP` for most), given the words of one
    /// sentence, `$START` first. Long sentences are checked in pieces.
    pub fn tag(&self, words: &[String]) -> Result<Vec<String>, String> {
        let mut tags = vec!["$KEEP".to_string(); words.len()];
        let mut from = 0;
        while from < words.len() {
            // As many words as fit in the model's window, always with $START in front.
            let mut ids = vec![BOS, START_ID];
            let mut firsts = Vec::new();
            let mut to = from;
            for (i, w) in words.iter().enumerate().skip(from) {
                let sub = if i == 0 { vec![START_ID] } else { self.bpe.encode_word(w) };
                if i == 0 {
                    // Already there.
                    firsts.push(1);
                    to = 1;
                    continue;
                }
                if ids.len() + sub.len() + 1 > MAX_SUBWORDS && to > from {
                    break;
                }
                firsts.push(ids.len());
                ids.extend(sub);
                to = i + 1;
            }
            ids.push(EOS);
            let h = self.encode(&ids).map_err(|e| e.to_string())?;
            let logits = self.label_head.forward(&h).and_then(|t| t.broadcast_add(&self.label_b)).map_err(|e| e.to_string())?;
            let probs = candle_nn::ops::softmax_last_dim(&logits).map_err(|e| e.to_string())?;
            let detect = self.detect_head.forward(&h).and_then(|t| t.broadcast_add(&self.detect_b)).and_then(|t| candle_nn::ops::softmax_last_dim(&t)).map_err(|e| e.to_string())?;
            let probs: Vec<Vec<f32>> = probs.to_vec2().map_err(|e| e.to_string())?;
            let detect: Vec<Vec<f32>> = detect.to_vec2().map_err(|e| e.to_string())?;
            // Nothing in this piece is wrong enough to touch.
            let (keep_bias, min_prob) = thresholds();
            let worst = firsts.iter().map(|&j| detect[j][1]).fold(0f32, f32::max);
            if worst >= min_prob {
                for (w, &j) in (from..to).zip(&firsts) {
                    let mut row = probs[j].clone();
                    row[self.keep] += keep_bias;
                    let (best, p) = row.iter().enumerate().fold((self.keep, f32::MIN), |acc, (i, &p)| if p > acc.1 { (i, p) } else { acc });
                    if p >= min_prob {
                        tags[w] = self.labels.get(best).cloned().unwrap_or_else(|| "$KEEP".into());
                    }
                }
            }
            from = to.max(from + 1);
        }
        Ok(tags)
    }

    fn verb(&self, word: &str, tags: &str) -> Option<String> {
        self.verbs.get(&format!("{word}_{tags}")).cloned()
    }
}

// ---- words of a line, and the edits the tags mean for them

/// A word the model sees, and where it is on the line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Word {
    pub text: String,
    pub from: usize,
    pub to: usize,
}

/// The words of a markdown line, as the model wants them: markdown's own
/// marks left out, punctuation and contractions split off as the model was
/// trained on, each word remembering where it came from.
pub fn words(line: &[char]) -> Vec<Word> {
    let mut out = Vec::new();
    let n = line.len();
    let mut i = line.iter().take_while(|c| c.is_whitespace()).count();
    // Markers at the start of the line are structure, not prose.
    let rest: String = line[i..].iter().collect();
    for mark in ["- [ ] ", "- [x] ", "- [X] ", "- ", "* ", "+ ", "> "] {
        if rest.starts_with(mark) {
            i += mark.len();
            break;
        }
    }
    while i < n && line[i] == '#' {
        i += 1;
    }
    let digits = line[i..].iter().take_while(|c| c.is_ascii_digit()).count();
    if digits > 0 && digits < 4 && matches!(line.get(i + digits), Some('.') | Some(')')) && line.get(i + digits + 1) == Some(&' ') {
        i += digits + 2;
    }
    let mut in_code = false;
    while i < n {
        if line[i].is_whitespace() {
            i += 1;
            continue;
        }
        let start = i;
        while i < n && !line[i].is_whitespace() {
            i += 1;
        }
        let raw: String = line[start..i].iter().collect();
        // `code` is not prose, nor is a link's address or a web address.
        let ticks = raw.matches('`').count();
        if in_code || ticks % 2 == 1 {
            in_code = (in_code as usize + ticks) % 2 == 1;
            continue;
        }
        if ticks > 0 || raw.starts_with("http://") || raw.starts_with("https://") || raw.starts_with("![") || !raw.chars().any(char::is_alphanumeric) {
            continue;
        }
        // `plan](trips/japan.md)`: the link's text is prose, its address is not.
        let stop = match raw.find("](") {
            Some(cut) => start + raw[..cut].chars().count(),
            None => i,
        };
        // Emphasis marks and brackets around a word are not part of it.
        let strip = |c: char| "*_~=[]()\"“”‘’<>".contains(c);
        let lead = line[start..stop].iter().take_while(|c| strip(**c)).count();
        let trail = line[start..stop].iter().rev().take_while(|c| strip(**c)).count();
        if lead + trail >= stop - start {
            continue;
        }
        let (a, b) = (start + lead, stop - trail);
        // Punctuation at the end is its own word, as in the model's training data.
        let mut end = b;
        let mut tail: Vec<Word> = Vec::new();
        while end > a && ",.;:!?".contains(line[end - 1]) {
            tail.push(Word { text: line[end - 1].to_string(), from: end - 1, to: end });
            end -= 1;
        }
        let core: String = line[a..end].iter().collect();
        if !core.is_empty() {
            // "wasn't" is "was" + "n't", "what's" is "what" + "'s".
            let lower = core.to_lowercase();
            let split = ["n't", "'s", "'re", "'ve", "'ll", "'d", "'m"].iter().find(|c| lower.ends_with(*c) && lower.len() > c.len()).map(|c| c.len());
            match split {
                Some(k) if !core.chars().all(|c| c.is_ascii_digit() || c == '\'') => {
                    let cut = end - core[core.len() - k..].chars().count();
                    out.push(Word { text: line[a..cut].iter().collect(), from: a, to: cut });
                    out.push(Word { text: line[cut..end].iter().collect(), from: cut, to: end });
                }
                _ => out.push(Word { text: core, from: a, to: end }),
            }
        }
        out.extend(tail.into_iter().rev());
    }
    out
}

fn capitalize(word: &str) -> String {
    let mut c = word.chars();
    match c.next() {
        Some(first) => first.to_uppercase().chain(c.flat_map(char::to_lowercase)).collect(),
        None => String::new(),
    }
}

/// Punctuation and clitics sit against the word before them; everything else after a space.
fn attaches(token: &str) -> bool {
    token.chars().all(|c| ",.;:!?)".contains(c)) || ["'s", "n't", "'re", "'ve", "'ll", "'d", "'m"].contains(&token.to_lowercase().as_str())
}

/// Tokens back into text: "Let" "'s" "try" "," → "Let's try,".
fn join(tokens: &[String]) -> String {
    let mut out = String::new();
    for (i, t) in tokens.iter().enumerate() {
        if i > 0 && !attaches(t) {
            out.push(' ');
        }
        out.push_str(t);
    }
    out
}

/// What one tag makes of the word it is on, as the reference implementation
/// does it: `None` means the word goes.
fn apply(model: &Model, word: &str, tag: &str) -> Option<Vec<String>> {
    let one = |w: String| Some(vec![w]);
    match tag {
        "$DELETE" => None,
        t if t.starts_with("$APPEND_") => Some(vec![word.to_string(), t["$APPEND_".len()..].to_string()]),
        t if t.starts_with("$REPLACE_") => one(t["$REPLACE_".len()..].to_string()),
        "$TRANSFORM_CASE_LOWER" => one(word.to_lowercase()),
        "$TRANSFORM_CASE_UPPER" => one(word.to_uppercase()),
        "$TRANSFORM_CASE_CAPITAL" => one(capitalize(word)),
        "$TRANSFORM_CASE_CAPITAL_1" => {
            let mut c = word.chars();
            let first = c.next().map(String::from).unwrap_or_default();
            one(format!("{first}{}", capitalize(c.as_str())))
        }
        "$TRANSFORM_AGREEMENT_PLURAL" => one(format!("{word}s")),
        "$TRANSFORM_AGREEMENT_SINGULAR" => {
            let mut w = word.to_string();
            w.pop();
            one(w)
        }
        "$TRANSFORM_SPLIT_HYPHEN" => Some(word.split('-').map(str::to_string).collect()),
        t if t.starts_with("$TRANSFORM_VERB_") => one(model.verb(word, &t["$TRANSFORM_VERB_".len()..]).unwrap_or_else(|| word.to_string())),
        _ => one(word.to_string()),
    }
}

/// One pass: every word's tag applied. `$START` stays first; merges join a word to the next.
fn edited(model: &Model, words: &[String], tags: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut glue: Option<&str> = None;
    for (i, (word, tag)) in words.iter().zip(tags).enumerate() {
        let pieces = if i == 0 {
            // $START: an append here is a word before the first.
            match tag.strip_prefix("$APPEND_") {
                Some(w) => vec![word.clone(), w.to_string()],
                None => vec![word.clone()],
            }
        } else {
            apply(model, word, tag).unwrap_or_default()
        };
        for piece in pieces {
            match (glue.take(), out.last_mut()) {
                (Some(g), Some(last)) => {
                    last.push_str(g);
                    last.push_str(&piece);
                }
                _ => out.push(piece),
            }
        }
        match tag.as_str() {
            "$MERGE_SPACE" if i > 0 => glue = Some(""),
            "$MERGE_HYPHEN" if i > 0 => glue = Some("-"),
            _ => {}
        }
    }
    out
}

/// Where two word lists part and meet again: (old range, new range) for each difference.
fn differences(old: &[String], new: &[String]) -> Vec<(std::ops::Range<usize>, std::ops::Range<usize>)> {
    let (n, m) = (old.len(), new.len());
    // Longest common subsequence, from the end.
    let mut best = vec![vec![0u16; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            best[i][j] = if old[i] == new[j] { best[i + 1][j + 1] + 1 } else { best[i + 1][j].max(best[i][j + 1]) };
        }
    }
    let (mut i, mut j) = (0, 0);
    let mut out = Vec::new();
    let mut start: Option<(usize, usize)> = None;
    while i < n || j < m {
        if i < n && j < m && old[i] == new[j] {
            if let Some((a, b)) = start.take() {
                out.push((a..i, b..j));
            }
            i += 1;
            j += 1;
            continue;
        }
        start.get_or_insert((i, j));
        if j < m && (i == n || best[i][j + 1] >= best[i + 1][j]) {
            j += 1;
        } else {
            i += 1;
        }
    }
    if let Some((a, b)) = start {
        out.push((a..n, b..m));
    }
    out
}

/// How many passes a line gets: the model fixes what it can see, and a fix
/// can uncover the next ("let" → "let's" → "Let's").
const PASSES: usize = 3;

/// Check one line of a note: the problems on it, with `row` left at 0. Each
/// is offered as its final wording, all passes done.
pub fn check_line(model: &Model, line: &[char]) -> Result<Vec<Problem>, String> {
    let words = words(line);
    // One word is a title, a label, a fragment: not a sentence to correct.
    if words.iter().filter(|w| w.text.chars().any(char::is_alphabetic)).count() < 2 {
        return Ok(Vec::new());
    }
    let original: Vec<String> = words.iter().map(|w| w.text.clone()).collect();
    let mut current: Vec<String> = std::iter::once("$START".to_string()).chain(original.iter().cloned()).collect();
    for _ in 0..PASSES {
        let tags = model.tag(&current)?;
        let next = edited(model, &current, &tags);
        if next == current {
            break;
        }
        current = next;
    }
    Ok(problems(line, &words, &current[1..], false))
}

/// The problems in `line` that turn its `words` into `fixed`, each offered
/// as its final wording. `strict`, for a language model's rewrite: only what
/// is plainly a mistake counts, not a comma it would add, a full stop, an
/// accent, a capital mid-sentence or a phrase said its own way.
fn problems(line: &[char], words: &[Word], fixed: &[String], strict: bool) -> Vec<Problem> {
    let original: Vec<String> = words.iter().map(|w| w.text.clone()).collect();
    let hunks = differences(&original, fixed);
    if strict {
        // Half the sentence changed is a rewrite, not a correction.
        let changed: usize = hunks.iter().map(|(was, now)| was.len().max(now.len())).sum();
        if changed * 2 > original.len() {
            return Vec::new();
        }
    }
    let mut out = Vec::new();
    for (was, now) in hunks {
        if strict && !plainly_a_mistake(&original[was.clone()], &fixed[now.clone()], was.start == 0) {
            continue;
        }
        let now: Vec<String> = fixed[now].to_vec();
        // Where the change sits: the words it replaces, or for a pure insertion
        // the word before it (the word after, at the start of the line).
        let (range, replacement) = if !was.is_empty() {
            (was.clone(), join(&now))
        } else if was.start > 0 {
            let before = was.start - 1;
            let mut with = vec![original[before].clone()];
            with.extend(now.iter().cloned());
            (before..was.start, join(&with))
        } else {
            let mut with = now.clone();
            with.push(original[0].clone());
            (0..1, join(&with))
        };
        // A full stop at the end of a line is how prose ends, not how notes do.
        if was.is_empty() && was.start == original.len() && now.iter().all(|t| t == ".") {
            continue;
        }
        let (from, to) = (words[range.start].from, words[range.end - 1].to);
        let current_text: String = line[from..to].iter().collect();
        if replacement == current_text {
            continue;
        }
        let problem = if replacement.is_empty() {
            // Take a space with it, so the line does not get two.
            let from = if from > 0 && line[from - 1] == ' ' { from - 1 } else { from };
            Problem { row: 0, from, to, word: line[from..to].iter().collect(), message: "This may not belong here".into(), fixes: vec![Fix::Remove], spelling: false }
        } else {
            let message = if replacement.to_lowercase() == current_text.to_lowercase() {
                if replacement.chars().next().is_some_and(char::is_uppercase) { "A capital letter here?".to_string() } else { "Lower case here?".to_string() }
            } else {
                format!("Did you mean “{replacement}”?")
            };
            Problem { row: 0, from, to, word: current_text, message, fixes: vec![Fix::Replace(replacement)], spelling: false }
        };
        out.push(problem);
    }
    out
}

/// A language model's change worth underlining: words, not punctuation; a
/// short fix, not a rephrasing; more than an accent; and a capital only
/// where a sentence starts, or for "I".
fn plainly_a_mistake(was: &[String], now: &[String], starts_sentence: bool) -> bool {
    let wordy = |ts: &[String]| ts.iter().any(|t| t.chars().any(char::is_alphanumeric));
    if !wordy(was) && !wordy(now) {
        return false;
    }
    if was.len() > 3 || now.len() > 3 {
        return false;
    }
    let (a, b) = (join(was), join(now));
    if a.to_lowercase() == b.to_lowercase() {
        return starts_sentence || a == "i";
    }
    // "cafe" and "café": the same word.
    let accent = |x: char, y: char| x == y || (x.is_ascii_alphabetic() != y.is_ascii_alphabetic() && x.is_alphabetic() && y.is_alphabetic());
    let (a, b): (Vec<char>, Vec<char>) = (a.to_lowercase().chars().collect(), b.to_lowercase().chars().collect());
    !(a.len() == b.len() && a.iter().zip(&b).all(|(x, y)| accent(*x, *y)))
}

// ---- a language model you run, as the checker

const LLM_INSTRUCTIONS: &str = "You correct grammar and spelling mistakes in the user's sentence. Change as little as possible: fix only real errors, never rephrase, never change style, tone, or meaning. If the sentence is already correct, return it unchanged. Reply with the sentence only.";

/// Correct sentences come back unchanged: shown, not only told.
const LLM_EXAMPLES: [(&str, &str); 4] = [
    ("We moved the launch because the payment was late.", "We moved the launch because the payment was late."),
    ("we has three tree in the garden", "we have three trees in the garden"),
    ("Honestly it was kinda fun", "Honestly it was kinda fun"),
    ("Its done, lets ship it.", "It's done, let's ship it."),
];

/// The server, and what it said of each sentence it has read.
struct Llm {
    settings: crate::complete::Settings,
    agent: ureq::Agent,
    read: HashMap<String, Vec<String>>,
}

impl Llm {
    /// The sentence as the model would write it, in words.
    fn fixed(&mut self, sentence: &str) -> Result<Vec<String>, String> {
        if let Some(words) = self.read.get(sentence) {
            return Ok(words.clone());
        }
        // The guess after the cursor is what the typing waits on: let it go first.
        let since = std::time::Instant::now();
        while crate::complete::GUESSING.load(Ordering::Relaxed) && since.elapsed() < std::time::Duration::from_secs(3) {
            std::thread::sleep(std::time::Duration::from_millis(15));
        }
        let tokens = sentence.len() / 2 + 16;
        let reply = crate::complete::chat(&self.agent, &self.settings, LLM_INSTRUCTIONS, &LLM_EXAMPLES, sentence, tokens)?;
        let reply = reply.trim().lines().next().unwrap_or("").trim();
        // Quotes it put around the answer are not part of it.
        let reply = match (reply.strip_prefix('"').and_then(|r| r.strip_suffix('"')), sentence.starts_with('"')) {
            (Some(inner), false) => inner,
            _ => reply,
        };
        let chars: Vec<char> = reply.chars().collect();
        let fixed: Vec<String> = words(&chars).into_iter().map(|w| w.text).collect();
        if self.read.len() > CACHE_LINES {
            self.read.clear();
        }
        self.read.insert(sentence.to_string(), fixed.clone());
        Ok(fixed)
    }
}

/// Check one line with a language model, a sentence at a time: a sentence
/// already read is not asked about again.
fn check_line_llm(llm: &mut Llm, line: &[char]) -> Result<Vec<Problem>, String> {
    let words = words(line);
    if words.iter().filter(|w| w.text.chars().any(char::is_alphabetic)).count() < 2 {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    for sentence in sentences(&words) {
        let words = &words[sentence];
        if words.iter().filter(|w| w.text.chars().any(char::is_alphabetic)).count() < 2 {
            continue;
        }
        let text: String = line[words[0].from..words[words.len() - 1].to].iter().collect();
        let fixed = llm.fixed(&text)?;
        if !fixed.is_empty() {
            out.extend(problems(line, words, &fixed, true));
        }
    }
    Ok(out)
}

/// The sentences among a line's words: each ends after a . ! or ?
fn sentences(words: &[Word]) -> Vec<std::ops::Range<usize>> {
    let mut out = Vec::new();
    let mut start = 0;
    for (i, w) in words.iter().enumerate() {
        if matches!(w.text.as_str(), "." | "!" | "?") {
            out.push(start..i + 1);
            start = i + 1;
        }
    }
    if start < words.len() {
        out.push(start..words.len());
    }
    out
}

// ---- the thread

pub enum Job {
    Check(u64, Vec<(usize, String)>),
}

pub enum News {
    Found(u64, Vec<Problem>),
    /// The model could not be used: no file, or a broken one. Said once.
    Unavailable(String),
    Loaded,
}

/// What reads the sentences: the model downloaded for it, or a language
/// model you run yourself (`complete.*` in the settings).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Backend {
    Builtin(PathBuf),
    Llm(crate::complete::Settings),
}

enum Reader {
    Builtin(Box<Model>),
    Llm(Llm),
}

pub struct Checker {
    jobs: Sender<Job>,
    news: Receiver<News>,
}

impl Checker {
    pub fn start(backend: Backend) -> Self {
        let (jobs, inbox) = channel::<Job>();
        let (tell, news) = channel();
        std::thread::spawn(move || {
            let mut model: Option<Reader> = None;
            let mut gone = false;
            let mut cache: HashMap<String, Vec<Problem>> = HashMap::new();
            let mut pending: Option<Job> = None;
            loop {
                let job = match pending.take() {
                    Some(job) => job,
                    None => match inbox.recv() {
                        Ok(job) => job,
                        Err(_) => return,
                    },
                };
                // Only the latest text matters.
                let Job::Check(mut n, mut lines) = job;
                while let Ok(Job::Check(n2, l2)) = inbox.try_recv() {
                    (n, lines) = (n2, l2);
                }
                if model.is_none() && !gone {
                    let loaded = match &backend {
                        Backend::Builtin(path) => Model::load(path).map(|m| Reader::Builtin(Box::new(m))),
                        Backend::Llm(settings) => Ok(Reader::Llm(Llm { settings: settings.clone(), agent: crate::complete::agent(), read: HashMap::new() })),
                    };
                    match loaded {
                        Ok(m) => {
                            model = Some(m);
                            let _ = tell.send(News::Loaded);
                        }
                        Err(e) => {
                            gone = true;
                            let _ = tell.send(News::Unavailable(e));
                        }
                    }
                }
                let Some(m) = &mut model else { continue };
                if cache.len() > CACHE_LINES {
                    cache.clear();
                }
                let mut found = Vec::new();
                for (row, text) in lines {
                    // Typing went on: this answer would be stale. Start on the new text.
                    if let Ok(newer) = inbox.try_recv() {
                        pending = Some(newer);
                        break;
                    }
                    let problems = match cache.get(&text) {
                        Some(p) => p.clone(),
                        None => {
                            let chars: Vec<char> = text.chars().collect();
                            let p = match m {
                                Reader::Builtin(m) => check_line(m, &chars).unwrap_or_default(),
                                Reader::Llm(llm) => match check_line_llm(llm, &chars) {
                                    Ok(p) => p,
                                    // The server is not there: say so once, and stop asking.
                                    Err(e) => {
                                        let _ = tell.send(News::Unavailable(e));
                                        return;
                                    }
                                },
                            };
                            cache.insert(text, p.clone());
                            p
                        }
                    };
                    found.extend(problems.into_iter().map(|mut p| {
                        p.row = row;
                        p
                    }));
                }
                if pending.is_none() && tell.send(News::Found(n, found)).is_err() {
                    return;
                }
            }
        });
        Checker { jobs, news }
    }

    pub fn check(&self, n: u64, lines: Vec<(usize, String)>) {
        let _ = self.jobs.send(Job::Check(n, lines));
    }

    pub fn news(&self) -> Option<News> {
        self.news.try_recv().ok()
    }
}

// ---- getting the model

/// SHA-256, fed as the bytes arrive: enough to know a 75 MB download is whole.
struct Sha256 {
    h: [u32; 8],
    buf: Vec<u8>,
    len: u64,
}

const K: [u32; 64] = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc,
    0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
    0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3, 0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208,
    0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
];

impl Sha256 {
    fn new() -> Self {
        Sha256 { h: [0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19], buf: Vec::with_capacity(64), len: 0 }
    }

    fn block(&mut self, chunk: &[u8]) {
        let mut w = [0u32; 64];
        for (i, word) in chunk.chunks(4).enumerate() {
            w[i] = u32::from_be_bytes([word[0], word[1], word[2], word[3]]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16].wrapping_add(s0).wrapping_add(w[i - 7]).wrapping_add(s1);
        }
        let mut v = self.h;
        for i in 0..64 {
            let s1 = v[4].rotate_right(6) ^ v[4].rotate_right(11) ^ v[4].rotate_right(25);
            let ch = (v[4] & v[5]) ^ (!v[4] & v[6]);
            let t1 = v[7].wrapping_add(s1).wrapping_add(ch).wrapping_add(K[i]).wrapping_add(w[i]);
            let s0 = v[0].rotate_right(2) ^ v[0].rotate_right(13) ^ v[0].rotate_right(22);
            let t2 = s0.wrapping_add((v[0] & v[1]) ^ (v[0] & v[2]) ^ (v[1] & v[2]));
            v = [t1.wrapping_add(t2), v[0], v[1], v[2], v[3].wrapping_add(t1), v[4], v[5], v[6]];
        }
        for (a, b) in self.h.iter_mut().zip(v) {
            *a = a.wrapping_add(b);
        }
    }

    fn update(&mut self, mut data: &[u8]) {
        self.len += data.len() as u64;
        if !self.buf.is_empty() {
            let need = 64 - self.buf.len();
            let take = need.min(data.len());
            self.buf.extend_from_slice(&data[..take]);
            data = &data[take..];
            if self.buf.len() == 64 {
                let full = std::mem::take(&mut self.buf);
                self.block(&full);
            }
        }
        let whole = data.len() / 64 * 64;
        for chunk in data[..whole].chunks(64) {
            self.block(chunk);
        }
        self.buf.extend_from_slice(&data[whole..]);
    }

    fn hex(mut self) -> String {
        let bits = self.len * 8;
        let mut tail = std::mem::take(&mut self.buf);
        tail.push(0x80);
        while tail.len() % 64 != 56 {
            tail.push(0);
        }
        tail.extend_from_slice(&bits.to_be_bytes());
        for chunk in tail.chunks(64) {
            self.block(chunk);
        }
        self.h.iter().map(|x| format!("{x:08x}")).collect()
    }
}

/// A download on its way: how far it has got, and how it ended.
pub struct Download {
    pub got: Arc<AtomicU64>,
    pub total: Arc<AtomicU64>,
    stop: Arc<AtomicBool>,
    done: Receiver<Result<(), String>>,
}

impl Download {
    pub fn start(to: PathBuf) -> Self {
        let url = std::env::var("OMANOTE_GRAMMAR_URL").ok().filter(|u| !u.is_empty()).unwrap_or_else(|| MODEL_URL.to_string());
        let (got, total, stop) = (Arc::new(AtomicU64::new(0)), Arc::new(AtomicU64::new(MODEL_BYTES)), Arc::new(AtomicBool::new(false)));
        let (tell, done) = channel();
        let (g, t, st) = (got.clone(), total.clone(), stop.clone());
        std::thread::spawn(move || {
            let _ = tell.send(fetch(&url, &to, &g, &t, &st));
        });
        Download { got, total, stop, done }
    }

    pub fn cancel(&self) {
        self.stop.store(true, Ordering::Relaxed);
    }

    pub fn finished(&self) -> Option<Result<(), String>> {
        self.done.try_recv().ok()
    }
}

fn fetch(url: &str, to: &Path, got: &AtomicU64, total: &AtomicU64, stop: &AtomicBool) -> Result<(), String> {
    let dir = to.parent().ok_or("no folder to put the model in")?;
    std::fs::create_dir_all(dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    let agent = ureq::AgentBuilder::new().timeout_connect(std::time::Duration::from_secs(20)).timeout_read(std::time::Duration::from_secs(60)).redirects(8).build();
    let response = agent.get(url).set("User-Agent", concat!("omanote/", env!("CARGO_PKG_VERSION"))).call().map_err(|e| match e {
        ureq::Error::Status(404, _) => "it is not published at its address yet".to_string(),
        ureq::Error::Status(code, _) => format!("the server answered HTTP {code}"),
        ureq::Error::Transport(t) => format!("no connection ({})", t.kind()),
    })?;
    if let Some(size) = response.header("Content-Length").and_then(|l| l.parse::<u64>().ok()) {
        total.store(size, Ordering::Relaxed);
    }
    // Into a draft next to it, moved into place only once it is whole and checked:
    // a half-downloaded model is never mistaken for one.
    let draft = to.with_extension("part");
    let mut out = std::io::BufWriter::new(std::fs::File::create(&draft).map_err(|e| format!("cannot write {}: {e}", draft.display()))?);
    let mut reader = response.into_reader().take(4 * MODEL_BYTES);
    let mut hash = Sha256::new();
    let mut chunk = vec![0u8; 256 * 1024];
    let failed = |why: String| {
        let _ = std::fs::remove_file(&draft);
        Err(why)
    };
    loop {
        if stop.load(Ordering::Relaxed) {
            return failed("cancelled".into());
        }
        let n = match reader.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) => return failed(format!("the download broke off: {e}")),
        };
        hash.update(&chunk[..n]);
        if let Err(e) = out.write_all(&chunk[..n]) {
            return failed(format!("cannot write the model: {e}"));
        }
        got.fetch_add(n as u64, Ordering::Relaxed);
    }
    if let Err(e) = out.flush() {
        return failed(format!("cannot write the model: {e}"));
    }
    drop(out);
    let sum = hash.hex();
    let wanted = std::env::var("OMANOTE_GRAMMAR_SHA256").ok().filter(|h| !h.is_empty()).unwrap_or_else(|| MODEL_SHA256.to_string());
    if sum != wanted {
        return failed("what arrived is not the model it should be (its checksum is wrong); nothing was kept".into());
    }
    std::fs::rename(&draft, to).map_err(|e| format!("cannot move the model into place: {e}"))
}

#[cfg(test)]
mod tests {

    fn strict(line: &str, fixed: &str) -> Vec<(String, String)> {
        let line: Vec<char> = line.chars().collect();
        let fixed: Vec<String> = words(&fixed.chars().collect::<Vec<_>>()).into_iter().map(|w| w.text).collect();
        problems(&line, &words(&line), &fixed, true).into_iter().map(|p| (p.word, p.fixes[0].label())).collect()
    }

    #[test]
    fn a_language_models_mistakes_are_kept_and_its_taste_is_not() {
        let pair = |a: &str, b: &str| vec![(a.to_string(), b.to_string())];
        assert_eq!(strict("We stayed for tree nights.", "We stayed for three nights."), pair("tree", "three"));
        assert_eq!(strict("we has a meeting", "we have a meeting"), pair("has", "have"));
        assert_eq!(strict("Its a nice day", "It's a nice day"), pair("Its", "It's"));
        assert_eq!(strict("ho are you", "How are you"), pair("ho", "How"));
        assert_eq!(strict("i think so", "I think so"), pair("i", "I"));
        // Its taste: commas, a full stop, an accent, a capital after a colon, a rewrite.
        assert!(strict("Honestly it was weird but whatever", "Honestly, it was weird, but whatever").is_empty());
        assert!(strict("Bought milk, eggs and bread", "Bought milk, eggs and bread.").is_empty());
        assert!(strict("We met at the cafe", "We met at the café").is_empty());
        assert!(strict("TODO: fix the login bug", "TODO: Fix the login bug").is_empty());
        assert!(strict("me and him went", "he and I went there together today").is_empty());
    }

    #[test]
    fn a_line_is_read_a_sentence_at_a_time() {
        let line: Vec<char> = "It rained. We stayed in! Then what".chars().collect();
        let ws = words(&line);
        let texts: Vec<String> = sentences(&ws).into_iter().map(|r| ws[r].iter().map(|w| w.text.as_str()).collect::<Vec<_>>().join(" ")).collect();
        assert_eq!(texts, ["It rained .", "We stayed in !", "Then what"]);
    }

    use super::*;

    fn w(line: &str) -> Vec<(String, usize, usize)> {
        words(&line.chars().collect::<Vec<_>>()).into_iter().map(|w| (w.text, w.from, w.to)).collect()
    }

    #[test]
    fn splits_a_line_into_the_words_the_model_expects() {
        assert_eq!(w("teh hotel, booked."), [("teh".into(), 0, 3), ("hotel".into(), 4, 9), (",".into(), 9, 10), ("booked".into(), 11, 17), (".".into(), 17, 18)]);
        assert_eq!(w("- [ ] it wasn't **bold** what's"), [("it".into(), 6, 8), ("was".into(), 9, 12), ("n't".into(), 12, 15), ("bold".into(), 18, 22), ("what".into(), 25, 29), ("'s".into(), 29, 31)]);
        assert_eq!(w("## Trip notes"), [("Trip".into(), 3, 7), ("notes".into(), 8, 13)]);
        assert_eq!(w("2. see [the plan](trips/japan.md) and `teh code` here"), [("see".into(), 3, 6), ("the".into(), 8, 11), ("plan".into(), 12, 16), ("and".into(), 34, 37), ("here".into(), 49, 53)], "a link's text is prose, its address is not");
        assert_eq!(w("> a `multi word code` span http://x.org ok"), [("a".into(), 2, 3), ("span".into(), 22, 26), ("ok".into(), 40, 42)]);
        assert_eq!(w("- - -"), []);
        assert_eq!(w("it's 5 o'clock"), [("it".into(), 0, 2), ("'s".into(), 2, 4), ("5".into(), 5, 6), ("o'clock".into(), 7, 14)]);
    }

    #[test]
    fn checks_what_it_downloads() {
        let hex = |data: &[u8], piece: usize| {
            let mut h = Sha256::new();
            for chunk in data.chunks(piece) {
                h.update(chunk);
            }
            h.hex()
        };
        assert_eq!(hex(b"abc", 1), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
        assert_eq!(hex(b"", 1), "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855");
        let long: Vec<u8> = (0..10_000u32).map(|i| (i * 7 % 251) as u8).collect();
        assert_eq!(hex(&long, 1), hex(&long, 4096), "the same however it arrives");
        assert_eq!(hex(&long, 63), hex(&long, 65));
    }

    #[test]
    fn pretokenizes_like_gpt2() {
        assert_eq!(Bpe::pieces(" booked,"), [" booked", ","]);
        assert_eq!(Bpe::pieces(" wasn't"), [" wasn", "'t"]);
        assert_eq!(Bpe::pieces(" PCIe"), [" PCIe"]);
        assert_eq!(Bpe::pieces(" x16"), [" x", "16"]);
        assert_eq!(Bpe::pieces(" ..."), [" ..."]);
        assert_eq!(parse_vocab(r#"{"<s>": 0, "Ġthe": 5, "a\"b": 7}"#).get("Ġthe"), Some(&5));
        assert_eq!(byte_to_char()[b' ' as usize], 'Ġ');
    }

    #[test]
    fn differences_are_found_word_by_word() {
        let v = |s: &str| s.split(' ').map(str::to_string).collect::<Vec<_>>();
        assert_eq!(differences(&v("let do it"), &v("Let 's do it")), [(0..1, 0..2)]);
        assert_eq!(differences(&v("teh hotel were booked"), &v("The hotel was booked")), [(0..1, 0..1), (2..3, 2..3)]);
        assert_eq!(differences(&v("a the dog"), &v("a dog")), [(1..2, 1..1)]);
        assert_eq!(differences(&v("go home"), &v("go home .")), [(2..2, 2..3)]);
        assert_eq!(join(&v("Let 's try , what n't ok")), "Let's try, whatn't ok");
    }

    /// Needs the model file; without it this only says so.
    #[test]
    fn the_model_finds_what_the_dictionary_cannot() {
        let path = model_path();
        if !path.exists() {
            eprintln!("no model at {}: skipped", path.display());
            return;
        }
        let model = Model::load(&path).unwrap();
        // The tokenizer must agree with the reference implementation, byte for byte.
        let ids: Vec<u32> = ["$START", "teh", "hotel", "were", "booked,", "wasn't", "PCIe"].iter().flat_map(|w| model.bpe.encode_word(w)).collect();
        assert_eq!(ids, [50265, 3055, 298, 2303, 58, 7512, 6, 938, 75, 43680]);

        let fix = |line: &str| -> Vec<(String, String)> {
            let chars: Vec<char> = line.chars().collect();
            check_line(&model, &chars).unwrap().into_iter().map(|p| (p.word, p.fixes[0].label())).collect()
        };
        assert_eq!(fix("ho are you doing"), [("ho".to_string(), "How".to_string())]);
        let hotel = fix("teh hotel were booked for tree nights and we has to confirm it untill friday");
        // int4 says "had" where 8-bit says "have": either way "we has" is caught.
        assert!(hotel.contains(&("were".into(), "was".into())) && hotel.contains(&("tree".into(), "three".into())) && hotel.iter().any(|(w, _)| w == "has"), "{hotel:?}");
        assert_eq!(fix("The hotel was booked for three nights."), []);
        assert_eq!(fix("- [ ] Reported PCIe links at capture: RTX 8000 at Gen 3 x16; see [the plan](trips/japan.md)."), []);
        assert!(fix("I has a dog.").iter().any(|(w, _)| w == "has"));
        // Every pass done before it is offered: one fix, the final one.
        assert_eq!(fix("let do something interesting"), [("let".to_string(), "Let's".to_string())]);
        assert_eq!(fix("Let try to write something here."), [("Let".to_string(), "Let's".to_string())]);
    }
    /// How long a line takes, on this machine: `cargo test --release grammar::tests::timing -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn timing() {
        let path = model_path();
        let started = std::time::Instant::now();
        let model = Model::load(&path).unwrap();
        eprintln!("load: {:?}", started.elapsed());
        let lines = [
            "Let try to write something here.",
            "let try to write something here",
            "ho are you doing",
            "teh hotel were booked for tree nights and we has to confirm it untill friday",
            "I has a dog. She go home yesterday.",
            "The hotel was booked for three nights and we have to confirm it by Friday.",
            "so how are you doting, whats up? this one actually shows ok and I want to see how long a longer line with more words in it takes to check",
            "This is an test of of the the checker and its not good",
            "Book the ryokan in Kyoto and buy some milk tomorrow.",
            "hi wats up",
            "- [ ] by some milk tomorow",
            "She don't know where he live, and they was late for there meeting.",
            "Me and him goes to the store every days.",
            "I seen him yesterday and he say he will comes tomorrow.",
            "We should of left earlier, the traffic were terrible.",
        ];
        for line in lines {
            let chars: Vec<char> = line.chars().collect();
            let t = std::time::Instant::now();
            let found = check_line(&model, &chars).unwrap();
            let fixes: Vec<String> = found.iter().map(|p| format!("{} -> {}", p.word, p.fixes[0].label())).collect();
            eprintln!("{:>5} ms  {} words  {fixes:?}", t.elapsed().as_millis(), line.split_whitespace().count());
        }
        let status = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
        let field = |name: &str| status.lines().find(|l| l.starts_with(name)).unwrap_or("").split_whitespace().nth(1).and_then(|k| k.parse::<u64>().ok()).unwrap_or(0) / 1024;
        drop(model);
        let after = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
        let rss_after = after.lines().find(|l| l.starts_with("VmRSS")).unwrap_or("").split_whitespace().nth(1).and_then(|k| k.parse::<u64>().ok()).unwrap_or(0) / 1024;
        eprintln!("RAM: {} MB in use with the model, {} MB peak, {} MB once it is dropped", field("VmRSS"), field("VmHWM"), rss_after);
    }

}
