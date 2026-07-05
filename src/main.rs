//! koen — Korean → English prompt compressor for LLM pipelines.
//!
//! Translates Korean prompts to concise English using a CHEAP model
//! (Claude Haiku / Codex mini / free OpenRouter model), so the expensive
//! model (Opus/Fable/GPT-5) receives fewer tokens for the same meaning.

use std::env;
use std::ffi::CString;
use std::io::{Read, Write};
use std::mem;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;

const HELP: &str = "\
koen — Korean → English prompt compressor for LLM pipelines.

Usage:
    koen \"한국어 프롬프트\"
    echo \"한국어 프롬프트\" | koen
    koen -f spec.ko.md > spec.en.md

    koen claude [claude args...]     # run the REAL claude/codex TUI in a pty;
    koen codex  [codex args...]      # koen intercepts Enter, translates Hangul
                                     # lines via the cheap model, and submits
                                     # English — dialogs/skills/settings intact
    koen claude --lower haiku        # pick the cheap translator model
    KOEN_YOLO=1 koen claude          # skip all permission/approval prompts
                                     # (claude bypass-permissions, codex --yolo)

Harness rules:
    - responses are shown in Korean, cheaply: with claude the upper model
      answers in English (minimal output tokens) and a session-scoped Stop
      hook has the CHEAP model translate it, shown natively in the TUI;
      with codex the upper model is asked to reply in Korean directly.
      Disable with KOEN_REPLY=en
    - code fences, `inline code`, \"quoted\"/'quoted' text, and URLs in your
      prompt are never translated — restored verbatim
    - lines starting with / ! # are never translated (slash/bash commands)
    - arrow-key edits (left/right/home/end/backspace) are tracked, and a line
      recalled with up/down or otherwise reshaped is read back off the screen,
      so whatever is actually in the input box at Enter gets translated
    - translation failure -> the original line is submitted unchanged

Backends (auto-detected, or force with KOEN_BACKEND=claude|codex|openrouter):
    claude     -> claude -p --model $KOEN_CLAUDE_MODEL   (default: claude-haiku-4-5)
    codex      -> codex exec [-m $KOEN_CODEX_MODEL]      (default: codex config default)
    openrouter -> curl to /chat/completions, model $KOEN_OPENROUTER_MODEL
                  (default: meta-llama/llama-3.3-70b-instruct:free, needs OPENROUTER_API_KEY)
";

const INSTRUCTION: &str = "You are a translation filter inside an LLM prompt pipeline. \
Translate the following Korean prompt into concise, precise English.\n\
Rules:\n\
- Keep every ⟦K#⟧ placeholder exactly as written, in place.\n\
- Preserve technical terms, code identifiers, file paths, product names, \
numbers, and every constraint or requirement. Do not drop nuance.\n\
- Do NOT answer, execute, or comment on the prompt. Only translate it.\n\
- Output ONLY the translated prompt, nothing else.\n\nPROMPT:\n";

const INSTRUCTION_KO: &str = "You are a translation filter. Translate the \
following English text into natural, clear Korean.\n\
Rules:\n\
- Keep every ⟦K#⟧ placeholder exactly as written, in place.\n\
- Keep technical terms, code identifiers, file paths, commands, and error \
messages in their original form — do not translate them.\n\
- Do NOT answer, execute, or comment on the text. Only translate it.\n\
- Output ONLY the translation, nothing else.\n\nTEXT:\n";

/// Load persistent settings from ~/.koenrc (or $KOEN_CONFIG) so nothing has
/// to be exported per shell. Real environment variables win over the file.
/// Format: KEY=VALUE lines; `#` comments and an `export ` prefix are allowed,
/// so the file can be copy-pasted from a shell rc.
fn load_config() {
    let path = env::var("KOEN_CONFIG")
        .unwrap_or_else(|_| format!("{}/.koenrc", env::var("HOME").unwrap_or_default()));
    let Ok(s) = std::fs::read_to_string(&path) else { return };
    for line in s.lines() {
        let line = line.trim();
        let line = line.strip_prefix("export ").unwrap_or(line);
        if line.starts_with('#') {
            continue;
        }
        let Some((k, v)) = line.split_once('=') else { continue };
        let (k, mut v) = (k.trim(), v.trim());
        for q in ['"', '\''] {
            if v.len() >= 2 && v.starts_with(q) && v.ends_with(q) {
                v = &v[1..v.len() - 1];
            }
        }
        if (k.starts_with("KOEN_") || k.starts_with("OPENROUTER_")) && env::var(k).is_err() {
            env::set_var(k, v);
        }
    }
}

/// First `max` chars of a string, on a char boundary — byte slicing a UTF-8
/// string (e.g. for a log preview) panics mid-character otherwise.
fn clip(s: &str, max: usize) -> &str {
    match s.char_indices().nth(max) {
        Some((i, _)) => &s[..i],
        None => s,
    }
}

/// Append a line to $KOEN_DEBUG if set. Used to diagnose the harness live:
/// what the shadow captured, why a line was (not) swapped.
fn dbg_log(msg: &str) {
    if let Ok(f) = env::var("KOEN_DEBUG") {
        if let Ok(mut fh) = std::fs::OpenOptions::new().create(true).append(true).open(f) {
            let _ = writeln!(fh, "{}", msg);
        }
    }
}

fn has_hangul(s: &str) -> bool {
    s.chars().any(|c| ('\u{AC00}'..='\u{D7A3}').contains(&c))
}

fn placeholder(i: usize) -> String {
    format!("⟦K{}⟧", i)
}

/// Hide code fences, inline code, quoted text, and URLs behind placeholders
/// so the translator never touches them.
fn protect(text: &str) -> (String, Vec<String>) {
    let patterns = [
        r"(?s)```.*?```",   // fenced code blocks
        r"`[^`\n]+`",       // inline code
        r#""[^"\n]+""#,     // "double-quoted" text: keep verbatim
        r"'[^'\n]+'",       // 'single-quoted' text: keep verbatim
        r"https?://\S+",    // URLs
    ];
    let mut saved: Vec<String> = Vec::new();
    let mut out = text.to_string();
    for p in patterns {
        let rx = regex::Regex::new(p).unwrap();
        out = rx
            .replace_all(&out, |caps: &regex::Captures| {
                saved.push(caps[0].to_string());
                placeholder(saved.len() - 1)
            })
            .into_owned();
    }
    (out, saved)
}

fn restore(text: &str, saved: &[String]) -> Result<String, String> {
    // Single pass over the masked text: each ⟦K#⟧ is substituted once and the
    // inserted content is NOT rescanned, so a saved value that itself contains a
    // placeholder token can't be clobbered by a later substitution.
    let rx = regex::Regex::new(r"⟦K(\d+)⟧").unwrap();
    let mut seen = vec![false; saved.len()];
    let out = rx
        .replace_all(text, |caps: &regex::Captures| match caps[1]
            .parse::<usize>()
            .ok()
            .and_then(|i| saved.get(i).map(|s| (i, s)))
        {
            Some((i, s)) => {
                seen[i] = true;
                s.clone()
            }
            None => caps[0].to_string(), // unknown index: leave verbatim
        })
        .into_owned();
    if let Some(i) = seen.iter().position(|&s| !s) {
        return Err(format!("backend lost placeholder {}", placeholder(i)));
    }
    Ok(out)
}

fn which(prog: &str) -> bool {
    env::var("PATH")
        .unwrap_or_default()
        .split(':')
        .any(|d| std::fs::metadata(format!("{}/{}", d, prog)).is_ok())
}

fn pick_backend() -> Vec<String> {
    if let Ok(forced) = env::var("KOEN_BACKEND") {
        return vec![forced];
    }
    let mut order = Vec::new();
    if which("claude") {
        order.push("claude".into());
    }
    if which("codex") {
        order.push("codex".into());
    }
    if env::var("OPENROUTER_API_KEY").is_ok() {
        order.push("openrouter".into());
    }
    order
}

fn timeout_secs() -> u64 {
    env::var("KOEN_TIMEOUT").ok().and_then(|v| v.parse().ok()).unwrap_or(60)
}

