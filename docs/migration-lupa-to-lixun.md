# Migration: `lupa` → `lixun` (v0.2.0 → v0.3.0)

The project was renamed from **Lupa** to **Lixun** (利寻).

## Binary names

| Before | After |
|---|---|
| `/usr/bin/lupa` | `/usr/bin/lixun` |
| `/usr/bin/lupad` | `/usr/bin/lixund` |
| `/usr/bin/lupa-gui` | `/usr/bin/lixun-gui` |

## One-shot migration (Arch Linux example)

Stop the old daemon:

```
systemctl --user stop lupad.service
systemctl --user disable lupad.service
```

Install the new package (it conflicts with `lupa-bin`, so the old package is
replaced automatically):

```
sudo pacman -U packaging/arch/lixun-bin-0.3.0-1-x86_64.pkg.tar.zst
systemctl --user daemon-reload
systemctl --user enable --now lixund.service
```

Move existing config + index + cache (no backward-compatible paths are read):

```
mv ~/.config/lupa                  ~/.config/lixun
mv ~/.local/share/lupa             ~/.local/share/lixun    2>/dev/null || true
mv ~/.local/state/lupa             ~/.local/state/lixun    2>/dev/null || true
mv ~/.cache/lupa-runtime           ~/.cache/lixun-runtime  2>/dev/null || true
```

Rebind the KDE global hotkey (KDE portal remembers the old `lupa` app id):

```
systemctl --user restart xdg-desktop-portal.service
systemctl --user restart lixund.service
```

Then open System Settings → Shortcuts → Global Shortcuts, remove any stale
`Lupa` entry, and retrigger the Super+Space binding from a fresh `lixund`
session (see `crates/lixun-daemon/src/hotkeys.rs` for wire protocol).

## What changed

- Workspace crates: `lupa-*` → `lixun-*` (12 crates).
- Rust identifiers: `lupa_<name>` → `lixun_<name>`, `LupaIndex` → `LixunIndex`,
  `LupaSchema` → `LixunSchema`.
- IPC socket: `$XDG_RUNTIME_DIR/lupa.sock` → `…/lixun.sock`, fallback
  `/tmp/lupa-$UID.sock` → `/tmp/lixun-$UID.sock`.
- Config dir: `~/.config/lupa/config.toml` → `~/.config/lixun/config.toml`.
- GTK application_id: `hk.dkp.lupa.gui` → `app.lixun.gui`.
- systemd unit: `lupad.service` → `lixund.service`.
- Desktop entry: `lupa.desktop` (`Name=Lupa`) → `lixun.desktop` (`Name=Lixun`).
- Arch package: `lupa-bin` → `lixun-bin`, with `conflicts=("lupa" "lupa-bin")`.
- CSS classes: `.lupa-*` → `.lixun-*` (you will need to redo any custom
  `style.css` overrides).
- Arch `pacman` hotkey token prefix: `lupa_` → `lixun_` (XDG global shortcuts
  session/request tokens).

No backward-compatible fallback is read anywhere. The fingerprint of the
tantivy index schema is unchanged, so after moving
`~/.local/share/lupa → lixun` the existing index stays valid and does NOT
force a reindex.
