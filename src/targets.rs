//! Builds the list of repos to check: [[repo]] entries plus starred repos.

use crate::archive::{folder_owner, Target};
use crate::config::Config;
use crate::github::Client;
use crate::util::{glob_match, sanitize};
use anyhow::Result;
use log::info;
use std::collections::HashSet;

pub fn build(cfg: &Config, client: &Client) -> Result<Vec<Target>> {
    let mut targets = Vec::new();
    let mut used_folders = HashSet::new();
    let mut listed = HashSet::new();

    for r in &cfg.repos {
        let folder = cfg.project_folder(r);
        used_folders.insert(folder.to_lowercase());
        listed.insert(r.repo.to_lowercase());
        targets.push(Target { repo: r.repo.clone(), folder, opts: cfg.effective(&r.options) });
    }

    let Some(s) = cfg.starred_enabled() else { return Ok(targets) };
    let mut starred = client.starred(s.user.as_deref())?;
    // Sorted, so folder-name conflicts resolve the same way every run.
    starred.sort_by_key(|r| r.full_name.to_lowercase());
    let opts = cfg.effective(&s.options);
    let mut added = 0;
    for r in starred {
        let full = r.full_name;
        let Some((owner, name)) = full.split_once('/') else { continue };
        if listed.contains(&full.to_lowercase())
            || s.ignore.iter().any(|p| glob_match(p, &full))
            || (r.fork && !s.include_forks)
            || (r.archived && !s.include_archived)
        {
            continue;
        }
        let folder = pick_folder(owner, name, &full, &used_folders, |f| folder_owner(&cfg.archive_root.join(f)));
        used_folders.insert(folder.to_lowercase());
        targets.push(Target { repo: full, folder, opts: opts.clone() });
        added += 1;
    }
    info!("{} repo(s) from the config, {added} from stars", cfg.repos.len());
    Ok(targets)
}

/// Uses the repo name as the folder unless another repo already has it, either
/// in this run or on disk; then "name (owner)".
fn pick_folder(
    owner: &str,
    name: &str,
    full: &str,
    used: &HashSet<String>,
    owner_on_disk: impl Fn(&str) -> Option<String>,
) -> String {
    let plain = sanitize(name);
    let taken = used.contains(&plain.to_lowercase())
        || owner_on_disk(&plain).is_some_and(|o| !o.eq_ignore_ascii_case(full));
    if taken {
        sanitize(&format!("{name} ({owner})"))
    } else {
        plain
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folder_conflicts() {
        let used: HashSet<String> = ["tool".to_string()].into();
        let none = |_: &str| None;
        assert_eq!(pick_folder("a", "app", "a/app", &used, none), "app");
        assert_eq!(pick_folder("b", "Tool", "b/Tool", &used, none), "Tool (b)");
        let disk = |_: &str| Some("other/app".to_string());
        assert_eq!(pick_folder("a", "app", "a/app", &HashSet::new(), disk), "app (a)");
        let mine = |_: &str| Some("A/App".to_string());
        assert_eq!(pick_folder("a", "app", "a/app", &HashSet::new(), mine), "app");
    }
}
