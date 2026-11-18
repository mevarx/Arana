#[path = "../src/management/fsops.rs"]
mod fsops;

use serde_json::Value;
use std::{fs, path::Path};
use tempfile::tempdir;

fn ok(v: Value) -> bool {
    v["ok"].as_bool().unwrap_or(true)
}

#[test]
fn setup_materializes_and_status_reports_skill() {
    let temp = tempdir().unwrap();
    let root = temp.path().join("arana");
    let target = temp.path().join("skills");
    let result = fsops::setup_skill(&root, Some(&target)).unwrap();
    assert!(ok(result));
    fsops::setup_skill(&root, Some(&target)).unwrap();
    assert!(root.join("SKILL.md").is_file());
    assert!(
        target.join("arana/SKILL.md").is_file()
            || fs::symlink_metadata(target.join("arana")).is_ok()
    );
    let status = fsops::skill(&root, &["status".into()]).unwrap();
    assert!(status["installed"].as_bool().unwrap_or(false));
    assert!(Path::new(status["path"].as_str().unwrap()).is_file());
}

#[test]
fn setup_discovers_only_known_agent_targets_beside_isolated_root() {
    let temp = tempdir().unwrap();
    let root = temp.path().join("arana");
    fs::create_dir_all(temp.path().join(".claude")).unwrap();
    fsops::setup_skill(&root, None).unwrap();
    assert!(temp.path().join(".claude/skills/arana/SKILL.md").is_file());
}

#[test]
fn installed_skill_carries_actionable_setup_without_automatic_install() {
    let temp = tempdir().unwrap();
    let root = temp.path().join("arana");
    let target = temp.path().join("skills");
    fsops::setup_skill(&root, Some(&target)).unwrap();
    let text = fs::read_to_string(root.join("SKILL.md")).unwrap();
    for needle in [
        "arana version",
        "https://github.com/mevarx/Arana/releases",
        ".sha256",
        "SHA256SUMS",
    ] {
        assert!(
            text.contains(needle),
            "skill lacks setup guidance: {needle}"
        );
    }
    assert!(
        text.to_ascii_lowercase()
            .contains("do not download, install, or update"),
        "skill must forbid automatic installation"
    );
}

#[test]
fn explicit_skill_link_and_unlink_are_idempotent_and_recorded() {
    let temp = tempdir().unwrap();
    let root = temp.path().join("arana");
    let dir = temp.path().join("skills");
    let link = vec!["link".into(), dir.to_string_lossy().into_owned()];
    fsops::skill(&root, &link).unwrap();
    fsops::skill(&root, &link).unwrap();
    let path = dir.join("arana/SKILL.md");
    assert!(path.is_file());
    let unlink = vec!["unlink".into(), dir.to_string_lossy().into_owned()];
    fsops::skill(&root, &unlink).unwrap();
    assert!(!path.exists());
    assert!(fsops::skill(&root, &unlink).is_err());
}

#[test]
fn unlink_refuses_unrecorded_and_preserves_changed_external_skill() {
    let temp = tempdir().unwrap();
    let root = temp.path().join("arana");
    let target = temp.path().join("skills");
    fs::create_dir_all(target.join("arana")).unwrap();
    fs::write(target.join("arana/SKILL.md"), "unowned").unwrap();
    assert!(fsops::skill(&root, &["unlink".into(), target.to_string_lossy().into()]).is_err());
    assert_eq!(
        fs::read_to_string(target.join("arana/SKILL.md")).unwrap(),
        "unowned"
    );
}

#[test]
fn unlink_preserves_a_recorded_copy_after_external_edit() {
    let temp = tempdir().unwrap();
    let root = temp.path().join("arana");
    let target = temp.path().join("skills");
    fsops::setup_skill(&root, Some(&target)).unwrap();
    let installed = target.join("arana/SKILL.md");
    fs::write(&installed, "changed by user").unwrap();
    assert!(fsops::skill(&root, &["unlink".into(), target.to_string_lossy().into()]).is_err());
    assert_eq!(fs::read_to_string(installed).unwrap(), "changed by user");
}