/// Kills the child by pid if it outlives `secs`; cancelled on drop so a child
/// that finishes in time is never touched. Guards against a hung `claude -p` /
/// `codex exec` freezing the whole harness after Enter.
struct Watchdog(std::sync::Arc<std::sync::atomic::AtomicBool>);
impl Drop for Watchdog {
    fn drop(&mut self) {
        self.0.store(true, std::sync::atomic::Ordering::Relaxed);
    }
}
fn watchdog(pid: u32, secs: u64) -> Watchdog {
    let flag = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let f = flag.clone();
    thread::spawn(move || {
        for _ in 0..secs * 10 {
            thread::sleep(std::time::Duration::from_millis(100));
            if f.load(std::sync::atomic::Ordering::Relaxed) {
                return; // child already finished
            }
        }
        unsafe { libc::kill(pid as libc::pid_t, libc::SIGKILL) };
    });
    Watchdog(flag)
}

fn run_stdin(cmd: &mut Command, input: &str) -> Result<String, String> {
    let mut child = cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| e.to_string())?;
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input.as_bytes())
        .map_err(|e| e.to_string())?;
    let _guard = watchdog(child.id(), timeout_secs());
    let out = child.wait_with_output().map_err(|e| e.to_string())?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        return Err(format!("exit {}: {}", out.status, clip(&err, 300)));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn backend_claude(instruction: &str, prompt: &str) -> Result<String, String> {
    let model = env::var("KOEN_CLAUDE_MODEL").unwrap_or_else(|_| "claude-haiku-4-5".into());
    run_stdin(
        Command::new("claude").args(["-p", "--model", &model]),
        &format!("{}{}", instruction, prompt),
    )
}

fn backend_codex(instruction: &str, prompt: &str) -> Result<String, String> {
    let out_file = env::temp_dir().join(format!("koen-{}.txt", std::process::id()));
    let mut cmd = Command::new("codex");
    cmd.args(["exec", "--skip-git-repo-check", "-s", "read-only", "--output-last-message"])
        .arg(&out_file);
    if let Ok(m) = env::var("KOEN_CODEX_MODEL") {
        cmd.args(["-m", &m]);
    }
    cmd.arg(format!("{}{}", instruction, prompt));
    let mut child = cmd
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| e.to_string())?;
    let _guard = watchdog(child.id(), timeout_secs());
    let status = child.wait().map_err(|e| e.to_string())?;
    let text = std::fs::read_to_string(&out_file);
    let _ = std::fs::remove_file(&out_file); // clean up on failure paths too
    if !status.success() {
        return Err(format!("codex exit {}", status));
    }
    Ok(text.map_err(|e| e.to_string())?.trim().to_string())
}

fn backend_openrouter(instruction: &str, prompt: &str) -> Result<String, String> {
    let key = env::var("OPENROUTER_API_KEY").map_err(|_| "OPENROUTER_API_KEY not set")?;
    let model = env::var("KOEN_OPENROUTER_MODEL")
        .unwrap_or_else(|_| "meta-llama/llama-3.3-70b-instruct:free".into());
    let body = serde_json::json!({
        "model": model,
        "messages": [{"role": "user", "content": format!("{}{}", instruction, prompt)}],
    });
    let out = run_stdin(
        Command::new("curl").args([
            "-s",
            "--max-time",
            "120",
            "https://openrouter.ai/api/v1/chat/completions",
            "-H",
            &format!("Authorization: Bearer {}", key),
            "-H",
            "Content-Type: application/json",
            "-d",
            "@-",
        ]),
        &body.to_string(),
    )?;
    let v: serde_json::Value = serde_json::from_str(&out).map_err(|e| e.to_string())?;
    v["choices"][0]["message"]["content"]
        .as_str()
        .map(|s| s.trim().to_string())
        .ok_or_else(|| format!("unexpected response: {}", clip(&out, 200)))
}

fn hangul_ratio(s: &str) -> f32 {
    let (mut ko, mut alpha) = (0f32, 0f32);
    for c in s.chars() {
        if ('\u{AC00}'..='\u{D7A3}').contains(&c) {
            ko += 1.0;
        } else if c.is_ascii_alphabetic() {
            alpha += 1.0;
        }
    }
    if ko + alpha == 0.0 { 0.0 } else { ko / (ko + alpha) }
}

/// Translate Korean→English (`to_ko=false`) or English→Korean (`to_ko=true`).
/// Any failure passes the original through — meaning is never silently lost.
fn translate_dir(text: &str, to_ko: bool) -> String {
    if !to_ko && !has_hangul(text) {
        return text.to_string(); // already English: passthrough, zero cost
    }
    if to_ko && hangul_ratio(text) > 0.5 {
        return text.to_string(); // already (mostly) Korean
    }
    if let Ok(fake) = env::var("KOEN_FAKE_TRANSLATION") {
        return fake; // test hook: deterministic, offline
    }
    let instruction = if to_ko { INSTRUCTION_KO } else { INSTRUCTION };
    let (masked, saved) = protect(text);
    let order = pick_backend();
    if order.is_empty() {
        eprintln!("koen: no backend available (need claude, codex, or OPENROUTER_API_KEY)");
        return text.to_string();
    }
    for name in &order {
        let result = match name.as_str() {
            "claude" => backend_claude(instruction, &masked),
            "codex" => backend_codex(instruction, &masked),
            "openrouter" => backend_openrouter(instruction, &masked),
            other => Err(format!("unknown backend {}", other)),
        };
        let ok_direction = |out: &str| if to_ko { has_hangul(out) } else { !has_hangul(out) };
        match result {
            Ok(out) if !out.is_empty() && ok_direction(&out) => match restore(&out, &saved) {
                Ok(r) => return r,
                Err(e) => eprintln!("koen: backend {}: {}", name, e),
            },
            Ok(_) => eprintln!("koen: backend {}: empty or wrong-language output", name),
            Err(e) => eprintln!("koen: backend {}: {}", name, e),
        }
    }
    eprintln!("koen: all backends failed, passing original through");
    text.to_string()
}

fn translate(text: &str) -> String {
    translate_dir(text, false)
}

/// Extract the final assistant text from a Claude Code transcript (JSONL).
fn last_assistant_text(transcript: &str) -> String {
    let mut last = String::new();
    for line in transcript.lines() {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else { continue };
        if v["type"] != "assistant" {
            continue;
        }
        let mut text = String::new();
        if let Some(blocks) = v["message"]["content"].as_array() {
            for b in blocks {
                if b["type"] == "text" {
                    if let Some(t) = b["text"].as_str() {
                        if !text.is_empty() {
                            text.push('\n');
                        }
                        text.push_str(t);
                    }
                }
            }
        }
        if !text.trim().is_empty() {
            last = text;
        }
    }
    last
}

/// Claude Code UserPromptSubmit hook: show the Korean line the user actually
/// typed (stashed by the harness right before the English swap) as a native
/// systemMessage under the submitted prompt. Display-only — zero tokens.
fn prompt_hook() {
    let Ok(path) = env::var("KOEN_ORIG_FILE") else { return };
    let Ok(orig) = std::fs::read_to_string(&path) else { return };
    if orig.trim().is_empty() {
        return;
    }
    let _ = std::fs::write(&path, ""); // consume: only show once
    println!(
        "{}",
        serde_json::json!({ "systemMessage": format!("원문: {}", orig.trim()) })
    );
}

/// Claude Code Stop hook: translate the English response to Korean with the
/// cheap model and hand it back as a systemMessage, which the TUI renders
/// natively under the response. This keeps the expensive model's output in
/// cheap English tokens while the user reads Korean.
fn stop_hook() {
    let dbg = |m: String| dbg_log(&m);
    let mut input = String::new();
    if std::io::stdin().read_to_string(&mut input).is_err() {
        return;
    }
    dbg(format!("input: {}", clip(&input, 400)));
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&input) else { return };
    // the hook input carries the response directly; the transcript file is a
    // fallback (interactive sessions may not have flushed it to disk yet)
    let mut text = v["last_assistant_message"].as_str().unwrap_or("").to_string();
    if text.trim().is_empty() {
        if let Some(path) = v["transcript_path"].as_str() {
            if let Ok(transcript) = std::fs::read_to_string(path) {
                text = last_assistant_text(&transcript);
            } else {
                dbg(format!("cannot read {}", path));
            }
        }
    }
    if text.trim().is_empty() {
        dbg("empty assistant text".into());
        return;
    }
    let ko = translate_dir(&text, true);
    if ko == text {
        dbg(format!("no-op translation for: {}", clip(&text, 200)));
        return; // translation failed or already Korean: show nothing extra
    }
    dbg(format!("ok: {}", clip(&ko, 200)));
    println!("{}", serde_json::json!({ "systemMessage": ko }));
}

