//! What a Go thread runs under a synthetic vDSO entry on aarch64, read out of the machine code.
//!
//! A Go runtime built without cgo keeps its goroutine pointer in x28, and its signal handler takes
//! the pointer from there. A Go thread inside one of the entries can take a signal at any
//! instruction, so every instruction it runs there either leaves x28 as the runtime left it or
//! runs with signals held back. The library holds them around the hook and around everything that
//! calls into libc or writes a log line. The read path between those is ordinary compiled code,
//! which nothing in the compiler keeps off x28: these cases check that the release build a user
//! loads does keep off it, and that the kernel's clock the read path calls does too.
#![cfg(all(target_os = "linux", target_arch = "aarch64"))]
#![expect(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    dead_code,
    reason = "test support: a failed assumption should fail the test loudly, and not every helper \
              is used by every case"
)]

mod harness;

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::Path;

/// Where the walk starts: the functions the synthetic vDSO's clock symbols point at.
const ENTRIES: &str = "timewarp::vdso::entries::";

/// What the walk does not go into.
const UNWALKED: &[&str] = &[
    // Called with every signal held back, so what it runs may use x28.
    "common::platform::linux::run_held",
    // A panic under an entry aborts the process.
    "core::panicking::",
];

/// The functions of other objects the read path may call outside a held stretch. Both are leaves
/// that keep to the scratch registers in every libc: one adds the thread pointer to an offset, and
/// is reached only when the kernel refuses a clock; the other reads the auxiliary vector, for the
/// lookup of the kernel's vDSO that the constructor makes before any entry exists.
const FOREIGN: &[&str] = &["__errno_location", "getauxval"];

/// The branches that can leave a function for the start of another.
const BRANCHES: &[&str] = &["bl", "b", "cbz", "cbnz", "tbz", "tbnz"];

/// The branches through a register, whose destination the code does not say.
const INDIRECT: &[&str] = &[
    "blr", "br", "blraa", "blraaz", "blrab", "blrabz", "braa", "braaz", "brab", "brabz",
];

/// The instructions after which the next one does not run: an unconditional branch and the
/// returns.
const ENDS: &[&str] = &["b", "ret", "retaa", "retab"];

/// One function of a disassembly.
#[derive(Debug)]
struct Function {
    /// Its demangled name.
    name: String,
    /// Each instruction as its mnemonic and its operands.
    code: Vec<(String, String)>,
}

impl Function {
    /// Whether any instruction names x28 or its lower half.
    fn names_x28(&self) -> bool {
        self.code.iter().any(|(_, operands)| names_x28(operands))
    }
}

/// Whether `operands` name x28 or its lower half.
fn names_x28(operands: &str) -> bool {
    operands
        .split(|c: char| !c.is_ascii_alphanumeric())
        .any(|token| token == "x28" || token == "w28")
}

/// Every function in `listing`, the output of `objdump -d -C --no-show-raw-insn`, by the address it
/// starts at.
fn functions(listing: &str) -> BTreeMap<u64, Function> {
    let mut functions = BTreeMap::new();
    let mut current = None;
    for line in listing.lines() {
        if let Some((address, name)) = header(line) {
            functions.insert(
                address,
                Function {
                    name,
                    code: Vec::new(),
                },
            );
            current = Some(address);
        } else if let Some((_, instruction)) = instruction(line)
            && let Some(function) = current.and_then(|at| functions.get_mut(&at))
        {
            function.code.push(instruction);
        }
    }
    functions
}

/// A line that starts a function: `0000000000017030 <name>:`.
fn header(line: &str) -> Option<(u64, String)> {
    let (address, rest) = line.split_once(" <")?;
    let name = rest.strip_suffix(">:")?;
    Some((u64::from_str_radix(address, 16).ok()?, name.to_owned()))
}

/// A line that carries an instruction: `   17030:\tstp\tx29, x30, [sp, #-32]!`, as its address and
/// its mnemonic and operands, with any comment after `//` dropped.
fn instruction(line: &str) -> Option<(u64, (String, String))> {
    let (address, rest) = line.trim_start().split_once(":\t")?;
    let address = u64::from_str_radix(address, 16).ok()?;
    let rest = rest.split("//").next().unwrap_or_default();
    let (mnemonic, operands) = rest.split_once('\t').unwrap_or((rest, ""));
    Some((
        address,
        (mnemonic.trim().to_owned(), operands.trim().to_owned()),
    ))
}

