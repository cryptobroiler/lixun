# Lixun 利寻

[![version](https://img.shields.io/badge/version-0.3.0-blue)](https://repo.dkp.hk/denis/lixun/releases)
[![license](https://img.shields.io/badge/license-MIT-green)](LICENSE)
[![platform](https://img.shields.io/badge/platform-Linux-lightgrey)]()
[![rust](https://img.shields.io/badge/rust-2024-orange)](https://www.rust-lang.org/)

A **Spotlight-style launcher for Linux**. Press `Super + Space`, start typing,
get instant results across applications, files, and email. Backed by a
local [Tantivy](https://github.com/quickwit-oss/tantivy) full-text index
and a long-lived daemon that keeps everything fresh via filesystem watchers.

---

## What it does

- **Apps** — all `.desktop` entries, launched with `GDK_BACKEND` respected.
- **Files** — everything under your home (minus sensible excludes) with
  full-text body extraction from PDF/DOCX/XLSX/PPTX/RTF/DOC/XLS/PPT/plain text.
- **Email (Thunderbird)** — reads the running profile's
  `global-messages-db.sqlite` directly; subjects, bodies, senders,
  recipients all searchable. Optional attachment indexing from mbox files.
- **Email (Maildir)** — any number of maildir roots (mutt/neomutt/offlineimap
  /isync/fdm-style layouts).
- **Calculator** — type `sqrt(16) + pi` and get `7.1415…` at the top.

Everything local. No telemetry, no cloud, no vendor.

---

## Install

### Arch Linux (binary package)

```sh
git clone https://repo.dkp.hk/denis/lixun.git
cd lixun
cargo build --workspace --release
cp target/release/{lixun,lixund,lixun-gui} /tmp/lixun-arch-tarball/
tar -C /tmp/lixun-arch-tarball -czf packaging/arch/lixun-0.3.0-x86_64.tar.gz .
cd packaging/arch && makepkg -f
sudo pacman -U lixun-bin-0.3.0-1-x86_64.pkg.tar.zst
systemctl --user enable --now lixund.service
```

### From source (any Linux)

```sh
cargo build --workspace --release
install -Dm755 target/release/lixund    /usr/local/bin/lixund
install -Dm755 target/release/lixun     /usr/local/bin/lixun
install -Dm755 target/release/lixun-gui /usr/local/bin/lixun-gui
install -Dm644 packaging/systemd/lixund.service \
  ~/.config/systemd/user/lixund.service
systemctl --user enable --now lixund.service
```

**Runtime dependencies:** `gtk4`, `gtk4-layer-shell`, `poppler` (PDF),
`zstd`, `sqlite`.
**Optional (extraction):** `libreoffice-fresh` (docx/xlsx/pptx),
`catdoc` or `antiword` (legacy .doc).

---

## Quick start

### Bind the global hotkey

Lixun uses the **XDG GlobalShortcuts portal** (works on KDE Plasma 6+, GNOME,
Hyprland, etc). On first run it registers a request for `Super+space`;
accept it in the portal dialog, or configure your compositor directly:

```toml
# ~/.config/lixun/config.toml
[keybindings]
global_toggle = "Super+space"
```

If the portal rejects the binding, write it in spec form: `LOGO+space`.

### Command-line usage

```sh
lixun toggle        # show/hide the GUI
lixun search <q>    # search without GUI (prints to stdout)
lixun reindex       # fire-and-forget full reindex (returns in ms)
lixun status        # daemon health, index stats, reindex progress
```

---

## Configuration

Copy the example config and edit:

```sh
mkdir -p ~/.config/lixun
cp docs/config.example.toml ~/.config/lixun/config.toml
```

Key toggles:

```toml
max_file_size_mb = 50
extractor_timeout_secs = 15

exclude = [".thunderbird", "target", "node_modules"]   # substring
exclude_regex = ['\.sqlite-wal$', '/target/(debug|release)/']

[ranking]
# scalar multipliers applied to each category's score

[keybindings]
global_toggle = "Super+space"
close = "Escape"

# ─── Mail plugins (presence of section = plugin loaded) ───────────────

[[maildir]]
id = "personal"
paths = ["~/Mail/INBOX", "~/Mail/Archive"]
open_cmd = ["neomutt", "-f", "{folder}"]

[thunderbird]
enabled = true
gloda_batch_size = 2500       # tick batch: smaller = lower memory, slower catch-up
attachments = true             # index mbox attachments (reindex-on-demand only)
# profile = "/path/to/thunderbird/XXX.profile"   # override auto-detect
```

See [`docs/config.example.toml`](docs/config.example.toml) for the full reference.

---

## Architecture

```
┌────────────────────────────────────────────────────────────────────┐
│  lixun-gui (GTK4 + layer-shell)   ◄──── unix socket ────►  lixund  │
│                                                                    │
│  lixun (CLI)                      ◄──── unix socket ────►          │
└────────────────────────────────────────────────────────────────────┘
                                                               │
                                                               ▼
    ┌─────────────────┐    ┌──────────────────┐    ┌──────────────────────┐
    │  lixun-sources  │───►│  lixun-indexer   │───►│   lixun-index         │
    │  (fs, apps,     │    │  (writer task,   │    │   (Tantivy wrapper)   │
    │   plugin trait) │    │   tick scheduler)│    │                       │
    └─────────────────┘    └──────────────────┘    └──────────────────────┘
            ▲
            │ inventory::submit!
            │
    ┌───────┴────────────────────────────┐
    │  lixun-source-thunderbird  (gloda + attachments)
    │  lixun-source-maildir
    │  … (add your own: see below)
    └────────────────────────────────────┘
```

- **Daemon (`lixund`)** — owns the Tantivy writer, runs tick scheduler,
  watches filesystem via `notify`, serves IPC.
- **GUI (`lixun-gui`)** — GTK4 window with `gtk4-layer-shell`, spawned on
  demand by the daemon. Fully stateless; re-spawnable.
- **CLI (`lixun`)** — thin IPC client.
- **Plugin registration** — `inventory::submit!` + anchor crate pattern.
  Adding a plugin: new crate with `PluginFactory` impl → add to
  `lixun-plugin-bundle` Cargo features → nothing else to touch in the daemon.

### Plugin lifecycle

A source plugin is loaded **if and only if** its config section is present:

- `[thunderbird]` → `lixun-source-thunderbird` registered
- `[[maildir]]` → `lixun-source-maildir` registered (one instance per block)
- Nothing → plugin stays dormant, zero runtime cost

No code in `lixund` names any plugin. The daemon iterates
`inventory::iter::<PluginFactoryEntry>` and hands each factory its
matching TOML table.

---

## Development

```sh
cargo build  --workspace
cargo test   --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

### Layout

| Crate | Role |
|---|---|
| `lixun-core` | Shared types (`Document`, `Query`, `Hit`, …) |
| `lixun-ipc` | Unix-socket codec + request/response schema |
| `lixun-index` | Tantivy schema + writer wrapper |
| `lixun-extract` | File-format → text extractors |
| `lixun-sources` | `PluginFactory` trait + built-in `fs`/`apps` sources |
| `lixun-indexer` | Writer task, tick scheduler, fs watcher glue |
| `lixun-daemon` | IPC server, hotkey, startup, systemd glue |
| `lixun-gui` | GTK4 launcher window |
| `lixun-cli` | `lixun` command |
| `lixun-source-maildir` | Maildir plugin |
| `lixun-source-thunderbird` | Gloda + mbox attachments |
| `lixun-plugin-bundle` | Linker anchor (holds `use lixun_source_X as _`) |

### Writing a new source plugin

1. New crate `lixun-source-xxx` depending on `lixun-core` + `lixun-sources`.
2. Implement `PluginFactory` (parses its TOML section → returns one or more
   `PluginInstance`s).
3. `inventory::submit!` at crate root with a `PluginFactoryEntry`.
4. Add it as an optional dep in `lixun-plugin-bundle/Cargo.toml` under a feature.

Zero changes to `lixund`. Config-driven, auto-registered.

---

## Performance notes

- **Writer heap:** 32 MiB (bounded by Tantivy).
- **Memory after full reindex of ~500k documents:** ~260 MiB RSS (jemalloc).
  `systemd MemoryPeak` will show much higher — that's page cache attributed
  to the cgroup, not daemon heap. Check `/proc/$(pgrep lixund)/status` for
  the real number.
- **Fire-and-forget reindex:** `lixun reindex` returns in ~6 ms; daemon
  does the work in the background. Watch progress via `lixun status`.
- **gloda batch size:** tune `[thunderbird].gloda_batch_size` to trade off
  catch-up latency vs peak memory. Default 2500 is balanced for a 255k-message
  gloda database.

---

## Migration from Lupa

Previous versions shipped under the name **Lupa**. If you are upgrading from
`v0.2.x`, follow [`docs/migration-lupa-to-lixun.md`](docs/migration-lupa-to-lixun.md):
binary names, config dir, IPC socket, systemd unit, GTK app id, and
Arch package all changed.

---

## License

MIT. See [`LICENSE`](LICENSE) (when present) or the `license` field in
`Cargo.toml`.
