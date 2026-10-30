//! `reprise uninstall`: everything reprise set up goes, the vault and Claude Code's own settings stay.
#![cfg(unix)]

use std::path::PathBuf;
use std::process::{Command, Output};

struct Env {
    _tmp: tempfile::TempDir,
    root: PathBuf,
}

impl Env {
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        Self { _tmp: tmp, root }
    }

    fn reprise(&self, args: &[&str]) -> Output {
        let out = Command::new(env!("CARGO_BIN_EXE_reprise"))
            .args(args)
            .env(
                "REPRISE_SETTINGS",
                self.root.join("config/reprise/settings.json"),
            )
            .env("CLAUDE_CONFIG_DIR", self.root.join("claude"))
            .env("REPRISE_BIN_DIR", self.root.join("bin"))
            .env("CODEX_HOME", self.root.join("codex"))
            .env_remove("REPRISE_VAULT")
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        out
    }
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn everything_but_the_vault_goes() {
    let env = Env::new();
    let vault = env.root.join("vault");
    std::fs::create_dir_all(&vault).unwrap();
    let claude = env.root.join("claude");
    std::fs::create_dir_all(&claude).unwrap();
    let own = r#"{"theme": "dark"}"#;
    std::fs::write(claude.join("settings.json"), own).unwrap();
    env.reprise(&["setup", "set", "--vault", vault.to_str().unwrap(), "--auto"]);
    env.reprise(&["connect", "claude", "--yes"]);
    let config = env.root.join("config/reprise");
    assert!(config.join("connections/claude.json").is_file());
    assert!(claude.join("settings.json.reprise-backup").is_file());

    // Without --yes it only lists.
    let plan = stdout(&env.reprise(&["uninstall"]));
    assert!(plan.contains("disconnect Claude Code"), "{plan}");
    assert!(plan.contains("settings.json.reprise-backup"), "{plan}");
    assert!(plan.contains("The vault stays"), "{plan}");
    assert!(plan.contains("--yes"), "{plan}");
    assert!(
        config.join("settings.json").is_file(),
        "nothing changed yet"
    );

    let done = stdout(&env.reprise(&["uninstall", "--yes"]));
    assert!(done.contains("To remove the program"), "{done}");
    assert!(!config.exists(), "reprise's settings folder is gone");
    assert!(!claude.join("settings.json.reprise-backup").exists());
    let json = |text: &str| serde_json::from_str::<serde_json::Value>(text).unwrap();
    assert_eq!(
        json(&std::fs::read_to_string(claude.join("settings.json")).unwrap()),
        json(own),
        "Claude Code's settings are as they were"
    );
    assert!(
        vault.join(".reprise/vault.toml").is_file(),
        "the vault stays"
    );
    assert!(
        !env.root.join("bin/reprise").exists(),
        "the link connecting made goes"
    );

    let again = stdout(&env.reprise(&["uninstall"]));
    assert!(again.contains("Nothing of reprise's is set up"), "{again}");
}
