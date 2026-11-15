//! One binary, many programs.
//!
//! A binary lists its entries at the top of `main` with [`dispatch`], or makes
//! [`dispatch_or_else`] its whole `main`. Running `<binary> <entry name> ARGS...` then runs
//! that entry instead of the app. Code that needs an entry starts it with [`command`], which
//! runs the current binary again.
//!
//! Entry names are flags such as `--screen`, so a person can run one by hand too. Only an
//! exact first argument matches, and a name must not reuse one of the app's own flags.

use std::ffi::{OsStr, OsString};
use std::io;
use std::process::Command;
use std::sync::OnceLock;

/// A program built into this binary, selected by its name as the first argument.
#[derive(Clone, Copy, Debug)]
pub struct Entry {
    /// The first argument that selects it, such as `--screen`.
    pub name: &'static str,
    /// Runs with the arguments after the name and returns the exit code.
    pub run: fn(Vec<OsString>) -> i32,
}

static ENTRIES: OnceLock<&'static [Entry]> = OnceLock::new();

/// Call first in `main`, before any thread starts. When the first argument names one of
/// `entries`, runs it and exits. Otherwise records `entries` for [`command`] and returns.
pub fn dispatch(entries: &'static [Entry]) {
    let mut args = std::env::args_os().skip(1);
    if let Some(entry) = args.next().and_then(|name| find(entries, &name)) {
        std::process::exit((entry.run)(args.collect()));
    }
    let _ = ENTRIES.set(entries);
}

/// The whole of `main`: runs the entry the first argument names, or else `app`, and exits
/// with the code it returns.
pub fn dispatch_or_else(entries: &'static [Entry], app: fn() -> i32) -> ! {
    dispatch(entries);
    std::process::exit(app())
}

/// A command that runs entry `name` from the current binary. Fails when this binary did not
/// register it, so a missing entry is an error instead of a second copy of the app.
pub fn command(name: &str) -> io::Result<Command> {
    if ENTRIES
        .get()
        .and_then(|entries| find(entries, OsStr::new(name)))
        .is_none()
    {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("this binary has no {name} entry"),
        ));
    }
    let mut command = Command::new(std::env::current_exe()?);
    command.arg(name);
    Ok(command)
}

fn find<'a>(entries: &'a [Entry], name: &OsStr) -> Option<&'a Entry> {
    entries.iter().find(|entry| name == entry.name)
}

#[cfg(test)]
mod tests {
    use super::*;

    const ENTRIES: &[Entry] = &[Entry {
        name: "--screen",
        run: |_| 0,
    }];

    #[test]
    fn finds_entries_by_exact_name() {
        assert!(find(ENTRIES, OsStr::new("--screen")).is_some());
        assert!(find(ENTRIES, OsStr::new("screen")).is_none());
        assert!(find(ENTRIES, OsStr::new("--screen ")).is_none());
    }

    #[test]
    fn refuses_unregistered_entries() {
        let error = command("--screen").unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::NotFound);
    }
}
