//! `qoop paths`: where qoop keeps each kind of file on this machine, and how big it is.
//!
//! One folder per kind, each where the platform expects it: settings a person edits
//! (config), what must survive (data), what a restart makes again (runtime), what can be
//! rebuilt (cache), and logs. Asked by name (`qoop paths qass-data`) it prints only that
//! path, for a script or a `--cwd`.

use std::path::{Path, PathBuf};

use qoop_runtime::paths;

struct Place {
    name: &'static str,
    what: &'static str,
    path: Option<PathBuf>,
}

fn places() -> Vec<Place> {
    let runtime = paths::runtime_dir().ok();
    vec![
        Place {
            name: "config",
            what: "your settings: config.toml, remotes, paired phones, hooks, no-auto-update",
            path: Some(paths::config_dir()),
        },
        Place {
            name: "data",
            what: "what must survive: spaces and tabs (qoop.db), screen history, this Mac's key",
            path: runtime
                .as_deref()
                .map(paths::state_location)
                .or_else(paths::data_dir),
        },
        Place {
            name: "runtime",
            what: "sockets and locks, made again at every start",
            path: paths::runtime_base().ok(),
        },
        Place {
            name: "cache",
            what: "what qoop can build again: the notch app",
            path: paths::cache_dir(),
        },
        Place {
            name: "logs",
            what: "the bridge, hub and tunnel logs, update.log",
            path: paths::logs_dir(),
        },
        Place {
            name: "qass-config",
            what: "qass: the lessons it keeps from you",
            path: Some(paths::qass_config_dir()),
        },
        Place {
            name: "qass-data",
            what: "qass's home, where it always starts: its skill, notes, goals and scratch",
            path: paths::qass_data_dir(),
        },
        Place {
            name: "skills",
            what: "the qass skill, in qass's home (`.agents/skills/qass` holds the same)",
            path: paths::qass_data_dir().map(|d| d.join(".claude/skills/qass")),
        },
    ]
}

/// Bytes under a path, links not followed; 0 for one that is not there.
fn size(path: &Path) -> u64 {
    let Ok(meta) = std::fs::symlink_metadata(path) else {
        return 0;
    };
    if !meta.is_dir() {
        return meta.len();
    }
    std::fs::read_dir(path)
        .map(|entries| entries.flatten().map(|e| size(&e.path())).sum())
        .unwrap_or(0)
}

fn human(bytes: u64) -> String {
    match bytes {
        b if b >= 1 << 30 => format!("{:.1}G", b as f64 / (1u64 << 30) as f64),
        b if b >= 1 << 20 => format!("{:.1}M", b as f64 / (1u64 << 20) as f64),
        b if b >= 1 << 10 => format!("{}K", b >> 10),
        b => format!("{b}B"),
    }
}

pub fn run(args: &[String]) -> std::io::Result<()> {
    let json = args.iter().any(|a| a == "--json");
    let name = args.iter().find(|a| !a.starts_with("--"));
    let all = places();
    if let Some(name) = name {
        let Some(place) = all.iter().find(|p| p.name == name) else {
            let names: Vec<_> = all.iter().map(|p| p.name).collect();
            return Err(std::io::Error::other(format!(
                "no place named {name}; one of: {}",
                names.join(", ")
            )));
        };
        let path = place
            .path
            .as_ref()
            .ok_or_else(|| std::io::Error::other(format!("{name}: no HOME to put it under")))?;
        println!("{}", path.display());
        return Ok(());
    }
    if json {
        let list: Vec<_> = all
            .iter()
            .map(|p| {
                serde_json::json!({
                    "name": p.name,
                    "path": p.path.as_ref().map(|p| p.display().to_string()),
                    "what": p.what,
                    "exists": p.path.as_ref().is_some_and(|p| p.exists()),
                    "bytes": p.path.as_deref().map_or(0, size),
                })
            })
            .collect();
        println!("{}", serde_json::Value::Array(list));
        return Ok(());
    }
    for p in &all {
        let (path, bytes) = match &p.path {
            Some(path) if path.exists() => (path.display().to_string(), human(size(path))),
            Some(path) => (path.display().to_string(), "-".into()),
            None => ("(no HOME)".into(), "-".into()),
        };
        println!(
            "{:<12} {:>6}  {path}\n{:<12}         {}",
            p.name, bytes, "", p.what
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_add_up_and_read_short() {
        let dir = tempfile::tempdir().expect("dir");
        std::fs::create_dir(dir.path().join("a")).expect("a");
        std::fs::write(dir.path().join("a/f"), vec![0u8; 1500]).expect("f");
        std::fs::write(dir.path().join("g"), vec![0u8; 600]).expect("g");
        assert_eq!(size(dir.path()), 2100);
        assert_eq!(size(&dir.path().join("none")), 0);
        assert_eq!(human(2100), "2K");
        assert_eq!(human(3 << 20), "3.0M");
        assert_eq!(human(12), "12B");
    }

    #[test]
    fn every_place_has_a_distinct_name() {
        let names: std::collections::HashSet<_> = places().iter().map(|p| p.name).collect();
        assert_eq!(names.len(), places().len());
    }
}
