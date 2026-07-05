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

Harness rules:
    - responses are shown in Korean, cheaply: with claude the upper model
      answers in English (minimal output tokens) and a session-scoped Stop
      hook has the CHEAP model translate it, shown natively in the TUI;
      with codex the upper model is asked to reply in Korean directly.
      Disable with KOEN_REPLY=en
    - code fences, `inline code`, \"quoted\"/'quoted' text, and URLs in your
      prompt are never translated — restored verbatim
    - lines starting with / ! # are never translated (slash/bash commands)
    - lines edited with arrow keys / tab-complete pass through untranslated
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
    let mut out = text.to_string();
    for (i, s) in saved.iter().enumerate() {
        let ph = placeholder(i);
        if !out.contains(&ph) {
            return Err(format!("backend lost placeholder {}", ph));
        }
        out = out.replace(&ph, s);
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
    let out = child.wait_with_output().map_err(|e| e.to_string())?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        return Err(format!("exit {}: {}", out.status, &err[..err.len().min(300)]));
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
    let status = cmd
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(|e| e.to_string())?;
    if !status.success() {
        return Err(format!("codex exit {}", status));
    }
    let text = std::fs::read_to_string(&out_file).map_err(|e| e.to_string())?;
    let _ = std::fs::remove_file(&out_file);
    Ok(text.trim().to_string())
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
        .ok_or_else(|| format!("unexpected response: {}", &out[..out.len().min(200)]))
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

/// Claude Code Stop hook: translate the English response to Korean with the
/// cheap model and hand it back as a systemMessage, which the TUI renders
/// natively under the response. This keeps the expensive model's output in
/// cheap English tokens while the user reads Korean.
fn stop_hook() {
    let dbg = |m: String| {
        if let Ok(f) = env::var("KOEN_DEBUG") {
            if let Ok(mut fh) = std::fs::OpenOptions::new().create(true).append(true).open(f) {
                let _ = writeln!(fh, "{}", m);
            }
        }
    };
    let mut input = String::new();
    if std::io::stdin().read_to_string(&mut input).is_err() {
        return;
    }
    dbg(format!("input: {}", &input[..input.len().min(400)]));
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
        dbg(format!("no-op translation for: {}", &text[..text.len().min(200)]));
        return; // translation failed or already Korean: show nothing extra
    }
    dbg(format!("ok: {}", &ko[..ko.len().min(200)]));
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

/// poll a single fd for readability; returns true if readable
fn readable(fd: libc::c_int, timeout_ms: libc::c_int) -> bool {
    let mut p = libc::pollfd { fd, events: libc::POLLIN, revents: 0 };
    unsafe { libc::poll(&mut p, 1, timeout_ms) > 0 && p.revents & (libc::POLLIN | libc::POLLHUP) != 0 }
}

/// Forward pending child output to our stdout. False on child EOF.
fn pump(master: libc::c_int, timeout_ms: libc::c_int) -> bool {
    if readable(master, timeout_ms) {
        let mut buf = [0u8; 65536];
        let n = unsafe { libc::read(master, buf.as_mut_ptr() as *mut _, buf.len()) };
        if n <= 0 {
            return false;
        }
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
            }
        }
    }
}

/// Appended per translated turn for codex, which has no hook channel.
const REPLY_KO_SUFFIX: &str = " Reply in Korean; keep code, paths, and commands as-is.";

struct Shadow {
    buf: String,     // shadow of the TUI's current input line
    pend: Vec<u8>,   // bytes of a split utf-8 char
    dirty: bool,     // cursor moved / tab-completed: shadow unreliable, skip
    paste: bool,     // inside bracketed paste
    suffix: &'static str, // appended after a swapped (translated) line
}

