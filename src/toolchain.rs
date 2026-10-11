//! Toolchain: CPython from python-build-standalone and zig as the C compiler.
//! Both are downloaded on first use into ~/.nicethon (or $NICETHON_HOME) and
//! shared by all projects.

use std::fmt::Display;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{exit, Command};

pub const DEFAULT_PYTHON: &str = "3.14";
const SUPPORTED_PYTHONS: [&str; 3] = ["3.12", "3.13", "3.14"];
const PYTHON_RELEASE: &str = "20261009";
const ZIG_VERSION: &str = "0.16.0";

pub fn fail(message: impl Display) -> ! {
    eprintln!("error: {message}");
    exit(1)
}

pub fn home() -> PathBuf {
    if let Some(home) = std::env::var_os("NICETHON_HOME") {
        return PathBuf::from(home);
    }
    let user = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
        .unwrap_or_else(|| fail("cannot find the home directory, set NICETHON_HOME"));
    PathBuf::from(user).join(".nicethon")
}

fn platform() -> &'static str {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => "aarch64-apple-darwin",
        ("macos", "x86_64") => "x86_64-apple-darwin",
        ("linux", "aarch64") => "aarch64-unknown-linux-gnu",
        ("linux", "x86_64") => "x86_64-unknown-linux-gnu",
        ("windows", "x86_64") => "x86_64-pc-windows-msvc",
        (os, arch) => fail(format!("no python builds for {os} on {arch}")),
    }
}