// ---------------------------------------------------------------------------
// Harness mode: run the REAL claude/codex TUI inside a pty, pass its screen
// through untouched (permission dialogs, /model, skills, settings all work),
// and only intercept the input stream: when Enter is pressed on a line that
// contains Hangul, translate it with the cheap model, erase the Hangul from
// the TUI's input box with backspaces, and submit the English instead.
// Claude Code hooks were verified unable to replace prompts, so pty
// interception is the only way to keep the native UX.
// ---------------------------------------------------------------------------

static MASTER_FD: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(-1);

// ---------------------------------------------------------------------------
// Minimal terminal emulator. It consumes claude's OUTPUT stream to keep a
// screen grid + cursor, so at Enter time koen can read the *real* input-box
// text even when the keystroke shadow cannot know it — history recall (up/down),
// tab-complete, autocomplete. It is only a fallback for "dirty" lines; ordinary
// typing/editing still uses the exact shadow, so a bug here can't hurt the
// common path.
//
// UI-change resistance: the input text is recovered by stripping the longest
// common prefix between the current input row and a baseline snapshot of the
// empty prompt row. Whatever glyphs claude uses for the prompt/border are
// identical in both, so they cancel out — nothing about claude's look is
// hardcoded.
// ---------------------------------------------------------------------------

/// Display columns a char occupies (Korean/CJK/emoji are wide). No stdlib for
/// this; the ranges below cover what shows up in a Korean prompt.
fn char_width(ch: char) -> usize {
    let u = ch as u32;
    let wide = (0x1100..=0x115F).contains(&u)   // Hangul Jamo
        || (0x2E80..=0xA4CF).contains(&u)       // CJK, Kangxi, kana, ...
        || (0xAC00..=0xD7A3).contains(&u)       // Hangul syllables
        || (0xF900..=0xFAFF).contains(&u)       // CJK compat
        || (0xFE30..=0xFE4F).contains(&u)       // CJK compat forms
        || (0xFF00..=0xFF60).contains(&u)       // fullwidth forms
        || (0xFFE0..=0xFFE6).contains(&u)
        || (0x1F300..=0x1FAFF).contains(&u)     // emoji (approx)
        || (0x20000..=0x3FFFD).contains(&u);    // CJK ext B+
    if wide { 2 } else { 1 }
}

const CONT: char = '\u{1}'; // right half of a wide char, skipped when reading

static SCREEN: std::sync::Mutex<Option<Screen>> = std::sync::Mutex::new(None);

struct Screen {
    rows: usize,
    cols: usize,
    grid: Vec<Vec<char>>, // rows x cols
    r: usize,             // cursor row / col
    c: usize,
    sr: usize,            // saved cursor
    sc: usize,
    pend: Vec<u8>,        // partial utf-8
    st: u8,               // 0 normal, 1 after ESC, 2 CSI, 3 OSC, 4 ESC-intermediate
    csi: Vec<u8>,         // CSI params being collected
}

impl Screen {
    fn new(rows: usize, cols: usize) -> Screen {
        // fall back to a standard size when the tty size is unknown (0)
        let rows = if rows == 0 { 24 } else { rows };
        let cols = if cols == 0 { 80 } else { cols };
        Screen {
            rows, cols,
            grid: vec![vec![' '; cols]; rows],
            r: 0, c: 0, sr: 0, sc: 0,
            pend: Vec::new(), st: 0, csi: Vec::new(),
        }
    }

    fn resize(&mut self, rows: usize, cols: usize) {
        self.rows = if rows == 0 { 24 } else { rows };
        self.cols = if cols == 0 { 80 } else { cols };
        self.grid = vec![vec![' '; self.cols]; self.rows];
        self.r = self.r.min(self.rows - 1);
        self.c = self.c.min(self.cols - 1);
    }

    fn newline(&mut self) {
        if self.r + 1 >= self.rows {
            self.grid.remove(0);
            self.grid.push(vec![' '; self.cols]);
        } else {
            self.r += 1;
        }
    }

    fn put(&mut self, ch: char) {
        let w = char_width(ch);
        if self.c + w > self.cols {
            self.c = 0;
            self.newline();
        }
        if self.r < self.rows && self.c < self.cols {
            self.grid[self.r][self.c] = ch;
            if w == 2 && self.c + 1 < self.cols {
                self.grid[self.r][self.c + 1] = CONT;
            }
        }
        self.c += w;
        if self.c > self.cols {
            self.c = self.cols;
        }
    }

    /// numeric CSI params (default 0 for empty), and whether it's a `?` private
    fn params(&self) -> (Vec<usize>, bool) {
        let body = &self.csi;
        let priv_ = body.first() == Some(&b'?');
        let body = if priv_ { &body[1..] } else { &body[..] };
        let s = String::from_utf8_lossy(body);
        let out: Vec<usize> = s
            .split(';')
            .map(|p| p.parse::<usize>().unwrap_or(0))
            .collect();
        (out, priv_)
    }

    fn erase_row(&mut self, row: usize, from: usize, to: usize) {
        for x in from..to.min(self.cols) {
            self.grid[row][x] = ' ';
        }
    }

    fn csi_dispatch(&mut self, final_b: u8) {
        let (p, priv_) = self.params();
        let n = |i: usize| *p.get(i).filter(|&&v| v > 0).unwrap_or(&1);
        match final_b {
            b'A' => self.r = self.r.saturating_sub(n(0)),
            b'B' => self.r = (self.r + n(0)).min(self.rows - 1),
            b'C' => self.c = (self.c + n(0)).min(self.cols - 1),
            b'D' => self.c = self.c.saturating_sub(n(0)),
            b'E' => { self.r = (self.r + n(0)).min(self.rows - 1); self.c = 0; }
            b'F' => { self.r = self.r.saturating_sub(n(0)); self.c = 0; }
            b'G' => self.c = (n(0) - 1).min(self.cols - 1),
            b'd' => self.r = (n(0) - 1).min(self.rows - 1),
            b'H' | b'f' => {
                self.r = (n(0) - 1).min(self.rows - 1);
                self.c = (n(1) - 1).min(self.cols - 1);
            }
            b'J' => {
                let mode = *p.first().unwrap_or(&0);
                let (r, c) = (self.r, self.c);
                if mode == 0 {
                    self.erase_row(r, c, self.cols);
                    for row in r + 1..self.rows { self.erase_row(row, 0, self.cols); }
                } else if mode == 1 {
                    for row in 0..r { self.erase_row(row, 0, self.cols); }
                    self.erase_row(r, 0, c + 1);
                } else {
                    for row in 0..self.rows { self.erase_row(row, 0, self.cols); }
                }
            }
            b'K' => {
                let mode = *p.first().unwrap_or(&0);
                let (r, c) = (self.r, self.c);
                match mode {
                    0 => self.erase_row(r, c, self.cols),
                    1 => self.erase_row(r, 0, c + 1),
                    _ => self.erase_row(r, 0, self.cols),
                }
            }
            b'P' => { // delete chars: shift left, blank-fill at end
                let k = n(0).min(self.cols - self.c);
                let (r, c) = (self.r, self.c);
                for x in c..self.cols {
                    self.grid[r][x] = if x + k < self.cols { self.grid[r][x + k] } else { ' ' };
                }
            }
            b'@' => { // insert blanks
                let k = n(0).min(self.cols - self.c);
                let (r, c) = (self.r, self.c);
                for x in (c..self.cols).rev() {
                    self.grid[r][x] = if x >= c + k { self.grid[r][x - k] } else { ' ' };
                }
            }
            b'X' => { let (r, c) = (self.r, self.c); self.erase_row(r, c, c + n(0)); }
            b's' => { self.sr = self.r; self.sc = self.c; }
            b'u' => { self.r = self.sr; self.c = self.sc; }
            b'h' | b'l' if priv_ => {
                // alt-screen switch (1049/1047/47): clear so stale text doesn't
                // bleed into the input read
                if p.contains(&1049) || p.contains(&1047) || p.contains(&47) {
                    for row in 0..self.rows { self.erase_row(row, 0, self.cols); }
                    self.r = 0; self.c = 0;
                }
            }
            _ => {} // SGR (m), scroll region (r), modes, etc.: no effect on layout
        }
    }

