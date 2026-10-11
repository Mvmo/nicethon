mod ast;
mod check;
mod codegen;
mod expr;
mod hir;
mod lexer;
mod parser;
mod project;
mod toolchain;

use project::Project;
use std::path::{Path, PathBuf};
use std::process::{exit, Command};
use toolchain::fail;

const PRELUDE: &str = include_str!("../lib/prelude.nice");
const BINDINGS: &str = include_str!("../lib/bindings.nice");
const RUNTIME: &str = include_str!("../runtime/nicethon.h");

struct Loaded {
    names: Vec<String>,
    texts: Vec<String>,
    sources: Vec<check::Source>,
}

fn location(text: &str, pos: u32) -> (usize, usize) {
    let before = &text[..(pos as usize).min(text.len())];
    let line = before.matches('\n').count() + 1;
    let column = before.len() - before.rfind('\n').map_or(0, |i| i + 1) + 1;
    (line, column)
}

fn report(loaded: &Loaded, diags: &[(usize, u32, String)]) -> ! {
    let mut sorted: Vec<_> = diags
        .iter()
        .map(|(module, pos, message)| {
            let (line, column) = location(&loaded.texts[*module], *pos);
            (*module, line, column, message)
        })
        .collect();
    sorted.sort();
    for (module, line, column, message) in sorted {
        eprintln!("{}:{line}:{column}: error: {message}", loaded.names[module]);
    }
    exit(1);
}

fn load(entry: &Path) -> Loaded {
    let mut loaded = Loaded { names: vec![], texts: vec![], sources: vec![] };
    let mut paths: Vec<Option<PathBuf>> = vec![];
    let mut diags = vec![];
    let mut queue = vec![
        (None, "<prelude>".to_string(), PRELUDE.to_string()),
        (None, "<bindings>".to_string(), BINDINGS.to_string()),
    ];
    let text = std::fs::read_to_string(entry).unwrap_or_else(|error| {
        eprintln!("cannot read {}: {error}", entry.display());
        exit(1)
    });
    queue.push((Some(entry.canonicalize().unwrap()), entry.display().to_string(), text));
    let mut next = 0;
    while next < queue.len() {
        let (path, name, text) = queue[next].clone();
        next += 1;
        let (module, errors) = parser::parse_module(&text);
        let index = loaded.sources.len();
        diags.extend(errors.into_iter().map(|(pos, message)| (index, pos, message)));
        let mut imports = vec![];
        for import in &module.imports {
            if import.py {
                imports.push(None);
                continue;
            }
            let base = path.as_ref().and_then(|p| p.parent().map(Path::to_path_buf)).unwrap_or_default();
            let target = base.join(format!("{}.nice", import.from));
            let Ok(target) = target.canonicalize() else {
                diags.push((index, import.pos, format!("cannot find module {}", import.from)));
                imports.push(None);
                continue;
            };
            let existing = queue.iter().position(|(p, _, _)| p.as_ref() == Some(&target));
            let target_index = match existing {
                Some(found) => found,
                None => {
                    let text = std::fs::read_to_string(&target).unwrap_or_default();
                    queue.push((Some(target.clone()), target.display().to_string(), text));
                    queue.len() - 1
                }
            };
            imports.push(Some(target_index));
        }
        paths.push(path);
        loaded.names.push(name);
        loaded.texts.push(text);
        loaded.sources.push(check::Source { module, imports });
    }
    if !diags.is_empty() {
        report(&loaded, &diags);
    }
    loaded
}

fn build(entry: &Path, out_dir: &Path, python: &Path, release: bool) -> String {
    let loaded = load(entry);
    let module_name = entry.file_stem().unwrap().to_string_lossy().to_string();
    if !module_name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        fail(format!(
            "{}: the file name must only use letters, digits and _, it becomes the python module name",
            entry.display()
        ));
    }
    let program = match check::check(&loaded.sources, 2) {
        Ok(program) => program,
        Err(diags) => report(&loaded, &diags.into_iter().map(|d| (d.module, d.pos, d.message)).collect::<Vec<_>>()),
    };
    let c_source = codegen::generate(&program, &module_name, &loaded.names, &loaded.texts);
    std::fs::create_dir_all(out_dir).unwrap_or_else(|error| fail(error));
    std::fs::write(out_dir.join("nicethon.h"), RUNTIME).unwrap();
    let c_path = out_dir.join(format!("{module_name}.c"));
    std::fs::write(&c_path, c_source).unwrap();
    toolchain::compile(python, &c_path, &module_name, release).unwrap_or_else(|error| fail(error));
    module_name
}

