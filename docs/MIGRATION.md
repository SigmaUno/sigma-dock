# SigmaDock crate migration

Starting with version 0.1.2, packages use the `sigmadock` namespace and the canonical homepage is https://sigmadock.dev. The GitHub repository remains https://github.com/SigmaUno/sigma-dock.

Previously published 0.1.0 and 0.1.1 packages remain available, unchanged and unyanked. Cargo cannot rename an existing package. Update dependency names and Rust imports to the new packages below; pin an old version if you need the previous API.

| Previous package | New package |
| --- | --- |
| sigma-dock-core | sigmadock-core |
| sigma-dock-store | sigmadock-store |
| sigma-dock-pty | sigmadock-pty |
| sigma-dock-git | sigmadock-git |
| sigma-dock-forge | sigmadock-forge |
| sigma-dock-agents | sigmadock-agents |
| sigma-dock-ports | sigmadock-ports |
| sigma-dock-mcp | sigmadock-mcp |
| sigma-dockerd | sigmadockd |
| sigma-dock-ui | sigmadock-ui |
| sigma-dock-cli | sigmadock-cli |
| sigma-dock-terminal | sigmadock-terminal |

Rust library imports change from `sigma_dock_*` to `sigmadock_*`. Install the daemon using `cargo install sigmadockd`; its executable is now `sigmadockd`. The MCP executable is `sigmadock-mcp`. There is no legacy daemon executable shim: update scripts that explicitly launch `sigma-dockerd` or `sigma-dock-mcp`.

All twelve renamed 0.1.2 packages are published on crates.io. Users of Cargo-installed binaries can update with:

```sh
cargo install sigmadockd --version 0.1.2
cargo install sigmadock-mcp --version 0.1.2
cargo install sigmadock-cli --version 0.1.2 --force
cargo install sigmadock-ui --version 0.1.2 --force
```

The CLI and UI need `--force` when replacing executables owned by the previous package names. Git, native build dependencies and agent CLIs remain separate prerequisites. Existing daemon processes continue using their original executable until they are stopped; finish workers before restarting. App-bundle users can replace `SigmaDock.app` normally; Homebrew preview users can run `brew upgrade --cask sigma-dock-preview` after the cask update.

The graphical executable remains `sigma-dock` and the command-line client remains `sdk` for compatibility. The app bundle remains `SigmaDock.app`. Existing `SIGMA_DOCK_*` environment variables, local data/config directories, daemon socket paths, database schema, IPC protocol and worktree branch prefixes remain unchanged. Upgrading does not move or delete projects, settings, session history or worktrees.

An already running compatible daemon can still be reached through the existing socket. Finish or archive live workers before replacing or restarting that daemon; installing a new executable does not migrate running PTYs into a new process. The renamed app discovers `sigmadockd` beside its executable or on PATH.

Release scripts publish the new packages in dependency order. Old published versions and release tags must never be moved or overwritten.
