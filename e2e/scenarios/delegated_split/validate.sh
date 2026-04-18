#!/bin/sh
# Pass when both workspaces produced their expected files.
set -e

check() {
    file=$1
    expected=$2
    if [ ! -f "$file" ]; then
        echo "missing $file" >&2
        return 1
    fi
    actual=$(cat "$file")
    if [ "$actual" != "$expected" ]; then
        echo "unexpected content in $file:" >&2
        echo "$actual" >&2
        return 1
    fi
    return 0
}

check "alpha/result.txt" "alpha"
check "beta/result.txt" "beta"
exit 0
