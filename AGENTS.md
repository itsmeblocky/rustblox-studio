# AGENTS.md — Rustblox Studio

Rustblox Studio is a **Roblox Studio remake written in Rust**. This file records the project vision,
locked-in decisions, architecture, and open questions so any agent picking up work has full context.

---

## Vision

A Rust-native recreation of Roblox Studio with:

1. **One-click `.rbxl` import** — drop a Roblox place file in, convert it to Rustblox's own format.
2. **Steam export** — turn a converted game into a **standalone Rust game compiled to a single `.exe`**,
   ready to upload to Steam (via steamcmd).
3. **100% pixel-exact Roblox Studio UI** — full Studio chrome *including* the topbar (menu bar + ribbon).
   The only UI element removed is the **in-game topbar overlay** (the Roblox logo + chat icon shown at the
   top of the game screen).
4. **Full Roblox Studio plugin support** — run real community plugins (`.rbxm`/`.rbxmx`, Plugin API, dock widgets).
5. **Roblox account login via OAuth** — browse the Toolbox and install your owned plugins.
6. **Built-in P2P collaboration** rendered *inside the UI* (not a separate service/panel).

---

## Hard Decisions (locked in)

| Topic | Decision |
|---|---|
| Steam target | Standalone Rust game, **porting the game onto a single exe**. |
| Studio UI fidelity | **100% exact to Roblox Studio, including the topbar** (menu bar + ribbon). Only the **in-game topbar** (Roblox logo + chat icon) is removed. |
| Studio UI toolkit | **Qt** (Rust UI shell built with Qt; binding/crate + Qt version TBD). |
| In-game settings | Provided by `SettingsService` (scriptable) + developer-built GUIs. |
| `SteamworksService` access | **Server-only** (safety: prevent clients self-granting achievements). |
| Collaboration | **Built-in Studio GUI feature**, *not* a Luau service. Rendered inside Explorer / Properties / Viewport. |
| Toolbox / account scope | **Full Toolbox access** via Roblox OAuth (not just owned plugins). |
| Roblox format | `.rbxl` / `.rbxlx` place files (binary + XML). |

---

## Architecture

```
rustblox-studio/
├── Cargo.toml                 (workspace)
├── crates/
│   ├── rbx-core/              # data model: Instance tree, CFrame, Color3, command/undo
│   ├── rbx-player/            # standalone game runtime → single exe (Steam build)
│   ├── rbx-studio/            # toolkit-agnostic design data: icons, tokens, layout, strings, ribbon
│   ├── rbx-studio-app/        # Qt UI shell (bootstrap) — consumes rbx-studio data
│   ├── rbx-render/            # wgpu renderer (shared by studio + player)
│   ├── rbx-gui/               # Roblox GUI instance renderer (shared) [planned]
│   ├── rbx-script/            # Luau host (mlua): data model, task scheduler, signals, services
│   └── rbx-collab/            # P2P sync transport for built-in collaboration [planned]
├── assets/
└── .tools/Qt/                 # local Qt dev install (gitignored, via aqtinstall)
```

### Crate responsibilities
- **rbx-core**: typed `Instance` enum/tree (use generational IDs via `slotmap`, not `Rc`), `CFrame`
  (position + quaternion), `Color3`, `Vector3`. Command pattern for undo/redo from day one.
  Serialization via `serde`. `mesh::parse_mesh` decodes Roblox `.mesh` v1–v5 (via `rbx_mesh`) into
  positions/normals/UVs; `Scene`/`Part` carry `mesh_file` (cached `.mesh`) and `decal` (cached image).
- **rbx-player**: loads the converted scene, runs the game loop (winit + wgpu), embeds Steamworks.
- **rbx-studio**: toolkit-agnostic Studio design/data crate. Feeds the Qt UI (and future tooling).
  - `icons` (`IconRegistry`): loads `assets/studio/icons/manifest.json`, resolves logical keys to
    `1x`/`2x`/`3x` with DPI-aware fallback, and provides `class_icon` / `widget_icon` / `ribbon_icon`.
    - Panel icons: `Shared/WidgetIcons/...` (Explorer, Properties, Toolbox, Output, CommandBar, ...).
    - Ribbon/topbar: `Shared/Ribbon/Dark/{Medium,Standard,...}` (e.g. `RibbonPlaySmall`,
      `RibbonSelect`, `RibbonMove`, `RibbonPart`). Undo/redo live under `Shared/Clipboard/...`.
    - Class icons: `Shared/InsertableObjects/...` (e.g. `Part`, `Script`, `Model`).
  - `theme` (`DesignTokens`): parses extracted Roblox Foundation tokens
    (`assets/studio/design/tokens/{Default,Classic}_{Dark,Light}.json`). Default Dark surfaces:
    `Surface_0 #121215`, `Surface_100 #191A1F`, `Surface_200 #202227`, `Surface_300 #272930`;
    content `#D5D7DD/#F7F7F8/#BCBEC8`, accent/link `#528BFF`; typography uses Builder Sans
    (e.g. `BodyMedium` 18px regular).
  - `layout` (`DockLayout`): parses Studio's Qtitan dock XML extracted from the registry to
    `assets/studio/layout/{edit,play,pserv}.xml`. Captures panel IDs, splitter/tab structure,
    dock sizes (`DockCX/DockCY`), active tabs, and floating-window rects.
  - `strings` (`StringBundle`/`Strings`): en-us/source UI strings extracted from built-in plugin
    `LocalizationTable`s (`assets/studio/strings/{ribbon,explorer,properties,toolbox}.json`).
  - `ribbon` (`RibbonDefinition`): reference-derived tab/group/control definition for static ribbon tabs
    (`assets/studio/design/ribbon/definition.json`); Plugins tab is dynamic.
  - Bins: `dump-icons`, `extract-tokens`, `dump-theme`, `dump-layout`, `dump-strings`, `dump-ribbon`.
