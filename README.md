# qc

`qc` is a small interactive command runner built in Rust on top of `reedline`.

It lets you:

- define named shortcuts in `shortcuts.txt`
- execute raw shell commands directly
- get inline autosuggestions while typing
- highlight exact, prefix, and fuzzy matches differently
- persist non-shortcut commands in `history.txt`
- use templated shortcuts with runtime prompts such as `{app}` or `{pod}`
- reload config and shortcuts without restarting
- require confirmation for obviously dangerous commands

## Features

- Interactive prompt powered by `reedline`
- Fish-style inline hinting for shortcut names and saved history
- Three match modes in the editor:
  - exact match: highlighted in light green italics
  - prefix match: matched portion italic, unmatched portion dimmed
  - fuzzy subsequence match: highlighted in yellow italics
- Shortcut definitions loaded from `shortcuts.txt`
- Template placeholders in shortcut commands using `{name}` syntax
- Persistent history for raw commands in `history.txt`
- Deduplicated history entries
- Configurable history retention in `config.txt`
- Built-in commands:
  - `:help` / `:?`
  - `:doctor`
  - `:set dry-run on|off`
  - `:profile list`
  - `:profile use <name>`
  - `:completion bash|powershell`
  - `:shortcuts` / `:s`
  - `:shortcuts tag <tag>`
  - `:shortcuts add <name>[tag1,tag2]=<command>`
  - `:shortcuts del <name>`
  - `:history` / `:h`
  - `:history ranked`
  - `:history add <command>`
  - `:history pin <index>` / `:history unpin <index>`
  - `:history del <index>` / `:history del <start-end>`
  - `:history dedupe`
  - `:history clear`
  - `:find <text>` / `:find! <text>` / `:find run <index>`
  - `:export <file>` / `:import <file>` / `:undo`
  - `:reload` / `:r`
  - `:exit` / `:quit` / `:q`
- Safety prompt for suspicious commands such as `rm -rf`, `del /f`, `drop table`, etc.

## Requirements

- Rust toolchain with Cargo
- A shell available on your platform
  - Windows: `cmd` or `%COMSPEC%`
  - Unix-like systems: `sh`

## Build And Run

Run directly with Cargo:

```bash
cargo run
```

Or build a binary first:

```bash
cargo build --release
```

## How It Works

When `qc` starts, it loads:

- `config.txt` for application settings
- `shortcuts.txt` for named shortcuts
- `history.txt` for previously executed raw commands

It then builds a live hint list from shortcut names plus history entries.

At the prompt:

- entering a shortcut name runs the mapped command
- entering any other text runs it as a raw shell command
- raw commands are appended to `history.txt` if history is enabled
- shortcuts are not written to `history.txt`

## Usage

### Run A Shortcut

If `shortcuts.txt` contains:

```text
kube-dev=kubectl config use-context dev-cluster
```

then typing this at the prompt:

```text
kube-dev
```

runs:

```text
kubectl config use-context dev-cluster
```

### Run A Raw Command

Typing a command that is not a shortcut runs it through the platform shell.

Example:

```text
kubectl get pods -A
```

On Windows this is executed with `cmd /C`.
On Unix-like systems this is executed with `sh -c`.

### Use Template Shortcuts

Shortcuts support optional placeholder variants. **Shortcuts without placeholders execute immediately when you type the name and press Enter.**

Placeholder syntax:

- `{name}` required value
- `{name?default}` optional value with default
- `{name!}` sensitive value (hidden input, never stored in placeholder history)
- `{name?default!}` sensitive with default

Examples without placeholders (execute immediately):

```text
kube-dev[k8s]=kubectl config use-context dev-cluster
kube-prod[k8s]=kubectl config use-context prod-cluster
```

Examples with placeholders (prompt for values):

```text
kube-tail[k8s,debug]=kubectl logs deployment/{app} -n {namespace?default} --tail={lines?200}
api-call[prod]=curl -H "Authorization: Bearer {token!}" https://api.example.com
```

When you run a templated shortcut (one with placeholders), `qc` prompts for each field and can reuse remembered values for non-sensitive placeholders.

If the same placeholder appears multiple times in one command, it is only prompted once.

## Built-In Commands

Built-ins are reserved under the `:` namespace so they do not collide with normal shell commands.

### `:help` / `:?`

Shows the built-in command list.

### `:shortcuts` / `:s`

Prints all loaded shortcuts in `name = command` format.

### `:shortcuts add <name>=<command>`

Adds a new shortcut, or updates it if the shortcut name already exists.

Example:

```text
:shortcuts add kube-ns=kubectl config set-context --current --namespace={ns}
```

### `:shortcuts del <name>`

Deletes a shortcut by name.

Example:

```text
:shortcuts del kube-ns
```

### `:history` / `:h`

Prints the current contents of `history.txt` with line numbers.

### `:history add <command>`

