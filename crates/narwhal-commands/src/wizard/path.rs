//! Filesystem path completion for the wizard's SQLite file picker.

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PathCompletion {
    NoMatch,
    Single,
    Multiple { count: usize, samples: Vec<String> },
}

pub(super) struct CompletionResult {
    pub(super) replacement: Option<String>,
    pub(super) report: PathCompletion,
}

pub(super) fn complete_path(input: &str) -> CompletionResult {
    use std::path::Path;

    // Resolve `~` so completion works inside the home directory.
    let expanded = expand_tilde(input);
    let path = Path::new(&expanded);

    // Split into a directory + basename prefix. Trailing-slash inputs
    // list every child of the directory.
    let (dir, prefix): (std::path::PathBuf, String) =
        if expanded.is_empty() || expanded.ends_with('/') {
            (
                if expanded.is_empty() {
                    std::path::PathBuf::from(".")
                } else {
                    path.to_path_buf()
                },
                String::new(),
            )
        } else {
            (
                path.parent()
                    .filter(|p| !p.as_os_str().is_empty()).map_or_else(|| std::path::PathBuf::from("."), Path::to_path_buf),
                path.file_name()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_default(),
            )
        };

    let Ok(entries) = std::fs::read_dir(&dir) else {
        return CompletionResult {
            replacement: None,
            report: PathCompletion::NoMatch,
        };
    };
    let mut matches: Vec<(String, bool)> = entries
        .filter_map(std::result::Result::ok)
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            if !name.starts_with(&prefix) {
                return None;
            }
            // Skip dotfiles unless the user explicitly typed a leading dot.
            if name.starts_with('.') && !prefix.starts_with('.') {
                return None;
            }
            let is_dir = e.file_type().is_ok_and(|t| t.is_dir());
            Some((name, is_dir))
        })
        .collect();
    matches.sort_by(|a, b| a.0.cmp(&b.0));

    match matches.len() {
        0 => CompletionResult {
            replacement: None,
            report: PathCompletion::NoMatch,
        },
        1 => {
            let (name, is_dir) = &matches[0];
            let mut joined = dir.join(name).to_string_lossy().into_owned();
            if *is_dir {
                joined.push('/');
            }
            CompletionResult {
                replacement: Some(joined),
                report: PathCompletion::Single,
            }
        }
        _ => {
            // Extend to the longest common prefix so successive Tabs
            // converge on the user's target.
            let names: Vec<&str> = matches.iter().map(|(n, _)| n.as_str()).collect();
            let lcp = longest_common_prefix(&names);
            let replacement = if lcp.len() > prefix.len() {
                Some(dir.join(lcp).to_string_lossy().into_owned())
            } else {
                None
            };
            let samples: Vec<String> = matches.iter().take(8).map(|(n, _)| n.clone()).collect();
            CompletionResult {
                replacement,
                report: PathCompletion::Multiple {
                    count: matches.len(),
                    samples,
                },
            }
        }
    }
}

fn expand_tilde(s: &str) -> String {
    if let Some(rest) = s.strip_prefix("~/") {
        if let Some(home) = std::env::var_os("HOME") {
            let mut p = std::path::PathBuf::from(home);
            p.push(rest);
            return p.to_string_lossy().into_owned();
        }
    }
    if s == "~" {
        if let Some(home) = std::env::var_os("HOME") {
            return std::path::PathBuf::from(home)
                .to_string_lossy()
                .into_owned();
        }
    }
    s.to_owned()
}

fn longest_common_prefix(strs: &[&str]) -> String {
    if strs.is_empty() {
        return String::new();
    }
    let mut prefix = strs[0].to_owned();
    for s in &strs[1..] {
        while !s.starts_with(&prefix) {
            prefix.pop();
            if prefix.is_empty() {
                return String::new();
            }
        }
    }
    prefix
}