- **rbx-studio-app**: **Qt UI shell** (bootstrap). Uses `qmetaobject` (QML bootstrap) and loads
  `DesignTokens` + `RibbonDefinition` from `rbx-studio`. Qt dev: `.tools\Qt\6.8.3\msvc2022_64`
  (installed via `aqtinstall`, gitignored). Build/run (MSVC host):
  `set QTDIR=.tools\Qt\6.8.3\msvc2022_64` + add `%QTDIR%\bin` to `PATH` (and `QT_PLUGIN_PATH=%QTDIR%\plugins`,
  `QML2_IMPORT_PATH=%QTDIR%\qml` for runtime), then `cargo run -p rbx-studio-app`. Current shell renders
  menu/ribbon-tab chrome and dock placeholders from extracted data; next: render ribbon groups/controls
  from `definition.json` with real icons, model docks from `edit.xml`, apply Builder Sans/Studio metrics,
  and evolve toward exact Qt Widgets/docking chrome as needed for fidelity.
- **rbx-render**: wgpu renderer — camera, meshes, selection highlights, grid, axis widget. Three
  front-ends, one draw path (`scene_pass::ScenePass`: pipeline, uniforms, culling/sort, upload, draw):
  - `renderer::ViewportRenderer` — headless offscreen target + `render_to_rgba` readback. Used by the
    QML `Image` bridge and PNG dumps. Slowest (GPU→CPU readback); now only a fallback.
  - `native_viewport` — detached winit window with its own swapchain on a dedicated thread
    (`AutoNoVsync`). Toolbar chip “Detach 3D”.
  - `embedded::EmbeddedViewport` — **default docked path**. A Win32 `WS_CHILD` window parented to the
    Qt shell window, own wgpu swapchain + render thread (`Fifo`, draws on demand). Its `WndProc` runs on
    Qt’s own message pump and edits the shared camera (`MA_NOACTIVATE`, so Qt keeps keyboard focus).
    QML (`studio.viewport_attach` / `viewport_set_rect` / `viewport_show`, property
    `studio.viewport_embedded`) positions it over the viewport item, insets the top 34 px so the chips
    row stays clickable, hides it behind the start page / Settings, and stops the readback timer.
    `sync_async` copies the embedded camera/fps back into the shell and drops it on a render-thread
    error so the QML fallback takes over. `ScenePass` rebuilds meshes only on scene edits.
  - Camera flight (`camera::Camera::fly` / `fly_input` / `fly_speed`): `W/S` move along the view axis,
    `D/A` strafe towards screen-right/left (`right = cross(look, up)`, matching the view matrix),
    `Q/E` move down/up along world Y, `Shift` = 4× speed. Speed scales with the zoom distance and
    `fly_input` ramps it from ~10 % to full over `FLY_RAMP_SECONDS` (0.6 s, smoothstep), resetting the
    ramp whenever input stops (`reset_fly` when the pointer leaves the viewport mid-flight). Keys are
    polled from the OS (`camera::keyboard_fly_axes`, `GetAsyncKeyState`) because the
    embedded child window never takes focus (`WS_EX_NOACTIVATE`); it arms while the pointer hovers the
    viewport (`win::cursor_over`) or the right mouse button is held, and the render loop wakes every
    33 ms while armed (`250 ms` idle). The detached winit window tracks key state itself in
    `about_to_wait`, and the QML readback path drives `studio.fly_camera(dt)` from a 16 ms timer.
  - `textures::TextureBank` — one `texture_2d_array` (14 procedural material layers at 256² + 48 decal
    slots, mips, anisotropy 8) shared by every front-end; `mesh::mesh_from_file` parses + caches `.mesh`
    geometry and normalizes it into the part's box.
- **rbx-gui**: renders Roblox GUI instances (`Frame`, `TextLabel`, `ImageLabel`, layouts, `UICorner`, etc.).
  Must be shared between studio plugin dock widgets and in-game `ScreenGui`.
- **rbx-script**: Luau VM host (mlua 0.12 `luau` + `error-send`). One VM per play-solo session,
  instance tree (`InstanceStore`), `Instance.new`/`FindFirstChild`/`WaitForChild`/`Changed`/`GetService`,
  Roblox datatype prelude (`Vector3`/`CFrame`/`Color3`/`UDim2`/`Enum`/...), `task` scheduler
  (`task.wait/spawn/delay/defer`, yields in pure Luau), `require` for `ModuleScript`s, and services
  (`PhysicsService` collision groups, `Humanoid` stubs, scriptable `SettingsService`, server-only
  `SteamworksService`). Tests: `cargo test -p rbx-script` (10 end-to-end host tests). The Qt shell
  drives it from the command bar (`studio.run_luau`) and steps it while playing (`studio.step_scripts`).