#[cfg(unix)]
#[test]
fn setup_refuses_symlinked_skills_parent() {
    use std::os::unix::fs::symlink;
    let temp = tempdir().unwrap();
    let root = temp.path().join("arana");
    let actual = temp.path().join("actual");
    let alias = temp.path().join("alias");
    fs::create_dir_all(&actual).unwrap();
    symlink(&actual, &alias).unwrap();
    assert!(fsops::setup_skill(&root, Some(&alias)).is_err());
    assert!(!actual.join("arana").exists());
}

#[test]
fn uninstall_ignores_forged_external_skill_record_with_wrong_target() {
    use sha2::{Digest, Sha256};
    let temp = tempdir().unwrap();
    let root = temp.path().join("arana");
    let outside = temp.path().join("outsider/SKILL.md");
    fs::create_dir_all(outside.parent().unwrap()).unwrap();
    let content = b"owned-looking skill";
    fs::write(&outside, content).unwrap();
    let hash = format!("{:x}", Sha256::digest(content));
    reach_core::state::Store::new(&root).mutate(|s| {
        s["skill"]["links"] = serde_json::json!([{"kind":"copy","path":outside,"target":temp.path().join("different").to_string_lossy(),"sha256":hash}]);
    }).unwrap();
    fsops::uninstall(&root, true).unwrap();
    assert_eq!(fs::read(outside).unwrap(), content);
}

#[test]
fn confirmed_uninstall_removes_only_hash_matching_owned_config_leaf() {
    use sha2::{Digest, Sha256};
    let temp = tempdir().unwrap();
    let root = temp.path().join("arana");
    fs::create_dir_all(&root).unwrap();
    let config = b"setting: private\n";
    fs::write(root.join("config.yaml"), config).unwrap();
    fs::write(root.join("unowned.txt"), "keep").unwrap();
    let hash = format!("{:x}", Sha256::digest(config));
    reach_core::state::Store::new(&root)
        .mutate(|s| {
            s["owned_paths"]["config.yaml"] =
                serde_json::json!({"owner":"arana","scope":"private","kind":"file","sha256":hash});
        })
        .unwrap();
    fs::write(root.join("config.yaml"), "externally edited\n").unwrap();
    fsops::uninstall(&root, true).unwrap();
    assert_eq!(
        fs::read_to_string(root.join("config.yaml")).unwrap(),
        "externally edited\n"
    );
    assert_eq!(
        fs::read_to_string(root.join("unowned.txt")).unwrap(),
        "keep"
    );
}

#[test]
fn migration_preview_and_collision_are_nondestructive() {
    let temp = tempdir().unwrap();
    let root = temp.path().join("arana");
    let legacy = temp.path().join("legacy");
    fs::create_dir_all(&legacy).unwrap();
    fs::write(legacy.join("config.yaml"), "keep: old\n").unwrap();
    fs::create_dir_all(&root).unwrap();
    fs::write(root.join("config.yaml"), "keep: new\n").unwrap();
    let preview = fsops::migrate(&root, &legacy, false).unwrap();
    assert_eq!(preview["dry_run"], true);
    assert_eq!(
        fs::read_to_string(legacy.join("config.yaml")).unwrap(),
        "keep: old\n"
    );
    assert!(fsops::migrate(&root, &legacy, true).is_err());
    assert_eq!(
        fs::read_to_string(root.join("config.yaml")).unwrap(),
        "keep: new\n"
    );
}

