//! Management operations: health, live checks, channels, tools, skills,
//! login hand-off, migration, and confirmed uninstall.
//!
//! `doctor` is read-only and inspects recorded tools/versions; `check` is the
//! only command that runs live probes. Login is always performed by the human
//! in the dedicated browser or the upstream CLI's own flow.
mod fsops;

use reach_core::{
    state::{self, Store},
    ReachError, Result,
};
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

/// Budget for one tool install/update. Downloads and child processes share it.
const TOOL_TIMEOUT: Duration = Duration::from_secs(1800);
/// Browser/OpenCLI context preparation is bounded separately from tools.
const BRIDGE_TIMEOUT: Duration = Duration::from_secs(180);

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
fn bad(message: impl Into<String>) -> ReachError {
    ReachError::new("BAD_INPUT", message)
}
fn added(ch: &reach_channels::Channel, data: &Value) -> bool {
    data["channels"].get(&ch.name).is_some()
        || (ch.level == "default"
            && !data["removed_defaults"]
                .as_array()
                .is_some_and(|a| a.contains(&json!(ch.name))))
}
pub(super) fn channel_fingerprint(
    ch: &reach_channels::Channel,
    root: &Path,
    data: &Value,
) -> String {
    super::fingerprint(
        &json!([
            env!("CARGO_PKG_VERSION"),
            env!("ARANA_BUILD_SIGNATURE"),
            ch.name,
            data["tools"],
            data["session_revisions"][&ch.name],
            reach_core::state::read_private(&root.join("config.yaml"), 1024 * 1024).ok()
        ])
        .to_string(),
    )
}

/// Tool spec names plus their recorded dependency closure.
fn closure<'a>(names: impl IntoIterator<Item = &'a str>) -> BTreeSet<String> {
    let mut found = BTreeSet::new();
    let mut stack: Vec<String> = names.into_iter().map(str::to_owned).collect();
    while let Some(name) = stack.pop() {
        if !found.insert(name.clone()) {
            continue;
        }
        if let Some(spec) = reach_tools::registry::tool(&name) {
            stack.extend(spec.needs.iter().map(|need| (*need).to_string()));
        }
    }
    found
}
fn channel_tools(ch: &reach_channels::Channel) -> BTreeSet<String> {
    closure(ch.prereqs.iter().chain(ch.tools.iter()).map(String::as_str))
}
fn uses_browser(ch: &reach_channels::Channel) -> bool {
    !ch.login_url.is_empty()
        || ch.actions.iter().any(|action| {
            action
                .backends
                .iter()
                .any(|(label, _)| matches!(label.as_str(), "opencli" | "boss-agent-cli"))
        })
}
fn login_state(ch: &reach_channels::Channel, root: &Path) -> Value {
    if !ch.login_url.is_empty() {
        return json!({
            "via": "browser",
            "state": "unknown",
            "command": format!("arana login {}", ch.name),
            "detail": "Cookie/login state cannot be proven without a live site check and is never read from the user's browser database",
        });
    }
    if ch.session.is_empty() {
        return json!({"via": "none", "state": "not_required"});
    }
    let state = match reach_channels::session::has_login_state(&ch.session, root) {
        Ok(true) => "logged_in",
        Ok(false) => "logged_out",
        Err(_) => "unknown",
    };
    json!({"via": "session", "state": state, "command": format!("arana login {}", ch.name)})
}

async fn inspect(tool: &str, root: &Path) -> Value {
    match reach_tools::management::inspect_tool(tool, root).await {
        Ok(value) => value,
        Err(error) => json!({
            "tool": tool,
            "installed": false,
            "version": null,
            "error": error,
        }),
    }
}
fn version_problem(tool: &str, inspected: &Value, channel: &str) -> Option<String> {
    if inspected["installed"] != true {
        return Some(format!("{tool} is not installed; run arana add {channel}"));
    }
    if inspected["allowed"] == false {
        return Some(format!(
            "{tool} {} is outside the supported versions (tested {}, floor {})",
            inspected["version"].as_str().unwrap_or("unknown"),
            inspected["tested"].as_str().unwrap_or("any"),
            inspected["floor"].as_str().unwrap_or("any")
        ));
    }
    None
}