- **rbx-collab**: ops-based P2P sync (Steam P2P transport), Lamport clocks, host authority.

---

## UI Assets (extracted from Roblox Studio)

Real Studio assets are extracted into `assets/studio/` (see `assets/studio/README.md` for details):

- `assets/studio/icons/` — `content/studio_svg_textures` (7,425 PNGs, 2,475 manifest entries) +
  `manifest.json` mapping each icon key to its `1x`/`2x`/`3x` variants.
  - Pattern: `<Category>/<Theme Dark|Light>/<Size Standard|Large|Medium|SI-Standard>/<Name>[@2x|@3x].png`
  - `Shared/InsertableObjects/**` holds the **327 class icons** for the Explorer tree / Insert dialog.
  - `Shared/Ribbon/**` holds Studio topbar/ribbon icons (kept, since the Studio topbar is reproduced).
- `assets/studio/fonts/` — Builder Sans, Builder Mono, Fira Mono, Source Sans Pro.
  (Noto Sans CJK intentionally excluded — re-copy when localization is added.)
- `assets/studio/textures/` — `content/textures` UI chrome, cursors, `MenuBar` (in-game topbar icons kept
  for reference only, since the in-game topbar is not reproduced).
- `assets/studio/design/` — copied `RbxDesignFoundations` Lua sources + generated token JSON
  (`tokens/{Default,Classic}_{Dark,Light}.json`). Tokens are parsed textually (the shipped files stub
  `Color3.fromRGB` to green, so runtime evaluation is misleading).
- `assets/studio/layout/` — Qtitan dock layouts extracted from
  `HKCU\Software\Roblox\RobloxStudio\LayoutSettings\Docking\3` (`edit.xml`, `play.xml`, `pserv.xml`) and
  `window_state_beta_ribbon` / `window_geometry_ribbon` binaries.
- `assets/studio/strings/` — en-us/source localization bundles from built-in plugins
  (`ribbon.json`, `explorer.json`, `properties.json`, `toolbox.json`).
- `assets/studio/reference/` — Pywinauto-captured Studio window/ribbon screenshots
  (`ribbon_{Home,Avatar,UI,Script,Model}.png`, `studio_full.png`) used to verify UI.
- `assets/studio/design/ribbon/definition.json` — data-driven ribbon tab/group/control definition for
  static tabs (Home, Avatar, UI, Script, Model) derived from the captured screenshots + user breakdown;
  Plugins is dynamic/non-hardcoded. Loaded by `rbx_studio::ribbon::RibbonDefinition` (`dump-ribbon`).
- Source: local install `version-67c8b1dedbb74fed`. **Roblox-owned assets — personal/local use; review IP
  before public redistribution.**

### Reverse-engineering notes (Studio `version-67c8b1dedbb74fed`)
- **Chrome is Qt5 + Qtitan**: `Qt5*.dll`, `QtitanBase.dll`, `QtitanDocking.dll`,
  `QtitanOffice2016Style.dll` are present; **no `QtitanRibbon`**. The ribbon is Luau/React-drawn, not
  native Qt.
- **Built-in plugin source is stripped**: `BuiltInPlugins/.../Optimized_Embedded_Signature/*.rbxm` and
  `BuiltInStandalonePlugins/...` contain `ModuleScript` instances with zero `Source` and no embedded
  bytecode; the `SIGN` chunk is a small signature. Ribbon/group definitions (e.g.
  `BuiltInRibbonDefinition`, `TabControls/*`) therefore cannot be recovered from the install. Ribbon
  structure is reconstructed from instance tree names, localization keys/icons, Qtitan layout, and
  `CustomRibbonTabs/RibbonLayout.json`.
- **Authoritative ribbon tab order/visibility** (from `%LOCALAPPDATA%\Roblox\4286075344\CustomRibbonTabs\RibbonLayout.json`):
  Home, Avatar, UI, Script, Model, Plugins, plus custom tabs. `GenerateTab` exists in the ribbon plugin
  tree but is not in the default visible layout.
- **Exact dock arrangement** is recoverable: Qtitan XML in registry → `assets/studio/layout/*.xml`.
  Edit mode currently docks `edit_builtin_Toolbox.rbxm_Toolbox`, `rplg_sabuiltin_ExplorerPlugin.rbxm_ExplorerPlugin`,
  and `propertiesWidgetPanel`; the document area hosts `ideDocDocumentPanel`/script tabs. Output,
  Asset Manager, Command Bar, Terrain Editor, Tag Editor, etc. appear as named panels/layout refs.
