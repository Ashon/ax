#!/bin/sh
# Pass when greeter/hello.sh exists + prints the expected line AND
# the daemon's persisted task state carries the Completion Reporting
# Contract marker ("remaining owned dirty files="). The marker check
# is the whole reason this scenario exists: it proves the worker's
# update_task(Completed) went through the contract-enforced path
# instead of being silently accepted.
set -e

file="greeter/hello.sh"
if [ ! -f "$file" ]; then
    echo "missing $file" >&2
    exit 1
fi
if [ ! -x "$file" ]; then
    echo "$file exists but is not executable" >&2
    exit 1
fi

out=$("$file")
if [ "$out" != "hello, ax" ]; then
    echo "unexpected output from $file:" >&2
    printf '%s\n' "$out" >&2
    exit 1
fi

# The daemon persists tasks to tasks-state.json beside its socket
# (the socket lives one level above the project). If the harness
# relocates state, update this path — keep the scenario honest
# rather than silently passing on a stale file.
state="../tasks-state.json"
if [ ! -f "$state" ]; then
    echo "no daemon task state at $state (wrong sandbox layout?)" >&2
    exit 1
fi
if ! grep -q "remaining owned dirty files=" "$state"; then
    echo "completion marker missing from $state" >&2
    echo "-- task state snapshot --" >&2
    cat "$state" >&2
    exit 1
fi

exit 0