async fn doctor(args: &[String], root: &Path) -> Result<Value> {
    if !args.is_empty() && args != ["--text"] {
        return Err(bad("usage: arana doctor [--text]"));
    }
    let data = Store::new(root).load()?;
    let mut inspections = BTreeMap::new();
    for spec in reach_tools::registry::tools() {
        inspections.insert(spec.name, inspect(spec.name, root).await);
    }
    let mut rows = Vec::new();
    for ch in reach_channels::registry() {
        let is_added = added(ch, &data);
        let mut problems = Vec::new();
        if is_added {
            for tool in channel_tools(ch) {
                if let Some(inspected) = inspections.get(tool.as_str()) {
                    if let Some(problem) = version_problem(&tool, inspected, &ch.name) {
                        problems.push(problem);
                    }
                }
            }
            if !ch.needs_key.is_empty() && reach_tools::config::get(root, &ch.needs_key).is_none() {
                problems.push(format!(
                    "An explicit provider key is required; use arana configure {}-key --stdin",
                    ch.needs_key.trim_end_matches("_key").replace('_', "-")
                ));
            }
        }
        let login = login_state(ch, root);
        let check = &data["checks"][&ch.name];
        let signature = channel_fingerprint(ch, root, &data);
        let mut fresh = serde_json::Map::new();
        if let Some(actions) = check["actions"].as_object() {
            for (name, passed) in actions {
                let evidence = &check["evidence"][name];
                let at = evidence["at"].as_u64();
                if ch.action(name).is_some()
                    && passed.is_boolean()
                    && evidence["fingerprint"] == signature
                    && evidence["verified"] == true
                    && at.is_some_and(|at| at <= now() && now().saturating_sub(at) <= 7 * 86400)
                {
                    fresh.insert(name.clone(), passed.clone());
                }
            }
        }
        let failures: Vec<_> = fresh
            .iter()
            .filter(|(_, value)| **value == false)
            .map(|(key, _)| key.as_str())
            .collect();
        let status = if !is_added {
            "not_added"
        } else if !problems.is_empty() {
            "needs_setup"
        } else if fresh.is_empty() {
            "unverified"
        } else if !failures.is_empty() {
            if failures
                .iter()
                .all(|action| check["codes"][*action] == "NEED_LOGIN")
            {
                "needs_login"
            } else if failures
                .iter()
                .all(|action| check["codes"][*action] == "RATE_LIMITED")
            {
                "cooldown"
            } else if failures
                .iter()
                .all(|action| check["codes"][*action] == "PLATFORM_BLOCKED")
            {
                "needs_user_action"
            } else {
                "broken"
            }
        } else if check["scope"] == "search" {
            "search_ready"
        } else {
            "ready"
        };
        let regressed = fresh
            .iter()
            .any(|(action, ok)| *ok == false && check["previous"][action] == true);
        let next = match status {
            "not_added" => format!("arana add {}", ch.name),
            "needs_login" => format!("arana login {}", ch.name),
            "unverified" => format!("arana check {}", ch.name),
            "ready" | "search_ready" => format!("arana check {}", ch.name),
            _ => problems
                .first()
                .cloned()
                .unwrap_or_else(|| format!("arana check {}", ch.name)),
        };
        rows.push(json!({
            "channel": ch.name,
            "level": ch.level,
            "status": status,
            "problems": problems,
            "regressed": regressed,
            "checked_at": check.get("at"),
            "verified_actions": fresh.keys().collect::<Vec<_>>(),
            "login": login,
            "next": next,
        }));
    }
    let tools: Vec<Value> = inspections
        .into_values()
        .map(|mut inspected| {
            inspected["version_verified"] =
                json!(inspected["installed"] == true && !inspected["version"].is_null());
            inspected
        })
        .collect();
    Ok(json!({
        "version": env!("CARGO_PKG_VERSION"),
        "channels": rows,
        "tools": tools,
        "network_probed": false,
    }))
}