/// Where a direct branch goes: the address of `17030 <name>` or `17030 <name+0x10>`, after any
/// register and bit number the branch tests.
fn target(mnemonic: &str, operands: &str) -> Option<u64> {
    let base = mnemonic.split('.').next().unwrap_or(mnemonic);
    if !BRANCHES.contains(&base) {
        return None;
    }
    let address = operands.split(" <").next().unwrap_or(operands);
    let address = address.rsplit(", ").next().unwrap_or(address);
    u64::from_str_radix(address, 16).ok()
}

/// Where a branch goes when it goes to the start of a function: `17030 <name>`, or `<name@plt>`
/// for a function of another object. A destination with an offset is inside a function.
fn destination(mnemonic: &str, operands: &str) -> Option<(u64, String)> {
    let address = target(mnemonic, operands)?;
    let symbol = operands.split_once(" <")?.1.strip_suffix('>')?;
    if symbol.contains('+') {
        return None;
    }
    Some((address, symbol.to_owned()))
}

/// The address of every instruction of `code` a run from `start` can reach: each direct branch is
/// taken, and each instruction but those in [`ENDS`] and [`INDIRECT`] also falls through to the
/// next. A run that reaches an address `code` has no instruction at fails the case.
fn reachable(code: &BTreeMap<u64, (String, String)>, start: u64, listing: &str) -> BTreeSet<u64> {
    const WIDTH: u64 = 4;
    let mut reached = BTreeSet::from([start]);
    let mut queue = VecDeque::from([start]);
    while let Some(at) = queue.pop_front() {
        let Some((mnemonic, operands)) = code.get(&at) else {
            panic!(
                "a run from {start:#x} reaches {at:#x}, where there is no instruction:\n{listing}"
            );
        };
        let ends = ENDS.contains(&mnemonic.as_str()) || INDIRECT.contains(&mnemonic.as_str());
        let next = (!ends).then_some(at + WIDTH);
        for to in target(mnemonic, operands).into_iter().chain(next) {
            if reached.insert(to) {
                queue.push_back(to);
            }
        }
    }
    reached
}

/// Everything the walk from the entries found.
#[derive(Debug, Default)]
struct Walk {
    /// Every function reached, with the one it was first reached from.
    reached: BTreeMap<u64, Option<u64>>,
    /// The functions of other objects called, with a caller of each.
    foreign: BTreeMap<String, u64>,
    /// The functions with a branch through a register.
    indirect: BTreeSet<u64>,
    /// The functions called and not walked into.
    unwalked: BTreeSet<String>,
}

/// Follows every branch from `roots` to the start of another function, breadth first.
fn walk(functions: &BTreeMap<u64, Function>, roots: &[u64]) -> Walk {
    let mut walk = Walk::default();
    let mut queue: VecDeque<u64> = roots.iter().copied().collect();
    for &root in roots {
        walk.reached.insert(root, None);
    }
    while let Some(at) = queue.pop_front() {
        let function = &functions[&at];
        for (mnemonic, operands) in &function.code {
            if INDIRECT.contains(&mnemonic.as_str()) {
                walk.indirect.insert(at);
                continue;
            }
            let Some((to, symbol)) = destination(mnemonic, operands) else {
                continue;
            };
            if let Some(name) = symbol.strip_suffix("@plt") {
                walk.foreign.entry(name.to_owned()).or_insert(at);
                continue;
            }
            if to == at || walk.reached.contains_key(&to) {
                continue;
            }
            let Some(callee) = functions.get(&to) else {
                continue;
            };
            if UNWALKED.iter().any(|stop| callee.name.contains(stop)) {
                walk.unwalked.insert(callee.name.clone());
                continue;
            }
            walk.reached.insert(to, Some(at));
            queue.push_back(to);
        }
    }
    walk
}

impl Walk {
    /// How the walk got from an entry to `at`, one function per line.
    fn path(&self, functions: &BTreeMap<u64, Function>, at: u64) -> String {
        let mut names = vec![functions[&at].name.clone()];
        let mut next = self.reached.get(&at).copied().flatten();
        while let Some(caller) = next {
            names.push(format!("  called from {}", functions[&caller].name));
            next = self.reached.get(&caller).copied().flatten();
        }
        names.join("\n")
    }
}

