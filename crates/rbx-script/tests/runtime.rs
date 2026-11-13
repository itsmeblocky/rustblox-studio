//! End-to-end tests for the Luau host: prelude datatypes, instance tree, tasks,
//! signals, modules and the gated services.

use rbx_core::{Scene, ScriptInstance, ScriptKind};
use rbx_script::context::ExecContext;
use rbx_script::{HostConfig, ScriptHost};

fn host() -> ScriptHost {
    ScriptHost::new(HostConfig::default().with_bundled_api_dump()).expect("host starts")
}

fn logs(host: &ScriptHost) -> Vec<String> {
    host.take_logs().into_iter().map(|l| l.message).collect()
}

#[test]
fn prelude_datatypes_work() {
    let host = host();
    let out = host
        .exec(
            r#"
            local a = Vector3.new(1, 2, 3)
            local b = Vector3.new(4, 5, 6)
            local sum = a + b
            local cf = CFrame.new(0, 5, 0) * CFrame.Angles(0, math.rad(90), 0)
            local p = cf * Vector3.new(1, 0, 0)
            return string.format("%.0f,%.0f,%.0f|%.0f,%.0f,%.0f|%.0f,%.0f,%.0f|%s",
                sum.X, sum.Y, sum.Z,
                p.X, p.Y, p.Z,
                cf.Position.X, cf.Position.Y, cf.Position.Z,
                typeof(a))
            "#,
            "Test/Vector",
            ExecContext::Server,
        )
        .expect("exec ok");
    let text = out.to_string().expect("string result").to_string();
    assert_eq!(text, "5,7,9|0,5,-1|0,5,0|Vector3");
}

#[test]
fn instance_tree_and_properties() {
    let host = host();
    host.exec(
        r#"
        local part = Instance.new("Part")
        part.Name = "TestPart"
        part.Size = Vector3.new(2, 3, 4)
        part.Anchored = true
        part.Parent = workspace
        local found = workspace:FindFirstChild("TestPart")
        assert(found, "part should be findable")
        assert(found.Size.Y == 3, "size round-trip")
        assert(found:IsA("BasePart"), "IsA BasePart")
        assert(found.Anchored == true, "anchored round-trip")
        print(found:GetFullName(), found.Size.X)
        "#,
        "Test/Instance",
        ExecContext::Server,
    )
    .expect("exec ok");
    assert_eq!(logs(&host), vec!["Workspace.TestPart 2"]);
}

#[test]
fn task_wait_resumes_after_steps() {
    let host = host();
    host.exec(
        r#"
        print("before")
        task.wait(0.5)
        print("after")
        "#,
        "Test/Wait",
        ExecContext::Server,
    )
    .expect("exec ok");
    assert_eq!(logs(&host), vec!["before"]);

    // 31 frames at 1/60s >= 0.5s (floating point needs the extra frame)
    for _ in 0..31 {
        host.step(1.0 / 60.0);
    }
    assert_eq!(logs(&host), vec!["after"]);
}

#[test]
fn task_spawn_and_delay() {
    let host = host();
    host.exec(
        r#"
        task.spawn(function()
            print("spawned")
        end)
        task.delay(0.25, function()
            print("delayed")
        end)
        "#,
        "Test/Tasks",
        ExecContext::Server,
    )
    .expect("exec ok");
    assert_eq!(logs(&host), vec!["spawned"]);
    for _ in 0..16 {
        host.step(1.0 / 60.0);
    }
    assert_eq!(logs(&host), vec!["delayed"]);
}

#[test]
fn signals_connect_and_fire() {
    let host = host();
    host.exec(
        r#"
        local part = Instance.new("Part")
        part.Name = "SignalPart"
        part.Parent = workspace
        local got = nil
        part.Changed:Connect(function(prop)
            got = prop
        end)
        part.Name = "Renamed"
        assert(got == "Name", "Changed fired with " .. tostring(got))
        print("signal ok")
        "#,
        "Test/Signal",
        ExecContext::Server,
    )
    .expect("exec ok");
    assert_eq!(logs(&host), vec!["signal ok"]);
    assert_eq!(host.stats().errors, 0);
}