async fn checks(args: &[String], root: &Path) -> Result<Value> {
    let mut names = Vec::new();
    let mut timeout = 60.0_f64;
    let mut search = false;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--text" => {}
            "--search" => search = true,
            "--timeout" => {
                index += 1;
                timeout = args
                    .get(index)
                    .and_then(|s| s.parse::<f64>().ok())
                    .filter(|n| n.is_finite() && *n > 0.0 && *n <= 86400.0)
                    .ok_or_else(|| {
                        ReachError::new(
                            "BAD_INPUT",
                            "timeout must be finite, positive and at most 86400 seconds",
                        )
                    })?;
            }
            name if !name.starts_with('-') && reach_channels::channel(name).is_some() => {
                if !names.iter().any(|s| s == name) {
                    names.push(name.to_owned());
                }
            }
            _ => {
                return Err(ReachError::new(
                    "BAD_INPUT",
                    "check requires known channel names, --search, --text or --timeout SECONDS",
                ))
            }
        }
        index += 1;
    }
    let saved = Store::new(root).load()?;
    let selected: Vec<_> = reach_channels::registry()
        .iter()
        .filter(|ch| {
            if names.is_empty() {
                added(ch, &saved)
            } else {
                names.contains(&ch.name)
            }
        })
        .collect();
    let probes: Value =
        serde_json::from_str(include_str!("../../../tests/golden/manifest.json"))
            .map_err(|_| ReachError::new("UPSTREAM_BROKEN", "Invalid embedded check probes"))?;
    let mut rows = Vec::new();
    let mut outcomes = BTreeMap::<(String, String), bool>::new();
    for ch in &selected {
        for action in &ch.actions {
            if search && !matches!(action.name.as_str(), "search" | "jobs") {
                continue;
            }
            let params = action.live_test.clone().or_else(|| {
                probes["records"]
                    .as_array()?
                    .iter()
                    .find(|r| r["channel"] == ch.name && r["action"] == action.name)
                    .map(|r| r["params"].clone())
            });
            let Some(params) = params else { continue };
            let backends: Vec<Option<&str>> = if search {
                vec![None]
            } else {
                action.backends.iter().map(|b| Some(b.0.as_str())).collect()
            };
            for backend in backends {
                let mut cli = vec!["--timeout".into(), timeout.to_string()];
                if let Some(backend) = backend {
                    cli.extend(["--backend".into(), backend.into()]);
                }
                let mut positional = Vec::new();
                for param in &action.params {
                    if let Some(value) = params.get(&param.name).filter(|v| !v.is_null()) {
                        let value = value
                            .as_str()
                            .map(str::to_owned)
                            .unwrap_or_else(|| value.to_string());
                        if param.required {
                            positional.push(value);
                        } else {
                            cli.extend([format!("--{}", param.name), value]);
                        }
                    }
                }
                cli.push("--".into());
                cli.extend(positional);
                let out = super::action(ch, &action.name, &cli, root).await;
                let count = out["items"].as_array().map_or(0, Vec::len);
                let passed = out["ok"] == true && count > 0;
                let error = if passed {
                    Value::Null
                } else if out["error"].is_object() {
                    out["error"].clone()
                } else {
                    json!({"code":"EMPTY","message":"The action returned no items"})
                };
                let code = error["code"].as_str().unwrap_or("").to_owned();
                let label = out["backend"]
                    .as_str()
                    .map(str::to_owned)
                    .or_else(|| backend.map(str::to_owned));
                rows.push(json!({"channel":ch.name,"action":action.name,"backend":label,"passed":passed,"verified":passed,"item_count":count,"elapsed_ms":out["meta"]["elapsed_ms"],"attempts":out["meta"]["attempts"],"access":out["meta"]["access"],"error":error}));
                outcomes
                    .entry((ch.name.clone(), action.name.clone()))
                    .and_modify(|v| *v |= passed)
                    .or_insert(passed);
                if matches!(
                    code.as_str(),
                    "NEED_LOGIN" | "RATE_LIMITED" | "PLATFORM_BLOCKED"
                ) {
                    break;
                }
            }
        }
    }
    let stamp = now();
    let latest = Store::new(root).load()?;
    let signatures: Vec<_> = selected
        .iter()
        .map(|ch| (&ch.name, channel_fingerprint(ch, root, &latest)))
        .collect();
    Store::new(root).mutate(|data| {
        for (name, signature) in &signatures {
            let previous = data["checks"][*name]["actions"]
                .as_object()
                .cloned()
                .unwrap_or_default();
            let mut actions = previous.clone();
            let mut codes = data["checks"][*name]["codes"]
                .as_object()
                .cloned()
                .unwrap_or_default();
            let mut evidence = data["checks"][*name]["evidence"]
                .as_object()
                .cloned()
                .unwrap_or_default();
            for ((channel, action), passed) in &outcomes {
                if channel != *name {
                    continue;
                }
                actions.insert(action.clone(), json!(passed));
                let best = rows
                    .iter()
                    .filter(|r| r["channel"] == **name && r["action"] == *action)
                    .find(|r| r["passed"] == true)
                    .or_else(|| {
                        rows.iter()
                            .rev()
                            .find(|r| r["channel"] == **name && r["action"] == *action)
                    });
                if let Some(best) = best {
                    if *passed {
                        codes.remove(action);
                    } else {
                        codes.insert(action.clone(), best["error"]["code"].clone());
                    }
                    evidence.insert(action.clone(), json!({"backend":best["backend"],"at":stamp,"item_count":best["item_count"],"verified":best["verified"],"fingerprint":signature,"access":best["access"]}));
                }
            }
            if outcomes.keys().any(|(channel, _)| channel == *name) {
                data["checks"][*name] = json!({"at":stamp,"actions":actions,"previous":previous,"codes":codes,"evidence":evidence,"scope":if search{"search"}else{"all"}});
            }
        }
    })?;
    Ok(
        json!({"ok":!outcomes.is_empty()&&outcomes.values().all(|v|*v),"scope":if search{"search"}else{"all"},"results":rows}),
    )
}

/// Attach an OpenCLI context-bridge preparation result to a browser setup/add
/// payload. Failure stays visible; it never turns a successful install into a
/// false "connected" claim on platforms that require manual extension loading.
pub(super) async fn bind_opencli(mut result: Value, root: &Path) -> Value {
    if reach_tools::registry::resolve("opencli", root).is_none() {
        result["opencli"] = json!({"prepared": false, "reason": "opencli is not installed"});
        return result;
    }
    let prepared = reach_browser::prepare_opencli(root, BRIDGE_TIMEOUT).await;
    result["opencli"] = match prepared {
        Ok(()) => json!({"prepared": true}),
        Err(error) => json!({"prepared": false, "error": error}),
    };
    result
}

async fn ensure_tools(
    ch: &reach_channels::Channel,
    root: &Path,
) -> Result<(Vec<Value>, Option<Value>)> {
    let mut installed = Vec::new();
    let mut names: Vec<String> = ch.prereqs.clone();
    names.extend(ch.tools.clone());
    let mut seen = BTreeSet::new();
    for name in names {
        if !seen.insert(name.clone()) {
            continue;
        }
        installed.push(reach_tools::management::ensure_tool(&name, root, TOOL_TIMEOUT).await?);
    }
    let browser = if uses_browser(ch) {
        let mut status = reach_browser::manage("setup", &[], root).await?;
        status = bind_opencli(status, root).await;
        Some(status)
    } else {
        None
    };
    Ok((installed, browser))
}

