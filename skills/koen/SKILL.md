---
name: koen
description: >
  Korean→English prompt compression to save tokens. Use when the user hands
  you a Korean document, spec, or long Korean text that will be used as
  working input (specs, requirements, issue bodies), or when they ask to
  "translate prompt", "토큰 절약", "프롬프트 번역", "koen". Translates via a
  cheap model (Haiku/Codex/free LLM) instead of burning expensive-model tokens.
---

# koen — cheap-model Korean→English prompt compression

Korean text costs roughly 2x the tokens of equivalent English on most LLM
tokenizers. This skill routes translation through a cheap model so the
expensive model only ever sees English.

## When to use

Savings only happen when the Korean text has NOT yet entered the expensive
model's context. That means:

- The user names a **Korean file path** (spec, PRD, issue export) — do NOT
  Read it yourself; pipe it through `koen -f` so only English enters context.
- The user asks to prepare/compress a Korean prompt **for another model /
  headless run** (`koen | claude -p`).

Do NOT use when the Korean is already in the conversation (typed or pasted
messages, file contents you already Read). Those tokens are already spent;
translating them adds cost instead of saving it — just answer directly.

## How

Translate with the CLI (never translate long Korean documents yourself —
that spends expensive-model output tokens, which is what we're avoiding):

```bash
# file
koen -f spec.ko.md > spec.en.md

# text / stdin
koen "한국어 텍스트"
echo "한국어 텍스트" | koen
```

Then work from the English output. Code blocks, inline code, and URLs are
placeholder-protected and restored verbatim, so translated specs are safe
to execute against.

Backend is auto-detected (claude→codex→openrouter); force with
`KOEN_BACKEND=claude|codex|openrouter`. If the command is missing, the repo
is at github.com/JH-321/prompt-translate — `make install` symlinks it.