#[test]
fn migration_imports_known_files_without_changing_legacy_source() {
    let temp = tempdir().unwrap();
    let root = temp.path().join("arana");
    let legacy = temp.path().join("legacy");
    fs::create_dir_all(legacy.join("sessions/tool")).unwrap();
    fs::create_dir_all(legacy.join("browser/profile")).unwrap();
    fs::write(legacy.join("config.yaml"), "key: value\n").unwrap();
    fs::write(legacy.join("sessions/tool/session.json"), "{\"ok\":true}").unwrap();
    fs::write(legacy.join("browser/profile/Preferences"), "{}\n").unwrap();
    let result = fsops::migrate(&root, &legacy, true).unwrap();
    assert_eq!(result["source_preserved"], true);
    assert_eq!(
        fs::read_to_string(root.join("config.yaml")).unwrap(),
        "key: value\n"
    );
    assert_eq!(
        fs::read_to_string(legacy.join("config.yaml")).unwrap(),
        "key: value\n"
    );
    assert!(root.join("sessions/tool/session.json").is_file());
    assert!(root.join("browser/profile/Preferences").is_file());
}

#[test]
fn uninstall_preview_and_forged_ledger_do_not_delete_unowned_files() {
    let temp = tempdir().unwrap();
    let root = temp.path().join("arana");
    fs::create_dir_all(root.join("keep")).unwrap();
    fs::write(root.join("keep/user.txt"), "mine").unwrap();
    fs::create_dir_all(root.join("outside")).unwrap();
    fs::write(root.join("outside/file"), "mine").unwrap();
    let store = reach_core::state::Store::new(&root);
    store
        .mutate(|s| {
            s["owned_paths"]["outside"] =
                serde_json::json!({"owner":"arana","scope":"private","kind":"directory"})
        })
        .unwrap();
    let preview = fsops::uninstall(&root, false).unwrap();
    assert_eq!(preview["dry_run"], true);
    assert!(root.join("outside/file").is_file());
    assert!(root.join("keep/user.txt").is_file());
}

#[test]
fn uninstall_removes_only_matching_owned_browser_extension_files() {
    use sha2::{Digest, Sha256};
    let temp = tempdir().unwrap();
    let root = temp.path().join("arana");
    let owned = root.join("browser/extension/1.0.27/page.js");
    let changed = root.join("browser/extension/1.0.27/changed.js");
    fs::create_dir_all(owned.parent().unwrap()).unwrap();
    fs::write(&owned, "extension asset").unwrap();
    fs::write(&changed, "claimed asset").unwrap();
    let hash = format!("{:x}", Sha256::digest(b"extension asset"));
    reach_core::state::Store::new(&root)
        .mutate(|s| {
            s["owned_paths"]["browser/extension/1.0.27/page.js"] = serde_json::json!({
                "owner":"arana","scope":"private","kind":"file","sha256":hash
            });
            s["owned_paths"]["browser/extension/1.0.27/changed.js"] = serde_json::json!({
                "owner":"arana","scope":"private","kind":"file","sha256":format!("{:x}", Sha256::digest(b"different"))
            });
        })
        .unwrap();
    fsops::uninstall(&root, true).unwrap();
    assert!(!owned.exists());
    assert_eq!(fs::read_to_string(&changed).unwrap(), "claimed asset");
}

#[test]
fn crlf_private_skill_copy_is_refreshed_instead_of_refused() {
    let temp = tempdir().unwrap();
    let root = temp.path().join("arana");
    fsops::setup_skill(&root, None).unwrap();
    let path = root.join("SKILL.md");
    let crlf = fs::read_to_string(&path).unwrap().replace('\n', "\r\n");
    fs::write(&path, crlf).unwrap();
    fsops::skill(&root, &["update".into()]).expect("a CRLF copy is still our skill");
    assert!(fs::read_to_string(&path)
        .unwrap()
        .starts_with("---\nname: arana\n"));
}

