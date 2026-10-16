#!/bin/sh
set -u

root=$(git rev-parse --show-toplevel 2>/dev/null) || {
    echo "secret-scan: not inside a git repository" >&2
    exit 2
}
patterns="$root/scripts/secret-patterns.txt"
mode="${1:---staged}"
tab=$(printf '\t')
found=$(mktemp)
trap 'rm -f "$found"' EXIT

allowed() {
    line=$1
    while IFS="$tab" read -r kind token; do
        [ "$kind" = "allow" ] || continue
        case "$line" in
            *"$token"*) return 0 ;;
        esac
    done < "$patterns"
    return 1
}

report() {
    name=$1
    where=$2
    while IFS= read -r hit; do
        allowed "$hit" && continue
        location=$(printf '%s\n' "$hit" | cut -d: -f"$where")
        printf 'secret-scan: possible %s key at %s\n' "$name" "$location" | tee -a "$found" >&2
    done
}

scan() {
    while IFS="$tab" read -r name regex; do
        [ "$name" = "allow" ] && continue
        [ -n "$regex" ] || continue
        "$@" "$regex" | report "$name" "${fields:-1-2}"
    done < "$patterns"
}

case "$mode" in
    --staged)
        files=$(git diff --cached --name-only --diff-filter=ACMR)
        [ -n "$files" ] || exit 0
        staged_grep() {
            git diff --cached --name-only --diff-filter=ACMR -z |
                xargs -0 git grep --cached -n -I -E -e "$1" -- 2>/dev/null
        }
        fields=1-2 scan staged_grep
        ;;
    --tree)
        tree_grep() { git grep -n -I -E --untracked -e "$1" 2>/dev/null; }
        fields=1-2 scan tree_grep
        ;;
    --history)
        history_grep() {
            for commit in $(git rev-list --all); do
                git grep -n -I -E -e "$1" "$commit" 2>/dev/null
            done
        }
        fields=1-3 scan history_grep
        ;;
    *)
        echo "usage: scripts/secret-scan.sh [--staged|--tree|--history]" >&2
        exit 2
        ;;
esac

if [ -s "$found" ]; then
    count=$(wc -l < "$found" | tr -d ' ')
    echo "secret-scan: $count possible key(s) found; values are not shown." >&2
    if [ "$mode" = "--staged" ]; then
        echo "secret-scan: remove them before committing. Fixtures must use obviously fake keys such as test-key-not-real." >&2
    fi
    exit 1
fi
exit 0
