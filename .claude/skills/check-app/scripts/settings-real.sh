#!/usr/bin/env bash
# The user's real settings files, as Git Bash sees them (programs started from the Claude desktop
# app write to a packaged copy instead: gm.ps1's Backup-GmSettings / Compare-GmSettings check that
# view). Records their hashes before a check and compares after; never writes the user's files.
#
#   .claude/skills/check-app/scripts/settings-real.sh backup|compare
set -euo pipefail
root="$(cd "$(dirname "$0")/../../../.." && pwd)"
out="$root/target/agent/check"
settings="$APPDATA/Game Manager"
mkdir -p "$out"

hashes() {
    [ -d "$settings" ] || return 0
    (cd "$settings" && find . -type f \( -name '*.json' -o -path './covers/*' \) -print0 | sort -z | xargs -0 -r sha1sum)
}

case "${1:-}" in
backup)
    hashes > "$out/settings-real.sha1"
    echo "recorded $(wc -l < "$out/settings-real.sha1") real settings files"
    ;;
compare)
    if diff <(cat "$out/settings-real.sha1") <(hashes) > "$out/settings-real.diff"; then
        echo "settings unchanged (real files)"
    else
        echo "settings CHANGED (real files; < before, > now):"
        sed 's/^/  /' "$out/settings-real.diff"
    fi
    ;;
*)
    echo "usage: $0 backup|compare" >&2
    exit 2
    ;;
esac