    fn feed(&mut self, bytes: &[u8]) {
        for &b in bytes {
            match self.st {
                1 => { // after ESC
                    self.st = 0;
                    match b {
                        b'[' => { self.st = 2; self.csi.clear(); }
                        b']' => self.st = 3,
                        b'7' => { self.sr = self.r; self.sc = self.c; }
                        b'8' => { self.r = self.sr; self.c = self.sc; }
                        b'M' => { if self.r == 0 { self.grid.insert(0, vec![' '; self.cols]); self.grid.pop(); } else { self.r -= 1; } }
                        b'(' | b')' | b'#' | b'%' => self.st = 4, // consume one more byte
                        _ => {}
                    }
                }
                2 => { // CSI: collect until final byte 0x40..=0x7e
                    if (0x40..=0x7e).contains(&b) {
                        self.csi_dispatch(b);
                        self.st = 0;
                    } else {
                        self.csi.push(b);
                    }
                }
                3 => { // OSC: skip until BEL or ESC \
                    if b == 0x07 { self.st = 0; }
                    else if b == 0x1b { self.st = 1; } // ESC \ terminator (approx)
                }
                4 => self.st = 0, // charset/intermediate: swallowed one byte
                _ => match b {
                    0x1b => self.st = 1,
                    b'\r' => self.c = 0,
                    b'\n' => self.newline(),
                    0x08 => self.c = self.c.saturating_sub(1),
                    0x09 => self.c = ((self.c / 8) * 8 + 8).min(self.cols - 1),
                    0x07 => {}
                    _ if b < 0x20 => {}
                    _ => {
                        self.pend.push(b);
                        match std::str::from_utf8(&self.pend) {
                            Ok(s) => { let ch = s.chars().next().unwrap(); self.pend.clear(); self.put(ch); }
                            Err(e) if e.error_len().is_some() || self.pend.len() > 4 => self.pend.clear(),
                            Err(_) => {} // wait for more bytes of this char
                        }
                    }
                },
            }
        }
    }

    fn row_chars(&self, row: usize) -> &[char] {
        &self.grid[row]
    }

    /// Read the current input line by cancelling out the empty-prompt baseline.
    /// Returns (input text, cursor char-offset from the input start).
    fn read_input(&self, baseline: &[char]) -> Option<(String, usize)> {
        let row = &self.grid[self.r];
        let mut start = 0;
        while start < row.len() && start < baseline.len() && row[start] == baseline[start] {
            start += 1;
        }
        let text: String = row[start..].iter().filter(|&&ch| ch != CONT).collect();
        let text = text.trim_end().trim_end_matches(|ch| "│┃▏▕|╮╯".contains(ch)).trim_end().to_string();
        if text.is_empty() {
            return None;
        }
        let mut off = 0;
        for x in start..self.c.min(row.len()) {
            if row[x] != CONT {
                off += 1;
            }
        }
        // the cursor may sit past the trimmed text (in trailing space); clamp so
        // callers can compute (len - off) without underflowing
        let n = text.chars().count();
        Some((text, off.min(n)))
    }
}

/// Feed claude's output into the shared screen model.
fn screen_feed(bytes: &[u8]) {
    if let Ok(mut g) = SCREEN.lock() {
        if let Some(s) = g.as_mut() {
            s.feed(bytes);
        }
    }
}

/// Snapshot the cursor row (used as the empty-prompt baseline).
fn screen_cursor_row() -> Vec<char> {
    SCREEN.lock().ok().and_then(|g| g.as_ref().map(|s| s.row_chars(s.r).to_vec())).unwrap_or_default()
}

/// Read the real input line off the screen (fallback for dirty shadow lines).
fn screen_read_input(baseline: &[char]) -> Option<(String, usize)> {
    SCREEN.lock().ok().and_then(|g| g.as_ref().and_then(|s| s.read_input(baseline)))
}

/// True when the screen's input row is back to the empty prompt (baseline).
fn screen_input_empty(baseline: &[char]) -> bool {
    screen_read_input(baseline).is_none()
}

/// Kill the whole input line (Ctrl-U) and confirm via the screen that it
/// emptied. Chip- and wrap-agnostic. Returns false — leaving the box untouched
/// — when there's no reliable baseline or the box refused to clear, so the
/// caller submits the line as-is rather than mangling it.
fn clear_via_kill(baseline: &[char], master: libc::c_int) -> bool {
    if !baseline.iter().any(|&c| c != ' ' && c != CONT) {
        return false;
    }
    for _ in 0..3 {
        wr_master(master, &[0x15]); // Ctrl-U
        for _ in 0..4 {
            pump(master, 15); // let claude redraw before checking
        }
        if screen_input_empty(baseline) {
            return true;
        }
    }
    false
}

static WINCH: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Restores the terminal to `old` (cooked) mode on drop — the safety net if the
/// harness unwinds on a panic instead of reaching its explicit restore.
struct TermiosGuard(libc::termios);
impl Drop for TermiosGuard {
    fn drop(&mut self) {
        unsafe { libc::tcsetattr(0, libc::TCSADRAIN, &self.0) };
    }
}

extern "C" fn on_winch(_: libc::c_int) {
    let master = MASTER_FD.load(std::sync::atomic::Ordering::Relaxed);
    if master >= 0 {
        unsafe {
            let mut ws: libc::winsize = mem::zeroed();
            if libc::ioctl(0, libc::TIOCGWINSZ, &mut ws) == 0 {
                libc::ioctl(master, libc::TIOCSWINSZ, &ws);
            }
        }
    }
    // resize the screen model off the signal path (locking here isn't safe)
    WINCH.store(true, std::sync::atomic::Ordering::Relaxed);
}

fn wr(fd: libc::c_int, mut buf: &[u8]) {
    while !buf.is_empty() {
        let n = unsafe { libc::write(fd, buf.as_ptr() as *const _, buf.len()) };
        if n <= 0 {
            return;
        }
        buf = &buf[n as usize..];
    }
}

fn is_eagain() -> bool {
    // EAGAIN and EWOULDBLOCK are the same value on macOS/Linux
    std::io::Error::last_os_error().raw_os_error() == Some(libc::EAGAIN)
}

/// Write to the (non-blocking) pty master, draining the child's output whenever
/// the write would block. Forwarding a large burst (e.g. a big paste) can fill
/// the master's input buffer; a plain blocking write would then wedge while the
/// child is itself blocked writing output we haven't read — a pty deadlock.
/// Pumping between partial writes keeps both directions moving.
fn wr_master(master: libc::c_int, mut buf: &[u8]) {
    while !buf.is_empty() {
        let n = unsafe { libc::write(master, buf.as_ptr() as *const _, buf.len()) };
        if n > 0 {
            buf = &buf[n as usize..];
        } else if n < 0 && is_eagain() {
            if !pump(master, 5) {
                return; // child gone
            }
        } else {
            return; // real write error (EPIPE, etc.)
        }
    }
}

/// poll a single fd for readability; returns true if readable
fn readable(fd: libc::c_int, timeout_ms: libc::c_int) -> bool {
    let mut p = libc::pollfd { fd, events: libc::POLLIN, revents: 0 };
    // include POLLERR/POLLNVAL: a broken master must count as "go read" so the
    // read returns EOF and the loop exits, instead of spinning forever.
    let mask = libc::POLLIN | libc::POLLHUP | libc::POLLERR | libc::POLLNVAL;
    unsafe { libc::poll(&mut p, 1, timeout_ms) > 0 && p.revents & mask != 0 }
}