fn next_step(ch: &reach_channels::Channel) -> Option<String> {
    if !ch.login_url.is_empty() || !ch.session.is_empty() {
        Some(format!(
            "If access requires login, run arana login {}; the user completes it themselves.",
            ch.name
        ))
    } else if ch.needs_key.is_empty() {
        None
    } else {
        Some(format!(
            "Provide the provider key with arana configure {}-key --stdin",
            ch.needs_key.trim_end_matches("_key").replace('_', "-")
        ))
    }
}
fn mark_added(root: &Path, name: &str) -> Result<()> {
    Store::new(root).mutate(|data| {
        if data["channels"].get(name).is_none() {
            if let Some(reuse) = data["session_reuse"].as_object_mut() {
                reuse.remove(name);
            }
        }
        data["channels"][name] = json!({"added_at": now()});
        if let Some(removed) = data["removed_defaults"].as_array_mut() {
            removed.retain(|value| value != name);
        }
    })
}
async fn add_channel(name: &str, root: &Path, allow_upgrade: bool) -> Result<Value> {
    let ch =
        reach_channels::channel(name).ok_or_else(|| bad(format!("unknown channel: {name}")))?;
    let previously_added = added(ch, &Store::new(root).load()?);
    let (installed, browser) = ensure_tools(ch, root).await?;
    mark_added(root, name)?;
    if !previously_added {
        // A fresh add starts with no stale reuse/grant decision.
        Store::new(root).mutate(|data| {
            if let Some(reuse) = data["session_reuse"].as_object_mut() {
                reuse.remove(name);
            }
        })?;
    }
    Ok(json!({
        "channel": name,
        "added": true,
        "allow_upgrade": allow_upgrade,
        "tools": installed,
        "browser": browser,
        "login": login_state(ch, root),
        "next": next_step(ch),
    }))
}
async fn add(args: &[String], root: &Path) -> Result<Value> {
    let mut name = None;
    let mut allow_upgrade = false;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--allow-upgrade" => allow_upgrade = true,
            value if !value.starts_with('-') && name.is_none() => name = Some(value.to_owned()),
            _ => return Err(bad("usage: arana add <channel> [--allow-upgrade]")),
        }
        index += 1;
    }
    let name = name.ok_or_else(|| bad("usage: arana add <channel> [--allow-upgrade]"))?;
    if name == "obscura" {
        let installed = reach_tools::management::ensure_tool("obscura", root, TOOL_TIMEOUT).await?;
        return Ok(json!({
            "channel": "obscura",
            "added": true,
            "tools": [installed],
            "next": "arana web read <url> --backend obscura",
        }));
    }
    add_channel(&name, root, allow_upgrade).await
}

fn needed_tools(data: &Value) -> BTreeSet<String> {
    let mut needed = BTreeSet::new();
    for ch in reach_channels::registry() {
        if added(ch, data) {
            needed.extend(channel_tools(ch));
            if ch.name == "web" {
                needed.insert("obscura".into());
            }
        }
    }
    needed
}
fn prune_tools(root: &Path) -> Result<Vec<Value>> {
    let needed = needed_tools(&Store::new(root).load()?);
    let mut removed = Vec::new();
    for spec in reach_tools::registry::tools() {
        if needed.contains(spec.name) {
            continue;
        }
        let result = reach_tools::management::remove_tool(spec.name, root)?;
        if result["removed"] == true {
            removed.push(result);
        }
    }
    Ok(removed)
}
async fn clear_browser_site(ch: &reach_channels::Channel, root: &Path) -> Result<usize> {
    if ch.domains.is_empty() {
        return Ok(0);
    }
    let status = reach_browser::manage("status", &[], root).await?;
    if status["running"] != true || status["owned"] != true {
        return Ok(0);
    }
    let mut cleared = 0;
    for domain in &ch.domains {
        cleared += reach_browser::manage("expire", std::slice::from_ref(domain), root).await?
            ["expired"]
            .as_u64()
            .unwrap_or(0) as usize;
    }
    Ok(cleared)
}
async fn remove(args: &[String], root: &Path) -> Result<Value> {
    if args.len() != 1 || args[0].starts_with('-') {
        return Err(bad("usage: arana remove <channel>"));
    }
    let name = &args[0];
    if name == "obscura" {
        return reach_tools::management::remove_tool("obscura", root);
    }
    let ch =
        reach_channels::channel(name).ok_or_else(|| bad(format!("unknown channel: {name}")))?;
    let data = Store::new(root).load()?;
    if !added(ch, &data) {
        return Ok(
            json!({"channel":name,"removed":false,"removed_tools":[],"cleared_cookies":0,"warnings":[]}),
        );
    }
    let mut warnings = Vec::new();
    let cleared_cookies = match clear_browser_site(ch, root).await {
        Ok(count) => count,
        Err(error) => {
            warnings.push(error.message);
            0
        }
    };
    if !ch.session.is_empty() {
        match reach_channels::session::clear_session(&ch.session, root) {
            Ok(true) => {}
            Ok(false) => warnings.push(format!(
                "No arana-owned session for {} was recorded; it was left untouched",
                ch.session
            )),
            Err(error) => warnings.push(error.message),
        }
    }
    Store::new(root).mutate(|saved| {
        if let Some(map) = saved["channels"].as_object_mut() {
            map.remove(name);
        }
        if let Some(map) = saved["checks"].as_object_mut() {
            map.remove(name);
        }
        for key in [
            "backend_health",
            "cooldowns",
            "reuse_sources",
            "setup_failures",
        ] {
            if let Some(map) = saved[key].as_object_mut() {
                map.remove(name);
            }
        }
        saved["session_reuse"][name] = json!({"blocked":true,"at":now()});
        saved["session_revisions"][name] = json!(now());
        if ch.level == "default"
            && !saved["removed_defaults"]
                .as_array()
                .is_some_and(|list| list.contains(&json!(name)))
        {
            saved["removed_defaults"]
                .as_array_mut()
                .expect("normalized removed_defaults")
                .push(json!(name));
        }
    })?;
    let removed_tools = prune_tools(root)?;
    Ok(
        json!({"channel":name,"removed":true,"removed_tools":removed_tools,"cleared_cookies":cleared_cookies,"warnings":warnings}),
    )
}

