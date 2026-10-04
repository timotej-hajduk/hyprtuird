# hyprtuird

A small Rust terminal UI for moving Hyprland workspaces between physical monitors.

It talks to Hyprland directly through the command Unix socket at
`$XDG_RUNTIME_DIR/hypr/$HYPRLAND_INSTANCE_SIGNATURE/.socket.sock`, falling back
to `/tmp/hypr/$HYPRLAND_INSTANCE_SIGNATURE/.socket.sock` when needed. Each
Hyprland request opens a fresh socket connection, writes one command, reads the
response, and closes the connection.

Workspace moves use Hyprland's Lua dispatcher syntax and require a session using Lua configuration (Hyprland 0.55 or newer).

## Install or update

From this project directory:

```sh
cargo install --path . --force --locked
```

This builds a release binary and installs it in Cargo's bin directory (normally
`~/.cargo/bin/hyprtuird`). Close any running copy and launch `hyprtuird` again to
use the update. Desktop launchers that run `hyprtuird` use the installed binary.

## Usage

Run it from inside a Hyprland session:

```sh
cargo run
```

Keys:

- `Up`/`Down` or `k`/`j`: move selection
- `Tab`: switch between workspaces and monitors
- `Enter`: move selected workspace to selected monitor
- `r`: refresh Hyprland state
- `q` or `Esc`: quit
