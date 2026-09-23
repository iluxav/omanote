//! Type-ahead: a word or two of what comes next, shown dim after the cursor
//! and taken with Tab. The guess comes from a language model you run
//! yourself, named in the settings (`complete.model`), asked over HTTP.
//!
//! Two ways of asking are spoken. A plain address (`http://localhost:11434`,
//! the default) is Ollama's own API, asked in raw mode so the note is
//! continued rather than answered: Ollama's OpenAI-style endpoint wraps the
//! text in the chat template, and the model replies to it instead. An address
//! ending in `/v1` is an OpenAI-style `/completions` endpoint, as served by
//! llama.cpp, LM Studio or vLLM.
//!
//! Asking happens on a thread of its own. Only the newest question is
//! answered, and an answer to a note that has since changed is thrown away.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::Duration;

/// `complete.*` in the settings. Nothing is asked until a model is named.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Settings {
    pub url: String,
    pub model: String,
    /// Sent as a bearer token, for servers that want one.
    pub key: String,
    /// The most words a guess shows.
    pub words: usize,
    /// The model's context window, in tokens. Ollama reserves memory for the
    /// whole window, and a model's own is often 128k: far more than a guess
    /// needs, and many gigabytes. It also caps how much of the note is sent.
    pub context: usize,
}

impl Default for Settings {
    fn default() -> Self {
        Settings { url: "http://localhost:11434".into(), model: String::new(), key: String::new(), words: 3, context: 2048 }
    }
}

impl Settings {
    pub fn on(&self) -> bool {
        !self.model.trim().is_empty()
    }

    fn openai(&self) -> bool {
        self.url.trim_end_matches('/').ends_with("/v1")
    }