/// Forward pending child output to our stdout. False on child EOF.
fn pump(master: libc::c_int, timeout_ms: libc::c_int) -> bool {
    if readable(master, timeout_ms) {
        let mut buf = [0u8; 65536];
        let n = unsafe { libc::read(master, buf.as_mut_ptr() as *mut _, buf.len()) };
        if n < 0 {
            return !is_eagain(); // EAGAIN on the non-blocking master: no data, not EOF
        }
        if n == 0 {
            return false; // real EOF
        }
        screen_feed(&buf[..n as usize]); // keep the screen model in sync
        wr(1, &buf[..n as usize]);
    }
    true
}

/// Translate in a thread so the TUI keeps rendering; hold typed keys.
fn translate_while_pumping(text: &str, master: libc::c_int) -> (String, Vec<u8>) {
    let (tx, rx) = mpsc::channel();
    let owned = text.to_string();
    thread::spawn(move || {
        let _ = tx.send(translate(&owned));
    });
    let mut held = Vec::new();
    loop {
        match rx.try_recv() {
            Ok(v) => return (v, held),
            Err(mpsc::TryRecvError::Disconnected) => return (text.to_string(), held),
            Err(mpsc::TryRecvError::Empty) => {}
        }
        if !pump(master, 50) {
            return (text.to_string(), held); // child died; outer loop will notice
        }
        if readable(0, 0) {
            let mut buf = [0u8; 65536];
            let n = unsafe { libc::read(0, buf.as_mut_ptr() as *mut _, buf.len()) };
            if n > 0 {
                held.extend_from_slice(&buf[..n as usize]);
                // raw mode swallows Ctrl-C into `held`; treat it as "abort the
                // translation" so a slow/stuck cheap model can't wedge the TUI
                if held.contains(&0x03) {
                    return (text.to_string(), held);
                }
            }
        }
    }
}

/// Appended per translated turn for codex, which has no hook channel.
const REPLY_KO_SUFFIX: &str = " Reply in Korean; keep code, paths, and commands as-is.";

struct Shadow {
    buf: Vec<char>,  // shadow of the TUI's current input line
    cursor: usize,   // insert position within buf (chars from start)
    pend: Vec<u8>,   // bytes of a split utf-8 char
    dirty: bool,     // up/down/tab-complete: shadow unreliable, read screen
    paste: bool,     // inside bracketed paste
    paste_seen: bool, // this line included a bracketed paste (claude may collapse it to a [Pasted text] chip)
    baseline: Vec<char>, // empty-prompt row, snapshot while the line is empty
    suffix: &'static str, // appended after a swapped (translated) line
}

fn starts_cmd(s: &str) -> bool {
    let h = s.trim_start();
    h.starts_with('/') || h.starts_with('!') || h.starts_with('#')
}

fn on_enter(st: &mut Shadow, master: libc::c_int) -> Vec<u8> {
    let cursor = st.cursor;
    let buf = mem::take(&mut st.buf);
    st.cursor = 0;
    st.pend.clear();
    let was_dirty = mem::replace(&mut st.dirty, false);
    let paste_seen = mem::replace(&mut st.paste_seen, false);

    // Instrument the paste case: the shadow holds the full pasted text, but
    // claude may show a collapsed [Pasted text] chip whose char count differs
    // from ours — log both so we can see how the box actually renders before
    // deciding how to erase/skip it.
    if paste_seen && env::var("KOEN_DEBUG").is_ok() {
        let shadow: String = buf.iter().collect();
        let screen_row = screen_cursor_row().iter().filter(|&&c| c != CONT).collect::<String>();
        dbg_log(&format!(
            "PASTE: shadow_chars={} shadow={:?} | screen_row={:?}",
            buf.len(), clip(&shadow, 300), screen_row.trim_end()
        ));
    }

    // The true input line + cursor char-offset. When the shadow is reliable
    // (ordinary typing/editing) it is exact; when it went dirty (up/down recall,
    // tab-complete) fall back to reading the line straight off the screen.
    let (text, cur_off): (String, usize) = if !was_dirty {
        (buf.iter().collect(), cursor)
    } else {
        // let claude's redraw (up/down recall, autocomplete) land in the screen
        // model first — Enter can arrive in the same read burst as the up-arrow
        for _ in 0..4 {
            pump(master, 15);
        }
        if env::var("KOEN_DEBUG").is_ok() {
            let cur: String = SCREEN.lock().ok()
                .and_then(|g| g.as_ref().map(|s| { let r = s.r; s.row_chars(r).iter().filter(|&&c| c != CONT).collect::<String>() }))
                .unwrap_or_default();
            dbg_log(&format!("on_enter: baseline={:?} cur_row={:?}",
                st.baseline.iter().filter(|&&c| c != CONT).collect::<String>(), cur));
        }
        match screen_read_input(&st.baseline) {
            Some(v) => v,
            None => (String::new(), 0),
        }
    };
    let total = text.chars().count();
    let translatable = !text.is_empty() && has_hangul(&text) && !starts_cmd(&text);
    dbg_log(&format!(
        "on_enter: dirty={} src={} hangul={} translatable={} chars={} line={:?}",
        was_dirty, if was_dirty { "screen" } else { "shadow" },
        has_hangul(&text), translatable, total, clip(&text, 200)
    ));
    if !translatable {
        wr_master(master, b"\r");
        return Vec::new();
    }
    let (eng, held) = translate_while_pumping(&text, master);
    // Ctrl-C during translation: don't swap, don't submit — hand the keys back
    // (the 0x03 reaches claude and clears its box). A clean abort.
    if held.contains(&0x03) {
        dbg_log("on_enter: aborted by Ctrl-C during translation");
        return held;
    }
    dbg_log(&format!(
        "on_enter: translated -> swapped={} out={:?}",
        eng != text && !has_hangul(&eng), clip(&eng, 200)
    ));
    if eng != text && !has_hangul(&eng) {
        // Erase what's in the box, then type the English. A pasted line may be
        // collapsed by claude into a [Pasted text] chip whose char count is not
        // ours, so backspacing our count would be wrong — kill the line and
        // verify it actually emptied; only swap when it did. Verified on claude
        // 2.1.201: Ctrl-U clears both typed text and the [Pasted text] chip.
        let cleared = if paste_seen || was_dirty {
            // Screen-derived text (paste chip, or an up/down recall that may wrap
            // across rows): our char count / cursor can't be trusted against the
            // box, so kill the whole line (Ctrl-U) and verify it emptied. Needs a
            // reliable empty-prompt baseline; without one, don't risk submitting
            // an empty prompt — leave the line untouched.
            clear_via_kill(&st.baseline, master)
        } else {
            // exact shadow: move a possibly mid-line cursor to the end, then one
            // backspace per char clears it. ponytail: if a wide-char mismatch
            // ever bites, count graphemes instead of chars.
            wr_master(master, "\x1b[C".repeat(total.saturating_sub(cur_off)).as_bytes());
            wr_master(master, &vec![0x7f; total]);
            true
        };
        if cleared {
            if let Ok(p) = env::var("KOEN_ORIG_FILE") {
                let _ = std::fs::write(p, &text); // shown by the UserPromptSubmit hook
            }
            wr_master(master, eng.as_bytes());
            wr_master(master, st.suffix.as_bytes());
            // the TUI treats a rapid burst as a paste, and a \r inside a paste
            // inserts a newline instead of submitting — pause so the Enter
            // registers as its own keypress
            for _ in 0..6 {
                pump(master, 50);
            }
        } else {
            // couldn't clear the chip — leave the paste untouched and submit it
            // as-is rather than corrupt it (claude expands it to the full text)
            dbg_log("on_enter: paste box not cleared; submitting paste untranslated");
        }
    }
    wr_master(master, b"\r");
    held
}

