//! Decides one thing: whether this build targets a glibc old enough to route `stat` through
//! `__xstat`.
//!
//! glibc exported `stat`, `fstat`, `lstat` and `fstatat` under their own names for the first time
//! at 2.33. Before that a program calls `__xstat` and its neighbours instead, so a build targeting
//! such a glibc has to interpose those too. Which glibc a build targets is the target's own and
//! not this machine's, so the answer comes from the C preprocessor for that target.
//!
//! A target we cannot ask about is taken to be a modern one: the extra names exist to reach an old
//! libc, and defining them against a libc that never wanted them is the worse guess.

use std::io::Write as _;
use std::process::{Command, Stdio};

/// The glibc that first exported `stat` and its neighbours under their own names.
const FIRST_PLAIN_STAT: (u32, u32) = (2, 33);

fn main() {
    println!("cargo::rerun-if-changed=build.rs");
    println!("cargo::rustc-check-cfg=cfg(glibc_pre_2_33)");
    println!("cargo::rerun-if-env-changed=CC");

    if target_glibc().is_some_and(|version| version < FIRST_PLAIN_STAT) {
        println!("cargo::rustc-cfg=glibc_pre_2_33");
    }
}

/// The glibc version this build targets, or `None` when the target has none or will not say.
fn target_glibc() -> Option<(u32, u32)> {
    if var("CARGO_CFG_TARGET_OS")? != "linux" || var("CARGO_CFG_TARGET_ENV")? != "gnu" {
        return None;
    }
    let target = var("TARGET")?;
    compilers(&target).iter().find_map(|compiler| ask(compiler))
}

/// Which compilers to ask, most specific first, in the order the `cc` crate reads them.
///
/// A bare `cc` is only asked for a native build: on a cross build it would answer for this machine
/// rather than for the target, which is the one question that matters here.
fn compilers(target: &str) -> Vec<String> {
    let mut named = Vec::new();
    for name in [
        format!("CC_{target}"),
        format!("CC_{}", target.replace('-', "_")),
        "TARGET_CC".to_owned(),
    ] {
        println!("cargo::rerun-if-env-changed={name}");
        named.extend(var(&name));
    }
    if var("HOST").as_deref() == Some(target) {
        named.push(var("CC").unwrap_or_else(|| "cc".to_owned()));
    }
    named
}

/// What `compiler` says the two glibc version macros expand to, or `None` when it will not say.
///
/// The preprocessor rather than a compiled program, because a cross build has nowhere to run one.
/// A libc that is not glibc leaves the macros unexpanded, which does not parse and so answers
/// `None` like a compiler that is not there at all.
fn ask(compiler: &str) -> Option<(u32, u32)> {
    const PROBE: &[u8] = b"#include <features.h>\nlibtimewarp_glibc __GLIBC__ __GLIBC_MINOR__\n";

    // A compiler setting may carry arguments of its own, as `CC="clang --target=..."` does.
    let mut words = compiler.split_whitespace();
    let mut child = Command::new(words.next()?)
        .args(words)
        .args(["-E", "-P", "-x", "c", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    child.stdin.take()?.write_all(PROBE).ok()?;
    let output = child.wait_with_output().ok()?;
    if !output.status.success() {
        return None;
    }

    let text = String::from_utf8(output.stdout).ok()?;
    let answer = text
        .lines()
        .find_map(|line| line.trim().strip_prefix("libtimewarp_glibc"))?;
    let mut numbers = answer.split_whitespace().map(str::parse::<u32>);
    Some((numbers.next()?.ok()?, numbers.next()?.ok()?))
}

/// One environment variable, empty read as unset the way every other setting of ours is.
fn var(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|value| !value.is_empty())
}
