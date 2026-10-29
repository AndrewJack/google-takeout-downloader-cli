#!/usr/bin/env bash
# PreToolUse(Bash) hook: main is PR-only for Claude (the user may commit to main). Blocks `git commit` while on main and any
# `git push` that would update main. Exit code 2 blocks the tool call and shows stderr to Claude.
#
# This parses command text, so it is a best-effort early warning (aliases, quoting or
# `sh -c` can slip past it). The enforcing layer is the git hooks in .githooks/, which see
# the resolved refs; this hook also stops attempts to skip them.
set -euo pipefail

input=$(cat)
cmd=$(jq -r '.tool_input.command // empty' <<<"$input")
cwd=$(jq -r '.cwd // empty' <<<"$input")
[[ -z $cmd ]] && exit 0

block() {
  echo "Blocked: $1. main only changes through PRs: create a branch (git switch -c <name>), commit there, push it, and open a PR (gh pr create)." >&2
  exit 2
}

# Skipping or redirecting the git hooks would disable the real enforcement.
# Setting core.hooksPath to .githooks itself (the repo default) is fine.
hooks_override=$(grep -Eo 'core\.hooksPath[= ]+[^ ;&|-][^ ;&|]*' <<<"$cmd" | grep -Ev '^core\.hooksPath[= ]+\.githooks/?$' || true)
if [[ -n $hooks_override ]] || grep -Eq -- '--no-verify|--unset(-all)? +core\.hooksPath|GIT_CONFIG_(PARAMETERS|COUNT|KEY)|CLAUDECODE|(^|[^[:alnum:]_])env( +-[^ ]*)* +(-[[:alnum:]]*i[[:alnum:]]*|-|--ignore-environment)( |$)' <<<"$cmd"; then
  echo "Blocked: this command skips or overrides the repo's git hooks (.githooks/), which enforce the PR-only workflow for main." >&2
  exit 2
fi

is_main() { [[ $1 == main || $1 == refs/heads/main ]]; }

# Branch as of each segment; updated by switch/checkout earlier in the same command.
switched=""

# Check each simple command in a compound one (&&, ||, ;, |, newlines).
while IFS= read -r seg; do
  read -ra w <<<"$seg" || true
  [[ ${#w[@]} -gt 0 && ${w[0]} == git ]] || continue

  # Skip git's global options, honouring -C <dir> for branch detection.
  dir=${cwd:-.}
  i=1
  while ((i < ${#w[@]})) && [[ ${w[i]} == -* ]]; do
    case ${w[i]} in
      -C)
        case ${w[i + 1]:-} in
          /*) dir=${w[i + 1]} ;;
          ?*) dir=$dir/${w[i + 1]} ;;
        esac
        ((i += 2))
        ;;
      -c) ((i += 2)) ;;
      *) ((i += 1)) ;;
    esac
  done
  sub=${w[i]:-}
  branch=${switched:-$(git -C "$dir" branch --show-current 2>/dev/null || true)}

  case $sub in
    switch | checkout)
      # Last non-flag argument is the target branch (e.g. `switch -c feat`, `checkout -b feat`).
      # Anything after `--` is a path (`checkout -- file`), which doesn't change branch.
      for arg in "${w[@]:i+1}"; do
        [[ $arg == -- ]] && break
        [[ $arg == -* ]] || switched=$arg
      done
      ;;
    commit)
      for arg in "${w[@]:i+1}"; do
        [[ $arg =~ ^-[a-zA-Z]*n[a-zA-Z]*$ ]] && block "git commit -n skips the repo's git hooks"
      done
      [[ $branch == main ]] && block "git commit on main"
      ;;
    push)
      positional=()
      for arg in "${w[@]:i+1}"; do
        case $arg in
          --all | --mirror) block "git push $arg includes main" ;;
          -*) ;;
          *) positional+=("$arg") ;;
        esac
      done
      # No refspec: pushes the current branch.
      if ((${#positional[@]} <= 1)); then
        [[ $branch == main ]] && block "git push from main"
        continue
      fi
      for ref in "${positional[@]:1}"; do
        ref=${ref#+}
        dst=${ref##*:}
        is_main "$dst" && block "git push to main"
        [[ $dst == HEAD && $branch == main ]] && block "git push of HEAD while on main"
      done
      ;;
  esac
done < <(sed -E 's/(&&|\|\||;|\|)/\n/g' <<<"$cmd")

exit 0
