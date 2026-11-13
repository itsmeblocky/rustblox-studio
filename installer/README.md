# Rustblox Studio MSI installer (Windows)

This folder contains the packaging scripts and WiX authoring to produce a **Windows `.msi`** installer for Rustblox Studio.

## What it does
- Builds `rbx-studio-app` (release by default)
- Stages the app into `installer/staging/`
- Deploys Qt runtime with `windeployqt` (from `.tools/Qt/6.8.3/msvc2022_64` by default) and writes a `qt.conf`
- Copies `assets/` (Studio UI/assets/data required at runtime)
- Harvests the staged tree and builds an MSI (when WiX Toolset is installed)

## Prerequisites
- **Rust toolchain** (MSVC target) and workspace dependencies
- **Qt 6.8.3 (msvc2022_64)** with `bin/windeployqt.exe` (repo default: `.tools/Qt/6.8.3/msvc2022_64`)
- **Visual C++ Redistributable** on target machines (the build uses `--compiler-runtime` with windeployqt when available; installing the VC++ redist on end-user machines is recommended)
- **WiX Toolset** to generate the `.msi`
  - **WiX v3** (recommended with this script): needs `heat.exe`, `candle.exe`, `light.exe` (e.g. WiX Toolset v3.11/v3.14)
  - **WiX v4**: `wix.exe` (script detects it, but harvesting/authoring flow is primarily set up for v3)

## Build the MSI
From the repository root (PowerShell):

```powershell
powershell -ExecutionPolicy Bypass -File installer/build-installer.ps1
```

Options:
```powershell
# debug build, custom Qt/WiX locations, version
powershell -ExecutionPolicy Bypass -File installer/build-installer.ps1 -Configuration debug -QtDir "C:\path\to\Qt\6.8.3\msvc2022_64" -WixBin "C:\Program Files (x86)\WiX Toolset v3.14\bin" -Version 0.1.0
```

Outputs:
- Staging: `installer/staging/`
- MSI (on success): `installer/out/RustbloxStudio-<Version>.msi`

If WiX is not installed, the script **stops after staging** and reports the staging directory so you can install WiX and re-run.

## Runtime/asset resolution
The app resolves bundled Studio assets from `assets/studio` relative to the **executable/install directory** (with dev-workspace fallback), so the installed `assets/` folder must remain alongside `rbx-studio-app.exe`.

## Notes
- `secrets/` and user credentials are **not** bundled. OAuth/Open Cloud credentials remain local/per-user.
- `assets/` can be large; harvesting (`heat.exe`) packages the full staged tree so runtime resources are included.
- The installer uses a per-machine install to `Program Files\Rustblox Studio` and creates Start Menu/Desktop shortcuts.