- **Live introspection / capture**: Studio MCP was later connected (`Place1`, Edit, official bridge).
  CoreGui/PluginGuiService did not expose built-in ribbon internals; `screen_capture` ran but image
  results weren’t viewable in this session. **Pywinauto (Python 3.13, temp venv
  `%LOCALAPPDATA%\Temp\opencode\pywinauto-venv`)** was used to capture the Studio window
  (UIA backend, read-only). Static ribbon tabs Home/Avatar/UI/Script/Model were captured to
  `assets/studio/reference/` and encoded as `assets/studio/design/ribbon/definition.json` from the
  user’s screenshot-based breakdown. Icon mappings are best-effort (many resolved to
  `Shared/Ribbon/Dark/Medium/*`); refine against the reference PNGs. Plugins tab remains dynamic.

---

## CoreScripts & Player Scripts (canonical local source)

**Use this folder for ALL corescripts and player scripts** (authoritative source for player/runtime and in-game CoreGui behavior):

- Path: `C:\Users\blocky\AppData\Local\Roblox\Versions\version-63eb6055af4e4688\ExtraContent\scripts`
- Roblox client version: `version-63eb6055af4e4688` (local install; path/version may change with client updates — re-point as needed)
- Contents: **2,603 files** (~10.1 MB), mostly `.lua` (+ `.rbxp` bundles)
  - `CoreScripts/` — client/server core scripts and modules
    - Root loaders/entry: `StarterScript.lua`, `LoadingScript.lua`, `Connection.lua`, `ServerStarterScript.lua`, `CEVStarterScript.lua`, `CaptureModeModuleStarterScript.lua`, `Studio-StarterScript.lua`
    - `CoreScripts/CoreScripts/` — in-experience core UI/behavior (chat, emotes, prompts, notifications, voice, screenshots, etc.)
    - `CoreScripts/Modules/` — shared CoreScript modules (TopBar/Chrome, InGameMenu, Settings, DevConsole, EmotesMenu, PlayerList, VoiceChat, VR, TrustAndSafety, etc.)
    - `CoreScripts/ServerCoreScripts/` — server-side core scripts (leaderstats, social, dialog, VIP/teleport, voice, etc.)
  - `PlayerScripts/` — player character/control/camera scripts
    - `StarterPlayer/PlayerModule/`, `StarterPlayerScripts/PlayerModule.module*`, `StarterPlayerScripts/PlayerScriptsLoader.lua`
    - `StarterPlayerScriptsCommon/RbxCharacterSounds.lua` (+ folder)
    - Legacy/packaged variants: `StarterPlayerScripts_old/*`, `PlayerScripts.rbxp`, `PlayerScriptsLoader.rbxp`, `CameraScript.rbxp`, `ControlScript.rbxp`, etc.

**Usage notes:** load/reference these for `rbx-player` and `rbx-script` (Luau host) when implementing CoreGui, default player controls/camera, character sounds, and core game behavior - prefer this extracted/local source over reimplementing from scratch where behavior must match Roblox. **Roblox-owned scripts - personal/local use; review IP/redistribution constraints before bundling publicly.**

See [`docs/physics-scripts.md`](docs/physics-scripts.md) for the audit of the physics-relevant scripts
(avatar unification, PlayerModule Control/Camera, `RbxCharacterSounds`): **no Luau physics solver exists**;
scripts only configure/read the engine-side solver (`Humanoid:Move`, `PhysicsService` collision groups,
constraint properties), which Rustblox must implement natively (`rapier3d` plan).

---

## `.rbxl` Import Pipeline

- Parse `.rbxl`/`.rbxlx` with the **`rbx_binary` / `rbx_types` / `rbx_dom_weak`** crates (the same
  crates Rojo uses).
- Walk the `WeakDom` instance tree; extract `Part` (Position/Size/Color/CFrame), scripts, etc.
- Serialize to Rustblox's own format — **start with JSON** (human-readable/debuggable), add a binary
  format (`bincode`/`rkyv`) later.

Current skeleton (already drafted, *do not rebuild without approval*):
- `crates/rbx-core/src/lib.rs` — `Scene`, `Part`, `Vector3`, `import_rbxl()`, `import_model()` +
  `parts_bounds()` (Toolbox/`.rbxm` insert: `rbx_binary` or `rbx_xml` → instance tree + parts).
- `crates/rbx-player/src/main.rs` — reads `.rbxl`, prints parts, writes `*.rustblox.json`.
- NOTE: crate versions were mid-fix (`rbx_dom_weak` 4.x, `rbx_binary` 1.x) — compilation not yet verified.

---

## Services (Luau-facing)

### `SteamworksService` — **SERVER-ONLY**
```lua
local Steamworks = game:GetService("SteamworksService")
Steamworks:Init()
Steamworks:IsOverlayEnabled()
Steamworks:RequestCurrentStats()
Steamworks:UnlockAchievement("ACH_WIN")
Steamworks:ClearAchievement("ACH_WIN")
Steamworks:GetAchievement("ACH_WIN")
Steamworks:SetStat("kills", 42)
Steamworks:GetStat("kills")
Steamworks:StoreStats()
Steamworks:FileWrite("save.dat", data)
Steamworks:FileRead("save.dat")
Steamworks.AchievementUnlocked:Connect(function(name) ... end)
```
- Backed by `steamworks` crate. `SteamContext` wraps `Client` + `UserStats`.
- `run_callbacks()` each frame in the event loop.
- Offline mode: no-op gracefully when Steam isn't running.
- Achievement IDs embedded at build time by the converter.
- **Enforcement**: `GetService("SteamworksService")` returns `nil`/errors from a client context.
  Script threads are tagged `ExecContext::Server` (`Script`) or `ExecContext::Client` (`LocalScript`).

