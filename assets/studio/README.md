# Studio UI Assets

Extracted from the locally installed Roblox Studio build:

- **Version folder:** `version-67c8b1dedbb74fed`
- **Source:** `%LOCALAPPDATA%\Roblox\Versions\version-67c8b1dedbb74fed\content\`
- **Extracted:** 2026-11-08

These assets are used to reproduce the Roblox Studio UI **pixel-exactly**.

> ⚠️ **Licensing:** These files are Roblox Corporation property (Roblox Studio art, Builder Sans /
> Source Sans Pro fonts, cursors, etc.). They are used here for local/personal fidelity work.
> Redistributing Roblox assets publicly has IP implications — review before any public release.

## Layout

```
assets/studio/
├── icons/            # content\studio_svg_textures  (7,425 PNGs, 1,059 unique icon names)
│   ├── Lua/          # panel-specific icons (Explorer, Toolbox, Terrain, StartPage, ...)
│   ├── Shared/       # shared icon sets (Ribbon, InsertableObjects, WidgetIcons, Cursors, ...)
│   └── manifest.json # generated index: base icon key -> { 1x, 2x, 3x } relative paths
├── fonts/            # StudioFonts (Builder Sans, Builder Mono, Fira Mono, Source Sans Pro)
└── textures/         # content\textures (UI chrome, cursors, MenuBar, ... 1,779 files)
```

### Icon naming

`<Category>/<Theme>/<Size>/<Name>[@2x|@3x].png`

- **Category** e.g. `Shared/Ribbon`, `Shared/InsertableObjects`, `Lua/Explorer`, `Lua/Toolbox`
- **Theme** `Dark` or `Light`
- **Size** `Standard`, `Large`, `Medium`, or `SI-Standard`
- **Scale suffix** none = 1x (e.g. 16x16), `@2x` (32x32), `@3x` (48x48)

`Shared/InsertableObjects/**/<Theme>/<Size>/<ClassName>.png` holds the **327 class icons** used by the
Explorer tree and Insert Object dialog.

`manifest.json` maps every unique icon (2,475 entries) to its available scale variants.

### Fonts included

- `BuilderSans-*` (Standard UI font)
- `BuilderMono-*` (monospace)
- `FiraMono-*` (script editor mono)
- `SourceSansPro-*` (legacy UI font)

`NotoSansCJK-*.ttc` (CJK, ~125 MB) was intentionally **excluded** to keep the repo light; copy it from the
install folder later if localization is added.

## Notes

- The in-game topbar icons (`textures/MenuBar/icon_chat.png`, `icon_leave.png`, ...) are present for
  reference, but the **in-game topbar itself is intentionally not reproduced** (per project decision).
- The Studio topbar (menu bar + ribbon) IS reproduced, and uses `icons/Shared/Ribbon/**`.