// Runs a command and returns its stdout; the error carries its stderr.
pub fn run(command: &mut Command, what: &str) -> Result<String, String> {
    let output = command.output().map_err(|error| format!("cannot run {what}: {error}"))?;
    if !output.status.success() {
        return Err(format!("{what} failed:\n{}", String::from_utf8_lossy(&output.stderr).trim_end()));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

fn download(url: &str, target: &Path) -> Result<Vec<u8>, String> {
    fs::create_dir_all(target.parent().unwrap()).map_err(|error| error.to_string())?;
    run(Command::new("curl").args(["-fsSL", "--retry", "2", "-o"]).arg(target).arg(url), &format!("curl {url}"))?;
    fs::read(target).map_err(|error| error.to_string())
}

// The python executable inside an installation or a virtual environment.
pub fn interpreter(dir: &Path, venv: bool) -> PathBuf {
    match (cfg!(windows), venv) {
        (true, true) => dir.join("Scripts").join("python.exe"),
        (true, false) => dir.join("python.exe"),
        (false, _) => dir.join("bin").join("python3"),
    }
}

// Python versions are written like 3.14.8+20261009: the CPython version,
// then the python-build-standalone release it comes from.
fn asset(version: &str) -> String {
    format!("cpython-{version}-{}-install_only.tar.gz", platform())
}

fn checksums(release: &str) -> Result<String, String> {
    let url = format!("https://github.com/astral-sh/python-build-standalone/releases/download/{release}/SHA256SUMS");
    let bytes = download(&url, &home().join("downloads").join(format!("SHA256SUMS-{release}")))?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

// The newest version of a minor release like "3.14", installed or not.
pub fn python_version(minor: &str) -> Result<String, String> {
    if !SUPPORTED_PYTHONS.contains(&minor) {
        return Err(format!("python {minor} is not supported, use one of {}", SUPPORTED_PYTHONS.join(", ")));
    }
    let prefix = format!("{minor}.");
    let suffix = format!("+{PYTHON_RELEASE}");
    let matches = |name: &str| {
        name.starts_with(&prefix)
            && name.ends_with(&suffix)
            && name[prefix.len()..name.len() - suffix.len()].bytes().all(|b| b.is_ascii_digit())
    };
    let installed = fs::read_dir(home().join("python"))
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().into_owned());
    if let Some(version) = installed.filter(|name| matches(name)).max() {
        return Ok(version);
    }
    let sums = checksums(PYTHON_RELEASE)?;
    sums.lines()
        .filter_map(|line| line.split_whitespace().nth(1))
        .filter_map(|name| name.strip_prefix("cpython-")?.strip_suffix(&format!("-{}-install_only.tar.gz", platform())))
        .find(|version| matches(version))
        .map(String::from)
        .ok_or_else(|| format!("no python {minor} build for {}", platform()))
}

// Installs a python version if needed and returns its interpreter.
pub fn python(version: &str) -> Result<PathBuf, String> {
    let dir = home().join("python").join(version);
    if !dir.exists() {
        let release = version.split_once('+').map_or(PYTHON_RELEASE, |(_, release)| release);
        let name = asset(version);
        let sums = checksums(release)?;
        let expected = sums
            .lines()
            .find_map(|line| line.strip_suffix(&format!("  {name}")))
            .ok_or_else(|| format!("no python build {name}"))?;
        eprintln!("installing python {version}");
        let archive = home().join("downloads").join(&name);
        let bytes = download(
            &format!("https://github.com/astral-sh/python-build-standalone/releases/download/{release}/{name}"),
            &archive,
        )?;
        if sha256(&bytes) != expected {
            return Err(format!("checksum mismatch for {name}"));
        }
        let partial = home().join("python").join(format!("{version}.partial"));
        let _ = fs::remove_dir_all(&partial);
        fs::create_dir_all(&partial).map_err(|error| error.to_string())?;
        run(Command::new("tar").arg("-xzf").arg(&archive).arg("-C").arg(&partial), "tar")?;
        fs::rename(partial.join("python"), &dir).map_err(|error| error.to_string())?;
        let _ = fs::remove_dir_all(&partial);
        let _ = fs::remove_file(&archive);
    }
    Ok(interpreter(&dir, false))
}

pub fn default_python() -> Result<PathBuf, String> {
    python(&python_version(DEFAULT_PYTHON)?)
}

// zig comes as a wheel from PyPI, so pip picks the right build per platform.
fn zig() -> Result<PathBuf, String> {
    let dir = home().join("zig").join(ZIG_VERSION);
    let executable = dir.join("ziglang").join(if cfg!(windows) { "zig.exe" } else { "zig" });
    if !executable.exists() {
        eprintln!("installing zig {ZIG_VERSION} (the C compiler)");
        let partial = home().join("zig").join(format!("{ZIG_VERSION}.partial"));
        let _ = fs::remove_dir_all(&partial);
        let pip =
            ["-m", "pip", "install", "--quiet", "--disable-pip-version-check", "--only-binary", ":all:", "--target"];
        run(
            Command::new(default_python()?).args(pip).arg(&partial).arg(format!("ziglang=={ZIG_VERSION}")),
            "pip install ziglang",
        )?;
        fs::rename(&partial, &dir).map_err(|error| error.to_string())?;
    }
    Ok(executable)
}

// Compiles generated C into an extension module for the given python and
// returns the module's path.
pub fn compile(python: &Path, c_path: &Path, module: &str, release: bool) -> Result<PathBuf, String> {
    let script = "import sys, sysconfig; print(sysconfig.get_paths()['include']); \
                  print(sysconfig.get_config_var('EXT_SUFFIX')); print(sys.base_prefix); \
                  print(f'python{sys.version_info[0]}{sys.version_info[1]}')";
    let info = run(Command::new(python).args(["-c", script]), "python")?;
    let [include, suffix, base, library] = info.lines().collect::<Vec<_>>()[..] else {
        return Err(format!("unexpected sysconfig output from {}", python.display()));
    };
    let out_dir = c_path.parent().unwrap();
    let output = out_dir.join(format!("{module}{suffix}"));
    let mut zig = Command::new(zig()?);
    // No fused multiply-add: python rounds after every float operation.
    zig.args(["cc", "-shared", "-w", "-std=c11", "-ffp-contract=off", if release { "-O2" } else { "-O1" }]);
    zig.arg(format!("-I{include}")).arg(format!("-I{}", out_dir.display()));
    match std::env::consts::OS {
        // Python symbols resolve when the interpreter loads the module.
        "macos" => _ = zig.args(["-fPIC", "-undefined", "dynamic_lookup"]),
        "windows" => {
            _ = zig.args(["-target", "x86_64-windows-gnu"]).arg(format!("-L{base}/libs")).arg(format!("-l{library}"))
        }
        _ => _ = zig.arg("-fPIC"),
    }
    zig.arg(c_path).arg("-o").arg(&output);
    run(&mut zig, "zig cc").map_err(|error| format!("{error}\nthe C code is in {}", c_path.display()))?;
    Ok(output)
}

fn sha256(data: &[u8]) -> String {
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5, 0xd807aa98,
        0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786,
        0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8,
        0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13,
        0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819,
        0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a,
        0x5b9cca4f, 0x682e6ff3, 0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];
    let mut state: [u32; 8] =
        [0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19];
    let mut message = data.to_vec();
    message.push(0x80);
    while message.len() % 64 != 56 {
        message.push(0);
    }
    message.extend_from_slice(&(data.len() as u64 * 8).to_be_bytes());
    for block in message.chunks(64) {
        let mut words = [0u32; 64];
        for i in 0..64 {
            words[i] = if i < 16 {
                u32::from_be_bytes(block[i * 4..i * 4 + 4].try_into().unwrap())
            } else {
                let low = words[i - 15].rotate_right(7) ^ words[i - 15].rotate_right(18) ^ (words[i - 15] >> 3);
                let high = words[i - 2].rotate_right(17) ^ words[i - 2].rotate_right(19) ^ (words[i - 2] >> 10);
                words[i - 16].wrapping_add(low).wrapping_add(words[i - 7]).wrapping_add(high)
            };
        }
        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = state;
        for i in 0..64 {
            let choice = (e & f) ^ (!e & g);
            let first = h
                .wrapping_add(e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25))
                .wrapping_add(choice)
                .wrapping_add(K[i])
                .wrapping_add(words[i]);
            let majority = (a & b) ^ (a & c) ^ (b & c);
            let second = (a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22)).wrapping_add(majority);
            (h, g, f, e, d, c, b, a) = (g, f, e, d.wrapping_add(first), c, b, a, first.wrapping_add(second));
        }
        for (slot, value) in state.iter_mut().zip([a, b, c, d, e, f, g, h]) {
            *slot = slot.wrapping_add(value);
        }
    }
    state.iter().map(|word| format!("{word:08x}")).collect()
}