#[test]
fn skill_update_refreshes_unchanged_copy_and_preserves_a_user_edit() {
    use sha2::{Digest, Sha256};
    let temp = tempdir().unwrap();
    let root = temp.path().join("arana");
    let target = temp.path().join("skills");
    fsops::setup_skill(&root, Some(&target)).unwrap();
    let installed = target.join("arana/SKILL.md");
    fs::write(&installed, "old arana copy").unwrap();
    let old_hash = format!("{:x}", Sha256::digest(b"old arana copy"));
    reach_core::state::Store::new(&root)
        .mutate(|s| {
            for link in s["skill"]["links"].as_array_mut().unwrap() {
                if link["path"]
                    .as_str()
                    .is_some_and(|path| std::path::Path::new(path) == installed)
                {
                    link["sha256"] = serde_json::json!(old_hash);
                }
            }
        })
        .unwrap();
    let after: Value =
        serde_json::from_str(&std::fs::read_to_string(root.join("state.json")).unwrap()).unwrap();
    assert_eq!(
        after["skill"]["links"][0]["sha256"].as_str(),
        Some(old_hash.as_str()),
        "test fixture did not rewrite the recorded hash"
    );
    let result = fsops::skill(&root, &["update".into()]).unwrap();
    assert_eq!(result["updated"].as_array().unwrap().len(), 1, "{result}");
    let refreshed = fs::read_to_string(&installed).unwrap();
    assert!(refreshed.starts_with("---\nname: arana\n"));
    fs::write(&installed, "user edit").unwrap();
    let result = fsops::skill(&root, &["update".into()]).unwrap();
    assert_eq!(result["updated"].as_array().unwrap().len(), 0);
    assert_eq!(fs::read_to_string(&installed).unwrap(), "user edit");
    assert!(!result["skipped"].as_array().unwrap().is_empty());
}

#[test]
fn link_does_not_adopt_a_preexisting_identical_copy_and_unlink_preserves_it() {
    let temp = tempdir().unwrap();
    let root = temp.path().join("arana");
    let seed = temp.path().join("seed");
    fsops::setup_skill(&root, Some(&seed)).unwrap();
    let preexisting = temp.path().join("preexisting/arana/SKILL.md");
    fs::create_dir_all(preexisting.parent().unwrap()).unwrap();
    fs::copy(seed.join("arana/SKILL.md"), &preexisting).unwrap();
    let dir = temp.path().join("preexisting");
    let link = vec!["link".into(), dir.to_string_lossy().into_owned()];
    fsops::skill(&root, &link).unwrap();
    let state: Value =
        serde_json::from_str(&fs::read_to_string(root.join("state.json")).unwrap()).unwrap();
    assert!(
        !state["skill"]["links"]
            .as_array()
            .unwrap()
            .iter()
            .any(|rec| rec["path"].as_str() == Some(preexisting.to_string_lossy().as_ref())),
        "pre-existing copy was adopted: {state}"
    );
    let unlink = vec!["unlink".into(), dir.to_string_lossy().into_owned()];
    let result = fsops::skill(&root, &unlink).unwrap();
    assert_eq!(result["removed"], false);
    assert!(
        preexisting.is_file(),
        "unlink deleted an unowned pre-existing copy"
    );
}

#[test]
fn setup_discovery_does_not_adopt_an_identical_preexisting_skill() {
    let temp = tempdir().unwrap();
    let root = temp.path().join("arana");
    let seed_root = temp.path().join("seed-root");
    let seed = temp.path().join("seed");
    fsops::setup_skill(&seed_root, Some(&seed)).unwrap();
    let discovered = temp.path().join(".claude/skills/arana/SKILL.md");
    fs::create_dir_all(discovered.parent().unwrap()).unwrap();
    fs::copy(seed.join("arana/SKILL.md"), &discovered).unwrap();
    fsops::setup_skill(&root, None).unwrap();
    let state: Value =
        serde_json::from_str(&fs::read_to_string(root.join("state.json")).unwrap()).unwrap();
    assert!(!state["skill"]["links"]
        .as_array()
        .unwrap()
        .iter()
        .any(|rec| rec["path"].as_str() == Some(discovered.to_string_lossy().as_ref())));
    fsops::uninstall(&root, true).unwrap();
    assert!(
        discovered.is_file(),
        "uninstall deleted an unowned discovered skill"
    );
}