fn on_enter(st: &mut Shadow, master: libc::c_int) -> Vec<u8> {
    let text = mem::take(&mut st.buf);
    st.pend.clear();
    let was_dirty = mem::replace(&mut st.dirty, false);
    let head = text.trim_start();
    let skip = was_dirty
        || !has_hangul(&text)
        || head.starts_with('/')
        || head.starts_with('!')
        || head.starts_with('#');
    if skip {
        wr(master, b"\r");
        return Vec::new();
    }
    let (eng, held) = translate_while_pumping(&text, master);
    if eng != text && !has_hangul(&eng) {
        // ponytail: one backspace per char erases the input box; if a
        // grapheme/wide-char mismatch ever bites, count graphemes instead
        wr(master, &vec![0x7f; text.chars().count()]);
        wr(master, eng.as_bytes());
        wr(master, st.suffix.as_bytes());
        // the TUI treats a rapid burst as a paste, and a \r inside a paste
        // inserts a newline instead of submitting — pause so the Enter
        // registers as its own keypress
        for _ in 0..6 {
            pump(master, 50);
        }
    }
    wr(master, b"\r");
    held
}

fn feed_shadow(st: &mut Shadow, chunk: &[u8]) {
    if chunk.iter().any(|&c| c < 0x20) {
        st.dirty = true; // tab-complete or other control key
    }
    st.pend.extend(chunk.iter().filter(|&&c| c >= 0x20));
    match std::str::from_utf8(&st.pend) {
        Ok(s) => {
            st.buf.push_str(s);
            st.pend.clear();
        }
        Err(e) => {
            let valid = e.valid_up_to();
            st.buf.push_str(std::str::from_utf8(&st.pend[..valid]).unwrap());
            st.pend.drain(..valid);
            if e.error_len().is_some() || st.pend.len() > 8 {
                st.pend.clear(); // garbage, not a split utf-8 char
                st.dirty = true;
            }
        }
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
                b"\x1b[200~" => st.paste = true,
                b"\x1b[201~" => st.paste = false,
                _ => st.dirty = true, // arrows etc: shadow no longer trustworthy
            }
            wr(master, seq);
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
            st.buf.push('\n'); // newline inside pasted text: part of the input
            wr(master, &q[i..=i]);
            i += 1;
        } else if b == 0x7f {
            st.buf.pop();
            wr(master, &q[i..=i]);
            i += 1;
        } else if b == 0x03 || b == 0x15 {
            // ctrl-c / ctrl-u clear the input line
            st.buf.clear();
            st.pend.clear();
            st.dirty = false;
            wr(master, &q[i..=i]);
            i += 1;
        } else {
            let mut j = i + 1;
            while j < q.len() && !SPECIAL.contains(&q[j]) {
                j += 1;
            }
            wr(master, &q[i..j]);
            let chunk: Vec<u8> = q[i..j].to_vec();
            feed_shadow(st, &chunk);
            i = j;
        }
    }
}

fn harness(target: &str, extra: &[String]) -> ! {
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
    // reply-in-Korean, cheap-model edition (disable with KOEN_REPLY=en):
    // - claude: the upper model answers in English (minimal output tokens);
    //   a session-scoped Stop hook (`koen --stop-hook`) translates the
    //   response with the cheap model and the TUI shows it natively.
    // - codex: no hook channel, so the upper model is asked to reply in
    //   Korean directly via a per-turn suffix.
    let reply_ko = env::var("KOEN_REPLY").map(|v| v != "en").unwrap_or(true);
    let mut suffix = "";
    if reply_ko {
        if target == "claude" {
            let exe = env::current_exe()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|_| "koen".into());
            args.push("--settings".into());
            args.push(
                serde_json::json!({ "hooks": { "Stop": [{ "hooks": [{
                    "type": "command",
                    "command": format!("'{}' --stop-hook", exe),
                    "timeout": 120
                }]}]}})
                .to_string(),
            );
        } else {
            suffix = REPLY_KO_SUFFIX;
        }
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

    let mut st = Shadow { buf: String::new(), pend: Vec::new(), dirty: false, paste: false, suffix };
    loop {
        if !pump(master, 20) {
            break;
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

    #[test]
    fn shadow_utf8_split() {
        let mut st = Shadow { buf: String::new(), pend: Vec::new(), dirty: false, paste: false, suffix: "" };
        let bytes = "안녕".as_bytes();
        feed_shadow(&mut st, &bytes[..2]); // split mid-char
        feed_shadow(&mut st, &bytes[2..]);
        assert_eq!(st.buf, "안녕");
        assert!(!st.dirty);
    }
}
