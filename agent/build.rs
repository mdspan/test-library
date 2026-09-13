//! Decides which names of libc's this build can reach: the `__xstat` family on an old glibc, and
//! the suffixed spellings libSystem keeps on some architectures.
//!
//! glibc exported `stat`, `fstat`, `lstat` and `fstatat` under their own names for the first time
//! at 2.33. Before that a program calls `__xstat` and its neighbours instead, so a build targeting
//! such a glibc has to interpose those too. Which glibc a build targets is the target's own and
//! not this machine's, so the answer comes from the C preprocessor for that target.
//!
//! A target we cannot ask about is taken to be a modern one: the extra names exist to reach an old
//! libc, and defining them against a libc that never wanted them is the worse guess.
//!
//! macOS keeps several spellings of one function, and which of them exist is the architecture's:
//! `x86_64` carries the suffixed forms the ABI a program was compiled against picks, and aarch64
//! carries none of them. A name that is not there is a link that fails rather than a row that goes
//! missing quietly, so each one is asked of the linker for the target architecture before it is
//! named in the source.

use std::io::Write as _;
use std::path::PathBuf;
use std::process::{Command, Stdio};

/// The glibc that first exported `stat` and its neighbours under their own names.
const FIRST_PLAIN_STAT: (u32, u32) = (2, 33);

fn main() {
    println!("cargo::rerun-if-changed=build.rs");
    println!("cargo::rustc-check-cfg=cfg(glibc_pre_2_33)");
    println!("cargo::rerun-if-env-changed=CC");
    for (flag, _, _) in DARWIN_NAMES {
        println!("cargo::rustc-check-cfg=cfg({flag})");
    }

    if target_glibc().is_some_and(|version| version < FIRST_PLAIN_STAT) {
        println!("cargo::rustc-cfg=glibc_pre_2_33");
    }
    darwin_names();
}

/// The names of libSystem's that this build asks the linker about: the `cfg` each one sets, the
/// symbol itself, and the architecture it is expected on, or `None` for one expected on all.
///
/// `stat$INODE64` stands for its three neighbours as well: the four come as one set, from the same
/// macro in the same header. `select$1050` is the conforming spelling a program that is not asking
/// for the Darwin extensions gets, and `select$DARWIN_EXTSN` the one it gets when it is; `pselect`
/// has the second of those and not the first. `kevent_qos` is a name Apple keeps private, so
/// whether the SDK lets anything link against it is a question rather than an assumption.
const DARWIN_NAMES: &[(&str, &str, Option<&str>)] = &[
    ("darwin_inode64", "stat$INODE64", Some("x86_64")),
    ("darwin_select_1050", "select$1050", Some("x86_64")),
    ("darwin_select_extsn", "select$DARWIN_EXTSN", Some("x86_64")),
    (
        "darwin_pselect_extsn",
        "pselect$DARWIN_EXTSN",
        Some("x86_64"),
    ),
    ("darwin_kevent_qos", "kevent_qos", None),
];

/// Sets a `cfg` for every name of [`DARWIN_NAMES`] the target's own libSystem exports.
///
/// A name that is missing simply gets no row in the interpose table and no lookup, which is the
/// call going unfaked rather than a library that will not link. One missing where this
/// architecture was meant to have it says so, because that is a row lost rather than a row that
/// was never there.
fn darwin_names() {
    if var("CARGO_CFG_TARGET_OS").as_deref() != Some("macos") {
        return;
    }
    let Some(arch) = var("CARGO_CFG_TARGET_ARCH") else {
        return;
    };
    for (flag, symbol, expected) in DARWIN_NAMES {
        if links(&arch, symbol) {
            println!("cargo::rustc-cfg={flag}");
        } else if expected.is_none_or(|only| only == arch) {
            println!("cargo::warning=libSystem for {arch} has no {symbol}, so it gets no row");
        }
    }
}

/// Whether the linker can resolve `symbol` against libSystem for `arch`.
///
/// The architecture is named rather than left to the compiler's default, because the suffixed
/// spellings are `x86_64`'s and a build for one may well be running on the other.
fn links(arch: &str, symbol: &str) -> bool {
    let Some(out) = var("OUT_DIR") else {
        return false;
    };
    let dir = PathBuf::from(out);
    let source = dir.join("darwin-probe.c");
    // An asm label, because the suffixes are not characters a C identifier may hold. The address
    // is taken and answered so that nothing folds the reference away before the linker sees it.
    let probe = format!(
        "extern void named(void) __asm(\"_{symbol}\");\n\
         void *anchor(void) {{ return (void *)&named; }}\n\
         int main(void) {{ return anchor() == 0; }}\n"
    );
    if std::fs::write(&source, probe).is_err() {
        return false;
    }
    let compiler = var("CC").unwrap_or_else(|| "cc".to_owned());
    let mut words = compiler.split_whitespace();
    let Some(program) = words.next() else {
        return false;
    };
    Command::new(program)
        .args(words)
        .args(["-arch", clang_arch(arch), "-o"])
        .arg(dir.join("darwin-probe"))
        .arg(&source)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

/// What clang calls an architecture cargo names another way: `aarch64` is `arm64` to Apple's
/// tools, and everything else is spelled the same on both sides.
const fn clang_arch(arch: &str) -> &str {
    if matches!(arch.as_bytes(), b"aarch64") {
        "arm64"
    } else {
        arch
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