const USAGE: &str = "usage:
  nicethon new <name>                 create a project
  nicethon add <package> [version]    add a dependency, e.g. nicethon add numpy \">=2\"
  nicethon remove <package>           remove a dependency
  nicethon sync                       install python and the packages in nice.lock
  nicethon update                     resolve nice.toml again, to the newest allowed versions
  nicethon check [file.nice]          type check
  nicethon run [file.nice] [--release] [-- program arguments]
  nicethon build [file.nice] [-o dir] [--release]
  nicethon python [arguments]         run the project's python
Without a file, check, run and build use the project's main file.";

fn main() {
    let all: Vec<String> = std::env::args().skip(1).collect();
    if all.first().is_some_and(|command| command == "python") {
        let python = project::python_for(Project::find(Path::new(".")).as_ref());
        let status = Command::new(python).args(&all[1..]).status().unwrap_or_else(|error| fail(error));
        exit(status.code().unwrap_or(1));
    }
    let split = all.iter().position(|a| a == "--").unwrap_or(all.len());
    let (args, program_args) = (&all[..split], all.get(split + 1..).unwrap_or_default());
    // Flags may stand anywhere before "--".
    let (mut positional, mut out_dir, mut release) = (vec![], "build", false);
    let mut rest = args.iter();
    while let Some(arg) = rest.next() {
        match arg.as_str() {
            "-o" => out_dir = rest.next().map_or(out_dir, String::as_str),
            "--release" => release = true,
            _ => positional.push(arg.as_str()),
        }
    }
    match positional[..] {
        ["new", name] => project::create(name),
        ["add", name] => project::edit_dependency(name, Some("*")),
        ["add", name, spec] => project::edit_dependency(name, Some(spec)),
        ["remove", name] => project::edit_dependency(name, None),
        ["sync"] => _ = Project::current().sync().unwrap_or_else(|error| fail(error)),
        ["update"] => _ = Project::current().update().unwrap_or_else(|error| fail(error)),
        [command @ ("check" | "build" | "run")] | [command @ ("check" | "build" | "run"), _] => {
            let (entry, project) = match positional.get(1) {
                Some(file) => {
                    let entry = PathBuf::from(file);
                    let dir = entry.parent().filter(|dir| !dir.as_os_str().is_empty()).unwrap_or(Path::new("."));
                    let project = Project::find(dir);
                    (entry, project)
                }
                None => {
                    let project = Project::current();
                    (project.root.join(&project.main), Some(project))
                }
            };
            match command {
                "check" => {
                    let loaded = load(&entry);
                    if let Err(diags) = check::check(&loaded.sources, 2) {
                        report(&loaded, &diags.into_iter().map(|d| (d.module, d.pos, d.message)).collect::<Vec<_>>());
                    }
                }
                "build" => _ = build(&entry, Path::new(out_dir), &project::python_for(project.as_ref()), release),
                _ => {
                    let python = project::python_for(project.as_ref());
                    let cache = project.map_or_else(
                        || std::env::temp_dir().join("nicethon-build"),
                        |project| project.root.join(".nice").join("build"),
                    );
                    let out_dir =
                        cache.join(entry.canonicalize().map(|p| format!("{:x}", hash(&p))).unwrap_or_default());
                    let module = build(&entry, &out_dir, &python, release);
                    let path = out_dir.display().to_string();
                    let script = format!(
                        "import sys, importlib; sys.argv = sys.argv[1:]; sys.path.insert(0, {path:?}); \
                         importlib.import_module({module:?}).main()"
                    );
                    let status = Command::new(&python)
                        .args(["-c", &script])
                        .arg(&entry)
                        .args(program_args)
                        .status()
                        .unwrap_or_else(|error| fail(error));
                    if status.code().is_none() {
                        eprintln!("the program crashed ({status})");
                    }
                    exit(status.code().unwrap_or(1));
                }
            }
        }
        _ => {
            eprintln!("{USAGE}");
            exit(2);
        }
    }
}

fn hash(path: &Path) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    path.hash(&mut hasher);
    hasher.finish()
}