Adds a command directly to history (respecting history deduplication and size limits).

Example:

```text
:history add kubectl get pods -A
```

### `:history del <index>`

Deletes a history entry by 1-based index as shown by `:history`.

Example:

```text
:history del 3
```

### `:history clear`

Clears all persisted history entries.

### `:reload` / `:r`

Reloads:

- `config.txt`
- `shortcuts.txt`
- the in-memory hint list

Use this after editing config or shortcut definitions while `qc` is still running.

## Hinting And Highlighting

Hints are built from:

- all shortcut names
- all persisted raw history entries

Behavior:

- blank input shows no hint
- prefix matches show an inline suggestion suffix
- exact matches are highlighted in light green italics
- partial prefix matches show matched text in italics and the rest dimmed
- fuzzy matches use subsequence matching and are highlighted in yellow italics

Example fuzzy match:

- typing `klg` can still visually match something like `kube-logs`

## Configuration

Configuration lives in `config.txt` using a simple `key=value` format.

Current supported keys:

```text
max_history_items=100
safety_policy=confirm
dry_run=false
active_profile=default
```

### `max_history_items`

- `-1`: unlimited history
- `0`: disable history entirely
- positive integer: keep only the newest `N` raw commands

### `safety_policy`

- `warn`: warn and continue
- `confirm`: ask before dangerous commands
- `block`: block dangerous commands unless `--force` is present

### `dry_run`

- `true`: command execution is skipped after preview/safety checks
- `false`: normal behavior

### `active_profile`

- `default`: uses `shortcuts.txt`
- any other value (for example `prod`): uses `shortcuts.prod.txt`

If history is disabled, `history.txt` is cleared on startup and no raw commands are stored.

## Shortcut File Format

Shortcuts are defined in `shortcuts.txt`.

Rules:

- one shortcut per line
- format is `name=command`
- blank lines are ignored
- lines starting with `#` are ignored
- both `name` and `command` must be non-empty

Example:

```text
# name=command
# Use {placeholder} to prompt for arguments at runtime.
kube-dev=kubectl config use-context dev-cluster
kube-logs=kubectl logs deployment/{app} --follow
kube-restart=kubectl rollout restart deployment/{app}
kube-exec=kubectl exec -it {pod} -- /bin/bash
```

## History Behavior

History is stored in `history.txt`.

Important details:

- only raw commands are stored
- shortcut names are not stored
- duplicate entries are ignored
- history may be pruned on startup if it exceeds the configured limit
- `:history` reads from the current file contents

## Dangerous Command Confirmation

Before executing a command, `qc` checks whether it contains a known dangerous pattern.

Current patterns include substrings such as:

- `rm -rf`
- `rm -fr`
- `del /f`
- `del /s`
- `del /q`
- `format `
- `mkfs`
- `dd if=`
- `drop table`
- `drop database`
- `truncate table`
- `:(){:|:&};:`

If a command matches one of these patterns, `qc` asks for confirmation before executing it.

Example:

```text
  Warning: 'rm -rf /tmp/demo' looks dangerous. Continue? [y/N]:
```

Any answer other than `y` or `yes` aborts the command.

## Files

The application currently uses these project-root files:

- `config.txt`: configuration
- `shortcuts.txt` and optional `shortcuts.<profile>.txt`: shortcut definitions
- `history.txt`: persisted raw command history
- `history_pins.txt`: pinned history entries
- `history_usage.txt`: command usage counters for ranking
- `placeholder_values.txt`: remembered non-sensitive placeholder values
- `audit.log`: append-only command audit trail
- `qc-completion.bash` / `qc-completion.ps1`: generated completion scripts

## Error Handling

`qc` uses `anyhow` for error reporting.

Examples of failures you may see:

- invalid `config.txt` lines not using `key=value`
- unknown config keys
- invalid `max_history_items` values
- invalid shortcut lines not using `name=command`
- failed shell execution

If `shortcuts.txt` cannot be loaded, `qc` starts anyway and prints a warning.

## Example Session

```text
$ cargo run
> :shortcuts
  kube-dev = kubectl config use-context dev-cluster
  kube-logs = kubectl logs deployment/{app} --follow
  kube-restart = kubectl rollout restart deployment/{app}
  kube-exec = kubectl exec -it {pod} -- /bin/bash

> kube-logs
  app: my-api
Running kube-logs -> kubectl logs deployment/my-api --follow

> kubectl get pods

> :history
    1  kubectl get pods

> :reload
Configuration reloaded.
```

## Development

Useful commands:

```bash
cargo check
cargo run
```

## Current Limitations

- shortcut matching is exact by name when executing
- fuzzy matching is visual only; it does not auto-select or auto-run a shortcut
- template arguments are always prompted interactively
- there is no shell escaping or argument validation for placeholder values
- only one configuration key is currently supported

## License

No license file is currently included in this repository.