    /// Where to ask, and what to send. `alternatives`: Ollama also lists the
    /// likeliest first pieces of the answer, not only the one it chose.
    fn request_with(&self, prompt: &str, alternatives: bool) -> (String, String) {
        let base = self.url.trim_end_matches('/');
        let model = json_string(self.model.trim());
        let prompt = json_string(prompt);
        let tokens = self.words * 3 + 4;
        if self.openai() {
            let body = format!(r#"{{"model":{model},"prompt":{prompt},"max_tokens":{tokens},"temperature":0,"stop":["\n"],"stream":false}}"#);
            (format!("{base}/completions"), body)
        } else {
            let listed = if alternatives { format!(r#""logprobs":true,"top_logprobs":{ALTERNATIVES},"#) } else { String::new() };
            let body = format!(r#"{{"model":{model},"prompt":{prompt},"raw":true,"stream":false,{listed}"keep_alive":"30m","options":{{"num_predict":{tokens},"num_ctx":{ctx},"temperature":0,"stop":["\n"]}}}}"#, ctx = self.context);
            (format!("{base}/api/generate"), body)
        }
    }
}

/// A guess is on its way. The grammar check, when it asks the same server,
/// waits for it: the guess is what the typing is waiting on.
pub static GUESSING: AtomicBool = AtomicBool::new(false);

/// A guess for the note as it was at edit `edits`, or why there is none.
pub type Answer = (u64, Result<String, String>);

pub struct Completer {
    pub settings: Settings,
    ask: Sender<(u64, String)>,
    answers: Receiver<Answer>,
    /// A failure is said once, not at every pause in the typing.
    pub complained: bool,
}

impl Completer {
    pub fn start(settings: Settings) -> Self {
        let (ask, questions) = channel::<(u64, String)>();
        let (tell, answers) = channel();
        let s = settings.clone();
        std::thread::spawn(move || {
            let agent = agent();
            // Load the model now, so the first guess does not wait for it.
            if !s.openai() {
                let body = format!(r#"{{"model":{},"keep_alive":"30m","options":{{"num_ctx":{}}}}}"#, json_string(s.model.trim()), s.context);
                let _ = post(&agent, &s, &format!("{}/api/generate", s.url.trim_end_matches('/')), &body);
            }
            let newest = std::cell::RefCell::new(None::<(u64, String)>);
            // Typing has moved on while a guess was being made: only the newest counts.
            let moved_on = || {
                let mut got = false;
                while let Ok(newer) = questions.try_recv() {
                    *newest.borrow_mut() = Some(newer);
                    got = true;
                }
                got
            };
            let mut words: Option<Arc<dyn harper_core::spell::Dictionary>> = None;
            loop {
                let next = match newest.borrow_mut().take() {
                    Some(next) => next,
                    None => match questions.recv() {
                        Ok(next) => next,
                        Err(_) => break,
                    },
                };
                *newest.borrow_mut() = Some(next);
                moved_on();
                let Some((id, prompt)) = newest.borrow_mut().take() else { continue };
                GUESSING.store(true, Ordering::Relaxed);
                let asker = Asker { agent: &agent, s: &s, moved_on: &moved_on };
                let answer = match partial_word(&prompt) {
                    "" => asker.ask(&prompt).map(|text| trim(&text, s.words)),
                    part => {
                        let known = words.get_or_insert_with(|| crate::spell::dictionary(&crate::vaults::home())).clone();
                        let is_word = |w: &str| known.contains_word_str(w) || known.contains_word_str(&w.to_lowercase());
                        asker.finish_word(&prompt, part, &is_word).map(|after| after.map(|a| trim(&a, s.words)).unwrap_or_default())
                    }
                };
                GUESSING.store(false, Ordering::Relaxed);
                // An answer to a question typed over is not wanted.
                if newest.borrow().is_some() {
                    continue;
                }
                if tell.send((id, answer)).is_err() {
                    break;
                }
            }
        });
        Completer { settings, ask, answers, complained: false }
    }

    /// Guess what follows `before`, the note up to the cursor.
    pub fn ask(&self, id: u64, before: &str) {
        // As many bytes as the window has tokens: English runs about four
        // characters to a token, so that fills a quarter of it.
        let _ = self.ask.send((id, tail(before, self.settings.context).to_string()));
    }

    pub fn answer(&self) -> Option<Answer> {
        self.answers.try_recv().ok()
    }
}

/// How many likely first pieces Ollama lists (its most).
const ALTERNATIVES: usize = 20;
/// The most questions one half-typed word may take.
const TRIES: usize = 8;

/// The word being typed at the end of `text`: its letters so far.
fn partial_word(text: &str) -> &str {
    let start = text.char_indices().rev().take_while(|(_, c)| c.is_alphabetic() || *c == '\'').last().map_or(text.len(), |(i, _)| i);
    // "don'" is a word being typed; a lone quote is not.
    let part = &text[start..];
    if part.chars().any(char::is_alphabetic) { part.trim_start_matches('\'') } else { "" }
}

/// The letters a continuation adds to the word being typed.
fn word_start(after: &str) -> &str {
    let end = after.char_indices().find(|(_, c)| !(c.is_alphabetic() || *c == '\'')).map_or(after.len(), |(i, _)| i);
    &after[..end]
}

/// One guess's worth of asking, with a way to tell the typing has moved on.
struct Asker<'a> {
    agent: &'a ureq::Agent,
    s: &'a Settings,
    moved_on: &'a dyn Fn() -> bool,
}

impl Asker<'_> {
    /// What the model writes after `prompt`.
    fn ask(&self, prompt: &str) -> Result<String, String> {
        Ok(self.ask_listing(prompt, false)?.0)
    }

    /// The same, and the likeliest first pieces it chose among, best first.
    fn ask_listing(&self, prompt: &str, alternatives: bool) -> Result<(String, Vec<String>), String> {
        let (url, body) = self.s.request_with(prompt, alternatives);
        let reply = post(self.agent, self.s, &url, &body)?;
        let json = Json::parse(&reply).ok_or("the server's answer was not JSON")?;
        let text = json.get(if self.s.openai() { "choices" } else { "response" }).and_then(|v| if self.s.openai() { v.at(0)?.get("text")?.str() } else { v.str() });
        let text = text.ok_or("the server's answer had no text in it")?.to_string();
        let listed = json.get("logprobs").and_then(|l| l.at(0)?.get("top_logprobs")).map(|top| top.items().iter().filter_map(|t| Some(t.get("token")?.str()?.to_string())).collect()).unwrap_or_default();
        Ok((text, listed))
    }

    /// The rest of the half-typed word `part` at the end of `prompt`, and
    /// what follows it; `None` when no real word is found.
    ///
    /// A model reads text in pieces, and "extre" ends in the middle of one:
    /// asked what comes next, it goes on from a piece that is not there and
    /// writes "extre" + "emly". So the word is looked for three ways, surest
    /// first, and the first that spells a word in the dictionary is the guess:
    ///
    /// 1. From the word's start ("this is" + " extre…"), among the likeliest
    ///    pieces there: a whole piece is always spelt right, but a word the
    ///    context does not call for is not among them.
    /// 2. Made to start with the letters typed (a pattern the server holds
    ///    it to): finds most words, though forcing a piece apart can misspell.
    /// 3. From a letter at a time back into the word, the pieces that agree
    ///    with the letters typed: finds something, not always the best.
    fn finish_word(&self, prompt: &str, part: &str, is_word: &dyn Fn(&str) -> bool) -> Result<Option<String>, String> {
        let before = &prompt[..prompt.len() - part.len()];
        let spelt = |after: &str| {
            let added = word_start(after);
            !added.is_empty() && is_word(&format!("{part}{added}"))
        };
        // A server that lists no pieces: its own guess, if it spells a word.
        if self.s.openai() {
            let after = self.ask(prompt)?;
            return Ok(spelt(&after).then_some(after));
        }
        let mut asked = 0;
        let budget = |asked: &mut usize| {
            *asked += 1;
            *asked <= TRIES && !(self.moved_on)()
        };

        // 1. From the word's start, space and all.
        let start = before.trim_end_matches([' ', '\t']);
        let typed = format!("{}{part}", &before[start.len()..]);
        if !budget(&mut asked) {
            return Ok(None);
        }
        let (said, listed) = self.ask_listing(start, true)?;
        let agrees = |piece: &str| !piece.trim().is_empty() && (piece.starts_with(typed.as_str()) || typed.starts_with(piece));
        let mut pieces: Vec<String> = Vec::new();
        for piece in std::iter::once(said.clone()).chain(listed) {
            let piece = if said.starts_with(piece.as_str()) && piece != said { said.clone() } else { piece };
            if agrees(&piece) && !pieces.contains(&piece) {
                pieces.push(piece);
            }
        }
        for piece in pieces {
            let written = if said.starts_with(piece.as_str()) && said.len() > typed.len() {
                said.clone()
            } else {
                if !budget(&mut asked) {
                    return Ok(None);
                }
                format!("{piece}{}", self.ask(&format!("{start}{piece}"))?)
            };
            if let Some(after) = written.strip_prefix(typed.as_str()).filter(|a| spelt(a)) {
                return Ok(Some(after.to_string()));
            }
        }

        // 2. Held to the letters typed.
        if part.chars().all(|c| c.is_ascii_alphabetic() || c == '\'') {
            if !budget(&mut asked) {
                return Ok(None);
            }
            let word = self.ask_word(before, part)?;
            if let Some(added) = word.strip_prefix(part).filter(|a| !a.is_empty() && is_word(&word)) {
                let rest = if self.s.words > 1 && budget(&mut asked) { self.ask(&format!("{prompt}{added}"))? } else { String::new() };
                return Ok(Some(format!("{added}{rest}")));
            }
        }

        // 3. A letter at a time back into the word.
        let letters: Vec<(usize, char)> = part.char_indices().collect();
        for &(i, _) in letters.iter().skip(1).rev() {
            let (from, rest) = (&prompt[..before.len() + i], &part[i..]);
            if !budget(&mut asked) {
                return Ok(None);
            }
            let (said, listed) = self.ask_listing(from, true)?;
            if let Some(after) = said.strip_prefix(rest).filter(|a| spelt(a)) {
                return Ok(Some(after.to_string()));
            }
            for piece in listed.into_iter().filter(|t| t.len() > rest.len() && t.starts_with(rest) && !said.starts_with(t.as_str())) {
                if !budget(&mut asked) {
                    return Ok(None);
                }
                let written = format!("{piece}{}", self.ask(&format!("{from}{piece}"))?);
                if let Some(after) = written.strip_prefix(rest).filter(|a| spelt(a)) {
                    return Ok(Some(after.to_string()));
                }
            }
        }
        Ok(None)
    }

    /// One word after `before`, made to start with `part`: the server holds
    /// the model to a pattern, as it does for JSON answers.
    fn ask_word(&self, before: &str, part: &str) -> Result<String, String> {
        let base = self.s.url.trim_end_matches('/');
        let pattern = json_string(&format!("^{part}['a-zA-Z]+$"));
        let body = format!(
            r#"{{"model":{},"prompt":{},"raw":true,"stream":false,"format":{{"type":"string","pattern":{pattern}}},"keep_alive":"30m","options":{{"num_predict":12,"num_ctx":{},"temperature":0}}}}"#,
            json_string(self.s.model.trim()),
            json_string(before),
            self.s.context
        );
        let reply = post(self.agent, self.s, &format!("{base}/api/generate"), &body)?;
        let text = Json::parse(&reply).and_then(|j| j.get("response")?.str().map(str::to_string)).ok_or("the server's answer had no text in it")?;
        // The answer is a JSON string: the word, in quotes. Cut short, it has no closing quote.
        Ok(Json::parse(&text).and_then(|j| j.str().map(str::to_string)).unwrap_or_else(|| text.trim_start_matches('"').to_string()))
    }
}

pub fn agent() -> ureq::Agent {
    ureq::AgentBuilder::new().timeout_connect(Duration::from_secs(2)).timeout(Duration::from_secs(30)).build()
}

/// One turn of a conversation: the instructions, example exchanges, and the
/// question; the model's reply. Asked through the chat endpoint, so an
/// instruction-tuned model answers as it was trained to.
pub fn chat(agent: &ureq::Agent, s: &Settings, system: &str, examples: &[(&str, &str)], question: &str, tokens: usize) -> Result<String, String> {
    let mut messages = vec![format!(r#"{{"role":"system","content":{}}}"#, json_string(system))];
    for (asked, answered) in examples {
        messages.push(format!(r#"{{"role":"user","content":{}}}"#, json_string(asked)));
        messages.push(format!(r#"{{"role":"assistant","content":{}}}"#, json_string(answered)));
    }
    messages.push(format!(r#"{{"role":"user","content":{}}}"#, json_string(question)));
    let (base, model, messages) = (s.url.trim_end_matches('/'), json_string(s.model.trim()), messages.join(","));
    let (url, body) = if s.openai() {
        (format!("{base}/chat/completions"), format!(r#"{{"model":{model},"messages":[{messages}],"temperature":0,"max_tokens":{tokens},"stream":false}}"#))
    } else {
        // The same window as the guesses ask for: a different one would have Ollama load the model again.
        (format!("{base}/api/chat"), format!(r#"{{"model":{model},"messages":[{messages}],"stream":false,"think":false,"keep_alive":"30m","options":{{"temperature":0,"num_predict":{tokens},"num_ctx":{}}}}}"#, s.context))
    };
    let reply = post(agent, s, &url, &body)?;
    json_field(&reply, "content").ok_or_else(|| "the server's answer had no text in it".to_string())
}

fn post(agent: &ureq::Agent, s: &Settings, url: &str, body: &str) -> Result<String, String> {
    let mut req = agent.post(url).set("Content-Type", "application/json");
    if !s.key.is_empty() {
        req = req.set("Authorization", &format!("Bearer {}", s.key));
    }
    match req.send_string(body) {
        Ok(reply) => reply.into_string().map_err(|e| format!("could not read the answer from {}: {e}", s.url)),
        Err(ureq::Error::Status(code, reply)) => {
            let said = reply.into_string().ok().and_then(|r| json_field(&r, "error").or_else(|| json_field(&r, "message"))).unwrap_or_default();
            Err(format!("{} answered HTTP {code}{}", s.url, if said.is_empty() { String::new() } else { format!(": {said}") }))
        }
        Err(ureq::Error::Transport(t)) => Err(format!("no answer from {} ({})", s.url, t.kind())),
    }
}

/// The last `max` bytes of `text`, cut at a character.
fn tail(text: &str, max: usize) -> &str {
    let mut from = text.len().saturating_sub(max);
    while !text.is_char_boundary(from) {
        from += 1;
    }
    &text[from..]
}

/// The guess, cut down to what is worth showing: at most `words` words, one
/// line, and no further than the end of a sentence. A guess that is only
/// space or punctuation is no guess.
pub fn trim(text: &str, words: usize) -> String {
    let text = text.split('\n').next().unwrap_or("");
    let mut out = String::new();
    let mut count = 0;
    let mut in_word = false;
    for c in text.chars() {
        if c.is_whitespace() {
            if in_word && (count >= words || out.ends_with(['.', '!', '?'])) {
                break;
            }
            in_word = false;
        } else if !in_word {
            in_word = true;
            count += 1;
        }
        out.push(c);
    }
    let out = out.trim_end().to_string();
    if out.chars().any(char::is_alphanumeric) { out } else { String::new() }
}

/// `text` as a JSON string, quotes included.
fn json_string(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// The first string value named `name` anywhere in a JSON reply. Enough for
/// the one or two fields read here, without pulling in a JSON crate.
fn json_field(json: &str, name: &str) -> Option<String> {
    let key = format!("\"{name}\"");
    let mut rest = json;
    loop {
        let at = rest.find(&key)?;
        rest = rest[at + key.len()..].trim_start();
        let Some(after) = rest.strip_prefix(':') else { continue };
        let after = after.trim_start();
        let Some(body) = after.strip_prefix('"') else { continue };
        return read_string(body);
    }
}

/// A JSON string's contents, from just after its opening quote.
fn read_string(body: &str) -> Option<String> {
    let mut out = String::new();
    let mut chars = body.chars();
    while let Some(c) = chars.next() {
        match c {
            '"' => return Some(out),
            '\\' => match chars.next()? {
                'n' => out.push('\n'),
                't' => out.push('\t'),
                'r' => out.push('\r'),
                'b' => out.push('\u{8}'),
                'f' => out.push('\u{c}'),
                'u' => {
                    let hex = |chars: &mut std::str::Chars| -> Option<u32> { u32::from_str_radix(&chars.by_ref().take(4).collect::<String>(), 16).ok() };
                    let mut code = hex(&mut chars)?;
                    // A character outside the first plane comes as two halves.
                    if (0xD800..0xDC00).contains(&code) {
                        let rest = chars.as_str();
                        if let Some(low) = rest.strip_prefix("\\u").and_then(|r| u32::from_str_radix(r.get(..4)?, 16).ok()) {
                            code = 0x10000 + ((code - 0xD800) << 10) + (low - 0xDC00);
                            chars = rest[6..].chars();
                        }
                    }
                    out.extend(char::from_u32(code));
                }
                other => out.push(other),
            },
            c => out.push(c),
        }
    }
    None
}

/// A JSON value, read just far enough for a server's answer.
#[derive(Debug, PartialEq)]
enum Json {
    Null,
    Bool(bool),
    Num(f64),
    Str(String),
    Arr(Vec<Json>),
    Obj(Vec<(String, Json)>),
}

impl Json {
    fn parse(text: &str) -> Option<Json> {
        let chars: Vec<char> = text.chars().collect();
        let mut at = 0;
        let value = Json::value(&chars, &mut at)?;
        Json::space(&chars, &mut at);
        (at == chars.len()).then_some(value)
    }

    fn get(&self, key: &str) -> Option<&Json> {
        match self {
            Json::Obj(fields) => fields.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    fn at(&self, i: usize) -> Option<&Json> {
        self.items().get(i)
    }

    fn items(&self) -> &[Json] {
        match self {
            Json::Arr(items) => items,
            _ => &[],
        }
    }

    fn str(&self) -> Option<&str> {
        match self {
            Json::Str(s) => Some(s),
            _ => None,
        }
    }

    fn space(c: &[char], at: &mut usize) {
        while c.get(*at).is_some_and(|c| c.is_whitespace()) {
            *at += 1;
        }
    }

    fn value(c: &[char], at: &mut usize) -> Option<Json> {
        Json::space(c, at);
        let word = |at: &mut usize, w: &str, v: Json| {
            let end = *at + w.chars().count();
            (c.get(*at..end)?.iter().copied().eq(w.chars())).then(|| {
                *at = end;
                v
            })
        };
        match c.get(*at)? {
            'n' => word(at, "null", Json::Null),
            't' => word(at, "true", Json::Bool(true)),
            'f' => word(at, "false", Json::Bool(false)),
            '"' => {
                let rest: String = c[*at + 1..].iter().collect();
                let (text, used) = read_string_len(&rest)?;
                *at += 1 + used;
                Some(Json::Str(text))
            }
            '[' => {
                *at += 1;
                let mut items = Vec::new();
                loop {
                    Json::space(c, at);
                    if c.get(*at) == Some(&']') {
                        *at += 1;
                        return Some(Json::Arr(items));
                    }
                    items.push(Json::value(c, at)?);
                    Json::space(c, at);
                    match c.get(*at)? {
                        ',' => *at += 1,
                        ']' => {}
                        _ => return None,
                    }
                }
            }
            '{' => {
                *at += 1;
                let mut fields = Vec::new();
                loop {
                    Json::space(c, at);
                    if c.get(*at) == Some(&'}') {
                        *at += 1;
                        return Some(Json::Obj(fields));
                    }
                    let Json::Str(key) = Json::value(c, at)? else { return None };
                    Json::space(c, at);
                    (c.get(*at)? == &':').then_some(())?;
                    *at += 1;
                    fields.push((key, Json::value(c, at)?));
                    Json::space(c, at);
                    match c.get(*at)? {
                        ',' => *at += 1,
                        '}' => {}
                        _ => return None,
                    }
                }
            }
            _ => {
                let start = *at;
                while c.get(*at).is_some_and(|c| c.is_ascii_digit() || "+-.eE".contains(*c)) {
                    *at += 1;
                }
                c[start..*at].iter().collect::<String>().parse().ok().map(Json::Num)
            }
        }
    }
}

/// A JSON string's contents from just after its opening quote, and how many
/// characters it took, closing quote included.
fn read_string_len(body: &str) -> Option<(String, usize)> {
    let text = read_string(body)?;
    // Walk it again to count: escapes make the two lengths differ.
    let mut chars = body.chars();
    let mut used = 0;
    while let Some(c) = chars.next() {
        used += 1;
        match c {
            '"' => return Some((text, used)),
            '\\' => {
                let e = chars.next()?;
                used += 1;
                if e == 'u' {
                    chars.by_ref().take(4).for_each(|_| used += 1);
                }
            }
            _ => {}
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_guess_is_a_few_words_on_one_line() {
        assert_eq!(trim(" not ready for the launch", 3), " not ready for");
        assert_eq!(trim("ration is late", 2), "ration is", "the end of a word counts as one");
        assert_eq!(trim(" done. Then we", 3), " done.", "a sentence ends the guess");
        assert_eq!(trim(" first line\nsecond", 5), " first line");
        assert_eq!(trim(" ...", 3), "");
        assert_eq!(trim("   ", 3), "");
    }

    #[test]
    fn json_goes_out_escaped_and_comes_back_read() {
        assert_eq!(json_string("a \"b\"\n\\c\u{1}"), r#""a \"b\"\n\\c\u0001""#);
        let reply = r#"{"model":"x","created_at":"t","response":" said \"hi\" é😀\n","done":true}"#;
        assert_eq!(json_field(reply, "response").as_deref(), Some(" said \"hi\" é😀\n"));
        let openai = r#"{"choices":[{"text":" not ready.","index":0}]}"#;
        assert_eq!(json_field(openai, "text").as_deref(), Some(" not ready."));
        assert_eq!(json_field(r#"{"text": 3, "x": {"text" : "yes"}}"#, "text").as_deref(), Some("yes"));
        assert_eq!(json_field(r#"{"other":"x"}"#, "text"), None);
    }

    #[test]
    fn ollama_is_asked_raw_and_openai_servers_at_completions() {
        let s = Settings { model: "qwen3.5:9b".into(), ..Settings::default() };
        let (url, body) = s.request_with("a \"quote\"", false);
        assert_eq!(url, "http://localhost:11434/api/generate");
        assert!(body.contains(r#""raw":true"#) && body.contains(r#""prompt":"a \"quote\"""#), "{body}");
        assert!(body.contains(r#""num_ctx":2048"#), "a small window, not the model's own: {body}");
        let s = Settings { url: "http://box:8080/v1/".into(), ..s };
        let (url, body) = s.request_with("x", false);
        assert_eq!(url, "http://box:8080/v1/completions");
        assert!(body.contains(r#""max_tokens":13"#) && !body.contains("raw"), "{body}");
    }

    #[test]
    fn the_word_being_typed_is_found() {
        assert_eq!(partial_word("this is extre"), "extre");
        assert_eq!(partial_word("this is "), "");
        assert_eq!(partial_word("I don'"), "don'");
        assert_eq!(partial_word("see (café"), "café");
        assert_eq!(partial_word("x = 42"), "");
        assert_eq!(word_start("mely helpful."), "mely");
        assert_eq!(word_start(" helpful"), "");
    }

    #[test]
    fn a_servers_answer_is_read_as_json() {
        let reply = r#"{"response":"re","done":true,"n":-1.5e2,"logprobs":[{"token":"re","top_logprobs":[{"token":"ream","logprob":-0.3},{"token":"re\"m","logprob":-1}]}],"x":null}"#;
        let json = Json::parse(reply).expect("parses");
        assert_eq!(json.get("response").and_then(Json::str), Some("re"));
        assert_eq!(json.get("n"), Some(&Json::Num(-150.0)));
        let top: Vec<&str> = json.get("logprobs").and_then(|l| l.at(0)?.get("top_logprobs")).unwrap().items().iter().filter_map(|t| t.get("token")?.str()).collect();
        assert_eq!(top, ["ream", "re\"m"]);
        assert_eq!(Json::parse(r#"{"a":"\u00e9\n"}"#).unwrap().get("a").and_then(Json::str), Some("é\n"));
        assert!(Json::parse("{\"a\":").is_none());
    }

    #[test]
    fn the_context_is_cut_at_a_character() {
        assert_eq!(tail("héllo", 4), "llo");
        assert_eq!(tail("hi", 10), "hi");
    }
}
