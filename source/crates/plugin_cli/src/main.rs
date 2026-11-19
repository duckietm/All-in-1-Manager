//! `aio-plugin`: the tool for All-in-1 Manager plugin authors.
//!
//! ```text
//! aio-plugin new <dir> --id <id> [--sdk-path <dir>]   a new plugin project
//! aio-plugin build [--locked] [--offline] [--features f]   cargo build for wasm32-wasip2, release
//! aio-plugin pack [--out file] [build flags]           build, then pack dist/<id>.aiop
//! aio-plugin check <file.aiop|dir>                     what the Manager will see: id, api, permissions, sha256
//! aio-plugin dev                                       how to load this folder in developer mode
//! ```
//!
//! `build`, `pack` and `dev` work on the plugin in the current folder. Every error goes to stderr with a
//! non-zero exit.

mod build;
mod check;
mod new;
mod pack;

use anyhow::{bail, Context, Result};
use std::collections::BTreeMap;
use std::path::PathBuf;

const USAGE: &str = "\
aio-plugin: create, build, pack and check All-in-1 Manager plugins

usage:
  aio-plugin new <dir> --id <id> [--sdk-path <dir>]
      A new plugin project in <dir> (which must not exist or be empty). --sdk-path uses a local copy of
      aio-plugin-sdk instead of the git dependency.
  aio-plugin build [--locked] [--offline] [--features <f>]
      Builds the plugin in the current folder for wasm32-wasip2 (release).
  aio-plugin pack [--out <file>] [--locked] [--offline] [--features <f>]
      Builds, then packs dist/<id>.aiop (or --out). The same build gives the same bytes.
  aio-plugin check <file.aiop|dir>
      Checks a package (or a built plugin folder) as the Manager does on install and load, and prints what
      it shows and the API the component needs.
  aio-plugin dev
      Prints how to load the plugin in the current folder in the Manager's developer mode.
";

/// `--flag value` options, `--switch` options and positional arguments.
pub struct Args {
    flags: BTreeMap<String, String>,
    switches: Vec<String>,
    positional: Vec<String>,
}

/// Options that are switches (no value).
const SWITCHES: &[&str] = &["locked", "offline"];

impl Args {
    fn parse(raw: &[String]) -> Result<Args> {
        let mut args = Args { flags: BTreeMap::new(), switches: Vec::new(), positional: Vec::new() };
        let mut it = raw.iter();
        while let Some(a) = it.next() {
            if let Some(name) = a.strip_prefix("--") {
                if SWITCHES.contains(&name) {
                    if args.switches.iter().any(|s| s == name) {
                        bail!("--{name} given twice");
                    }
                    args.switches.push(name.to_string());
                    continue;
                }
                let value = it.next().with_context(|| format!("--{name} needs a value"))?;
                if args.flags.insert(name.to_string(), value.clone()).is_some() {
                    bail!("--{name} given twice");
                }
            } else {
                args.positional.push(a.clone());
            }
        }
        Ok(args)
    }

    pub fn flag(&self, name: &str) -> Option<&str> {
        self.flags.get(name).map(String::as_str)
    }

    pub fn switch(&self, name: &str) -> bool {
        self.switches.iter().any(|s| s == name)
    }

    fn only(&self, allowed: &[&str], positional: usize) -> Result<()> {
        let given = self.flags.keys().chain(&self.switches);
        if let Some(f) = given.into_iter().find(|k| !allowed.contains(&k.as_str())) {
            bail!("unknown option --{f}");
        }
        if self.positional.len() != positional {
            bail!("expected {positional} argument(s), got {}", self.positional.len());
        }
        Ok(())
    }
}

fn run(raw: &[String]) -> Result<()> {
    // `--help` or `-h` anywhere (`aio-plugin new --help`) prints the usage.
    if raw.is_empty() || raw.iter().any(|a| a == "--help" || a == "-h") {
        print!("{USAGE}");
        return Ok(());
    }
    let (command, rest) = (&raw[0], &raw[1..]);
    let args = Args::parse(rest)?;
    let here = || std::env::current_dir().context("cannot read the current folder");
    match command.as_str() {
        "new" => {
            args.only(&["id", "sdk-path"], 1)?;
            let id = args.flag("id").context("missing --id")?;
            let sdk = args.flag("sdk-path").map(PathBuf::from);
            let dir = PathBuf::from(&args.positional[0]);
            new::create(&dir, id, sdk.as_deref())?;
            println!("Created the plugin {id} in {}", dir.display());
            println!("Next: cd {} && aio-plugin build && aio-plugin dev", dir.display());
        }
        "build" => {
            args.only(&["locked", "offline", "features"], 0)?;
            let wasm = build::build(&here()?, &build::Options::from(&args))?;
            println!("{}", wasm.display());
        }
        "pack" => {
            args.only(&["locked", "offline", "features", "out"], 0)?;
            let out = pack::pack(&here()?, &build::Options::from(&args), args.flag("out").map(PathBuf::from))?;
            println!("{}", out.display());
        }
        "check" => {
            args.only(&[], 1)?;
            check::check(&PathBuf::from(&args.positional[0]))?;
        }
        "dev" => {
            args.only(&[], 0)?;
            print!("{}", build::dev_text(&here()?)?);
        }
        "help" => print!("{USAGE}"),
        other => bail!("unknown command {other} (try aio-plugin help)"),
    }
    Ok(())
}

fn main() {
    let raw: Vec<String> = std::env::args().skip(1).collect();
    if let Err(error) = run(&raw) {
        eprintln!("error: {error:#}");
        std::process::exit(1);
    }
}
