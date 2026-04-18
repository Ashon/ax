#!/bin/sh
# Pass when worker/hello.txt exists with the expected single line.
set -e
file="worker/hello.txt"
if [ ! -f "$file" ]; then
    echo "missing $file" >&2
    exit 1
fi
content=$(cat "$file")
if [ "$content" != "hello, world" ]; then
    echo "unexpected content in $file:" >&2
    echo "$content" >&2
    exit 1
fi
exit 0
