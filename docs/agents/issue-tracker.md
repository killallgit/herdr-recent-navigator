# Issue tracker: Beads

Issues and specs for this repo live in **beads** (`bd`), backed by a local Dolt database. Use `bd` for all issue operations. Run `bd prime` for the full command reference.

## Conventions

- **Create an issue**: `bd create --title="..." --description="Why this exists and what needs to be done" --type=task|bug|feature --priority=2` (priority is 0-4; 0 = critical, 4 = backlog)
- **Read an issue**: `bd show <id>`
- **List issues**: `bd list --status=open`, `bd ready` (unblocked work), or `bd search <query>`
- **Comment on / annotate an issue**: `bd update <id> --append-notes="..."` (`--notes` replaces existing notes)
- **Apply / remove labels**: `bd update <id> --add-label <label>` / `--remove-label <label>`
- **Close**: `bd close <id> --reason="..."`
- **Hierarchy**: `bd create ... --parent=<id>` for a child under an epic or parent issue
- **Dependencies**: `bd dep add <issue> <depends-on>`

Never use `bd edit` (it opens `$EDITOR` and blocks the agent). Do not track work in TodoWrite, TaskCreate, or markdown TODO lists.

## When a skill says "publish to the issue tracker"

Create a beads issue with `bd create`.

## When a skill says "fetch the relevant ticket"

Run `bd show <id>`.

## Git

Beads sync uses `refs/dolt/data` on the git remote. Do not commit, push, or run Dolt remote sync unless asked.