### `SettingsService` — scriptable, drives developer-built GUIs
```lua
local Settings = game:GetService("SettingsService")

-- Windowing
Settings:SetSetting("WindowMode", "Fullscreen")   -- Windowed | Fullscreen | WindowedFullscreen
Settings:SetSetting("Resolution", Vector2.new(1920,1080))
Settings:SetSetting("VSync", true)
-- Input
Settings:SetSetting("ShiftLock", true)
Settings:SetSetting("MouseSensitivity", 2.0)
Settings:SetSetting("CameraMode", "Classic")        -- Classic | Follow | Orbital
-- Rendering
Settings:SetSetting("Anisotropy", 16)              -- 1/2/4/8/16 (MLSS anisotropic)
Settings:SetSetting("FrameGeneration", "FSR")      -- Off | FSR | DLSS
Settings:SetSetting("FrameGenQuality", "Quality")  -- Quality | Balanced | Performance | UltraPerformance
Settings:SetSetting("AntiAliasing", "TAA")         -- Off | FXAA | MSAA | TAA
Settings:SetSetting("RenderScale", 1.0)

Settings:GetSetting(name)
Settings.SettingChanged:Connect(function(name, value) ... end)
Settings:GetWindowModes() / GetFrameGenerationModes() / GetAnisotropyLevels()
Settings:ApplyPreset("Performance")                -- or "Quality"
-- Optional helper returning a ready-made ScreenGui devs can clone/modify:
Settings:CreateDefaultSettingsGui()
```
- Validates + persists to `settings.json` (per-user, with game-provided defaults).
- Expensive ops (window mode, DLSS init) debounced.
- Client may **read** settings; **write is server-only**.
- Lets developers build their own in-game settings GUIs (independent of the removed in-game topbar).

---

## Collaboration (built-in Studio GUI, NOT scriptable)

- **No `CollaborationService`.** No dedicated panel. Rendered *inside* existing UI:
  - **Explorer**: peer avatars/colored dots on instances being edited.
  - **Properties**: field-level presence (peer color/user on each property), inline conflict highlight.
  - **Viewport**: peers' cursors, selection outlines with name labels, camera ghosts.
  - **Panel headers / slim status strip**: presence avatars + share/invite button.
  - **Context menus**: Jump to peer's view, Follow peer, Kick, Set role.
- **Transport**: Steam P2P (preferred) with raw UDP hole-punch + TURN relay fallback. Peers identified by
  SteamID (or local ID offline).
- **Sync model**: ops-based (InsertInstance / SetProperty / DeleteInstance / MovePart) with Lamport
  timestamps + vector clocks; LWW per property; structural conflicts flagged for manual resolution.
- **Host authority**: the peer who opened the place is host; auto-promote longest-connected peer on disconnect.
- **Presence**: cursor/selection/camera broadcast ~10 Hz.
- **Roles**: Owner / Editor / Viewer.
- **Max peers**: ~10–20.
- Lives in `rbx-studio` (via `rbx-collab`), integrated with the `rbx-core` command/undo system.

---

## Roblox Plugin Support

Plugins = `.rbxm`/`.rbxmx` models containing scripts + GUI objects, run as `Script` with a `Plugin` global.
API surface to implement:
- `plugin:CreateToolbar(name)` → `PluginToolbar`; `toolbar:CreateButton(id, tooltip, icon, text)`.
- `plugin:CreateDockWidgetPluginGui(...)` → `DockWidgetPluginGui` (custom panels).
- `plugin:CreatePluginAction(...)`, `plugin:CreatePluginMenu(...)`.
- `plugin:GetMouse()`, `plugin:Activate()`, `plugin:Deactivate()`.
- Selection: `plugin:GetSelected()`, `Selection:Set(...)`.
- `plugin:SetSetting(key, value)` / `GetSetting`.
- Advanced: `ImportFbxAnimation/Rig`, `PromptForExistingAssetId`, `SaveSelectedToRoblox`,
  `getcustomasset`, `LoadAsset`.

### Phasing
- **P1**: plugin loader + `plugin` object + toolbar buttons + simple actions (no custom GUI). ~30% of plugins.
- **P2**: `DockWidgetPluginGui` + core GUI instances (`Frame`, `TextLabel`, `TextButton`, `ImageLabel`) +
  `UIListLayout`/`UIPadding`/`UICorner`. Most UI plugins.
- **P3**: full GUI instance set, layouts, tweens, fonts, image assets.
- **P4**: edge-case APIs (FBX import, asset prompts, HTTP service for plugins).

### Key decisions
- Plugins run in a **separate sandboxed Luau VM** with a `plugin` context flag (gates `PluginSecurity` APIs).
- Plugin docks + in-game `ScreenGui` share **one GUI renderer** (`rbx-gui`) — build the GUI engine once.
- Plugins participate in undo/redo.