#[test]
fn migration_drops_external_skill_ownership_from_the_imported_ledger() {
    use sha2::{Digest, Sha256};
    let temp = tempdir().unwrap();
    let root = temp.path().join("arana");
    let legacy = temp.path().join("legacy");
    let outside = temp.path().join("outside/arana/SKILL.md");
    fs::create_dir_all(legacy.join("sessions")).unwrap();
    fs::create_dir_all(outside.parent().unwrap()).unwrap();
    fs::write(&outside, "external original skill").unwrap();
    let hash = format!("{:x}", Sha256::digest(b"external original skill"));
    fs::write(
        legacy.join("state.json"),
        serde_json::json!({
            "skill": {"links": [{
                "agent": "custom", "kind": "copy", "owned": true,
                "path": outside, "target": outside.parent().unwrap().parent().unwrap(),
                "sha256": hash
            }], "unlinked": []},
            "owned_paths": {"state.json": "state ledger"}
        })
        .to_string(),
    )
    .unwrap();
    fs::write(legacy.join("config.yaml"), "keep: old\n").unwrap();
    fsops::migrate(&root, &legacy, true).unwrap();
    let state: Value =
        serde_json::from_str(&fs::read_to_string(root.join("state.json")).unwrap()).unwrap();
    assert_eq!(state["skill"]["links"].as_array().unwrap().len(), 0);
    fsops::uninstall(&root, true).unwrap();
    assert_eq!(
        fs::read_to_string(&outside).unwrap(),
        "external original skill",
        "uninstall followed an imported external skill link"
    );
}

#[test]
fn migration_does_not_adopt_a_preexisting_session_directory() {
    let temp = tempdir().unwrap();
    let root = temp.path().join("arana");
    let legacy = temp.path().join("legacy");
    fs::create_dir_all(root.join("sessions/bili-cli")).unwrap();
    fs::write(root.join("sessions/bili-cli/user-file.txt"), "mine").unwrap();
    let incoming = legacy.join("sessions/bili-cli/imported.json");
    fs::create_dir_all(incoming.parent().unwrap()).unwrap();
    fs::write(&incoming, "{}").unwrap();
    fsops::migrate(&root, &legacy, true).unwrap();
    let state: Value =
        serde_json::from_str(&fs::read_to_string(root.join("state.json")).unwrap()).unwrap();
    assert!(state["owned_paths"].get("sessions/bili-cli").is_none());
    assert!(reach_channels::session::session_env("bili-cli", &root).is_err());
    assert_eq!(
        fs::read_to_string(root.join("sessions/bili-cli/user-file.txt")).unwrap(),
        "mine"
    );
}

#[test]
fn migration_owns_a_copied_native_session_for_reuse() {
    let temp = tempdir().unwrap();
    let root = temp.path().join("arana");
    let legacy = temp.path().join("legacy");
    let credential = legacy.join("sessions/bili-cli/.bilibili-cli/credential.json");
    fs::create_dir_all(credential.parent().unwrap()).unwrap();
    fs::write(&credential, r#"{"sessdata":"dummy"}"#).unwrap();
    fsops::migrate(&root, &legacy, true).unwrap();
    let env = reach_channels::session::session_env("bili-cli", &root)
        .expect("imported session directory must be explicitly owned");
    assert_eq!(env[0].0, "HOME");
    assert!(reach_channels::session::has_login_state("bili-cli", &root).unwrap());
    let state: Value =
        serde_json::from_str(&fs::read_to_string(root.join("state.json")).unwrap()).unwrap();
    assert_eq!(
        state["owned_paths"]["sessions/bili-cli"]["kind"],
        "directory"
    );
    assert_eq!(state["owned_paths"]["sessions/bili-cli"]["owner"], "arana");
}