fn sync_skill(root: &Path) -> Value {
    match fsops::skill(root, &["update".into()]) {
        Ok(value) => value,
        Err(error) => json!({"installed":root.join("SKILL.md").is_file(),"error":error}),
    }
}
async fn sync(root: &Path) -> Result<Value> {
    let data = Store::new(root).load()?;
    let known: BTreeSet<&str> = reach_channels::registry()
        .iter()
        .map(|ch| ch.name.as_str())
        .collect();
    let dropped: Vec<String> = data["channels"]
        .as_object()
        .map(|map| {
            map.keys()
                .filter(|name| !known.contains(name.as_str()))
                .cloned()
                .collect()
        })
        .unwrap_or_default();
    if !dropped.is_empty() {
        Store::new(root).mutate(|saved| {
            if let Some(channels) = saved["channels"].as_object_mut() {
                for name in &dropped {
                    channels.remove(name);
                }
            }
            if let Some(checks) = saved["checks"].as_object_mut() {
                for name in &dropped {
                    checks.remove(name);
                }
            }
        })?;
    }
    let mut tools = Vec::new();
    let mut problems = Vec::new();
    let mut names = BTreeSet::new();
    for ch in reach_channels::registry() {
        if added(ch, &data) {
            names.extend(channel_tools(ch));
            if ch.name == "web" {
                names.insert("obscura".into());
            }
        }
    }
    for name in names {
        match reach_tools::management::update_tool(&name, root, TOOL_TIMEOUT).await {
            Ok(value) => tools.push(value),
            Err(error) => problems.push(
                json!({"tool":name,"code":error.code,"message":error.message,"hint":error.hint}),
            ),
        }
    }
    let removed_tools = prune_tools(root)?;
    let skill = sync_skill(root);
    Ok(json!({
        "version": env!("CARGO_PKG_VERSION"),
        "tools": tools,
        "problems": problems,
        "removed_tools": removed_tools,
        "dropped_channels": dropped,
        "skill": skill,
    }))
}
async fn update(args: &[String], root: &Path) -> Result<Value> {
    let mut check_only = false;
    let mut sync_only = false;
    for arg in args {
        match arg.as_str() {
            "--check" => check_only = true,
            "--sync-only" => sync_only = true,
            _ => return Err(bad("usage: arana update [--check] [--sync-only]")),
        }
    }
    if check_only {
        return Ok(json!({
            "version": env!("CARGO_PKG_VERSION"),
            "latest": null,
            "update_available": false,
            "release_check": "unavailable",
            "note": "This build has no release feed configured; tool updates remain available with arana update.",
        }));
    }
    let mut result = sync(root).await?;
    result["upgraded"] = json!(false);
    result["sync_only"] = json!(sync_only);
    Ok(result)
}

async fn setup(args: &[String], root: &Path) -> Result<Value> {
    let mut migrate_legacy = false;
    let mut legacy: Option<PathBuf> = None;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--text" => {}
            "--migrate" => migrate_legacy = true,
            "--legacy" => {
                index += 1;
                legacy = Some(PathBuf::from(
                    args.get(index)
                        .ok_or_else(|| bad("--legacy requires a directory"))?,
                ));
            }
            _ => {
                return Err(bad(
                    "usage: arana setup [--text] [--migrate [--legacy DIR]]",
                ))
            }
        }
        index += 1;
    }
    let legacy = legacy.unwrap_or_else(|| {
        root.parent()
            .unwrap_or_else(|| Path::new("."))
            .join(".agent-reach")
    });
    let migration = if legacy.is_dir() {
        if migrate_legacy {
            let imported = fsops::migrate(root, &legacy, true)?;
            Store::new(root).mutate(|data| {
                data["migrated_from"] = json!("agent-reach");
                data["migration_reported"] = json!(true);
            })?;
            Some(imported)
        } else {
            let mut preview = fsops::migrate(root, &legacy, false)?;
            preview["confirm"] = json!("arana setup --migrate");
            preview["source_preserved"] = json!(true);
            Some(preview)
        }
    } else if migrate_legacy {
        return Err(ReachError::new(
            "NEED_SETUP",
            format!("No legacy install found at {}", legacy.display()),
        ));
    } else {
        None
    };
    let skill = fsops::setup_skill(root, None)?;
    let defaults: Vec<&reach_channels::Channel> = reach_channels::registry()
        .iter()
        .filter(|ch| ch.level == "default")
        .collect();
    let data = Store::new(root).load()?;
    let mut default_channels = Vec::new();
    for ch in &defaults {
        if !added(ch, &data) {
            continue;
        }
        match ensure_tools(ch, root).await {
            Ok((tools, browser)) => default_channels.push(json!({
                "channel": ch.name,
                "ok": true,
                "tools": tools,
                "browser": browser,
            })),
            Err(error) => default_channels.push(json!({
                "channel": ch.name,
                "ok": false,
                "tools": [],
                "error": error,
            })),
        }
    }
    let latest = Store::new(root).load()?;
    let optional_channels: Vec<Value> = reach_channels::registry()
        .iter()
        .filter(|ch| !added(ch, &latest))
        .map(|ch| {
            let mut needs = Vec::new();
            if !ch.login_url.is_empty() || !ch.session.is_empty() {
                needs.push("login_if_required");
            }
            if !ch.needs_key.is_empty() {
                needs.push(ch.needs_key.as_str());
            }
            json!({"channel":ch.name,"level":ch.level,"add":format!("arana add {}",ch.name),"needs":needs})
        })
        .collect();
    let logins: Vec<Value> = defaults
        .iter()
        .filter(|ch| added(ch, &latest) && (!ch.login_url.is_empty() || !ch.session.is_empty()))
        .map(|ch| json!({"channel":ch.name,"command":format!("arana login {}",ch.name),"when":"only_if_required"}))
        .collect();
    let all_ok = default_channels.iter().all(|row| row["ok"] == true);
    let mut result = json!({
        "ok": all_ok,
        "version": env!("CARGO_PKG_VERSION"),
        "user_actions": [],
        "default_channels": default_channels,
        "skill": skill,
        "optional_channels": optional_channels,
        "logins": logins,
        "migration": migration,
    });
    let report = installation_report(&result);
    let path = root.join("installation-report.md");
    let (report_path, write_error) = match write_report(root, &path, report.as_bytes()) {
        Ok(()) => (json!(path.to_string_lossy()), Value::Null),
        Err(error) => (Value::Null, json!(error.message)),
    };
    result["installation_report"] = json!({
        "markdown": report,
        "path": report_path,
        "write_error": write_error,
    });
    Ok(result)
}

