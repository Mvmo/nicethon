mod ast;
mod check;
mod expr;
mod hir;
mod lexer;
mod parser;

use std::path::{Path, PathBuf};
use std::process::{exit, Command};

const PRELUDE: &str = include_str!("../lib/prelude.nice");
const BINDINGS: &str = include_str!("../lib/bindings.nice");

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
        _ => {
            eprintln!("unknown command {command}");
            exit(2);
        }
    }
}
