#!/usr/bin/env bash
# Runs the build: for each phase, the planner (Fable 5.2) writes tasks; the developer (Opus 5.5)
# carries them out one by one, each through scripts/build/check.sh; the reviewer (Fable 5.2)
# reviews the phase and writes fix tasks until it passes. Stops, and says why, on anything it
# cannot get past. Run it again to carry on where it stopped: finished work is never redone.
#
#   caffeinate -is scripts/build/run.sh            # all phases
#   PHASES="P1" scripts/build/run.sh               # some of them
#   touch docs/build/HOLD                          # pause before the next agent; rm to go on
set -uo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$ROOT"
OUT="${ZUTALK_BUILD_OUT:-$HOME/Zuzalu/zutalk-build}"
PHASES="${PHASES:-P1 P2 P3 P4 P5 P6 P7}"
PLANNER="${PLANNER:-anthropic/claude-fable-5-2}"
DEV="${DEV:-anthropic/claude-opus-5-5}"
LIMIT="${LIMIT:-5h}" # one agent's longest run
B=docs/build
P=scripts/build/prompts
mkdir -p "$OUT/logs" "$OUT/sessions"
STATUS="$OUT/STATUS.md"

if ! mkdir "$OUT/lock" 2>/dev/null; then
  echo "another build is running ($OUT/lock); remove it if that is not so"
  exit 1
fi
trap 'rmdir "$OUT/lock" 2>/dev/null' EXIT

say() {
  local line="$(date '+%m-%d %H:%M') $*"
  echo "$line"
  echo "- $line" >>"$STATUS"
}

stop() {
  say "停下：$*"
  osascript -e "display notification \"$*\" with title \"ZuTalk build stopped\"" 2>/dev/null || true
  exit 1
}

hold() {
  local told=""
  while [ -e "$B/HOLD" ] || [ -e "$OUT/HOLD" ]; do
    [ -n "$told" ] || say "暂停（有 HOLD 文件）"
    told=1
    sleep 60
  done
}

# The prompt for a role: the common part, then the role's, with {{NAME}} filled in.
render() {
  local role="$1"
  shift
  {
    cat "$P/common.md"
    echo
    cat "$P/$role.md"
  } | perl -pe 'BEGIN { %v = map { split /=/, $_, 2 } @ARGV; @ARGV = () } s/\{\{(\w+)\}\}/exists $v{$1} ? $v{$1} : ""/ge' "$@"
}

# One agent, to the end. $1 model, $2 name, rest: render arguments.
agent() {
  local model="$1" name="$2"
  shift 2
  hold
  local prompt
  prompt="$(render "$@")"
  say "开始 ${name}（${model}）"
  local started=$SECONDS
  timeout "$LIMIT" pi --print --model "$model" --thinking high \
    --session-dir "$OUT/sessions" --name "zutalk-build $name" -- "$prompt" \
    >"$OUT/logs/$name.log" 2>&1
  local code=$?
  say "结束 ${name}：退出码 ${code}，用时 $(((SECONDS - started) / 60)) 分钟"
  # The API refusing is not the agent failing: no fix attempt can get past it.
  if [ "$code" -ne 0 ] && grep -qE 'credit balance is too low|"type":"(authentication_error|permission_error)"' "$OUT/logs/$name.log"; then
    settle "Keep what $name had done when the API refused it, to be carried on"
    stop "API 拒绝了 ${name}（额度或钥匙），见 $OUT/logs/$name.log；补上后重跑"
  fi
  if [ "$code" -ne 0 ] && grep -qE '"type":"(rate_limit_error|overloaded_error|api_error)"' "$OUT/logs/$name.log"; then
    settle "Keep what $name had done when the API was busy, to be carried on"
    say "API 忙，10 分钟后重试 ${name}"
    sleep 600
    agent "$model" "$name" "$@"
    return $?
  fi
  return $code
}

# Whatever an agent left uncommitted is committed, so that each step starts clean.
settle() {
  if [ -n "$(git status --porcelain)" ]; then
    git add -A && git commit -q -m "$1"
  fi
}

gate() {
  scripts/build/check.sh >"$OUT/logs/check-$1.log" 2>&1
}

state_of() { [ -f "$1" ] && head -1 "$1" | tr -d '\r' || true; }

task() {
  local phase="$1" file="$2"
  local id
  id="$(basename "$file" .md)"
  local report="$B/reports/$id.md"
  case "$(state_of "$report")" in
    *完成*) return 0 ;;
  esac
  agent "$DEV" "$id" dev TASK="$id" PHASE="$phase" || true
  settle "Finish what task $id left uncommitted"
  local try
  for try in 1 2 3; do
    case "$(state_of "$report")" in
      *受阻*) stop "$id 受阻，见 $report" ;;
    esac
    if gate "$id-$try"; then
      case "$(state_of "$report")" in
        *完成*)
          say "通过 $id"
          return 0
          ;;
      esac
      local why="The gate is green, but $report is missing or does not start with 状态：完成. Write the report as the task asks, and commit."
    else
      local why="The gate (scripts/build/check.sh) failed after your work. The end of its output:

\`\`\`
$(tail -60 "$OUT/logs/check-$id-$try.log")
\`\`\`

Find the cause and fix it (a flaky test is a bug: make it deterministic). Update the report, and commit."
    fi
    agent "$DEV" "$id-fix$try" dev TASK="$id" PHASE="$phase" EXTRA="$why" || true
    settle "Finish what the fix for task $id left uncommitted"
  done
  stop "$id 三次修不过门禁，见 $OUT/logs/check-$id-3.log"
}

phase() {
  local ph="$1"
  [ -e "$B/phases/$ph.done" ] && return 0
  [ -f "$B/phases/$ph.base" ] || {
    git rev-parse HEAD >"$B/phases/$ph.base"
    settle "Mark where phase $ph begins"
  }
  local base
  base="$(cat "$B/phases/$ph.base")"
  if [ ! -f "$B/phases/$ph.md" ]; then
    agent "$PLANNER" "$ph-plan" plan PHASE="$ph" || true
    settle "Finish what the plan for phase $ph left uncommitted"
    [ -f "$B/phases/$ph.md" ] || stop "$ph 的计划没写出来，见 $OUT/logs/$ph-plan.log"
  fi
  local f
  for f in $(ls "$B/tasks/$ph"-[0-9][0-9]-*.md 2>/dev/null | sort); do
    task "$ph" "$f"
  done
  local round
  for round in 1 2 3; do
    local review="$B/reviews/$ph-r$round.md"
    if [ ! -f "$review" ]; then
      agent "$PLANNER" "$ph-review$round" review PHASE="$ph" ROUND="$round" BASE="$base" || true
      settle "Finish what review $round of phase $ph left uncommitted"
    fi
    case "$(state_of "$review")" in
      *通过*)
        touch "$B/phases/$ph.done"
        settle "Phase $ph passes review"
        say "阶段 $ph 完成"
        return 0
        ;;
      *要改*) ;;
      *) stop "$ph 第 $round 轮审查没有结论，见 $OUT/logs/$ph-review$round.log" ;;
    esac
    for f in $(ls "$B/tasks/$ph-R$round"-[0-9][0-9]-*.md 2>/dev/null | sort); do
      task "$ph" "$f"
    done
  done
  stop "$ph 三轮审查仍未通过"
}

say "构建开始：$PHASES"
[ -z "$(git status --porcelain)" ] || stop "工作区不干净，先提交或清理"
for ph in $PHASES; do
  phase "$ph"
done
say "全部完成：$PHASES"
