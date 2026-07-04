# koen — 한국어 프롬프트를 싼 모델로 영어 압축해서 토큰 아끼기

한국어는 대부분의 LLM 토크나이저에서 같은 의미의 영어보다 **약 1.5~2.5배 많은
토큰**을 소모합니다. 비싼 모델(Opus, Fable, GPT-5)에 한국어 프롬프트를 그대로
넣으면 그만큼 컨텍스트와 요금을 낭비합니다.

`koen`은 프롬프트가 비싼 모델에 도달하기 **전에**, 싼 모델(Claude Haiku,
Codex mini, 무료 OpenRouter 모델)로 한국어→영어 변환을 수행하는 CLI + Claude
Code 스킬입니다. 기계번역(Papago/Google Translate)과 달리 LLM 번역이라
기술 용어·제약조건·뉘앙스가 유지되고, 코드블록·URL은 아예 모델에 보내지 않고
원문 그대로 복원합니다.

```
사용자 (한국어) ──> koen (Haiku/Codex mini, 싼 토큰) ──> 영어 프롬프트 ──> 비싼 모델
                     └─ 코드블록/인라인코드/URL은 placeholder로 보호, 원문 복원
```

## 설치

```bash
git clone https://github.com/JH-321/prompt-translate
cd prompt-translate
make install   # /usr/local/bin/koen + ~/.claude/skills/koen 심볼릭 링크
```

요구사항: `python3` + 백엔드 하나 이상 (`claude` CLI / `codex` CLI /
`OPENROUTER_API_KEY`). 의존성 설치 없음 — 표준 라이브러리만 사용.

## 사용법

```bash
koen "src/auth.ts 파일에서 로그인 실패 시 429를 반환하도록 수정해줘"
# -> Modify src/auth.ts to return 429 when login fails.

koen -f 기획서.ko.md > spec.en.md      # 문서 통째로 변환
echo "한국어 프롬프트" | koen | claude -p   # 헤드리스 파이프라인
```

- 한글이 없는 입력은 API 호출 없이 그대로 통과 (비용 0).
- 백엔드 자동 선택: `claude`(Haiku) → `codex` → OpenRouter 무료 모델.
  강제 지정: `KOEN_BACKEND=claude|codex|openrouter`
- 모델 변경: `KOEN_CLAUDE_MODEL`(기본 `claude-haiku-4-5`), `KOEN_CODEX_MODEL`,
  `KOEN_OPENROUTER_MODEL`(기본 `meta-llama/llama-3.3-70b-instruct:free`)
- 번역이 실패하거나 placeholder가 손상되면 **원문을 그대로 출력** — 의미가
  조용히 훼손되는 일은 없습니다.

## Claude Code 통합

**스킬 (`~/.claude/skills/koen`)** — `make install`로 설치됨. 한국어 기획서나
긴 한국어 텍스트를 작업 입력으로 주면, Claude가 비싼 모델 토큰으로 직접
번역하는 대신 `koen`을 Bash로 호출해 싼 모델로 변환한 뒤 영어본으로 작업합니다.

**훅은 왜 없나** — Claude Code의 `UserPromptSubmit` 훅은 프롬프트를
*교체*할 수 없고 원문 옆에 컨텍스트를 *추가*만 할 수 있습니다(공식 훅 스펙:
`additionalContext`). 한국어 원문 + 영어 번역이 둘 다 모델에 들어가면 토큰이
오히려 늘어나므로, 훅 통합은 의도적으로 제외했습니다. 절약이 실제로 일어나는
지점은 프롬프트가 비싼 모델에 들어가기 전 경계뿐입니다: 파이프(`koen | claude -p`)
또는 스킬(문서 변환).

## 언제 쓰고 언제 안 쓰나

| 상황 | 권장 |
|------|------|
| 긴 한국어 기획서/스펙/이슈를 작업 입력으로 사용 | ✅ `koen -f` — 절약 큼 |
| 헤드리스 배치 (`claude -p`, `codex exec`) | ✅ 파이프에 끼우기 |
| 짧은 대화형 한국어 질문 | ❌ 번역 왕복 비용이 절약보다 큼 |

## 테스트

```bash
make test   # 오프라인 self-check (protect/restore 로직, API 호출 없음)
```

## License

MIT
