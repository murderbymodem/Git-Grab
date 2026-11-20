//! Works out which OS folder a release asset belongs in, using its file name.

use crate::config::OsRule;
use crate::util::glob_match;

pub const WINDOWS: &str = "Windows";
pub const LINUX: &str = "Linux";
pub const MACOS: &str = "macOS";
pub const ANDROID: &str = "Android";
pub const IOS: &str = "iOS";
pub const FREEBSD: &str = "FreeBSD";
pub const OPENBSD: &str = "OpenBSD";
pub const NETBSD: &str = "NetBSD";
pub const OTHER: &str = "Other";

/// Suffixes of companion files (checksums, signatures, SBOMs). They are removed
/// before classifying, so `tool-linux-x64.tar.gz.sha256` lands next to the
/// archive it describes.
const AUX_SUFFIXES: &[&str] = &[
    ".sha256", ".sha256sum", ".sha512", ".sha512sum", ".sha1", ".md5", ".sig", ".asc",
    ".minisig", ".pem", ".crt", ".sigstore", ".sigstore.json", ".intoto.jsonl",
    ".sbom.json", ".spdx.json", ".cdx.json",
];

/// Picks the OS folder for `file_name`: user rules first (in order), then the
/// built-in name heuristics.
pub fn classify(file_name: &str, rules: &[OsRule]) -> String {
    rules
        .iter()
        .find(|r| glob_match(&r.pattern, file_name))
        .map(|r| r.os.clone())
        .unwrap_or_else(|| detect(file_name).to_string())
}

/// Built-in guess based on tokens and extensions in the file name.
pub fn detect(file_name: &str) -> &'static str {
    let lower = file_name.to_ascii_lowercase();
    let base = strip_aux_suffixes(&lower);
    let tokens: Vec<&str> = base
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|t| !t.is_empty())
        .collect();
    let any = |f: &dyn Fn(&str) -> bool| tokens.iter().any(|t| f(t));
    let ext = |exts: &[&str]| exts.iter().any(|e| base.ends_with(e));
    let digits = |s: &str| s.chars().all(|c| c.is_ascii_digit());

    // Order matters: Android target triples contain "linux"
    // (aarch64-linux-android), and macOS/Windows GNU builds contain "gnu".
    if any(&|t| t.starts_with("android")) || ext(&[".apk", ".aab", ".apks", ".xapk"]) {
        return ANDROID;
    }
    if any(&|t| matches!(t, "ios" | "iphoneos" | "ipados")) || ext(&[".ipa"]) {
        return IOS;
    }
    if any(&|t| {
        matches!(t, "darwin" | "mac" | "apple" | "macosx")
            || t.starts_with("macos")
            || t.starts_with("osx")
    }) || ext(&[".dmg", ".pkg"])
    {
        return MACOS;
    }
    if any(&|t| {
        t.starts_with("windows")
            || matches!(t, "msvc" | "mingw" | "mingw32" | "mingw64" | "cygwin")
            || (t.starts_with("win") && digits(&t[3..])) // win, win32, win64
    }) || ext(&[".exe", ".msi", ".msix", ".msixbundle", ".appx", ".appxbundle"])
    {
        return WINDOWS;
    }
    if any(&|t| t.starts_with("freebsd")) {
        return FREEBSD;
    }
    if any(&|t| t.starts_with("openbsd")) {
        return OPENBSD;
    }
    if any(&|t| t.starts_with("netbsd")) {
        return NETBSD;
    }
    if any(&|t| {
        t.starts_with("linux")
            || matches!(
                t,
                "gnu" | "musl" | "gnueabi" | "gnueabihf" | "musleabi" | "musleabihf"
                    | "ubuntu" | "debian" | "fedora" | "centos" | "rhel" | "alpine"
                    | "appimage" | "flatpak" | "snap"
            )
    }) || ext(&[".deb", ".rpm", ".appimage", ".flatpak", ".snap"])
    {
        return LINUX;
    }
    OTHER
}

fn strip_aux_suffixes(name: &str) -> &str {
    let mut s = name;
    loop {
        match AUX_SUFFIXES.iter().find(|suf| s.len() > suf.len() && s.ends_with(*suf)) {
            Some(suf) => s = &s[..s.len() - suf.len()],
            None => return s,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_common_names() {
        let cases = [
            ("ripgrep-14.1.0-x86_64-pc-windows-msvc.zip", WINDOWS),
            ("ripgrep-14.1.0-x86_64-pc-windows-gnu.zip", WINDOWS),
            ("ripgrep-14.1.0-x86_64-unknown-linux-musl.tar.gz", LINUX),
            ("ripgrep-14.1.0-x86_64-unknown-linux-musl.tar.gz.sha256", LINUX),
            ("ripgrep_14.1.0-1_amd64.deb", LINUX),
            ("ripgrep-14.1.0-aarch64-apple-darwin.tar.gz", MACOS),
            ("app-aarch64-linux-android.tar.gz", ANDROID),
            ("app-release.apk", ANDROID),
            ("Setup-1.2.3.exe", WINDOWS),
            ("tool_win64.zip", WINDOWS),
            ("tool-win-x64.zip", WINDOWS), // .NET-style runtime IDs
            ("tool-linux-arm64.tar.gz", LINUX),
            ("Tool-1.0.dmg", MACOS),
            ("tool-osx64.tar.gz", MACOS),
            ("tool-macos-universal.zip", MACOS),
            ("App.ipa", IOS),
            ("tool-x86_64-unknown-freebsd.tar.gz", FREEBSD),
            ("Tool-1.0-x86_64.AppImage", LINUX),
            ("tool-1.0.x86_64.rpm", LINUX),
            ("SHA256SUMS", OTHER),
            ("checksums.txt", OTHER),
            ("tool-1.0.jar", OTHER),
        ];
        for (name, want) in cases {
            assert_eq!(detect(name), want, "{name}");
        }
    }

    #[test]
    fn user_rules_take_priority() {
        let rules = vec![OsRule { pattern: "*-portable.zip".into(), os: WINDOWS.into() }];
        assert_eq!(classify("Tool-1.0-portable.zip", &rules), WINDOWS);
        assert_eq!(classify("tool-linux.tar.gz", &rules), LINUX);
    }
}