---

## Roblox OAuth + Toolbox

- **OAuth 2.0 Authorization Code + PKCE**, system browser → loopback callback (`127.0.0.1:<port>`).
- Store tokens in the OS keychain / Windows Credential Manager. Never log tokens.
- Scopes (from `secrets/roblox_oauth.json` `scopes`): `openid profile` for identity, plus `universe:read`
  and `universe.place:read` to list the experiences the user granted this app access to and load their
  metadata/thumbnails for editing. There is no OAuth scope that lists all owned universes; access is limited
  to experiences granted during consent (see implementation below). Changing scopes requires a **new sign-in**
  so the user can consent to the added permissions (refresh tokens keep their original grant).
- **Credentials location**: `secrets/roblox_oauth.json` (client id / secret / redirect URI / scopes). The
  folder is ignored by both `secrets/.gitignore` (`*` + `!.gitignore`) and the root `.gitignore`
  (`secrets/*`, `!secrets/.gitignore`). **Never** commit, log, or paste these values; the app reads them
  from disk at runtime only.
- **Anti-impersonation rule**: never spoof the Roblox Studio client (headers/fingerprint/UA) to obtain a
  session. Public endpoints are called anonymously with an honest `RustbloxStudio/0.1` User-Agent;
  identity goes through official OAuth.
- **Toolbox UI**: browse/search Models, Plugins, Audio, Images, Meshes; thumbnails + metadata; "Insert"
  drops instances into the Explorer/workspace. Owned/favorites tabs when signed in.
- **Plugin library**: fetch owned `Plugin` assets, "Install" → download `.rbxm` → load via plugin loader.

### Implementation status
- `crates/rbx-studio-app/src/roblox_api.rs` — anonymous `toolbox-service/v1/marketplace/{category}`
  search (categories: 10 Models, 40 Meshes, 3 Audio, 13 Images, 38 Plugins, 24 Animations) followed by
  `items/details?assetIds=` and `thumbnails.roblox.com/v1/assets` (thumbnails are decorative; a failure
  keeps the results). Requests use the honest `RustbloxStudio/0.1` User-Agent.
- OAuth login runs the Authorization Code + PKCE flow from a worker thread: system browser →
  `http://localhost:53682/callback` → `oauth/v1/token` → `oauth/v1/userinfo`. The refresh token and
  identity are stored in the **Windows Credential Manager** (`RustbloxStudio/RobloxOAuth`); access tokens
  stay in memory. `secrets/roblox_oauth.json` supplies client id/secret/redirect URI/scopes.