fn feed_shadow(st: &mut Shadow, chunk: &[u8]) {
    // A control char normally means tab-complete / a cursor key → shadow
    // untrustworthy. But inside a bracketed paste every byte is literal content
    // (a tab in pasted code, etc.), so it must NOT mark the shadow dirty.
    if !st.paste && chunk.iter().any(|&c| c < 0x20) {
        st.dirty = true;
    }
    st.pend.extend(chunk.iter().filter(|&&c| c >= 0x20));
    let decoded = match std::str::from_utf8(&st.pend) {
        Ok(s) => {
            let s = s.to_string();
            st.pend.clear();
            s
        }
        Err(e) => {
            let valid = e.valid_up_to();
            let s = std::str::from_utf8(&st.pend[..valid]).unwrap().to_string();
            st.pend.drain(..valid);
            if e.error_len().is_some() || st.pend.len() > 8 {
                st.pend.clear(); // garbage, not a split utf-8 char
                if !st.paste {
                    st.dirty = true;
                }
            }
            s
        }
    };
    for ch in decoded.chars() {
        st.buf.insert(st.cursor, ch); // insert at cursor, not always the end
        st.cursor += 1;
    }
}

const SPECIAL: [u8; 6] = [0x1b, 0x0d, 0x0a, 0x7f, 0x03, 0x15];

fn process_input(st: &mut Shadow, input: &[u8], master: libc::c_int) {
    let mut q: Vec<u8> = input.to_vec();
    let mut i = 0;
    while i < q.len() {
        let b = q[i];
        if b == 0x1b {
            // escape sequence: consume to its final byte
            let mut j = i + 1;
            if q.get(j) == Some(&b'[') {
                j += 1;
                while j < q.len() && !(0x40..=0x7e).contains(&q[j]) {
                    j += 1;
                }
            }
            let end = j.min(q.len() - 1);
            let seq = &q[i..=end];
            match seq {
                b"\x1b[200~" => { st.paste = true; st.paste_seen = true; }
                b"\x1b[201~" => st.paste = false,
                // Cursor moves / edits within the line: track them so a prompt
                // fixed up with arrow keys still gets translated. Left/Right,
                // Home/End, and Delete keep the shadow in sync with the box.
                b"\x1b[C" => st.cursor = (st.cursor + 1).min(st.buf.len()), // Right
                b"\x1b[D" => st.cursor = st.cursor.saturating_sub(1),       // Left
                b"\x1b[H" | b"\x1b[1~" => st.cursor = 0,                    // Home
                b"\x1b[F" | b"\x1b[4~" => st.cursor = st.buf.len(),         // End
                b"\x1b[3~" => {                                             // Delete
                    if st.cursor < st.buf.len() {
                        st.buf.remove(st.cursor);
                    }
                }
                // option/shift+Enter soft newline: a real newline in the
                // prompt, not a cursor move.
                // ponytail: covers ESC-CR/ESC-LF (macOS Meta+Return). Terminals
                // that send \x1b[13;2u etc. fall through to dirty — safe, just
                // submits that prompt untranslated.
                b"\x1b\r" | b"\x1b\n" => {
                    st.buf.insert(st.cursor, '\n');
                    st.cursor += 1;
                }
                // up/down (history or wrapped-line moves), fn keys, unknown:
                // the shadow can no longer be trusted, so skip translation
                _ => st.dirty = true,
            }
            wr_master(master, seq);
            i = end + 1;
        } else if (b == 0x0d || b == 0x0a) && !st.paste {
            let held = on_enter(st, master);
            if !held.is_empty() {
                let rest = q.split_off(i + 1);
                q.extend_from_slice(&held);
                q.extend_from_slice(&rest);
            }
            i += 1;
        } else if (b == 0x0d || b == 0x0a) && st.paste {
            // newline inside pasted text: part of the input
            st.buf.insert(st.cursor, '\n');
            st.cursor += 1;
            wr_master(master, &q[i..=i]);
            i += 1;
        } else if b == 0x7f {
            if st.cursor > 0 {
                st.buf.remove(st.cursor - 1); // delete the char before the cursor
                st.cursor -= 1;
            }
            wr_master(master, &q[i..=i]);
            i += 1;
        } else if b == 0x03 || b == 0x15 {
            // ctrl-c / ctrl-u clear the input line
            st.buf.clear();
            st.cursor = 0;
            st.pend.clear();
            st.dirty = false;
            st.paste_seen = false;
            wr_master(master, &q[i..=i]);
            i += 1;
        } else {
            let mut j = i + 1;
            while j < q.len() && !SPECIAL.contains(&q[j]) {
                j += 1;
            }
            wr_master(master, &q[i..j]);
            let chunk: Vec<u8> = q[i..j].to_vec();
            feed_shadow(st, &chunk);
            i = j;
        }
    }
}

/// Remove koen temp files (koen-orig-<pid>.txt / koen-<pid>.txt) orphaned by a
/// previously killed session — best effort, only when the owning pid is dead so
/// a live session's file (even under a reused pid) is never touched.
fn sweep_temp_files() {
    let Ok(entries) = std::fs::read_dir(env::temp_dir()) else { return };
    for e in entries.flatten() {
        let name = e.file_name();
        let Some(name) = name.to_str() else { continue };
        let rest = name
            .strip_prefix("koen-orig-")
            .or_else(|| name.strip_prefix("koen-"));
        let Some(pid) = rest.and_then(|r| r.strip_suffix(".txt")).and_then(|r| r.parse::<i32>().ok())
        else { continue };
        let dead = unsafe { libc::kill(pid, 0) } != 0
            && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH);
        if dead {
            let _ = std::fs::remove_file(e.path());
        }
    }
}