#[test]
fn wait_for_child_yields_until_present() {
    let host = host();
    host.exec(
        r#"
        local folder = Instance.new("Folder")
        folder.Name = "Holder"
        folder.Parent = workspace

        task.spawn(function()
            local child = folder:WaitForChild("Later")
            print("found", child.Name)
        end)

        folder:FindFirstChild("Later") -- nil for now
        print("waiting")
        "#,
        "Test/WaitForChild",
        ExecContext::Server,
    )
    .expect("exec ok");
    assert_eq!(logs(&host), vec!["waiting"]);

    host.exec(
        r#"
        local child = Instance.new("Folder")
        child.Name = "Later"
        child.Parent = workspace.Holder
        "#,
        "Test/AddChild",
        ExecContext::Server,
    )
    .expect("exec ok");

    for _ in 0..2 {
        host.step(1.0 / 60.0);
    }
    let all = logs(&host);
    assert!(
        all.iter().any(|l| l == "found Later"),
        "expected WaitForChild to resume, got {all:?}"
    );
}

#[test]
fn require_runs_module_once() {
    let host = host();
    host.exec(
        r#"
        local module = Instance.new("ModuleScript")
        module.Name = "MathModule"
        module.Source = [[
            print("module body")
            return { double = function(n) return n * 2 end }
        ]]
        module.Parent = game:GetService("ReplicatedStorage")

        local m = require(module)
        local again = require(module)
        assert(m == again, "module cached")
        assert(m.double(21) == 42, "module function works")
        print("required")
        "#,
        "Test/Require",
        ExecContext::Server,
    )
    .expect("exec ok");
    assert_eq!(logs(&host), vec!["module body", "required"]);
}

#[test]
fn services_are_gated_by_context() {
    let host = host();

    // Client cannot see SteamworksService.
    let value = host
        .exec(
            "return game:GetService(\"SteamworksService\") == nil",
            "Test/ClientGate",
            ExecContext::Client,
        )
        .expect("exec ok");
    assert!(value.as_boolean().unwrap_or(false));

    // Client cannot write settings.
    let err = host
        .exec(
            "game:GetService(\"SettingsService\"):SetSetting(\"VSync\", false)",
            "Test/ClientSettings",
            ExecContext::Client,
        )
        .expect_err("client write must fail");
    assert!(err.contains("server-only"), "unexpected error: {err}");

    // Server can.
    host.exec(
        r#"
        local settings = game:GetService("SettingsService")
        settings:SetSetting("VSync", false)
        assert(settings:GetSetting("VSync") == false)
        "#,
        "Test/ServerSettings",
        ExecContext::Server,
    )
    .expect("server write ok");
}

#[test]
fn start_scripts_runs_scene_scripts() {
    let host = host();
    let scene = Scene::default();
    let mut server = ScriptInstance::new_file(
        "Hello",
        ScriptKind::Server,
        vec!["ServerScriptService".to_string()],
        "print(\"hello from\", script.Name)\nfor i = 1, 3 do print(i) end",
    );
    server.class_name = "Script".to_string();
    let mut module = ScriptInstance::new_file(
        "Shared",
        ScriptKind::Module,
        vec!["ReplicatedStorage".to_string()],
        "return 1",
    );
    module.class_name = "ModuleScript".to_string();
    let scene = rbx_core::Scene {
        scripts: vec![server, module],
        ..scene
    };
    host.load_scene(&scene);
    let ran = host.start_scripts();
    assert_eq!(ran, 1, "only the Script should autorun");
    assert_eq!(
        logs(&host),
        vec!["hello from Hello", "1", "2", "3"]
    );
}

#[test]
fn physics_service_tracks_collision_groups() {
    let host = host();
    host.exec(
        r#"
        local physics = game:GetService("PhysicsService")
        physics:CreateCollisionGroup("Players")
        local part = Instance.new("Part")
        part.Parent = workspace
        physics:SetPartCollisionGroup(part, "Players")
        assert(physics:CollisionGroupContainsPart("Players", part))
        assert(physics:GetMaxCollisionGroups() > 0)
        print("physics ok")
        "#,
        "Test/Physics",
        ExecContext::Server,
    )
    .expect("exec ok");
    assert_eq!(logs(&host), vec!["physics ok"]);
}