fn write_report(root: &Path, path: &Path, bytes: &[u8]) -> Result<()> {
    state::private_dir(root)?;
    state::ensure_safe(path)?;
    let _lock = state::FileLock::try_acquire(root, "arana-report")?;
    state::atomic_write(path, bytes)?;
    use sha2::{Digest, Sha256};
    let hash = format!("{:x}", Sha256::digest(bytes));
    Store::new(root).mutate(|saved| {
        saved["owned_paths"]["installation-report.md"] =
            json!({"owner":"arana","scope":"private","kind":"file","sha256":hash});
    })
}
fn installation_report(result: &Value) -> String {
    let mut out = format!("arana {}\n\n", result["version"].as_str().unwrap_or(""));
    out.push_str("Default channels\n");
    for row in result["default_channels"].as_array().into_iter().flatten() {
        let state = if row["ok"] == true {
            "ready-to-use"
        } else {
            "install failed"
        };
        out.push_str(&format!(
            "  {:<12} {}\n",
            row["channel"].as_str().unwrap_or(""),
            state
        ));
        if let Some(error) = row["error"].as_object() {
            out.push_str(&format!(
                "               {}: {}\n",
                error["code"].as_str().unwrap_or(""),
                error["message"].as_str().unwrap_or("")
            ));
        }
    }
    if let Some(migration) = result["migration"].as_object() {
        out.push_str("\nLegacy import\n");
        if migration["dry_run"] == true {
            let count = migration["would_import"].as_array().map_or(0, Vec::len);
            out.push_str(&format!(
                "  preview only: {count} files; re-run arana setup --migrate to import; the source is preserved\n"
            ));
        } else {
            out.push_str("  imported known legacy files; the source is preserved\n");
        }
    }
    if let Some(logins) = result["logins"].as_array().filter(|list| !list.is_empty()) {
        out.push_str("\nLogins (only if required)\n");
        for login in logins {
            out.push_str(&format!(
                "  {:<12} {}\n",
                login["channel"].as_str().unwrap_or(""),
                login["command"].as_str().unwrap_or("")
            ));
        }
    }
    out
}

async fn uninstall(args: &[String], root: &Path) -> Result<Value> {
    let mut confirmed = false;
    for arg in args {
        match arg.as_str() {
            "--dry-run" => {}
            "--yes" | "--confirm" => confirmed = true,
            _ => return Err(bad("usage: arana uninstall [--dry-run | --yes]")),
        }
    }
    let preview = fsops::uninstall(root, false)?;
    let data = Store::new(root).load()?;
    let owned_tools: Vec<String> = data["tools"]
        .as_object()
        .map(|map| {
            map.iter()
                .filter(|(_, entry)| entry["owner"] == "arana")
                .map(|(name, _)| name.clone())
                .collect()
        })
        .unwrap_or_default();
    if !confirmed {
        return Ok(json!({
            "dry_run": true,
            "would_remove": preview["would_remove"],
            "owned_tools": owned_tools,
            "kept_unrecognized": true,
            "confirm": "arana uninstall --yes",
        }));
    }
    let mut removed_tools = Vec::new();
    for name in owned_tools {
        let result = reach_tools::management::remove_tool(&name, root)?;
        if result["removed"] == true {
            removed_tools.push(json!(name));
        }
    }
    let mut result = fsops::uninstall(root, true)?;
    result["removed_tools"] = json!(removed_tools);
    result["dry_run"] = json!(false);
    Ok(result)
}

async fn login(args: &[String], root: &Path) -> Result<Value> {
    let mut import_existing = false;
    let mut sync_existing = false;
    let mut name = None;
    for arg in args {
        match arg.as_str() {
            "--import-existing" => import_existing = true,
            "--sync" => sync_existing = true,
            value if !value.starts_with('-') && name.is_none() => name = Some(value.to_owned()),
            _ => {
                return Err(bad(
                    "usage: arana login <channel> [--import-existing] [--sync]",
                ))
            }
        }
    }
    let name = name.ok_or_else(|| bad("usage: arana login <channel>"))?;
    let ch =
        reach_channels::channel(&name).ok_or_else(|| bad(format!("unknown channel: {name}")))?;
    if ch.login_url.is_empty() && ch.session.is_empty() {
        return Err(bad(format!("{name} has no login managed by arana")));
    }
    if import_existing {
        if ch.login_url.is_empty() || ch.domains.is_empty() {
            return Err(bad(format!(
                "{name} has no browser login surface to import cookies into"
            )));
        }
        return reach_browser::reuse_site(root, &ch.domains, BRIDGE_TIMEOUT).await;
    }
    if !ch.login_url.is_empty() && !sync_existing {
        add_channel(&name, root, false).await?;
        let opened = reach_browser::login(&name, &ch.login_url, root).await?;
        Store::new(root).mutate(|data| {
            data["session_reuse"][&name] = json!({"source":"manual","at":now()});
            data["session_revisions"][&name] = json!(now());
        })?;
        return Ok(json!({
            "logged_in": false,
            "opened": ch.login_url,
            "channel": name,
            "login": login_state(ch, root),
            "next": "Complete the login yourself in the opened window, then run arana check <channel>.",
            "browser": opened,
        }));
    }
    if ch.session.is_empty() {
        return Err(bad(format!(
            "{name} has nothing to sync; log in in the dedicated browser"
        )));
    }
    if sync_existing {
        return Ok(json!({
            "logged_in": reach_channels::session::has_login_state(&ch.session, root)?,
            "channel": name,
            "sync": "No bridge sync is configured for this native session; existing session files were checked in place.",
            "login": login_state(ch, root),
        }));
    }
    run_native_login(ch, root).await
}

