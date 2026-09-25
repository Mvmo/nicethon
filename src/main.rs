mod ast;
mod check;
mod codegen;
mod expr;
mod hir;
mod lexer;
mod parser;

use std::path::{Path, PathBuf};
use std::process::{exit, Command};

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
    let mut sorted: Vec<_> = diags.iter().map(|(module, pos, message)| {
        let (line, column) = location(&loaded.texts[*module], *pos);
        (*module, line, column, message)
    }).collect();
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
    let mut queue = vec![(None, "<prelude>".to_string(), PRELUDE.to_string()), (None, "<bindings>".to_string(), BINDINGS.to_string())];
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

fn python_config(python: &str) -> (String, String) {
    let script = "import sysconfig; print(sysconfig.get_paths()['include']); print(sysconfig.get_config_var('EXT_SUFFIX'))";
    let output = Command::new(python).args(["-c", script]).output().unwrap_or_else(|error| {
        eprintln!("cannot run {python}: {error}");
        exit(1)
    });
    let text = String::from_utf8_lossy(&output.stdout).to_string();
    let mut lines = text.lines();
    (lines.next().unwrap_or_default().to_string(), lines.next().unwrap_or(".so").to_string())
}

fn build(entry: &Path, out_dir: &Path, python: &str, release: bool) -> String {
    let loaded = load(entry);
    let module_name = entry.file_stem().unwrap().to_string_lossy().to_string();
    if !module_name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        eprintln!("{}: the file name must only use letters, digits and _, it becomes the python module name", entry.display());
        exit(1);
    }
    let program = match check::check(&loaded.sources, 2) {
        Ok(program) => program,
        Err(diags) => report(&loaded, &diags.into_iter().map(|d| (d.module, d.pos, d.message)).collect::<Vec<_>>()),
    };
    let c_source = codegen::generate(&program, &module_name, &loaded.names, &loaded.texts);
    std::fs::create_dir_all(out_dir).unwrap();
    std::fs::write(out_dir.join("nicethon.h"), RUNTIME).unwrap();
    let c_path = out_dir.join(format!("{module_name}.c"));
    std::fs::write(&c_path, c_source).unwrap();
    let (include, suffix) = python_config(python);
    let output = out_dir.join(format!("{module_name}{suffix}"));
    let mut cc = Command::new(std::env::var("CC").unwrap_or("cc".into()));
    cc.args(["-shared", "-fPIC", "-w", "-std=c11"]).arg(if release { "-O2" } else { "-O1" }).arg(format!("-I{include}")).arg(format!("-I{}", out_dir.display()));
    if cfg!(target_os = "macos") {
        cc.args(["-undefined", "dynamic_lookup"]);
    }
    cc.arg(&c_path).arg("-o").arg(&output);
    let status = cc.status().unwrap_or_else(|error| {
        eprintln!("cannot run the C compiler: {error}");
        exit(1)
    });
    if !status.success() {
        eprintln!("C compilation failed, see {}", c_path.display());
        exit(1);
    }
    module_name
}

fn main() {
    let all: Vec<String> = std::env::args().skip(1).collect();
    let split = all.iter().position(|a| a == "--").unwrap_or(all.len());
    let (args, program_args) = (&all[..split], all.get(split + 1..).unwrap_or_default());
    let flag = |name: &str| args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned();
    let python = flag("--python")
        .or_else(|| std::env::var("VIRTUAL_ENV").ok().map(|venv| format!("{venv}/bin/python")))
        .unwrap_or_else(|| "python3".to_string());
    let (Some(command), Some(file)) = (args.first(), args.get(1)) else {
        eprintln!("usage: nicethon <check|build|run> file.nice [-o dir] [--python path] [--release] [-- program arguments]");
        exit(2);
    };
    let entry = PathBuf::from(file);
    let release = args.iter().any(|a| a == "--release");
    match command.as_str() {
        "check" => {
            let loaded = load(&entry);
            if let Err(diags) = check::check(&loaded.sources, 2) {
                report(&loaded, &diags.into_iter().map(|d| (d.module, d.pos, d.message)).collect::<Vec<_>>());
            }
        }
        "build" => {
            let out_dir = PathBuf::from(flag("-o").unwrap_or("build".into()));
            build(&entry, &out_dir, &python, release);
        }
        "run" => {
            let out_dir = std::env::temp_dir().join("nicethon-build").join(entry.canonicalize().map(|p| format!("{:x}", hash(&p))).unwrap_or_default());
            let module = build(&entry, &out_dir, &python, release);
            let script = format!("import sys, importlib; sys.argv = sys.argv[1:]; sys.path.insert(0, {:?}); importlib.import_module({module:?}).main()", out_dir.display().to_string());
            let status = Command::new(&python).args(["-c", &script, file]).args(program_args).status().unwrap();
            if status.code().is_none() {
                eprintln!("the program crashed ({status})");
            }
            exit(status.code().unwrap_or(1));
        }
        _ => {
            eprintln!("unknown command {command}");
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
