# koen

> **Type Korean. Pay English prices.**

[![Rust](https://img.shields.io/badge/rust-single%20binary-orange?logo=rust)](https://www.rust-lang.org)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
[![Platform](https://img.shields.io/badge/platform-macOS%20%7C%20Linux-lightgrey)](#requirements)

[한국어 문서 (Korean README)](README.ko.md)

Korean text costs **1.5–2.5× more tokens** than equivalent English on most
LLM tokenizers. Feed Korean prompts straight into an expensive model (Opus,
Fable, GPT-5) and you burn that overhead on every turn — input, context
re-reads, all of it.

`koen` fixes this at the boundary: a **cheap** model (Claude Haiku, Codex
mini, or a free OpenRouter model) translates your Korean before it ever
reaches the expensive model, and translates the response back for you to
read. Unlike machine translation (Papago/Google Translate), an LLM
translator preserves technical terms, constraints, and nuance — and koen
never even sends your code to the translator.

```
you (Korean) ──▶ koen ──▶ cheap model (Haiku) ──▶ English ──▶ expensive model
                  │                                               │
                  │        code blocks / URLs / quotes            ▼
                  └── shielded via placeholders ──▶  English response
                                                          │
                       Korean translation ◀── cheap model ┘
```

## Highlights

- 🖥️ **Wraps the real TUI** — `koen claude` runs the actual Claude Code
  interface in a pty. Permission dialogs, `/model`, skills, streaming,
  keybindings: everything works, because nothing is re-implemented.
- 💸 **No expensive Korean tokens, either direction** — the upper model
  reads English and writes English; a cheap model handles both translations.
- 🔒 **Meaning is never silently lost** — code fences, `inline code`,
  `"quoted"`/`'quoted'` text, and URLs are placeholder-protected and restored
  byte-for-byte. If translation fails, the original line is submitted
  unchanged.
- 🦀 **One static Rust binary**, three small dependencies, no runtime.

## Install

```bash
git clone https://github.com/JH-321/prompt-koen
cd prompt-koen
make install          # Apple Silicon: make install PREFIX=/opt/homebrew
```

### Requirements

- Rust toolchain (`brew install rust` or [rustup](https://rustup.rs))
- At least one translation backend:
  - `claude` CLI, logged in (a Claude subscription includes Haiku), **or**
  - `codex` CLI, logged in, **or**
  - an `OPENROUTER_API_KEY` (free models available → zero-cost translation)

`make install` builds the release binary, links it into `$(PREFIX)/bin/koen`,
and seeds a commented config file at `~/.koenrc`.

## Usage

### Harness mode — `koen claude` / `koen codex`

Run the real TUI; koen sits invisibly on the input stream. When you press
Enter on a line containing Hangul, koen translates it with the cheap model,
swaps it into the input box, and submits the English.

```bash
koen claude                            # Claude Code, input/output translated
koen codex                             # Codex TUI
koen claude --model claude-opus-4-8    # args pass straight through to the CLI
koen claude --lower claude-haiku-4-5   # pick the translator (lower) model
```

What a turn looks like:

```
❯ Explain in one sentence why the sky is blue.      ← your Korean, swapped to English
⎿ UserPromptSubmit says: 원문: 왜 하늘이 파란색인지 한 문장으로 설명해줘.
                                                    ← what you actually typed (display-only)
⏺ Blue light scatters more efficiently ... Rayleigh scattering.
                                                    ← upper model answers in cheap English
⎿ Stop says: 태양의 파란빛이 ... Rayleigh scattering이라고 부릅니다.
                                                    ← cheap model's Korean translation
```

Rules of the road:

| Input | What koen does |
|---|---|
| Korean line + Enter | translate → erase → submit English (original echoed below) |
| English line | passes through untouched — zero cost |
| Line starting with `/` `!` `#` | never translated: skill names & command args stay intact |
| Line edited with arrows / tab-complete | passes through untranslated (shadow buffer can't be trusted) |
| Code fences, `` `inline` ``, `"quotes"`, `'quotes'`, URLs | never sent to the translator, restored verbatim |
| Translation failure | original line submitted — the session never breaks |

Response translation (claude only) works through a session-scoped Stop hook
injected at launch — the expensive model answers in English and the cheap
model's Korean rendering appears natively under it. Codex has no hook
channel, so there the upper model is simply asked to reply in Korean.
Set `KOEN_REPLY=en` to keep responses in English.

### One-shot / pipeline mode

```bash
koen "src/auth.ts에서 로그인 실패 시 429를 반환하게 수정해줘"
# → Modify src/auth.ts to return 429 on login failure.

koen -f spec.ko.md > spec.en.md          # translate a whole document
echo "한국어 프롬프트" | koen | claude -p     # headless pipeline
```

Input without Hangul passes through with **zero API calls**.

## Configuration

Persistent settings live in **`~/.koenrc`** (seeded by `make install` from
[`koenrc.example`](koenrc.example)). Shell environment variables override the
file per-invocation. Use `KOEN_CONFIG=<path>` for a custom location.

```bash
# ~/.koenrc
KOEN_CLAUDE_MODEL=claude-haiku-4-5
#KOEN_BACKEND=openrouter
#OPENROUTER_API_KEY=sk-or-...
```

| Variable | Purpose | Default |
|---|---|---|
| `KOEN_BACKEND` | Force a backend: `claude` \| `codex` \| `openrouter` | auto-detect (claude → codex → openrouter) |
| `KOEN_CLAUDE_MODEL` | Translator model for the claude backend | `claude-haiku-4-5` |
| `KOEN_CODEX_MODEL` | Translator model for the codex backend | codex's own default (`~/.codex/config.toml`) |
| `KOEN_OPENROUTER_MODEL` | OpenRouter model ID | `meta-llama/llama-3.3-70b-instruct:free` |
| `OPENROUTER_API_KEY` | Required for the OpenRouter backend | — |
| `KOEN_REPLY` | `en` = leave responses untranslated | Korean translation on |

The **upper** model is not koen's business — set it the way you always do
(`koen claude --model claude-opus-4-8`, or `/model` inside the session).

## Why a pty, not hooks or a skill?

We verified — against the official docs *and* empirically — that Claude
Code's `UserPromptSubmit` / `UserPromptExpansion` hooks **cannot replace a
prompt**; they can only add context alongside it, which would send both the
Korean *and* the English to the model and make things worse. A skill has the
same flaw: by the time it runs, your Korean is already in the expensive
context. The pty is the only interception point that keeps the native UX
while ensuring the expensive model never sees Korean at all.

## When it pays off

| Scenario | Korean reaches the expensive model? | Verdict |
|---|---|---|
| Typing inside `koen claude` / `koen codex` | ❌ swapped at Enter | ✅ the intended workflow |
| Pre-translating docs (`koen -f spec.ko.md`) | ❌ | ✅ great for long specs |
| Headless pipelines (`koen \| claude -p`) | ❌ | ✅ full savings |
| Typing Korean into bare `claude` | ⭕ full Korean token cost | ❌ use `koen claude` instead |

## Development

```bash
make test    # cargo unit tests + offline pty smoke tests (no API calls)
```

The smoke tests drive the real pty machinery against a capturing child
process, with a deterministic fake translator (`KOEN_FAKE_TRANSLATION`).
Debug the hooks with `KOEN_DEBUG=<file>`.

## License

[MIT](LICENSE)
