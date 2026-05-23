//! Stamps the build's git revision into KASATERM_GIT_REV at compile
//! time so the running binary can show which build it is (the launch
//! corner banner in main.rs). Falls back to "unknown" outside a git
//! checkout so a tarball build still compiles.
use std::process::Command;

fn main() {
    let rev = git_rev().unwrap_or_else(|| "unknown".to_string());
    println!("cargo:rustc-env=KASATERM_GIT_REV={rev}");
    // Re-stamp when HEAD or the staged index moves so a fresh commit
    // shows the new hash without a clean rebuild. Unstaged edits can't
    // be tracked this way, so the dirty '+' may lag until the next
    // build — acceptable for a launch banner.
    println!("cargo:rerun-if-changed=../../.git/HEAD");
    println!("cargo:rerun-if-changed=../../.git/index");
}

fn git_rev() -> Option<String> {
    let short = Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()?;
    if !short.status.success() {
        return None;
    }
    let mut rev = String::from_utf8(short.stdout).ok()?.trim().to_string();
    if rev.is_empty() {
        return None;
    }
    // Dirty working tree → trailing '+'.
    if let Ok(status) = Command::new("git")
        .args(["status", "--porcelain"])
        .output()
    {
        if status.status.success() && !status.stdout.is_empty() {
            rev.push('+');
        }
    }
    Some(rev)
}