fn harness(target: &str, extra: &[String]) -> ! {
    sweep_temp_files(); // clear leaks from previously killed sessions
    let mut args: Vec<String> = Vec::new();
    let mut it = extra.iter();
    while let Some(a) = it.next() {
        if a == "--lower" {
            // --lower <model>: pick the cheap translator model
            if let Some(m) = it.next() {
                env::set_var("KOEN_CLAUDE_MODEL", m);
                env::set_var("KOEN_CODEX_MODEL", m);
            }
        } else if a != "--" {
            args.push(a.clone());
        }
    }
    // Korean display without expensive-model Korean tokens (KOEN_REPLY=en
    // disables the reply translation):
    // - claude: session-scoped hooks are injected via --settings.
    //   UserPromptSubmit echoes the Korean line the user typed (stashed in
    //   KOEN_ORIG_FILE before the English swap); Stop translates the English
    //   response with the cheap model. Both render natively as systemMessage.
    // - codex: no hook channel, so the upper model is asked to reply in
    //   Korean directly via a per-turn suffix.
    // KOEN_YOLO=1 skips all permission/approval prompts in the upper TUI
    // (claude bypass-permissions, codex --yolo). Off by default; opt in via
    // ~/.koenrc so a fresh install never auto-runs without asking. This
    // disables every safety prompt, so it fails CLOSED: only explicit truthy
    // values enable it — KOEN_YOLO=false/off/no stays safe, not accidentally on.
    let yolo = env::var("KOEN_YOLO")
        .map(|v| matches!(v.trim().to_ascii_lowercase().as_str(), "1" | "true" | "yes" | "on"))
        .unwrap_or(false);
    if yolo {
        args.push(if target == "claude" {
            "--dangerously-skip-permissions".into()
        } else {
            "--yolo".into()
        });
    }
    let reply_ko = env::var("KOEN_REPLY").map(|v| v != "en").unwrap_or(true);
    let mut suffix = "";
    let mut own_orig_file = None; // created by us -> removed on exit
    if target == "claude" {
        if env::var("KOEN_ORIG_FILE").is_err() {
            let p = env::temp_dir().join(format!("koen-orig-{}.txt", std::process::id()));
            env::set_var("KOEN_ORIG_FILE", &p); // inherited by claude -> hooks
            own_orig_file = Some(p);
        }
        let exe = env::current_exe()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|_| "koen".into());
        let mut hooks = serde_json::json!({
            "UserPromptSubmit": [{ "hooks": [{
                "type": "command",
                "command": format!("'{}' --prompt-hook", exe)
            }]}]
        });
        if reply_ko {
            hooks["Stop"] = serde_json::json!([{ "hooks": [{
                "type": "command",
                "command": format!("'{}' --stop-hook", exe),
                "timeout": 120
            }]}]);
        }
        args.push("--settings".into());
        args.push(serde_json::json!({ "hooks": hooks }).to_string());
    } else if reply_ko {
        suffix = REPLY_KO_SUFFIX;
    }
    let mut cmd: Vec<String> = env::var("KOEN_HARNESS_CMD")
        .ok()
        .map(|v| v.split_whitespace().map(String::from).collect())
        .filter(|v: &Vec<String>| !v.is_empty())
        .unwrap_or_else(|| vec![target.to_string()]);
    cmd.extend(args);

    let interactive = unsafe { libc::isatty(0) } == 1;
    let mut ws: libc::winsize = unsafe { mem::zeroed() };
    if interactive {
        unsafe { libc::ioctl(0, libc::TIOCGWINSZ, &mut ws) };
    }
    let mut master: libc::c_int = -1;
    let pid = unsafe {
        libc::forkpty(
            &mut master,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            if interactive { &mut ws as *mut _ } else { std::ptr::null_mut() },
        )
    };
    if pid < 0 {
        eprintln!("koen: forkpty failed");
        std::process::exit(1);
    }
    if pid == 0 {
        unsafe {
            // utf-8-aware erase, so backspace deletes chars not bytes
            let mut t: libc::termios = mem::zeroed();
            if libc::tcgetattr(0, &mut t) == 0 {
                t.c_iflag |= libc::IUTF8;
                libc::tcsetattr(0, libc::TCSANOW, &t);
            }
            let cprog = CString::new(cmd[0].as_str()).unwrap();
            let cargs: Vec<CString> =
                cmd.iter().map(|a| CString::new(a.as_str()).unwrap()).collect();
            let mut argv: Vec<*const libc::c_char> =
                cargs.iter().map(|c| c.as_ptr()).collect();
            argv.push(std::ptr::null());
            libc::execvp(cprog.as_ptr(), argv.as_ptr());
            eprintln!("koen: cannot exec {}", cmd[0]);
            libc::_exit(127);
        }
    }

    MASTER_FD.store(master, std::sync::atomic::Ordering::Relaxed);
    // non-blocking master so wr_master() can drain output on EAGAIN instead of
    // deadlocking on a large forward; pump() tolerates EAGAIN reads too.
    unsafe {
        let fl = libc::fcntl(master, libc::F_GETFL);
        libc::fcntl(master, libc::F_SETFL, fl | libc::O_NONBLOCK);
    }
    *SCREEN.lock().unwrap() = Some(Screen::new(ws.ws_row as usize, ws.ws_col as usize));
    let mut old: libc::termios = unsafe { mem::zeroed() };
    if interactive {
        unsafe {
            libc::signal(libc::SIGWINCH, on_winch as *const () as libc::sighandler_t);
            libc::tcgetattr(0, &mut old);
            let mut raw = old;
            libc::cfmakeraw(&mut raw);
            libc::tcsetattr(0, libc::TCSADRAIN, &raw);
        }
    }
    // Restore cooked mode if the harness ever panics — a terminal left in raw
    // mode is unusable. On the normal path we exit via process::exit (skips
    // Drop) after the explicit tcsetattr below, so this only fires on unwind.
    let _term_guard = interactive.then_some(TermiosGuard(old));

    let mut st = Shadow { buf: Vec::new(), cursor: 0, pend: Vec::new(), dirty: false, paste: false, paste_seen: false, baseline: Vec::new(), suffix };
    loop {
        if !pump(master, 20) {
            break;
        }
        if WINCH.swap(false, std::sync::atomic::Ordering::Relaxed) {
            let mut ws2: libc::winsize = unsafe { mem::zeroed() };
            if unsafe { libc::ioctl(0, libc::TIOCGWINSZ, &mut ws2) } == 0 {
                if let Ok(mut g) = SCREEN.lock() {
                    if let Some(s) = g.as_mut() {
                        s.resize(ws2.ws_row as usize, ws2.ws_col as usize);
                    }
                }
            }
        }
        // While the input line is empty and reliable, keep a fresh snapshot of
        // the prompt row. It's the reference koen later subtracts to read a
        // recalled (up/down) line without knowing claude's prompt glyphs. Skip
        // an all-blank row (claude hasn't drawn the prompt yet) so a startup
        // race can't freeze a useless baseline.
        if st.buf.is_empty() && !st.dirty {
            let row = screen_cursor_row();
            if row.iter().any(|&c| c != ' ' && c != CONT) {
                st.baseline = row;
            }
        }
        if readable(0, 0) {
            let mut buf = [0u8; 65536];
            let n = unsafe { libc::read(0, buf.as_mut_ptr() as *mut _, buf.len()) };
            if n <= 0 {
                // stdin EOF (piped use): give the child time to drain the pty
                // before closing the master hangs it up
                let mut budget = 25;
                while budget > 0 && pump(master, 20) {
                    budget -= 1;
                }
                unsafe { libc::close(master) };
                break;
            }
            process_input(&mut st, &buf[..n as usize], master);
        }
    }

    if interactive {
        unsafe { libc::tcsetattr(0, libc::TCSADRAIN, &old) };
    }
    if let Some(p) = own_orig_file {
        let _ = std::fs::remove_file(p);
    }
    let mut status: libc::c_int = 0;
    unsafe { libc::waitpid(pid, &mut status, 0) };
    let code = if libc::WIFEXITED(status) {
        libc::WEXITSTATUS(status)
    } else {
        128 + libc::WTERMSIG(status)
    };
    std::process::exit(code);
}

