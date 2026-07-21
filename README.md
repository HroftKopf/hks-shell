# hks-shell

A personal Wayland desktop shell for [Niri](https://github.com/YaLTeR/niri),
written in Rust. Two components sharing one liquid-glass material:

1. **Launcher** — a macOS-Spotlight-style app launcher / search (global hotkey,
   fuzzy `.desktop` search, keyboard navigation, animated open/close).
2. **Top bar** — a thin interactive panel: active window, Niri workspaces &
   windows, and system modules (network, VPN, volume, RAM/SSD/CPU, date/time)
   with their own liquid-glass popups.

> Status: early prototype. Currently a single translucent glass surface with
> working drag; the launcher/bar UI and logic are being built up in stages.

## Architecture: client + compositor

The liquid-glass **optics are rendered by the compositor, not by this client.**
On Wayland a normal client cannot read the pixels behind its own surface, so the
blur / refraction / chromatic aberration / drop shadow are done compositor-side
and keyed to the surface's layer-shell namespace.

| Concern | Where |
| --- | --- |
| Surface shape, tint, light rim, drag, and (soon) text/icons/results/animations | **this repo** (`hks-shell` client) |
| Background blur, refraction, chromatic aberration, drop shadow | **niri-glass** compositor fork |

**Required compositor fork:** https://github.com/zaroutt/Niri-glass — a fork of
Niri that adds the `background-effect { liquid-glass { … } }` layer-rule this
shell depends on. The shell's launcher and bar both get the glass effect "for
free" by using matched namespaces + layer-rules (see `config/niri/`).

A future goal is an installer (or a small distro image) that provisions the
niri-glass fork, these configs, and the shell together.

## Repository layout

```
hks-shell/
├── Cargo.toml
├── config/
│   ├── default.toml          # shell config scaffold (knobs + defaults)
│   └── niri/hks-shell.kdl     # REQUIRED Niri fragment (blur + layer-rule + glass)
├── assets/
│   ├── icons/
│   └── shaders/
└── src/
    └── main.rs                # current prototype (SHM glass surface + drag)
```

Planned module split (added as code is extracted, not all present yet):
`src/{app, wayland, renderer/{glass,shaders,text,icons}, animation, launcher,
bar, popup, niri, system}`.

## Build & run

Requires a running **niri-glass** compositor (see above); on plain upstream Niri
the surface draws but without the glass optics.

```sh
cargo build
cargo run
```

Then include the Niri fragment from your personal Niri config so the compositor
applies the effect to this shell's surface:

```kdl
include "/absolute/path/to/hks-shell/config/niri/hks-shell.kdl"
```

Remove any inline `blur { }` / `layer-rule { match namespace="^hks-shell$" }`
blocks from your personal config first, to avoid duplicates.

## Configuration

`config/default.toml` documents the intended user-tunable knobs (launcher size,
radius, blur, refraction, edge width, tint, opacity, animation speed, bar
modules, hotkey, font). It is a scaffold — not yet read by the code. Glass optics
are currently tuned in `config/niri/hks-shell.kdl` (hot-reloaded by Niri).

## Target environment

Wayland · Niri (niri-glass fork) · NixOS · 3440×1440@180 · fractional scale ~1.2.

## Roadmap

Repo/architecture cleanup → extract a reusable `GlassSurface` → text input →
`.desktop` app search → results list + keyboard nav → open/close/resize
animations → top-bar surface → Niri data (workspaces/windows) → system modules →
interactive popups. Each stage stays compiling, runnable, and is its own commit.
