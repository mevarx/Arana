//! Ownership-aware filesystem operations for skills, migration, and uninstall.
use reach_core::{
    state::{self, FileLock, Store},
    ReachError, Result,
};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    fs,
    fs::OpenOptions,
    io::{Read, Write},
    path::{Path, PathBuf},
};

const SKILL_NAME: &str = "arana";
const MAX_FILE: u64 = 10 * 1024 * 1024;
const MAX_TOTAL: u64 = 50 * 1024 * 1024;
const MAX_FILES: usize = 1000;
const SKILL: &str = include_str!("../../../../skills/arana/SKILL.md");

fn bad(message: impl Into<String>) -> ReachError {
    ReachError::new("BAD_INPUT", message)
}
fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn safe(path: &Path) -> Result<()> {
    state::ensure_safe(path)
}
fn checked_read(path: &Path) -> Result<Vec<u8>> {
    safe(path)?;
    let meta = fs::symlink_metadata(path)?;
    if !meta.file_type().is_file() || meta.len() > MAX_FILE {
        return Err(bad("Refusing non-regular or oversized file"));
    }
    let mut bytes = Vec::with_capacity(meta.len() as usize);
    fs::File::open(path)?
        .take(MAX_FILE + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_FILE {
        return Err(bad("File exceeds copy budget"));
    }
    Ok(bytes)
}
fn state_value(root: &Path) -> Result<Value> {
    Store::new(root).load()
}
fn set_skill_link(root: &Path, record: Value) -> Result<()> {
    Store::new(root).mutate(|s| {
        let links = s["skill"]["links"]
            .as_array_mut()
            .expect("normalized links");
        links.retain(|v| v["path"] != record["path"]);
        links.push(record);
    })
}
fn skill_real(root: &Path) -> PathBuf {
    root.join("SKILL.md")
}
fn materialize(root: &Path) -> Result<String> {
    let path = skill_real(root);
    safe(&path)?;
    if path.exists() {
        let old = checked_read(&path)?;
        // Compare line-wise: a checkout with CRLF endings is still our skill,
        // and refusing it would look like an unrelated file occupying the path.
        let text = String::from_utf8_lossy(&old);
        let mut lines = text
            .lines()
            .map(|line| line.trim_end_matches('\r').to_string());
        let is_skill = lines.next().as_deref() == Some("---")
            && lines.next().as_deref() == Some("name: arana");
        if !is_skill {
            return Err(bad(
                "An unrelated file occupies the skill path; left untouched",
            ));
        }
    }
    state::atomic_write(&path, SKILL.as_bytes())?;
    let hash = digest(SKILL.as_bytes());
    Store::new(root).mutate(|s| {
        s["owned_paths"]["SKILL.md"] =
            json!({"owner":"arana","scope":"private","kind":"file","sha256":hash});
    })?;
    Ok(hash)
}
fn make_dirs(path: &Path) -> Result<()> {
    safe(path)?;
    fs::create_dir_all(path)?;
    safe(path)
}
fn skill_status(root: &Path) -> Result<Value> {
    let data = state_value(root)?;
    let links = data["skill"]["links"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let installed = skill_real(root).is_file();
    Ok(json!({"installed":installed,"path":skill_real(root).to_string_lossy(),"links":links}))
}

/// Manage status/link/unlink; link and unlink take an explicit skills directory.
pub fn skill(root: &Path, args: &[String]) -> Result<Value> {
    let action = args.first().map(String::as_str).unwrap_or("status");
    match action {
        "status" if args.len() == 1 => skill_status(root),
        "link" if args.len() == 2 => {
            let dir = PathBuf::from(&args[1]);
            if !dir.is_absolute() || dir.file_name().is_some_and(|n| n == SKILL_NAME) {
                return Err(bad("link requires an absolute skills directory"));
            }
            let target = dir.join(SKILL_NAME).join("SKILL.md");
            safe(&target)?;
            let _lock = FileLock::try_acquire(root, "arana-skill")?;
            let hash = materialize(root)?;
            let bytes = SKILL.as_bytes();
            let created = if target.exists() {
                let current = checked_read(&target)?;
                if digest(&current) != hash {
                    return Err(bad("Existing skill differs; left untouched"));
                }
                false
            } else {
                let parent = target.parent().ok_or_else(|| bad("Invalid skill target"))?;
                make_dirs(parent)?;
                state::atomic_write(&target, bytes)?;
                true
            };
            if created {
                set_skill_link(
                    root,
                    json!({"agent":"custom","path":target.to_string_lossy(),"kind":"copy","owned":true,"sha256":digest(bytes),"target":dir.to_string_lossy()}),
                )?;
            }
            skill_status(root)
        }
        "update" if args.len() == 1 => {
            let _lock = FileLock::try_acquire(root, "arana-skill")?;
            let hash = materialize(root)?;
            let mut data = state_value(root)?;
            let mut updated = Vec::new();
            let mut skipped = Vec::new();
            if let Some(links) = data["skill"]["links"].as_array_mut() {
                for rec in links.iter_mut() {
                    let Some(path) = rec["path"].as_str().map(str::to_owned) else {
                        continue;
                    };
                    if rec["kind"] != "copy" || rec["owned"] != true {
                        skipped.push(json!(path));
                        continue;
                    }
                    let target = PathBuf::from(&path);
                    if !target.is_absolute()
                        || rec["target"].as_str().is_none_or(|dir| {
                            target != Path::new(dir).join(SKILL_NAME).join("SKILL.md")
                        })
                    {
                        skipped.push(json!(path));
                        continue;
                    }
                    if safe(&target).is_err() {
                        skipped.push(json!(path));
                        continue;
                    }
                    let recorded = rec["sha256"].as_str().unwrap_or("").to_owned();
                    match checked_read(&target) {
                        Ok(current) if digest(&current) == hash => {
                            rec["sha256"] = json!(hash);
                        }
                        Ok(current) if recorded == digest(&current) => {
                            state::atomic_write(&target, SKILL.as_bytes())?;
                            rec["sha256"] = json!(hash);
                            updated.push(json!(path));
                        }
                        _ => skipped.push(json!(path)),
                    }
                }
            }
            let links = data["skill"]["links"].clone();
            Store::new(root).mutate(|saved| saved["skill"]["links"] = links)?;
            Ok(json!({
                "installed": skill_real(root).is_file(),
                "path": skill_real(root).to_string_lossy(),
                "updated": updated,
                "skipped": skipped,
            }))
        }
        "unlink" if args.len() == 2 => {
            let dir = PathBuf::from(&args[1]);
            if !dir.is_absolute() || dir.file_name().is_some_and(|n| n == SKILL_NAME) {
                return Err(bad("unlink requires an absolute skills directory"));
            }
            let target = dir.join(SKILL_NAME).join("SKILL.md");
            safe(&target)?;
            let _lock = FileLock::try_acquire(root, "arana-skill")?;
            let mut data = state_value(root)?;
            let links = data["skill"]["links"]
                .as_array_mut()
                .ok_or_else(|| bad("Invalid skill ledger"))?;
            let rec = links
                .iter()
                .find(|v| v["path"].as_str() == Some(target.to_string_lossy().as_ref()))
                .cloned();
            let Some(rec) = rec else {
                // A pre-existing identical copy is not ours and must survive.
                if target.exists()
                    && checked_read(&target)
                        .is_ok_and(|bytes| digest(&bytes) == digest(SKILL.as_bytes()))
                {
                    let mut status = skill_status(root)?;
                    status["removed"] = json!(false);
                    status["owned"] = json!(false);
                    status["preexisting"] = json!(true);
                    return Ok(status);
                }
                return Err(bad("No matching recorded skill link; left untouched"));
            };
            if rec["target"].as_str() != Some(dir.to_string_lossy().as_ref()) {
                return Err(bad("Recorded target mismatch; left untouched"));
            }
            if rec["owned"] != true {
                return Err(bad("Recorded skill is not owned by arana; left untouched"));
            }
            if rec["kind"] == "copy" {
                let current = checked_read(&target)?;
                if rec["sha256"] != digest(&current) {
                    return Err(bad("Recorded skill was modified; left untouched"));
                }
                fs::remove_file(&target)?;
                if let Some(parent) = target.parent() {
                    let _ = fs::remove_dir(parent);
                }
            } else {
                return Err(bad("Unsupported recorded skill kind"));
            }
            links.retain(|v| v["path"].as_str() != Some(target.to_string_lossy().as_ref()));
            let unlinked = data["skill"]["unlinked"]
                .as_array_mut()
                .ok_or_else(|| bad("Invalid skill ledger"))?;
            if !unlinked.iter().any(|v| v == &json!(dir.to_string_lossy())) {
                unlinked.push(json!(dir.to_string_lossy()));
            }
            Store::new(root).mutate(|saved| saved["skill"] = data["skill"].clone())?;
            skill_status(root)
        }
        _ => Err(bad(
            "Expected skill status, link <absolute-skills-dir>, or unlink <absolute-skills-dir>",
        )),
    }
}

/// Write the embedded skill and optionally install a copy into an explicit host skills directory.
pub fn setup_skill(root: &Path, target: Option<&Path>) -> Result<Value> {
    let _lock = FileLock::try_acquire(root, "arana-skill")?;
    let hash = materialize(root)?;
    let home = root
        .parent()
        .ok_or_else(|| bad("State root has no home directory"))?;
    let mut candidates = Vec::<(PathBuf, String)>::new();
    if let Some(dir) = target {
        candidates.push((dir.to_path_buf(), "custom".to_string()));
    } else {
        for (agent, config, skills) in [
            (
                "claude-code",
                home.join(".claude"),
                home.join(".claude/skills"),
            ),
            (
                "openclaw",
                home.join(".openclaw"),
                home.join(".openclaw/skills"),
            ),
            ("hermes", home.join(".hermes"), home.join(".hermes/skills")),
            ("cursor", home.join(".cursor"), home.join(".cursor/skills")),
            (
                "gemini-cli",
                home.join(".gemini"),
                home.join(".gemini/skills"),
            ),
            (
                "github-copilot",
                home.join(".copilot"),
                home.join(".copilot/skills"),
            ),
            (
                "windsurf",
                home.join(".codeium/windsurf"),
                home.join(".codeium/windsurf/skills"),
            ),
            ("codex", home.join(".codex"), home.join(".agents/skills")),
            (
                "opencode",
                home.join(".config/opencode"),
                home.join(".agents/skills"),
            ),
        ] {
            if safe(&config).is_ok()
                && fs::symlink_metadata(&config)
                    .is_ok_and(|m| m.is_dir() && !m.file_type().is_symlink())
            {
                candidates.push((skills, agent.to_string()));
            }
        }
    }
    let old = state_value(root)?;
    let unlinked = old["skill"]["unlinked"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    for (dir, agent) in candidates {
        if !dir.is_absolute() || dir.file_name().is_some_and(|n| n == SKILL_NAME) {
            return Err(bad("skill target must be an absolute skills directory"));
        }
        if target.is_none()
            && unlinked
                .iter()
                .any(|p| p.as_str() == Some(dir.to_string_lossy().as_ref()))
        {
            continue;
        }
        let created = copy_into(&dir, &agent, &hash)?;
        let path = dir.join(SKILL_NAME).join("SKILL.md");
        if created {
            Store::new(root).mutate(|s| {
                let links = s["skill"]["links"].as_array_mut().expect("normalized links");
                links.retain(|v| v["path"].as_str() != Some(path.to_string_lossy().as_ref()));
                links.push(json!({"agent":agent,"path":path.to_string_lossy(),"kind":"copy","owned":true,"sha256":hash,"target":dir.to_string_lossy()}));
                let unlinked = s["skill"]["unlinked"].as_array_mut().expect("normalized unlinked");
                unlinked.retain(|v| v.as_str() != Some(dir.to_string_lossy().as_ref()));
            })?;
        } else {
            // Never adopt a pre-existing identical copy; drop an old unowned claim.
            Store::new(root).mutate(|s| {
                if let Some(links) = s["skill"]["links"].as_array_mut() {
                    links.retain(|v| {
                        v["path"].as_str() != Some(path.to_string_lossy().as_ref())
                            || v["owned"] == true
                    });
                }
            })?;
        }
    }
    skill_status(root)
}

fn copy_into(dir: &Path, _agent: &str, hash: &str) -> Result<bool> {
    let path = dir.join(SKILL_NAME).join("SKILL.md");
    safe(&path)?;
    if path.exists() {
        if digest(&checked_read(&path)?) != hash {
            return Err(bad("Existing skill differs; left untouched"));
        }
        Ok(false)
    } else {
        make_dirs(path.parent().ok_or_else(|| bad("Invalid skill target"))?)?;
        state::atomic_write(&path, SKILL.as_bytes())?;
        Ok(true)
    }
}

fn gather_copy(
    src: &Path,
    dst: &Path,
    plan: &mut Vec<(PathBuf, PathBuf, Vec<u8>)>,
    conflicts: &mut Vec<String>,
    total: &mut u64,
) -> Result<()> {
    safe(src)?;
    let meta = fs::symlink_metadata(src)?;
    if meta.file_type().is_symlink() {
        return Err(bad("Migration source contains a link/reparse point"));
    }
    if meta.is_dir() {
        for entry in fs::read_dir(src)? {
            let entry = entry?;
            gather_copy(
                &entry.path(),
                &dst.join(entry.file_name()),
                plan,
                conflicts,
                total,
            )?;
        }
    } else if meta.is_file() {
        if plan.len() >= MAX_FILES {
            return Err(bad("Migration exceeds file-count budget"));
        }
        let bytes = checked_read(src)?;
        *total = total.saturating_add(bytes.len() as u64);
        if *total > MAX_TOTAL {
            return Err(bad("Migration exceeds total copy budget"));
        }
        safe(dst)?;
        if dst.exists() {
            conflicts.push(dst.to_string_lossy().into_owned());
        } else {
            plan.push((src.to_path_buf(), dst.to_path_buf(), bytes));
        }
    } else {
        return Err(bad("Migration source contains a non-file entry"));
    }
    Ok(())
}

/// Preview or import only known legacy config/state/session/browser paths; source is never changed.
pub fn migrate(root: &Path, legacy: &Path, confirm: bool) -> Result<Value> {
    safe(legacy)?;
    let mut plan = Vec::new();
    let mut conflicts = Vec::new();
    let mut total = 0;
    for name in ["config.yaml", "state.json", "sessions", "browser/profile"] {
        let source = legacy.join(name);
        match fs::symlink_metadata(&source) {
            Ok(_) => gather_copy(
                &source,
                &root.join(name),
                &mut plan,
                &mut conflicts,
                &mut total,
            )?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
    }
    let files: Vec<String> = plan
        .iter()
        .map(|(_, d, _)| d.to_string_lossy().into_owned())
        .collect();
    if !confirm {
        return Ok(
            json!({"dry_run":true,"imported":[],"would_import":files,"conflicts":conflicts,"bytes":total}),
        );
    }
    if !conflicts.is_empty() {
        return Err(bad("Migration destination collision; nothing imported"));
    }
    let _lock = FileLock::try_acquire(root, "arana-migrate")?;
    // Revalidate every target under lock, then use create_new so a late collision cannot be overwritten.
    for (_, dst, _) in &plan {
        safe(dst)?;
        if dst.exists() {
            return Err(bad(
                "Migration destination appeared during import; nothing overwritten",
            ));
        }
    }
    let mut planned_session_roots = BTreeSet::new();
    for (_, dst, _) in &plan {
        let normalized = dst
            .strip_prefix(root)
            .unwrap_or(dst)
            .to_string_lossy()
            .replace(char::from(92), "/");
        if let Some(rest) = normalized.strip_prefix("sessions/") {
            if let Some(tool) = rest.split('/').next().filter(|name| !name.is_empty()) {
                planned_session_roots.insert(format!("sessions/{tool}"));
            }
        }
    }
    let preexisting_session_roots: BTreeSet<String> = planned_session_roots
        .iter()
        .filter(|relative| root.join(relative).exists())
        .cloned()
        .collect();
    let mut created = Vec::new();
    for (_, dst, bytes) in &plan {
        let parent = dst
            .parent()
            .ok_or_else(|| bad("Invalid migration destination"))?;
        state::private_dir(parent)?;
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = match options.open(dst) {
            Ok(file) => file,
            Err(error) => {
                for path in &created {
                    let _ = fs::remove_file(path);
                }
                return Err(error.into());
            }
        };
        created.push(dst.clone());
        if let Err(error) = file.write_all(bytes).and_then(|_| file.sync_all()) {
            for path in &created {
                let _ = fs::remove_file(path);
            }
            return Err(error.into());
        }
    }
    let imported_state = plan
        .iter()
        .any(|(_, dst, _)| dst == &root.join("state.json"));
    Store::new(root).mutate(|s| {
        // Never inherit ownership claims or external skill links from an
        // imported state.json. Only files actually copied under this root are
        // recorded as arana-owned below. An existing arana state is preserved
        // unless state.json itself was imported.
        if imported_state {
            s["skill"] = json!({"links":[],"unlinked":[]});
            for key in [
                "channels",
                "tools",
                "checks",
                "backend_health",
                "cooldowns",
                "request_starts",
                "session_reuse",
                "session_revisions",
                "reuse_sources",
                "setup_failures",
                "session_migrations",
            ] {
                s[key] = json!({});
            }
            s["owned_paths"] = json!({});
        }
        s["migrated_from"] = json!("agent-reach");
        let mut session_roots = BTreeSet::new();
        for (_, dst, bytes) in &plan {
            let rel = dst
                .strip_prefix(root)
                .unwrap_or(dst)
                .to_string_lossy()
                .into_owned();
            let normalized = rel.replace(char::from(92), "/");
            if let Some(rest) = normalized.strip_prefix("sessions/") {
                if let Some(tool) = rest.split('/').next().filter(|name| !name.is_empty()) {
                    session_roots.insert(format!("sessions/{tool}"));
                }
            }
            let scope = if normalized.starts_with("sessions/") {
                "session"
            } else {
                "private"
            };
            s["owned_paths"][rel] =
                json!({"owner":"arana","scope":scope,"kind":"file","sha256":digest(bytes)});
        }
        for relative in session_roots {
            if root.join(&relative).is_dir() && !preexisting_session_roots.contains(&relative) {
                // A copied session must be explicitly owned as a directory so
                // native tools may reuse it; no legacy ledger entry is trusted.
                s["owned_paths"][relative] =
                    json!({"owner":"arana","scope":"session","kind":"directory"});
            }
        }
    })?;
    Ok(json!({"dry_run":false,"imported":files,"bytes":total,"source_preserved":true}))
}

fn owned_leaf(root: &Path, rel: &str, rec: &Value) -> Option<PathBuf> {
    let p = Path::new(rel);
    let normalized = rel.replace(char::from(92), "/");
    if normalized != "config.yaml"
        && normalized != "SKILL.md"
        && normalized != "installation-report.md"
        && !normalized.starts_with("sessions/")
        && !normalized.starts_with("browser/profile/")
        && !normalized.starts_with("browser/extension/")
    {
        return None;
    }
    if p.is_absolute()
        || p.components()
            .any(|c| !matches!(c, std::path::Component::Normal(_)))
        || rec["owner"] != "arana"
        || !matches!(rec["scope"].as_str(), Some("private" | "session"))
        || rec["kind"] != "file"
    {
        return None;
    }
    let path = root.join(p);
    safe(&path).ok()?;
    if fs::symlink_metadata(&path).ok()?.file_type().is_file() {
        Some(path)
    } else {
        None
    }
}

/// Preview or delete only owned recorded leaves and matching, unchanged skill copies.
pub fn uninstall(root: &Path, confirm: bool) -> Result<Value> {
    let data = state_value(root)?;
    let mut targets = Vec::new();
    if let Some(paths) = data["owned_paths"].as_object() {
        for (rel, rec) in paths {
            if rel == "state.json" && rec == "state ledger" {
                let path = root.join("state.json");
                if safe(&path).is_ok()
                    && fs::symlink_metadata(&path).is_ok_and(|m| m.file_type().is_file())
                {
                    targets.push(path);
                }
                continue;
            }
            if let Some(path) = owned_leaf(root, rel, rec) {
                if let Some(expected) = rec["sha256"].as_str() {
                    if checked_read(&path).is_ok_and(|bytes| digest(&bytes) == expected) {
                        targets.push(path);
                    }
                }
            }
        }
    }
    if let Some(links) = data["skill"]["links"].as_array() {
        for rec in links {
            let Some(pathstr) = rec["path"].as_str() else {
                continue;
            };
            let path = PathBuf::from(pathstr);
            let Some(targetstr) = rec["target"].as_str() else {
                continue;
            };
            let target = PathBuf::from(targetstr);
            if rec["kind"] == "copy"
                && rec["owned"] == true
                && target.is_absolute()
                && path == target.join(SKILL_NAME).join("SKILL.md")
                && safe(&path).is_ok()
            {
                if let Ok(bytes) = checked_read(&path) {
                    if rec["sha256"] == digest(&bytes) {
                        targets.push(path);
                    }
                }
            }
        }
    }
    targets.sort();
    targets.dedup();
    let names: Vec<String> = targets
        .iter()
        .map(|p| p.to_string_lossy().into_owned())
        .collect();
    if !confirm {
        return Ok(
            json!({"dry_run":true,"removed":[],"would_remove":names,"kept_unrecognized":true}),
        );
    }
    let _lock = FileLock::try_acquire(root, "arana-uninstall")?;
    // Revalidate immediately before deletion; directories are never recursively removed.
    let mut removed = Vec::new();
    for path in targets {
        safe(&path)?;
        if fs::symlink_metadata(&path).is_ok_and(|m| m.file_type().is_file()) {
            fs::remove_file(&path)?;
            removed.push(path.to_string_lossy().into_owned());
            if path.ends_with("SKILL.md") {
                if let Some(d) = path.parent() {
                    let _ = fs::remove_dir(d);
                }
            }
        }
    }
    Ok(json!({"dry_run":false,"removed":removed,"kept_unrecognized":true}))
}
