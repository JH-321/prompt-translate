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

### 하네스 모드 — `koen claude` / `koen codex`

터미널에서 상위 모델을 koen이 감싸서 실행합니다. 입력창에서 한글/영어를
받으면, 한글은 싼 모델(Haiku)이 의미 손실 없이 영어로 변환하고(변환문이
회색으로 표시됨), **상위 모델에는 영어만 도달**합니다. 대화 세션은 턴 사이에
유지됩니다.

```bash
koen claude                          # claude를 내부적으로 구동
koen codex                           # codex를 내부적으로 구동
koen claude -- --model claude-opus-4-8   # -- 뒤는 내부 CLI에 그대로 전달

koen> 로그인 버그 고쳐줘. src/auth.ts 봐.
→ Fix the login bug. Look at src/auth.ts.     # 싼 모델의 번역 (회색)
[상위 모델의 응답이 여기 표시됨]
```

REPL 명령: `/raw <텍스트>`(번역 없이 전송), `/exit`·`/quit`·Ctrl-D(종료).
영어 입력은 번역 단계를 건너뜁니다(비용 0).

제약: 내부 모델은 비대화형(`claude -p` / `codex exec`)으로 돌기 때문에 툴
권한 프롬프트에 답할 수 없습니다. 권한이 필요한 작업은
`koen claude -- --permission-mode acceptEdits` 처럼 플래그를 넘기세요.
응답은 턴이 끝날 때 한 번에 출력됩니다(스트리밍 없음).

### 단발 변환

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

절약의 조건은 하나입니다: **한국어가 비싼 모델의 컨텍스트에 들어가기 전에
가로챌 것.** 이미 들어간 텍스트를 번역하면 원문+영어가 둘 다 컨텍스트에
남아 오히려 손해입니다.

| 상황 | 한국어가 상위 모델에 도달? | 권장 |
|------|------|------|
| 한국어 기획서/스펙을 **파일 경로로** 전달 | ❌ (koen이 디스크에서 직접 읽음) | ✅ `koen -f` — 절약 큼 |
| 헤드리스 배치 (`koen \| claude -p`) | ❌ | ✅ 완전 절약 |
| 대화창에 한국어를 타이핑/붙여넣기 | ⭕ 이미 도달 (토큰 이미 소모됨) | ❌ 번역하면 이중 비용 |

## 테스트

```bash
make test   # 오프라인 self-check (protect/restore 로직, API 호출 없음)
```

## License

MIT