- **Experiences (editable games, Start page)**: `list_experiences` refreshes the session, calls
  `POST https://apis.roblox.com/oauth/v1/token/resources` (form `token`/`client_id`/`client_secret`,
  requires `universe:read`) to read `resource_infos[].resources.universe.ids` (only experiences granted at
  consent), fetches names/root places via `https://games.roblox.com/v1/games?universeIds=` (Bearer token),
  and thumbnails via `https://thumbnails.roblox.com/v1/games/multiget/thumbnails` (fallback
  `/v1/games/icons`). Opening an experience downloads the root place through Open Cloud asset delivery
  (`GET apis.roblox.com/asset-delivery-api/v1/assetId/{placeId}` with the linked `x-api-key` +
  `legacy-asset:manage`, same as Toolbox — so the key's account must own/be entitled to the place) and
  parses it with `rbx_core::import_place` (binary `.rbxl` or XML `.rbxlx`) to swap the scene, camera,
  Explorer tree, and window/place title. Place download is not exposed to OAuth bearer tokens, hence the
  API-key path.
- Qt bridge: `studio.toolbox_search(category, keyword)`, `studio.poll_async()` (QML timer drains worker
  state), `studio.login_roblox()`, `studio.restore_session()`, `studio.sign_out()`,
  `studio.refresh_experiences()`, `studio.open_experience(universeId, placeId, name)`; the Toolbox dock is a
  `GridView` of thumbnails with a category strip, search field and an account chip, and the Start page
  *Experiences* tab shows a clickable thumbnail grid of granted experiences. Signals:
  `experiences_changed`, `place_changed`, `explorer_load(treeJson)`.
- **Toolbox Insert (implemented, all asset types)**: `studio.insert_asset(assetId, name, assetType)` runs
  on a worker thread → `roblox_api::download_asset`
  (`GET apis.roblox.com/asset-delivery-api/v1/assetId/{id}` with `x-api-key` from the credential store →
  signed `location` → CDN fetch, gzip-aware, 128 MB cap) → on-disk cache →
  `insert::build` dispatch by asset type → `PendingInsert`. The GUI thread then centres the parts on the
  camera focus point (resting on the ground plane), appends them to `Scene.parts`, pushes the scene to the
  embedded/native viewports, and emits `explorer_insert(nodeJson, path)`; QML owns the Explorer model, so
  it splices the node under `Workspace` (Roblox-style `Name (2)` dedupe), expands it and selects it.
  Errors are mapped per HTTP code (401 = key/IP allowlist, 403 = missing `legacy-asset:manage`,
  404 = unknown asset, 409 = not entitled) and shown in the Toolbox footer + Output.

  | Type id | Asset | Instance produced | Scene effect |
  |---|---|---|---|
  | 10 | Model | imported tree (`rbx_core::import_model`) | every `BasePart` appended |
  | 3 | Audio | `Sound` + `SoundId` prop | no geometry (Explorer/Properties only) |
  | 1 / 13 | Image / Decal | thin `Part` + `Decal` child (`Texture`) | 8-stud-wide plate lying flat, aspect-correct depth from the decoded image |
  | 4 / 40 | Mesh / MeshPart | `MeshPart` + `MeshId` prop | real `.mesh` geometry, scaled to `Part.size` |
  | other | — | rejected with a clear message | — |

  `Part::decal` holds the cached image path and `Part::mesh_file` the cached mesh path, so the renderer
  resolves both through the same `TextureBank` / `mesh_from_file` cache. Per-instance property overrides
  (`InstanceNode.props`) flow to QML's `propertiesForSelected`, so `SoundId`/`MeshId`/`Texture` show real
  values instead of class defaults. Verified end-to-end: 6/6 free Toolbox models downloaded + parsed
  (e.g. "Noob NPC" → 7 parts).

  **Wrapper unwrapping** (`insert::build`): Toolbox image and mesh entries are often served as a
  `.rbxm`/`.rbxmx` container around the real asset, not as a raw PNG/`.mesh`. When a payload does not
  decode directly, `rbx_core::find_asset_reference` scans the parsed tree for a `Texture`/`TextureId`/
  `Image` (image) or `MeshId`/`Mesh`/`Mesh`-as-`MeshContent` (mesh) `Content` URI — including the
  `http://www.roblox.com/asset/?id=N` form — and the inner asset is downloaded in its place. If the
  wrapper carries no usable inner id (a `MeshPart` saved *without* `MeshId`, which the Toolbox does
  return), the wrapper model is inserted as a model instead of failing: the user still gets a sized,
  textured instance in the place. `rbx_xml` stores `<Content name="X">` as the property key `XContent`,
  which `find_asset_reference` accounts for.
- **Asset cache** (`crates/rbx-studio-app/src/asset_cache.rs`): `%LOCALAPPDATA%\RustbloxStudio\cache\assets\`
  with `models/ meshes/ textures/ audio/` subfolders, `<id>.<ext>` payload plus an `<id>.json` sidecar
  (asset type, content type, byte length, timestamp). `ext_for` sniffs magic bytes (`<roblox!` → `.rbxm`,
  `<` → `.rbxmx`, `version ` → `.mesh`, `CSGPHS` → `.csgphs`, PNG/JPEG/OGG/WAV/MP3 signatures) and falls
  back to the content type. A cache hit skips the network entirely; `describe()` (the folder path) is
  included in the `[toolbox insert]` log line. `find` ranks extensions (`ext_rank`) so a decoded
  `png`/`mesh` payload always wins over a `.rbxm` wrapper left behind by an earlier insert.
- **Mesh parsing** (`crates/rbx-core/src/mesh.rs`): `parse_mesh` wraps `rbx_mesh` 0.8.2 (`mesh` + `mesh-v1`
  features) for v1–v5, exposing positions/normals/UVs plus `bounds()`/`center()`. `rbx-render::mesh::
  mesh_from_file` caches parsed geometry per path, re-centres and normalizes it into a unit cube, flips
  Roblox's CW winding to CCW, then scales by `Part.size`; planar per-face UVs feed decals.
- **Surface textures** (`crates/rbx-render/src/textures.rs`): one `texture_2d_array` with 14 procedural
  material layers (Plastic studs, SmoothPlastic, Metal, Wood, Concrete, Grass, Sand, Slate, Fabric,
  Marble, Granite, Brick, Cobblestone, Neon) at 256² plus 48 dynamic decal slots, full mip chains and
  anisotropic filtering. The shader samples triplanar at `MATERIAL_UV_SCALE = 0.25` (≈1 stud spacing) and
  alpha-blends the decal over the base colour — the white baseplate now reads as studded plastic.
- **Tests** (`cargo test --workspace`): `rbx-render/tests/textures_render.rs` (stud detail on a slab +
  red decal at frame centre), `scene_pass.rs` (the real `ScenePass` pipeline: textured slab variance +
  decal red centre), `mesh_geometry.rs` (a `.mesh` quad normalizes to the part's box); `asset_cache` unit
  tests cover extension sniffing; `insert` unit tests cover the audio/image/mesh converters and the
  unsupported-type error; `rbx-core` covers inner-asset reference unwrapping. GPU tests return early when
  no adapter is present. The ignored `insert::tests::live_image_audio_and_mesh_inserts` hits the real
  Toolbox (network + linked Open Cloud key) — run with
  `cargo test -p rbx-studio-app -- --ignored live_ --nocapture`.
- **Studio Settings → Security & Accounts → Link OpenCloud**: `File → Studio Settings...` (or
  Avatar ▸ Settings) opens a modal dialog with an *Open Cloud API key* row. **Link OpenCloud** opens a
  masked input; the key is written to the **Windows Credential Manager** under
  `RustbloxStudio/OpenCloud` (not the place file, not logs) and only a masked form
  (`abcd••••••••wxyz`) is ever displayed. Bridge: `studio.opencloud_refresh()`,
  `studio.opencloud_save(key)`, `studio.opencloud_unlink()`; state via `opencloud_linked` /
  `opencloud_mask` / `opencloud_status` (`opencloud_changed`). `insert_asset` reads this key for
  asset downloads (never displays or logs it).
- **Troubleshooting**: Roblox shows "Scope not allowed for this application: \<scope\>" **before** any
  redirect when the Creator Dashboard OAuth 2.0 app is missing a requested permission — add every scope in
  `secrets/roblox_oauth.json` (`openid`, `profile`, `universe:read`, `universe.place:read`) under *Add
  permissions* in the Dashboard; no code change is needed. After adding scopes, sign out and sign in again
  so the consent screen grants them (stored refresh tokens do not gain new scopes). If experiences fail to
  load with 401/403, the token likely lacks the new grant — re-authenticate. Opening a place still requires a
  linked Open Cloud API key whose account can access that place (`legacy-asset:manage`, entitlement rules).

### ⚠️ Known risk
Roblox's OAuth scopes may **not** grant Toolbox/asset-download access. If so, the alternative is Open Cloud
API keys, or a `.ROBLOSECURITY`-style session (ToS-sensitive, brittle). **Validate OAuth scope reality
before committing to this feature.**

---

## UI Plan (Roblox-exact, only in-game topbar removed)

**Keep exact:** the full Studio topbar (menu bar + ribbon toolbar), docked panels (Explorer, Properties,
Toolbox, Output, Command bar, Asset Manager), 3D viewport with grid/camera/selection/move-rotate-scale
gizmos/axis widget, dark theme palette, fonts, icons, property editors (Vector3 spinboxes, Color3 picker,
Enum dropdowns, bools, strings, grouped categories), Explorer tree behavior (F2 rename, drag-reparent,
search, expand state), status bar.

**Omit:** only the **in-game topbar overlay** — the Roblox logo and chat icon displayed at the top of the
game screen. In-game UI is fully developer-authored (via `ScreenGui`, `SettingsService`, etc.).

**Tech:** **Qt** for the Studio UI shell. Real Studio uses Qt5 + Qtitan docking (no native QtitanRibbon).
Bootstrap uses **`qmetaobject` (QML)** + Qt 6.8.3 (`.tools/Qt`, aqtinstall) to get a running Qt window
quickly; for 100% exact docking/ribbon chrome, evolve to **Qt Widgets / `cxx-qt` / a C++ Qt shim** as
needed (Qtitan is commercial; model `QDockWidget`/ADS behavior from `assets/studio/layout/*.xml`).
Use extracted `DesignTokens` + Builder Sans + real icons for fidelity (IP caveat for redistribution).
Palette (Default Dark): `#121215/#191A1F/#202227/#272930`, `#D5D7DD/#F7F7F8/#BCBEC8`, accent `#528BFF`.

**Build order**: window + empty dock layout → Explorer + Properties → 3D viewport + selection → gizmos →
Toolbox + Output + Command bar → theme/icon polish.

---

## Rendering

- `wgpu` + `glam` for math.
- **FSR / DLSS** frame generation + upscaling: ship AMD FSR (open), DLSS optional via NVIDIA Streamline
  / DLSS SDK (dynamic load, graceful fallback). Anisotropy on the wgpu sampler (maxAnisotropy).
- **Physics**: `rapier3d` (planned).
- **Serialization**: `serde` + `serde_json` (v1), `bincode`/`rkyv` later.

---

## Open Questions (unresolved)

1. **Player renderer**: `wgpu` from scratch vs `bevy` (faster to ship)?
2. **Scripting in player**: port Luau via `mlua` (yes, needed for plugin API + game scripts) — confirm scope.
3. **Own format**: JSON-first confirmed in spirit; binary later — confirm.
4. **Roblox OAuth feasibility** for Toolbox/asset downloads (see risk above).
5. **Collaboration**: invite via Steam friends picker *and* session code? (recommended both) · chat panel? (recommended yes).
6. **FSR/DLSS shipping**: ship FSR only, DLSS optional download? (recommended yes).
7. **Default settings GUI**: ship an opt-in minimal default panel as fallback? (recommended yes).
8. **UI version target**: current 2024+ Studio layout (assumed yes).
9. **Dock layout persistence**: save/restore arrangement? (recommended yes).
10. **Multiple viewports**: support split camera views like Studio? (open).
11. **Asset CDN**: proxy Roblox thumbnails vs hotlink directly? (open).
12. **Plugin compat target**: "runs most community plugins" vs "byte-exact API parity"? (open).

---

## Working Rules for Agents

- **Do NOT build/compile until the user explicitly says so.** Planning/design only by default.
- Keep changes consistent with surrounding code style.
- Prefer the command pattern for all editor mutations (undo/redo).
- Share the GUI renderer (`rbx-gui`) between studio docks and in-game GUIs.
- Gate `SteamworksService` and settings *writes* to server context.
- Keep collaboration inside the UI, never as a Luau service.
