# Physics-relevant Roblox scripts (and what must be native)

Rustblox runs real Roblox Luau (`rbx-script`), so it helps to know exactly which
parts of "physics" actually live in scripts and which live in the engine.

**Short answer: there is no Luau physics solver.** Roblox's rigid-body simulation
is engine-side C++. The shipped scripts only *configure* and *read* it. Rustblox
therefore has to implement the solver itself (planned: `rapier3d`), while the
scripts drive it through the same property/API surface Roblox exposes.

Sources (per `AGENTS.md`):

- `C:\Users\blocky\AppData\Local\Roblox\Versions\version-63eb6055af4e4688\content\avatar\unification\`
- `...\ExtraContent\scripts\PlayerScripts\StarterPlayer\PlayerModule\`
- `...\ExtraContent\scripts\PlayerScripts\StarterPlayerScriptsCommon\`
- API dump: `assets/studio/api/API-Dump.json`

## Script-visible physics (must be exposed by Rustblox)

| Surface | Used by | Notes |
|---|---|---|
| `PhysicsService` collision groups (`CreateCollisionGroup`, `SetPartCollisionGroup`, `CollisionGroupContainsPart`, `GetMaxCollisionGroups`) | `SetupPhysicsParts.lua`, `v2/AdaptCharacterPropertiesAndPhysics.lua`, `v2/R6CollisionEmulation.lua` | Implemented in `rbx-script` (`CollisionGroups`); 32 groups like Roblox. The solver must honour the groups. |
| `BasePart` properties (`CFrame`, `Size`, `AssemblyLinearVelocity`, `AssemblyAngularVelocity`, `Massless`, `CanCollide`, `CanTouch`, `Anchored`) | avatar unification, PlayerModule | Values are read/written every frame; the native solver owns the integration. |
| `Humanoid` movement (`Humanoid:Move`, `MoveDirection`, `WalkSpeed`, `Jump`, `JumpPower`, `JumpHeight`, `UseJumpPower`, `PlatformStand`, `Sit`, `StateChanged`, `FloorMaterial`) | `ControlModule/init.lua`, `ClickToMoveController.lua`, `VehicleController.lua`, `AvatarAbilitiesInterface.lua`, `R6CollisionEmulation.lua` | Scripts command the humanoid; the engine applies forces and foot-planting. Rustblox needs a humanoid/motor layer on top of the solver. |
| Constraints/joints (`Motor6D`, `Weld`, `HingeConstraint`, `SpringConstraint`, `AlignPosition`, `Attachment`) | avatar rigs (`R15.rbxm`, `R6CollisionEmulation`, `SetupAdapterParts*`) | Scripts only set values; the solver enforces them. |
| `Raycast` / `Workspace:GetPartBoundsInBox` / `FindPartOnRay` | `ClickToMoveController.lua`, `MoveAdapterPartHitboxes.lua`, `CameraModule/BaseOcclusion.lua`, `Poppercam.lua` | Queries against the physics world; Rustblox needs a broad-phase query layer. |
| `RunService` (`Heartbeat`, `Stepped`, `PreSimulation`, `PostSimulation`) | `ControlModule/init.lua`, `RbxCharacterSounds.lua` | Frame ordering matters: scripts read post-step state and feed input for the next step. |
| `Touch` / `Touched` / `TouchEnded` | avatar unification, game scripts | Broad-phase contact events. |

## Scripts that look like physics but are not

- `content/avatar/unification/SetupPhysicsParts.lua` — builds the **collision
  proxy parts** (`*_Collision` parts) and assigns collision groups. It never
  integrates motion.
- `content/avatar/unification/CharacterEmulation.lua` — drives an R6/R15 rig
  through `Humanoid` state changes (animate scripts, sounds, camera);
  no solver.
- `content/avatar/unification/AdaptInstance.lua`,
  `v2/AdaptCharacterPropertiesAndPhysics.lua` — copy visual sizes/masses onto
  the collision proxies, set `CanCollide`, `Massless`, `Transparency`.
- `content/avatar/unification/v2/R6CollisionEmulation.lua` — swaps in the R6
  collision hull, sets `HipHeight`/collision sizes, then relies on the engine.
- `PlayerModule/ControlModule/init.lua` — input → `Humanoid:Move(direction, false)`,
  `Humanoid.Jump = true`, camera `CFrame`. Movement is executed by the engine.
- `PlayerModule/CameraModule/*` (incl. `VRCamera.lua`) — camera math + line-of-sight
  occlusion raycasts; no dynamics.
- `PlayerScripts/StarterPlayerScriptsCommon/RbxCharacterSounds.lua` — detects
  `Running`/`Jumping`/`Landed` from `Humanoid.StateChanged` and plays sounds.

## Native work still required

1. **Solver**: `rapier3d` world stepped at 240 Hz, bodies built from `BasePart`
   plus constraints/attachments; collision groups mapped from `PhysicsService`.
2. **Humanoid motor**: convert `MoveDirection`/`Jump` into forces/impulses,
   stand on `FloorMaterial`, apply `WalkSpeed`, `JumpPower`, `HipHeight`.
3. **Query layer**: `Raycast`, `GetPartBoundsInBox`, `GetPartsInPart`, `Touched`.
4. **Frame ordering**: `PreSimulation` → integrate → `Stepped`/`PostSimulation`,
   which is what the PlayerModule and avatar scripts expect.
5. **Net ownership**: client scripts drive their own humanoid in play-solo, so a
   single local authority is enough for now; revisit for collaboration/Steam.

`rbx-script` currently exposes `PhysicsService`, `Humanoid` (state + movement
properties) and part mass helpers (`compute_mass`) so scripts can run before the
solver lands; motion itself is a no-op until `rbx-physics`/`rapier3d` is wired.
