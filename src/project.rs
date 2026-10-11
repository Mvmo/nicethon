//! Projects: nice.toml says which python and which packages a program uses,
//! nice.lock pins the exact versions that were installed. The environment
//! lives in .nice/env next to nice.toml and always matches nice.lock.

use crate::toolchain::{self, fail, run, DEFAULT_PYTHON};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

type Table = BTreeMap<String, String>;

pub struct Project {
    pub root: PathBuf,
    pub main: String,
    python: String,
    dependencies: Table,
}

impl Project {
    // The nearest nice.toml in start or one of its parent directories.
    pub fn find(start: &Path) -> Option<Project> {
        let start = std::path::absolute(start).ok()?;
        let root = start.ancestors().find(|dir| dir.join("nice.toml").is_file())?;
        Some(Project::load(root))
    }

    fn load(root: &Path) -> Project {
        let path = root.join("nice.toml");
        let text =
            fs::read_to_string(&path).unwrap_or_else(|error| fail(format!("cannot read {}: {error}", path.display())));
        let mut tables =
            parse(&text).unwrap_or_else(|(line, message)| fail(format!("{}:{line}: {message}", path.display())));
        let mut project = tables.remove("project").unwrap_or_default();
        let dependencies = tables.remove("dependencies").unwrap_or_default();
        let unknown = tables
            .keys()
            .map(|table| format!("[{table}]"))
            .chain(project.keys().filter(|key| !["name", "python", "main"].contains(&key.as_str())).cloned());
        if let Some(unknown) = unknown.into_iter().next() {
            fail(format!(
                "{}: unknown entry {unknown}, allowed are [project] with name, python, main and [dependencies]",
                path.display()
            ));
        }
        for (name, spec) in &dependencies {
            check_spec(name, spec).unwrap_or_else(|error| fail(format!("{}: {error}", path.display())));
        }
        Project {
            root: root.to_path_buf(),
            main: project.remove("main").unwrap_or("main.nice".into()),
            python: project.remove("python").unwrap_or(DEFAULT_PYTHON.into()),
            dependencies,
        }
    }

    pub fn current() -> Project {
        Project::find(Path::new(".")).unwrap_or_else(|| {
            fail("no nice.toml here or in a parent directory, create a project with: nicethon new <name>")
        })
    }

    // Makes .nice/env match nice.lock, after resolving nice.toml into a new
    // nice.lock if the two disagree. Returns the environment's python.
    pub fn sync(&self) -> Result<PathBuf, String> {
        let lock_path = self.root.join("nice.lock");
        let lock_text = fs::read_to_string(&lock_path).unwrap_or_default();
        let lock = parse(&lock_text).unwrap_or_default();
        let locked_python = lock
            .get("")
            .and_then(|top| top.get("python"))
            .filter(|version| version.starts_with(&format!("{}.", self.python)));
        let env = self.root.join(".nice").join("env");
        let python = toolchain::interpreter(&env, true);
        let stamp = env.join("nice.lock");
        match locked_python {
            Some(version) if lock.get("requires").cloned().unwrap_or_default() == self.dependencies => {
                if fs::read_to_string(&stamp).is_ok_and(|text| text == lock_text) {
                    return Ok(python);
                }
                let pins =
                    lock.get("packages").into_iter().flatten().map(|(name, version)| format!("{name}=={version}"));
                create_env(&env, version, &pins.collect::<Vec<_>>(), &["--no-deps"])?;
                fs::write(&stamp, lock_text).map_err(|error| error.to_string())?;
            }
            _ => {
                let version = match locked_python {
                    Some(version) => version.clone(),
                    None => toolchain::python_version(&self.python)?,
                };
                let requirements: Vec<String> = self
                    .dependencies
                    .iter()
                    .map(|(name, spec)| if spec == "*" { name.clone() } else { format!("{name}{spec}") })
                    .collect();
                let base = create_env(&env, &version, &requirements, &[])?;
                let installed = run(pip(&base, &python).args(["list", "--format=freeze"]), "pip list")?;
                let mut text = format!(
                    "# Written by nicethon from nice.toml, do not edit.\n{}\n[requires]\n",
                    entry("python", &version)
                );
                for (name, spec) in &self.dependencies {
                    text += &entry(name, spec);
                }
                text += "\n[packages]\n";
                for (name, version) in installed.lines().filter_map(|line| line.split_once("==")) {
                    text += &entry(name, version);
                }
                fs::write(&lock_path, &text).map_err(|error| error.to_string())?;
                fs::write(&stamp, text).map_err(|error| error.to_string())?;
            }
        }
        Ok(python)
    }

    // Resolves nice.toml again, to the newest versions it allows.
    pub fn update(&self) -> Result<PathBuf, String> {
        let _ = fs::remove_file(self.root.join("nice.lock"));
        self.sync()
    }
}

// The python a file runs with: its project's environment, or the default
// python with only the standard library.
pub fn python_for(project: Option<&Project>) -> PathBuf {
    match project {
        Some(project) => project.sync(),
        None => toolchain::default_python(),
    }
    .unwrap_or_else(|error| fail(error))
}

fn pip(base: &Path, python: &Path) -> Command {
    let mut command = Command::new(base);
    command.args(["-m", "pip", "--disable-pip-version-check", "--python"]).arg(python);
    command
}

// A fresh environment without its own pip; the base python's pip installs into it.
fn create_env(env: &Path, version: &str, packages: &[String], flags: &[&str]) -> Result<PathBuf, String> {
    let base = toolchain::python(version)?;
    let _ = fs::remove_dir_all(env);
    run(Command::new(&base).args(["-m", "venv", "--without-pip"]).arg(env), "python -m venv")?;
    if !packages.is_empty() {
        eprintln!("installing {}", packages.join(", "));
        run(
            pip(&base, &toolchain::interpreter(env, true)).args(["install", "--quiet"]).args(flags).args(packages),
            "pip install",
        )?;
    }
    Ok(base)
}

