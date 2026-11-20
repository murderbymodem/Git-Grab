//! Small helpers: wildcard matching and Windows-safe file names.

/// Case-insensitive wildcard match supporting `*` (any run of characters) and
/// `?` (exactly one character).
pub fn glob_match(pattern: &str, text: &str) -> bool {
    let p: Vec<char> = pattern.to_lowercase().chars().collect();
    let t: Vec<char> = text.to_lowercase().chars().collect();
    let (mut pi, mut ti) = (0, 0);
    let mut backtrack: Option<(usize, usize)> = None;
    while ti < t.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == t[ti]) {
            pi += 1;
            ti += 1;
        } else if pi < p.len() && p[pi] == '*' {
            backtrack = Some((pi, ti));
            pi += 1;
        } else if let Some((bp, bt)) = backtrack {
            pi = bp + 1;
            ti = bt + 1;
            backtrack = Some((bp, bt + 1));
        } else {
            return false;
        }
    }
    p[pi..].iter().all(|&c| c == '*')
}

/// Turns an arbitrary string (tag name, asset name, project name) into a name
/// that is valid on both Windows/SMB shares and Linux.
pub fn sanitize(name: &str) -> String {
    const RESERVED: &[&str] = &[
        "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7",
        "COM8", "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
    ];
    let mut out: String = name
        .chars()
        .map(|c| match c {
            '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*' => '_',
            c if c.is_control() => '_',
            c => c,
        })
        .collect();
    while out.ends_with('.') || out.ends_with(' ') {
        out.pop();
    }
    let out = out.trim_start().to_string();
    if out.is_empty() || out == "." || out == ".." {
        return "_".to_string();
    }
    let stem = out.split('.').next().unwrap_or("").to_ascii_uppercase();
    if RESERVED.contains(&stem.as_str()) {
        return format!("_{out}");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn globbing() {
        assert!(glob_match("*.zip", "Tool-1.0.ZIP"));
        assert!(glob_match("*linux*", "tool-x86_64-linux.tar.gz"));
        assert!(glob_match("tool-?.0.zip", "tool-1.0.zip"));
        assert!(glob_match("*", ""));
        assert!(!glob_match("*.zip", "tool.tar.gz"));
        assert!(!glob_match("tool", "tool2"));
        assert!(glob_match("*a*b*c", "xxaxxbxxbxxc"));
    }

    #[test]
    fn sanitizing() {
        assert_eq!(sanitize("release/v1.0"), "release_v1.0");
        assert_eq!(sanitize("v1:2"), "v1_2");
        assert_eq!(sanitize("trailing. "), "trailing");
        assert_eq!(sanitize(".."), "_");
        assert_eq!(sanitize("con.txt"), "_con.txt");
        assert_eq!(sanitize("v1.2.3"), "v1.2.3");
    }
}
