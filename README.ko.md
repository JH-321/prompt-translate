# koen — 한국어 프롬프트를 싼 모델로 영어 압축해서 토큰 아끼기

> **Type Korean. Pay English prices.** — [English README](README.md)

한국어는 대부분의 LLM 토크나이저에서 같은 의미의 영어보다 **약 1.5~2.5배 많은
토큰**을 소모합니다. 비싼 모델(Opus, Fable, GPT-5)에 한국어 프롬프트를 그대로
넣으면 그만큼 컨텍스트와 요금을 낭비합니다.

`koen`은 프롬프트가 비싼 모델에 도달하기 **전에**, 싼 모델(Claude Haiku,
Codex mini, 무료 OpenRouter 모델)로 한국어→영어 변환을 수행하는 CLI + Claude
Code 하네스입니다. 기계번역(Papago/Google Translate)과 달리 LLM 번역이라
기술 용어·제약조건·뉘앙스가 유지되고, 코드블록·URL은 아예 모델에 보내지 않고
원문 그대로 복원합니다.

```
사용자 (한국어) ──> koen (Haiku/Codex mini, 싼 토큰) ──> 영어 프롬프트 ──> 비싼 모델
                     └─ 코드블록/인라인코드/URL은 placeholder로 보호, 원문 복원
```

## 설치

```bash
git clone https://github.com/JH-321/prompt-koen
cd prompt-koen
make install   # cargo build 후 /usr/local/bin/koen 링크 + ~/.koenrc 생성
```

요구사항: Rust(`cargo`) + 백엔드 하나 이상 (`claude` CLI / `codex` CLI /
`OPENROUTER_API_KEY`). 단일 정적 바이너리로 빌드됩니다.

## 사용법

### 하네스 모드 — `koen claude` / `koen codex`

**진짜 Claude Code / Codex TUI를 그대로 실행합니다.** koen은 pty(가상
터미널)로 감싸서 화면 출력은 손대지 않고 통과시키고, 입력만 감시합니다:
Enter를 누른 순간 입력줄에 한글이 있으면 싼 모델(Haiku)로 번역해서 입력창의
한글을 지우고 영어로 바꿔 제출합니다. **상위 모델에는 영어만 도달합니다.**

권한 다이얼로그, 선택지(AskUserQuestion), shift+tab 권한 모드 전환,
`/model`, 스킬, goal, config, 스트리밍 — Claude Code가 지원하는 모든 것이
그대로 동작합니다. koen이 UI를 재구현한 게 아니라 진짜 TUI이기 때문입니다.

```bash
koen claude                          # 진짜 claude TUI, 입력만 번역
koen codex                           # 진짜 codex TUI
koen claude --model claude-opus-4-8  # 인자는 전부 내부 CLI로 그대로 전달
koen claude --lower haiku            # 번역기(하위 모델)만 koen이 소비하는 플래그
```

상위 모델은 평소처럼 `--model`이나 `/model`로, 하위(번역) 모델은
`--lower` 또는 `KOEN_CLAUDE_MODEL` 환경변수로 지정합니다.

동작 규칙:
- **응답도 싼 모델이 한국어로 변환합니다** (claude): 상위 모델은 **영어로**
  응답해서 출력 토큰을 최소로 쓰고, koen이 세션 한정으로 주입한 Stop 훅이
  그 영어 응답을 Haiku로 번역해 응답 바로 아래에 네이티브로 표시합니다
  (`⎿ Stop says: <한국어>`). 코드·식별자·경로·기술용어는 번역하지 않고
  원문 유지. 양방향 모두 비싼 모델이 한국어 토큰을 쓰지 않는 구조입니다.
  ```
  ❯ Explain in one sentence why the sky is blue.   ← 한국어 입력이 영어로 교체 제출
  ⎿ UserPromptSubmit says: 원문: 왜 하늘이 파란색인지 한 문장으로 설명해줘.  ← 내가 친 원문
  ⏺ Blue light ... Rayleigh scattering.            ← 상위 모델 영어 응답 (토큰 최소)
  ⎿ Stop says: 태양의 파란빛이 ... Rayleigh scattering이라고 부릅니다.  ← Haiku 번역
  ```