pub fn create(name: &str) {
    let root = Path::new(name);
    if root.exists() {
        fail(format!("{name} already exists"));
    }
    let files = [
        (
            "nice.toml",
            format!("[project]\n{}{}\n[dependencies]\n", entry("name", name), entry("python", DEFAULT_PYTHON)),
        ),
        ("main.nice", format!("fn main() {{\n    print(\"Hello from {name}!\");\n}}\n")),
        (".gitignore", "/.nice\n/build\n".into()),
    ];
    for (file, text) in files {
        fs::create_dir_all(root).and_then(|_| fs::write(root.join(file), text)).unwrap_or_else(|error| fail(error));
    }
    println!("created {name}, run it with: cd {name} && nicethon run");
}

// Adds, changes (spec is Some) or removes a dependency in nice.toml and
// installs the result. nice.toml stays unchanged if that fails.
pub fn edit_dependency(name: &str, spec: Option<&str>) {
    if let Some(spec) = spec {
        check_spec(name, spec).unwrap_or_else(|error| fail(error));
    }
    let project = Project::current();
    let path = project.root.join("nice.toml");
    let old = fs::read_to_string(&path).unwrap_or_else(|error| fail(error));
    let mut lines: Vec<String> = old.lines().map(String::from).collect();
    let header = match lines.iter().position(|line| line.trim() == "[dependencies]") {
        Some(header) => header,
        None => {
            lines.extend(["".into(), "[dependencies]".into()]);
            lines.len() - 1
        }
    };
    let end = (header + 1..lines.len()).find(|&i| lines[i].trim_start().starts_with('[')).unwrap_or(lines.len());
    let existing = (header + 1..end).find(|&i| key_of(&lines[i]).is_some_and(|key| normalize(&key) == normalize(name)));
    match (existing, spec) {
        (Some(line), Some(spec)) => lines[line] = entry(name, spec).trim_end().into(),
        (Some(line), None) => _ = lines.remove(line),
        (None, Some(spec)) => {
            let last = (header..end).rev().find(|&i| !lines[i].trim().is_empty()).unwrap_or(header);
            lines.insert(last + 1, entry(name, spec).trim_end().into());
        }
        (None, None) => fail(format!("{name} is not a dependency")),
    }
    fs::write(&path, lines.join("\n") + "\n").unwrap_or_else(|error| fail(error));
    if let Err(error) = Project::load(&project.root).sync() {
        let _ = fs::write(&path, old);
        fail(error);
    }
}

fn check_spec(name: &str, spec: &str) -> Result<(), String> {
    if spec == "*" || spec.starts_with(['=', '<', '>', '~', '!']) {
        Ok(())
    } else {
        Err(format!("dependency {name}: write \"*\" or a version specifier like \">=2.0\", not \"{spec}\""))
    }
}

// Package names compare like pip compares them: case and -_. do not matter.
fn normalize(name: &str) -> String {
    name.to_lowercase().replace(['_', '.'], "-")
}

fn key_of(line: &str) -> Option<String> {
    parse(line).ok()?.remove("")?.into_keys().next()
}

fn entry(key: &str, value: &str) -> String {
    let quote = |text: &str| format!("\"{}\"", text.replace('\\', "\\\\").replace('"', "\\\""));
    let bare = !key.is_empty() && key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
    format!("{} = {}\n", if bare { key.to_string() } else { quote(key) }, quote(value))
}

// The part of TOML that nice.toml and nice.lock use: [tables] and
// key = "string" lines. Keys outside a table land in the table "".
fn parse(text: &str) -> Result<BTreeMap<String, Table>, (usize, String)> {
    let mut tables: BTreeMap<String, Table> = BTreeMap::new();
    let mut table = String::new();
    for (index, line) in text.lines().enumerate() {
        let error = |message: &str| (index + 1, message.to_string());
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(name) = line.strip_prefix('[').and_then(|rest| rest.strip_suffix(']')) {
            table = name.trim().to_string();
            tables.entry(table.clone()).or_default();
            continue;
        }
        let (key, rest) = if line.starts_with('"') {
            string(line).ok_or_else(|| error("unterminated string"))?
        } else {
            let end = line.find(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '-')).unwrap_or(line.len());
            (line[..end].to_string(), &line[end..])
        };
        let rest = rest
            .trim_start()
            .strip_prefix('=')
            .filter(|_| !key.is_empty())
            .ok_or_else(|| error("expected key = \"value\""))?;
        let (value, rest) =
            string(rest.trim_start()).ok_or_else(|| error("values must be strings in double quotes"))?;
        if !(rest.trim().is_empty() || rest.trim_start().starts_with('#')) {
            return Err(error("unexpected text after the value"));
        }
        if tables.entry(table.clone()).or_default().insert(key.clone(), value).is_some() {
            return Err(error(&format!("{key} is set twice")));
        }
    }
    Ok(tables)
}

// A double quoted string with \" and \\ escapes, and the text after it.
fn string(text: &str) -> Option<(String, &str)> {
    let mut value = String::new();
    let mut chars = text.strip_prefix('"')?.char_indices();
    while let Some((index, c)) = chars.next() {
        match c {
            '"' => return Some((value, &text[index + 2..])),
            '\\' => value.push(chars.next()?.1),
            _ => value.push(c),
        }
    }
    None
}
