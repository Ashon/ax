# Tasknote

Build a small task-tracking CLI split across two workspaces.

## Product

The final binary is `tasknote`.

Supported commands:

- `tasknote add <title>`
- `tasknote list`
- `tasknote done <id>`
- `tasknote export-markdown`

## Behavior

- Tasks are stored in a local `tasks.json` file in the current working directory.
- Task IDs start at `1` and increment by `1`.
- `add` trims surrounding whitespace from the title and persists the new task.
- `list` prints lines like `1. [ ] Write docs` and `2. [x] Ship release`.
- `done <id>` marks the matching task as done.
- `export-markdown` prints checklist lines like `- [ ] Write docs`.

## Ownership

- `core` owns pure task-domain logic and markdown rendering.
- `cli` owns file I/O, command parsing, and user-facing output.

## Constraints

- Do not modify the acceptance tests.
- Keep the core module dependency-free.

## Validation

- `cd core && go test ./...`
- `cd cli && go test ./...`
- `cd cli && go build ./cmd/tasknote`