- **입력 원문도 보입니다**: Enter로 영어 교체 제출된 직후, 방금 친 한국어
  원문이 `원문: ...` 줄로 프롬프트 아래에 표시됩니다(표시 전용 — 모델에는
  안 들어가서 토큰 비용 0).
  codex는 훅 채널이 없어 번역된 턴마다 "한국어로 답해" 지시를 덧붙이는
  방식입니다(상위 모델이 직접 한국어 생성). 영어 응답을 원하면 `KOEN_REPLY=en`.
- **번역하지 않는 것**: 코드 펜스(```), `인라인 코드`, `"큰따옴표"`/`'작은따옴표'`
  안의 텍스트, URL — placeholder로 감춰서 번역 모델에 아예 보내지 않고
  바이트 그대로 복원합니다.
- 영어 입력 → 번역 단계 생략, 그대로 통과 (비용 0)
- `/`, `!`, `#` 로 시작하는 줄(슬래시/배시/메모 명령) → 줄 전체를 건드리지
  않고 그대로 전달 — 스킬 이름·명령 인자가 변형될 위험 자체가 없음
- 화살표로 중간을 고치거나 up/down으로 이전 프롬프트를 불러온 줄 → 그래도
  번역됨. koen이 claude 화면을 에뮬레이션해 Enter 시점의 실제 입력창을 읽음
- 붙여넣기(claude가 `[Pasted text]` 칩으로 접기도 함) → koen은 전체 텍스트를
  갖고 있고, 줄을 지워 비었는지 확인되면 영어로 다시 입력, 아니면 붙여넣기를
  건드리지 않고 그대로 제출 — 절대 손상시키지 않음
- 번역 실패 → 원문 그대로 제출 (세션이 깨지지 않음)
- Enter 후 번역되는 몇 초 동안 입력창에 한글이 남아 있다가 영어로 바뀌며 제출됨

claude 하네스에서는 상위 모델의 입력·출력 어느 쪽에도 한국어가 들어가지
않습니다 — 번역(한→영, 영→한)은 전부 싼 모델(Haiku)이 수행합니다. 응답
번역이 도는 몇 초 동안 `running stop hooks…` 스피너가 표시됩니다.

*왜 훅이 아니라 pty인가*: Claude Code의 UserPromptSubmit/UserPromptExpansion
훅은 프롬프트를 교체할 수 없다는 것을 문서와 실험으로 확인했습니다
(`additionalContext` 추가만 가능 — 원문이 항상 모델에 전달됨). 네이티브
UX를 유지하면서 입력을 가로채는 유일한 지점이 pty입니다.

### 단발 변환

```bash
koen "src/auth.ts 파일에서 로그인 실패 시 429를 반환하도록 수정해줘"
# -> Modify src/auth.ts to return 429 when login fails.

koen -f 기획서.ko.md > spec.en.md      # 문서 통째로 변환
echo "한국어 프롬프트" | koen | claude -p   # 헤드리스 파이프라인
```

- 한글이 없는 입력은 API 호출 없이 그대로 통과 (비용 0).
- 번역이 실패하거나 placeholder가 손상되면 **원문을 그대로 출력** — 의미가
  조용히 훼손되는 일은 없습니다.

## 하위(번역) 모델 설정

번역 백엔드는 설치된 순서대로 자동 선택됩니다: `claude` → `codex` →
OpenRouter.

**영구 설정은 `~/.koenrc` 파일에 씁니다** — `make install`이 주석 달린
템플릿을 만들어 주고(`koenrc.example`), 한 번 써두면 단발 CLI·파이프라인·
하네스(`koen claude`/`koen codex`)·응답 번역(Stop 훅) 전부에 자동
적용됩니다. 셸 환경변수는 일회성 오버라이드로 파일보다 우선합니다.
다른 위치를 쓰려면 `KOEN_CONFIG=<경로>`.

```bash
# ~/.koenrc
KOEN_CLAUDE_MODEL=claude-haiku-4-5
#KOEN_BACKEND=claude
#OPENROUTER_API_KEY=sk-or-...
```

| 환경변수 | 역할 | 기본값 |
|---|---|---|
| `KOEN_BACKEND` | 백엔드 강제 지정: `claude` \| `codex` \| `openrouter` | 자동 감지 |
| `KOEN_CLAUDE_MODEL` | claude 백엔드의 번역 모델 (`claude -p --model <값>`) | `claude-haiku-4-5` |
| `KOEN_CODEX_MODEL` | codex 백엔드의 번역 모델 (`codex exec -m <값>`) | codex 설정(`~/.codex/config.toml`)의 기본 모델 |
| `KOEN_OPENROUTER_MODEL` | OpenRouter 모델 ID | `meta-llama/llama-3.3-70b-instruct:free` |
| `OPENROUTER_API_KEY` | OpenRouter 사용 시 필수 (없으면 이 백엔드는 후보에서 제외) | — |
| `KOEN_REPLY` | `en` = 응답을 영어 그대로 (번역 안 함) | 한국어 번역 켜짐 |
| `KOEN_YOLO` | `1` = 하네스에서 모든 권한·승인 프롬프트 건너뛰기 (claude `--dangerously-skip-permissions`, codex `--yolo`) | 꺼짐(프롬프트 표시) |

`KOEN_YOLO`는 **기본값이 꺼짐**이라 갓 설치한 사람은 확인 없이 명령이
실행되지 않습니다. 신뢰하는 디렉터리에서만 `~/.koenrc`의 `KOEN_YOLO=1`
주석을 풀어 켜세요(또는 한 세션만 `KOEN_YOLO=1 koen claude`).

```bash
# 일회성 오버라이드 예시 (~/.koenrc보다 우선)
KOEN_CLAUDE_MODEL=claude-sonnet-5 koen claude        # 번역을 Sonnet으로
KOEN_BACKEND=codex KOEN_CODEX_MODEL=gpt-5-mini koen "한국어 프롬프트"
```

하네스 전용 단축: `koen claude --lower <모델>` — `KOEN_CLAUDE_MODEL`과
`KOEN_CODEX_MODEL`을 그 값으로 설정하는 것과 동일합니다. 상위 모델은
koen 설정이 아니라 내부 CLI 인자(`koen claude --model claude-opus-4-8`)나
세션 안의 `/model`로 바꿉니다.

## 왜 훅/스킬 통합이 없나

Claude Code의 `UserPromptSubmit` 훅은 프롬프트를 *교체*할 수 없고 원문 옆에
컨텍스트를 *추가*만 할 수 있습니다(공식 훅 스펙 + 실험으로 확인). 한국어
원문 + 영어 번역이 둘 다 모델에 들어가면 토큰이 오히려 늘어납니다. 스킬
방식도 마찬가지로 한국어가 이미 비싼 모델 컨텍스트에 들어간 뒤에야 동작하므로
제외했습니다. 절약이 실제로 일어나는 지점은 프롬프트가 비싼 모델에 들어가기
전 경계뿐이고, 그게 koen 하네스(pty)와 파이프(`koen | claude -p`)입니다.
한국어 문서 파일은 `koen -f 파일.md`로 미리 변환해서 쓰면 됩니다.

## 언제 쓰고 언제 안 쓰나

절약의 조건은 하나입니다: **한국어가 비싼 모델의 컨텍스트에 들어가기 전에
가로챌 것.** 이미 들어간 텍스트를 번역하면 원문+영어가 둘 다 컨텍스트에
남아 오히려 손해입니다.

| 상황 | 한국어가 상위 모델에 도달? | 권장 |
|------|------|------|
| `koen claude`/`koen codex` 하네스에서 타이핑 | ❌ (Enter 시점에 영어로 교체) | ✅ 기본 사용법 |
| 한국어 문서를 미리 변환 (`koen -f 기획서.md`) | ❌ | ✅ 긴 스펙에 유용 |
| 헤드리스 배치 (`koen \| claude -p`) | ❌ | ✅ 완전 절약 |
| 맨 `claude`에 한국어를 직접 입력 | ⭕ 한국어 토큰 그대로 소모 | ❌ `koen claude`를 쓰세요 |

## 테스트

```bash
make test   # 오프라인 self-check (protect/restore 로직, API 호출 없음)
```

## License

MIT