async fn run_native_login(ch: &reach_channels::Channel, root: &Path) -> Result<Value> {
    let command = if ch.session == "bili-cli" {
        "bili"
    } else if ch.session == "boss-agent-cli" {
        "boss"
    } else {
        return Err(bad(format!(
            "{} has no native login command recorded",
            ch.session
        )));
    };
    let program = reach_tools::registry::resolve(command, root);
    if program.is_none() {
        return Err(ReachError::new(
            "NEED_SETUP",
            format!("{command} is not installed; run arana add {}", ch.name),
        ));
    }
    let invocation = reach_tools::registry::resolve_invocation(command, root)
        .ok_or_else(|| ReachError::new("NEED_SETUP", format!("{command} cannot be launched")))?;
    let mut child = tokio::process::Command::new(&invocation.program);
    child
        .args(&invocation.args_prefix)
        .arg("login")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    if ch.session_home {
        for (key, value) in reach_channels::session::session_env(&ch.session, root)? {
            child.env(key, value);
        }
    }
    let mut child = child.spawn().map_err(ReachError::from)?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| ReachError::new("UPSTREAM_BROKEN", "login output is unavailable"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| ReachError::new("UPSTREAM_BROKEN", "login output is unavailable"))?;
    let out_task = tokio::spawn(async move {
        use tokio::io::{AsyncBufReadExt, BufReader};
        let mut lines = BufReader::new(stdout).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            eprintln!("{line}");
        }
    });
    let err_task = tokio::spawn(async move {
        use tokio::io::{AsyncBufReadExt, BufReader};
        let mut lines = BufReader::new(stderr).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            eprintln!("{line}");
        }
    });
    let status = match tokio::time::timeout(TOOL_TIMEOUT, child.wait()).await {
        Ok(result) => result.map_err(ReachError::from)?,
        Err(_) => {
            let _ = child.kill().await;
            return Err(ReachError::new("TIMEOUT", "native login timed out"));
        }
    };
    let _ = out_task.await;
    let _ = err_task.await;
    if !status.success() {
        return Err(ReachError::new(
            "UPSTREAM_BROKEN",
            format!("login did not complete (exit {:?})", status.code()),
        ));
    }
    if !reach_channels::session::has_login_state(&ch.session, root)? {
        return Err(ReachError::new(
            "UPSTREAM_BROKEN",
            "the login command succeeded but saved no usable login state",
        ));
    }
    Store::new(root).mutate(|data| {
        data["session_reuse"][&ch.name] = json!({"source":"manual","at":now()});
        data["session_revisions"][&ch.name] = json!(now());
    })?;
    Ok(json!({
        "logged_in": true,
        "channel": ch.name,
        "backend": ch.session,
        "login": login_state(ch, root),
    }))
}

fn logout_keys(channel: &str) -> &'static [&'static str] {
    // Pinned upstream 1.6.1 channel metadata that the frozen Rust catalog does
    // not expose; keep this list adjacent to the one channel that uses it.
    match channel {
        "xueqiu" => &["xueqiu_cookie"],
        _ => &[],
    }
}
fn delete_config_key(root: &Path, key: &str) -> Result<bool> {
    let path = root.join("config.yaml");
    if !path.exists() {
        return Ok(false);
    }
    let mut lock = None;
    for _ in 0..250 {
        match state::FileLock::try_acquire(root, "reach-config-write") {
            Ok(guard) => {
                lock = Some(guard);
                break;
            }
            Err(error) if error.code == "BUSY" => std::thread::sleep(Duration::from_millis(20)),
            Err(error) => return Err(error),
        }
    }
    let _lock = lock.ok_or_else(|| ReachError::new("BUSY", "configuration is busy"))?;
    let config = match state::read_private(&path, 1024 * 1024) {
        Ok(text) => serde_yaml::from_str::<Value>(&text)
            .map_err(|_| bad("Existing configuration is invalid YAML"))?,
        Err(error) if error.code == "NEED_SETUP" => return Ok(false),
        Err(error) => return Err(error),
    };
    if !config.is_object() {
        return Err(bad("Configuration must be an object"));
    }
    if config.get(key).is_none() {
        return Ok(false);
    }
    let mut config = config;
    config.as_object_mut().expect("checked object").remove(key);
    let bytes = serde_yaml::to_string(&config)
        .map_err(|_| ReachError::new("UPSTREAM_BROKEN", "Could not serialize configuration"))?;
    Store::new(root).mutate(|data| {
        data["owned_paths"]["config.yaml"] =
            json!({"owner":"arana","scope":"private","kind":"file","sha256":super::fingerprint(&bytes)});
    })?;
    state::atomic_write(&path, bytes.as_bytes())?;
    Ok(true)
}
async fn logout(args: &[String], root: &Path) -> Result<Value> {
    if args.len() != 1 || args[0].starts_with('-') {
        return Err(bad("usage: arana logout <channel>"));
    }
    let name = &args[0];
    let ch =
        reach_channels::channel(name).ok_or_else(|| bad(format!("unknown channel: {name}")))?;
    if ch.login_url.is_empty() && ch.session.is_empty() && logout_keys(name).is_empty() {
        return Err(bad(format!("{name} has no login state managed by arana")));
    }
    let mut removed = false;
    let mut cleared_cookies = 0usize;
    if !ch.session.is_empty() {
        removed |= reach_channels::session::clear_session(&ch.session, root)?;
    }
    if !ch.login_url.is_empty() {
        cleared_cookies = clear_browser_site(ch, root).await?;
        removed |= cleared_cookies > 0 || !ch.domains.is_empty();
    }
    for key in logout_keys(name) {
        removed |= delete_config_key(root, key)?;
    }
    Store::new(root).mutate(|data| {
        if let Some(map) = data["checks"].as_object_mut() {
            map.remove(name);
        }
        data["session_reuse"][name] = json!({"blocked":true,"at":now()});
        data["session_revisions"][name] = json!(now());
        if let Some(map) = data["backend_health"].as_object_mut() {
            map.remove(name);
        }
        if let Some(map) = data["cooldowns"].as_object_mut() {
            map.remove(name);
        }
    })?;
    Ok(json!({
        "channel": name,
        "removed": removed,
        "cleared_cookies": cleared_cookies,
        "backend": if ch.session.is_empty() { "config" } else { ch.session.as_str() },
        "login": login_state(ch, root),
    }))
}