/// What `objdump` prints for `args`, or a failed case naming why it could not.
fn disassemble(args: &[&str], file: &Path) -> String {
    let output = harness::objdump()
        .args(args)
        .arg(file)
        .output()
        .expect("objdump runs; OBJDUMP names another disassembler");
    assert!(
        output.status.success(),
        "objdump {args:?} {} failed: {}",
        file.display(),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

#[test]
fn nothing_an_entry_runs_outside_a_held_stretch_uses_x28() {
    let library = harness::release_library();
    let functions = functions(&disassemble(&["-d", "-C", "--no-show-raw-insn"], &library));
    let roots: Vec<u64> = functions
        .iter()
        .filter(|(_, function)| function.name.starts_with(ENTRIES))
        .map(|(&at, _)| at)
        .collect();
    for entry in ["clock_gettime", "gettimeofday", "time"] {
        let name = format!("{ENTRIES}{entry}");
        assert!(
            roots.iter().any(|at| functions[at].name == name),
            "{name} is not in {}",
            library.display()
        );
    }

    let walk = walk(&functions, &roots);
    assert!(
        walk.unwalked.iter().any(|name| name.contains(UNWALKED[0])),
        "the walk reached no held stretch, so it is not following the read path: it reached {:?}",
        walk.reached
            .keys()
            .map(|at| &functions[at].name)
            .collect::<Vec<_>>()
    );

    let using: Vec<String> = walk
        .reached
        .keys()
        .filter(|at| functions[at].names_x28())
        .map(|&at| walk.path(&functions, at))
        .collect();
    assert!(
        using.is_empty(),
        "x28 is used outside a held stretch by\n{}\nA Go thread that takes a signal there hands its \
         handler whatever x28 holds. Split the function so fewer values live across its calls, or \
         run the part that needs x28 through hold_signals.",
        using.join("\n")
    );

    let unknown: Vec<String> = walk
        .foreign
        .iter()
        .filter(|(name, _)| !FOREIGN.contains(&name.as_str()))
        .map(|(name, &at)| format!("{name}, from {}", walk.path(&functions, at)))
        .collect();
    assert!(
        unknown.is_empty(),
        "the read path calls code of another object outside a held stretch:\n{}",
        unknown.join("\n")
    );
}

/// The kernel's vDSO as this process has it mapped: where it starts, and how long it is.
fn kernel_vdso() -> Option<(usize, usize)> {
    let maps = std::fs::read_to_string("/proc/self/maps").unwrap();
    let line = maps.lines().find(|line| line.ends_with("[vdso]"))?;
    let (low, high) = line.split_whitespace().next()?.split_once('-')?;
    let low = usize::from_str_radix(low, 16).unwrap();
    let high = usize::from_str_radix(high, 16).unwrap();
    Some((low, high - low))
}

#[test]
fn the_kernel_clock_the_read_path_calls_uses_no_x28() {
    // No vDSO, nothing of the kernel's for the read path to call.
    let Some((base, len)) = kernel_vdso() else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let copy = dir.path().join("vdso.so");
    // Safety: the range is the vDSO mapping this process's own maps name, readable for its whole
    // length, and nothing writes it.
    let image = unsafe { std::slice::from_raw_parts(std::ptr::with_exposed_provenance(base), len) };
    std::fs::write(&copy, image).unwrap();

    let symbols = disassemble(&["-T"], &copy);
    let start = symbols
        .lines()
        .find(|line| line.ends_with(" __kernel_clock_gettime"))
        .and_then(|line| u64::from_str_radix(line.split_whitespace().next()?, 16).ok())
        .expect("the kernel's vDSO exports __kernel_clock_gettime");
    // The functions it calls carry no symbol in the mapped image, so the run is followed
    // instruction by instruction through the whole image.
    let listing = disassemble(&["-d", "--no-show-raw-insn"], &copy);
    let code: BTreeMap<u64, (String, String)> = listing.lines().filter_map(instruction).collect();
    let run = reachable(&code, start, &listing);
    let shown = |at: &u64| {
        let (mnemonic, operands) = &code[at];
        format!("{at:x}:\t{mnemonic}\t{operands}")
    };

    let indirect: Vec<String> = run
        .iter()
        .filter(|at| INDIRECT.contains(&code[at].0.as_str()))
        .map(shown)
        .collect();
    assert!(
        indirect.is_empty(),
        "the kernel's __kernel_clock_gettime branches through a register, to code this case cannot \
         follow:\n{}",
        indirect.join("\n")
    );
    let using: Vec<String> = run
        .iter()
        .filter(|at| names_x28(&code[at].1))
        .map(shown)
        .collect();
    assert!(
        using.is_empty(),
        "the kernel's __kernel_clock_gettime runs code that uses x28, so a Go thread that takes a \
         signal while the read path is inside it hands its handler whatever x28 holds:\n{}",
        using.join("\n")
    );
}