fn main() {
    load_config();
    let args: Vec<String> = env::args().skip(1).collect();
    let text = match args.first().map(|s| s.as_str()) {
        Some("-h") | Some("--help") => {
            print!("{}", HELP);
            return;
        }
        Some("--stop-hook") => {
            stop_hook();
            return;
        }
        Some("--prompt-hook") => {
            prompt_hook();
            return;
        }
        Some("claude") | Some("codex") => harness(&args[0].clone(), &args[1..]),
        Some("-f") => {
            let path = args.get(1).unwrap_or_else(|| {
                eprintln!("koen: -f needs a file path");
                std::process::exit(2);
            });
            std::fs::read_to_string(path).unwrap_or_else(|e| {
                eprintln!("koen: {}: {}", path, e);
                std::process::exit(1);
            })
        }
        Some(_) => args.join(" "),
        None => {
            let mut s = String::new();
            std::io::stdin().read_to_string(&mut s).unwrap_or_else(|e| {
                eprintln!("koen: stdin: {}", e);
                std::process::exit(1);
            });
            s
        }
    };
    if text.trim().is_empty() {
        return;
    }
    println!("{}", translate(&text));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn passthrough_no_hangul() {
        assert!(!has_hangul("plain english"));
        assert!(has_hangul("한글 있음"));
    }

    #[test]
    fn protect_restore_roundtrip() {
        let src = "코드 ```py\nx=1\n``` 와 `inline` 그리고 https://a.b/c 확인";
        let (masked, saved) = protect(src);
        assert!(!masked.contains("```") && !masked.contains("https://") && !masked.contains("`inline`"));
        assert_eq!(saved.len(), 3);
        assert_eq!(restore(&masked, &saved).unwrap(), src);
    }

    #[test]
    fn restore_keeps_nested_placeholder_token() {
        // saved[0] literally contains a ⟦K1⟧ token; single-pass restore must not
        // let the K1 substitution reach inside it.
        let saved = vec!["literal ⟦K1⟧ text".to_string(), "SECOND".to_string()];
        let out = restore("a ⟦K0⟧ b ⟦K1⟧ c", &saved).unwrap();
        assert_eq!(out, "a literal ⟦K1⟧ text b SECOND c");
    }

    #[test]
    fn lost_placeholder_errors() {
        let (masked, saved) = protect("코드 `x` 끝");
        assert!(restore(&masked.replace(&placeholder(0), ""), &saved).is_err());
    }

    #[test]
    fn quotes_protected() {
        let src = r#"버튼 라벨을 "저장하기" 로, 메시지는 '완료됨' 으로 바꿔줘"#;
        let (masked, saved) = protect(src);
        assert!(!masked.contains("저장하기") && !masked.contains("완료됨"));
        assert_eq!(restore(&masked, &saved).unwrap(), src);
    }

    #[test]
    fn fences_hide_inner_tokens() {
        let (_, saved) = protect("```\n`a` https://x.y\n```");
        assert_eq!(saved.len(), 1);
    }

    #[test]
    fn last_assistant_text_picks_final_text() {
        let t = concat!(
            r#"{"type":"user","message":{"content":"q"}}"#, "\n",
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"first"}]}}"#, "\n",
            r#"{"type":"assistant","message":{"content":[{"type":"tool_use","name":"Bash"}]}}"#, "\n",
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"final answer"}]}}"#, "\n",
            "not json\n",
        );
        assert_eq!(last_assistant_text(t), "final answer");
    }

    #[test]
    fn hangul_ratio_direction_gate() {
        assert!(hangul_ratio("완전히 한국어 문장입니다") > 0.5);
        assert!(hangul_ratio("mostly english with 한글 one word") < 0.5);
    }

    fn test_shadow() -> (Shadow, libc::c_int) {
        use std::os::unix::io::IntoRawFd;
        let fd = std::fs::OpenOptions::new().write(true).open("/dev/null").unwrap().into_raw_fd();
        let st = Shadow { buf: Vec::new(), cursor: 0, pend: Vec::new(), dirty: false, paste: false, paste_seen: false, baseline: Vec::new(), suffix: "" };
        (st, fd)
    }
    fn shadow_text(st: &Shadow) -> String {
        st.buf.iter().collect()
    }

    #[test]
    fn soft_newline_keeps_shadow_translatable() {
        // option/shift+Enter (ESC-CR) must be a newline in the shadow, not a
        // dirty marker — else a multi-line Korean prompt submits untranslated.
        let (mut st, fd) = test_shadow();
        process_input(&mut st, "첫째\x1b\r둘째".as_bytes(), fd);
        assert_eq!(shadow_text(&st), "첫째\n둘째");
        assert!(!st.dirty, "soft newline must not mark the shadow dirty");
    }

    #[test]
    fn arrow_edit_tracks_cursor_not_dirty() {
        // the real bug: fixing a typo with arrow keys must NOT skip translation.
        let (mut st, fd) = test_shadow();
        process_input(&mut st, "가나".as_bytes(), fd); // buf="가나" cursor=2
        process_input(&mut st, "\x1b[D".as_bytes(), fd); // Left -> cursor=1
        process_input(&mut st, "다".as_bytes(), fd); // insert mid-line
        assert_eq!(shadow_text(&st), "가다나");
        assert!(!st.dirty, "left-arrow edit must stay translatable");
        process_input(&mut st, "\x1b[3~".as_bytes(), fd); // Delete at cursor=2 -> removes 나
        assert_eq!(shadow_text(&st), "가다");
        process_input(&mut st, "\x7f".as_bytes(), fd); // Backspace -> removes 다
        assert_eq!(shadow_text(&st), "가");
        assert!(!st.dirty);
        process_input(&mut st, "\x1b[A".as_bytes(), fd); // Up arrow: genuinely ambiguous
        assert!(st.dirty);
    }

    #[test]
    fn shadow_utf8_split() {
        let (mut st, _) = test_shadow();
        let bytes = "안녕".as_bytes();
        feed_shadow(&mut st, &bytes[..2]); // split mid-char
        feed_shadow(&mut st, &bytes[2..]);
        assert_eq!(shadow_text(&st), "안녕");
        assert!(!st.dirty);
    }

    #[test]
    fn paste_with_tab_stays_translatable() {
        // a tab inside a paste is literal content, not a cursor key: the line
        // must stay clean (not dirty) so the shadow's full text is used.
        let (mut st, fd) = test_shadow();
        process_input(&mut st, "\x1b[200~함수\t정의 확인\x1b[201~".as_bytes(), fd);
        assert!(!st.dirty, "paste content (incl. tab) must not go dirty");
        assert!(st.paste_seen);
        assert_eq!(shadow_text(&st), "함수정의 확인"); // tab filtered, text kept
    }

    #[test]
    fn char_width_korean_is_wide() {
        assert_eq!(char_width('a'), 1);
        assert_eq!(char_width('안'), 2);
        assert_eq!(char_width('中'), 2);
    }

    #[test]
    fn screen_basic_cursor_and_erase() {
        let mut s = Screen::new(3, 20);
        s.feed(b"hello");
        assert_eq!(s.c, 5);
        s.feed(b"\r"); // carriage return
        assert_eq!(s.c, 0);
        s.feed(b"HEY");
        assert_eq!(&s.row_chars(0)[..5], &['H', 'E', 'Y', 'l', 'o']);
        s.feed(b"\x1b[K"); // erase to end of line from cursor (col 3)
        assert_eq!(&s.row_chars(0)[..5], &['H', 'E', 'Y', ' ', ' ']);
        s.feed(b"\x1b[5;3H"); // move cursor (clamped to 3 rows)
        assert_eq!((s.r, s.c), (2, 2));
    }

    #[test]
    fn screen_reads_recalled_line_via_baseline() {
        // Emulate claude drawing an empty prompt, snapshot it, then redrawing
        // the row with a recalled Korean line. read_input must recover just the
        // Korean — no prompt glyph hardcoded.
        let mut s = Screen::new(3, 40);
        s.feed("\x1b[3;1H\x1b[K❯ ".as_bytes()); // empty prompt on the bottom row
        let baseline = s.row_chars(s.r).to_vec();
        s.feed("\x1b[3;1H\x1b[K❯ 안녕 세계".as_bytes()); // up-arrow recall redraw
        let (text, off) = s.read_input(&baseline).expect("should read the line");
        assert_eq!(text, "안녕 세계");
        assert_eq!(off, text.chars().count()); // cursor left at the end
    }

    #[test]
    fn screen_reads_line_inside_a_border_box() {
        // A bordered box: the left border + prompt are shared with the baseline
        // (stripped as common prefix); the right border is trimmed as trailing.
        let mut s = Screen::new(3, 30);
        s.feed("\x1b[2;1H│ ❯            │".as_bytes());
        let baseline = s.row_chars(s.r).to_vec();
        s.feed("\x1b[2;1H│ ❯ 버그 수정   │".as_bytes());
        let (text, _) = s.read_input(&baseline).expect("should read boxed line");
        assert_eq!(text, "버그 수정");
    }

    #[test]
    fn enter_reads_recalled_line_off_screen_and_swaps() {
        // Full seam: shadow is dirty (up/down recall), so on_enter must read the
        // recalled Korean line off the screen model and submit the translation.
        let mut s = Screen::new(3, 40);
        s.feed("\x1b[3;1H\x1b[K❯ ".as_bytes());
        let baseline = s.row_chars(s.r).to_vec();
        s.feed("\x1b[3;1H\x1b[K❯ 버그 고쳐줘".as_bytes()); // recalled line
        *SCREEN.lock().unwrap() = Some(s);
        std::env::set_var("KOEN_FAKE_TRANSLATION", "FIX THE BUG");

        let mut fds = [0 as libc::c_int; 2];
        assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0);
        let (rd, wfd) = (fds[0], fds[1]);
        let mut st = Shadow {
            buf: Vec::new(), cursor: 0, pend: Vec::new(),
            dirty: true, paste: false, paste_seen: false, baseline, suffix: "",
        };
        on_enter(&mut st, wfd);
        unsafe { libc::close(wfd) };

        let mut out = Vec::new();
        let mut b = [0u8; 4096];
        loop {
            let n = unsafe { libc::read(rd, b.as_mut_ptr() as *mut _, b.len()) };
            if n <= 0 { break; }
            out.extend_from_slice(&b[..n as usize]);
        }
        unsafe { libc::close(rd) };
        let sent = String::from_utf8_lossy(&out);
        // The static in-process screen can't clear in response to Ctrl-U (there's
        // no live claude to redraw), so on_enter reads the recalled Korean,
        // translates it, and *attempts* the kill-line clear — proven here by the
        // Ctrl-U it emits. The full clear→retype is covered by the mock e2e runs.
        assert!(sent.contains('\u{15}'), "must attempt kill-line clear (Ctrl-U): {:?}", sent);
        *SCREEN.lock().unwrap() = None;
        std::env::remove_var("KOEN_FAKE_TRANSLATION");
    }
}