pub(super) async fn run(command: &str, args: &[String], root: &Path) -> Result<Value> {
    match command {
        "doctor" => doctor(args, root).await,
        "check" => checks(args, root).await,
        "setup" => setup(args, root).await,
        "add" => add(args, root).await,
        "remove" => remove(args, root).await,
        "update" => update(args, root).await,
        "uninstall" => uninstall(args, root).await,
        "skill" => {
            let action = args.first().map(String::as_str).unwrap_or("status");
            let allowed = matches!(action, "status" | "link" | "unlink" | "update");
            if !allowed {
                return Err(bad(
                    "usage: arana skill [status|link <skills-dir>|unlink <skills-dir>|update]",
                ));
            }
            fsops::skill(root, args)
        }
        "login" => login(args, root).await,
        "logout" => logout(args, root).await,
        _ => Err(bad("Invalid management command")),
    }
}

/// Human-readable output for the explicit `--text` modes. JSON stays the
/// default and is never mixed into these lines.
pub(super) fn render_text(command: &str, result: &Value) -> Option<String> {
    match command {
        "check" => {
            let rows = result["results"].as_array()?;
            let mut by_action = BTreeMap::new();
            let mut out = String::new();
            for row in rows {
                let passed = row["passed"] == true;
                out.push_str(&format!(
                    "{}  {} {} [{}] {}ms",
                    if passed { "PASS" } else { "FAIL" },
                    row["channel"].as_str().unwrap_or(""),
                    row["action"].as_str().unwrap_or(""),
                    row["backend"].as_str().unwrap_or(""),
                    row["elapsed_ms"].as_u64().unwrap_or(0)
                ));
                if !passed {
                    out.push_str(&format!(
                        "  {}: {}",
                        row["error"]["code"].as_str().unwrap_or(""),
                        row["error"]["message"].as_str().unwrap_or("")
                    ));
                }
                out.push('\n');
                let key = (
                    row["channel"].as_str().unwrap_or("").to_owned(),
                    row["action"].as_str().unwrap_or("").to_owned(),
                );
                let entry = by_action.entry(key).or_insert(false);
                *entry |= passed;
            }
            let working = by_action.values().filter(|value| **value).count();
            out.push_str(&format!(
                "\n{working}/{} actions working\n",
                by_action.len()
            ));
            Some(out)
        }
        "doctor" => {
            let mut out = format!("arana {}\n\n", result["version"].as_str().unwrap_or(""));
            for row in result["channels"].as_array().into_iter().flatten() {
                let problems = row["problems"]
                    .as_array()
                    .map(|items| {
                        items
                            .iter()
                            .filter_map(Value::as_str)
                            .collect::<Vec<_>>()
                            .join("; ")
                    })
                    .unwrap_or_default();
                let extra = if row["regressed"] == true {
                    format!("REGRESSED (worked at the previous check) {problems}")
                } else if !problems.is_empty() {
                    problems
                } else {
                    row["next"].as_str().unwrap_or("").to_owned()
                };
                out.push_str(&format!(
                    "{:<13} {:<12} {:<13} {}\n",
                    row["status"].as_str().unwrap_or(""),
                    row["channel"].as_str().unwrap_or(""),
                    row["level"].as_str().unwrap_or(""),
                    extra
                ));
            }
            if let Some(tools) = result["tools"].as_array().filter(|list| !list.is_empty()) {
                out.push_str("\nTools\n");
                for tool in tools {
                    out.push_str(&format!(
                        "{:<16} {:<12} {}\n",
                        tool["tool"].as_str().unwrap_or(""),
                        tool["version"].as_str().unwrap_or("?"),
                        tool["owner"].as_str().unwrap_or("")
                    ));
                }
            }
            Some(out)
        }
        "setup" => result["installation_report"]["markdown"]
            .as_str()
            .map(str::to_owned),
        _ => None,
    }
}
