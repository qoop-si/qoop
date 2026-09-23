//! What a person would say is running in a terminal.
//!
//! The executable name alone is not that answer. An agent installed as an npm package is
//! executed by `node`, one installed as a wheel by `python3.12`, and an index that reports
//! the runtime tells a person nothing they did not already know. So the executable is only
//! the starting point, and `argv` is read to see through the wrapper.
//!
//! The opposite failure is the expensive one. A payload passed to `-c` or `-e` is a string,
//! not a program, and it very often mentions the name of a program: a naive
//! scan reports `python3 -c "import codex"` as an agent that was never
//! running. Unwrapping is therefore allowlisted on
//! both sides. Only a known runtime is unwrapped at all, and only up to the first eval flag.
//!
//! Two checks are deliberately absent, because this crate performs no I/O
//! by design. A symlinked `argv[0]` is not resolved, which would need `canonicalize`, so an
//! agent reached only through a symlink under an unrelated name is reported under that
//! name. An environment variable override is not read either, so a sandbox that hides its
//! child's argv cannot be corrected from the outside here. Both belong to whichever layer
//! already touches the process table.

/// What a terminal's foreground process is, once the wrappers are seen through.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Program {
    /// What to show a person: `claude`, `codex`, `zsh`.
    pub name: String,
    /// True when this is a coding agent rather than a shell or an ordinary command.
    pub agent: bool,
}

/// Known agents and the spellings they are launched under.
///
/// The first spelling in a row is the canonical one, so an index does not show the same
/// agent under two names depending on how it happened to be installed. Membership of this
/// table is the whole definition of `agent`: nothing is promoted by looking agent shaped.
const AGENT_ALIASES: &[&[&str]] = &[
    &["claude", "claude-code"],
    &["codex"],
    &["gemini"],
    &["cursor", "cursor-agent"],
    &["aider"],
    // `opencode2` is the OpenCode 2 binary, from its own v2 documentation.
    // `open-code` is a spelling observed in the wild.
    &["opencode", "opencode2", "open-code"],
    // `amp-local` is a spelling observed in the wild, carried for the same reason.
    &["amp", "amp-local"],
    &["goose"],
    // Agents with no bundled screen rules yet, listed here so that what is running in a
    // terminal is named correctly and so that a restart knows whose conversation to carry
    // on. Being in this table is what makes a program an agent; it says nothing about how
    // well its screen is read.
    &["droid"],
    &["qwen"],
    &["grok"],
    &["copilot"],
];

/// Flags whose argument is source text rather than a path.
///
/// Everything after one of these is a payload, so the search for a wrapped program stops
/// here rather than reading the payload's words as program names.
const EVAL_FLAGS: &[&str] = &["-c", "-e", "--eval", "-p", "--print"];

/// The program a person would say is running, given the executable path and its argv.
///
/// `argv` may or may not repeat the executable as its first entry, since that depends on
/// how the caller sampled the process table, and both shapes are accepted.
pub fn identify(executable: &str, argv: &[String]) -> Program {
    let base = basename(executable);
    if is_runtime(base)
        && let Some(script) = wrapped_script(base, argv)
    {
        return from_path(script);
    }
    from_path(executable)
}

/// Runtimes whose first non flag argument is the program a person means.
///
/// Shells are absent on purpose. `sh -c "sleep 60; /tmp/codex"` is a shell running a
/// string, and the string is not a program we are entitled to name, so a shell is never
/// unwrapped at all rather than being unwrapped and then filtered.
fn is_runtime(name: &str) -> bool {
    matches!(name, "node" | "bun" | "deno") || is_python(name) || is_shell(name)
}

/// A shell is a runtime too, when it was handed a script rather than a command.
///
/// A script with a shebang is executed by its interpreter, so the process is `/bin/sh` and
/// the script's own path is the first argument. Without this a program called `deploy` and
/// an agent installed as a shell wrapper both read as `sh`, which is the name of the
/// interpreter and not of the thing a person started. Measured with a stand in agent: the
/// index said `bash` while the agent's own prompt was on the screen next to it.
///
/// The `-c` case is excluded where the arguments are read (`wrapped_script`), because a
/// command string is a payload and not a program.
fn is_shell(name: &str) -> bool {
    matches!(name, "sh" | "bash" | "zsh" | "fish" | "dash" | "ksh")
}

/// Python ships under its version number, so the name is a family rather than a constant.
fn is_python(name: &str) -> bool {
    name.strip_prefix("python")
        .is_some_and(|rest| rest.chars().all(|c| c.is_ascii_digit() || c == '.'))
}

/// The first argv entry that names a program, or nothing if the runtime was handed a
/// payload instead.
fn wrapped_script<'a>(runtime: &str, argv: &'a [String]) -> Option<&'a str> {
    let _ = runtime;
    let mut rest = argv;
    // Leading interpreters are skipped by what they are, not by whether they match the
    // executable's name, because those two disagree in the ordinary case: a shebang script
    // run through `/bin/sh` reports its executable as `/bin/bash` on this machine, so an
    // argv of `["/bin/sh", "/tmp/claude"]` compared against the name `bash` skipped
    // nothing and answered `sh`. Measured on a stand in agent, whose own prompt was on the
    // screen while the index named the interpreter.
    while rest
        .first()
        .is_some_and(|first| is_runtime(basename(first)))
    {
        rest = &rest[1..];
    }
    for arg in rest {
        if EVAL_FLAGS.contains(&arg.as_str()) {
            return None;
        }
        // ponytail: a flag that takes a separate value would have that value read as the
        // script. No runtime we unwrap needs one before the script path, so the case is
        // left until a real invocation produces it.
        if arg.starts_with('-') {
            continue;
        }
        return Some(arg);
    }
    None
}

/// Resolve one path or bare name into a program.
fn from_path(path: &str) -> Program {
    let file = basename(path);
    let package = package_name(path);
    for candidate in package.into_iter().chain(std::iter::once(file)) {
        if let Some(canonical) = canonical_agent(candidate) {
            return Program {
                name: canonical.to_string(),
                agent: true,
            };
        }
    }
    Program {
        // The package name wins over the file name only when there is one, because a
        // package layout ends in something like `dist/cli.js` that names nothing.
        name: package.unwrap_or(file).to_string(),
        agent: false,
    }
}

/// The canonical spelling of a known agent, matched without regard to case.
///
/// Case is folded for the lookup only. An unknown program keeps the spelling it was
/// launched under, because that is a person's own script and its name is theirs.
///
/// A Windows script suffix (`.exe`, `.cmd`, `.bat`, `.ps1`) is stripped before the
/// lookup, so each agent has one name
/// to match: on Windows the Cursor agent runs as `cursor-agent.cmd`, and the
/// same stripping maps `opencode2.exe` to `opencode2`. `.js` is deliberately not
/// stripped: a node script named by its file goes through the package-name channel
/// above instead.
fn canonical_agent(name: &str) -> Option<&'static str> {
    let stripped;
    let mut candidate = name;
    for suffix in [".exe", ".cmd", ".bat", ".ps1"] {
        if candidate.len() > suffix.len()
            && candidate[candidate.len() - suffix.len()..].eq_ignore_ascii_case(suffix)
        {
            stripped = &candidate[..candidate.len() - suffix.len()];
            candidate = stripped;
            break;
        }
    }
    AGENT_ALIASES
        .iter()
        .find(|row| {
            row.iter()
                .any(|alias| alias.eq_ignore_ascii_case(candidate))
        })
        .and_then(|row| row.first().copied())
}

/// The package an npm install layout puts a script inside.
///
/// A second identification channel, independent of the alias table, for the common case
/// where the executed file is called something generic like `dist/cli.js`. It is
/// deliberately conservative: it supplies a better name to show, and nothing more. A
/// package found here still has to appear in the alias table to count as an agent, so an
/// ordinary tool run out of `node_modules` cannot promote itself into the index by living
/// in a directory that looks the same as an agent's.
fn package_name(path: &str) -> Option<&str> {
    let tail = path
        .rsplit_once("/node_modules/")
        .map(|(_, tail)| tail)
        .or_else(|| path.strip_prefix("node_modules/"))?;
    let mut parts = tail.split('/');
    let first = parts.next()?;
    // A scoped package spends its first component on the scope, which names a publisher
    // rather than a program.
    let name = if first.starts_with('@') {
        parts.next()?
    } else {
        first
    };
    // `.bin` holds the shims, whose own names are already the program names.
    (!name.is_empty() && name != ".bin").then_some(name)
}

/// The last segment of a path, tolerating a trailing slash and an empty string.
fn basename(path: &str) -> &str {
    path.trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or(path)
}

#[cfg(test)]
mod shebang {
    use super::*;

    fn argv(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|p| p.to_string()).collect()
    }

    #[test]
    fn a_script_is_named_by_itself_rather_than_by_its_interpreter() {
        // The shape the kernel builds for a shebang: the interpreter runs, and the script
        // it was asked to run is the first argument.
        let p = identify("/bin/sh", &argv(["/bin/sh", "/tmp/some/claude"].as_slice()));
        assert_eq!(p.name, "claude");
        assert!(p.agent);
    }

    #[test]
    fn a_script_that_is_not_an_agent_still_gets_its_own_name() {
        let p = identify("/bin/bash", &argv(["/bin/bash", "./deploy.sh"].as_slice()));
        assert_eq!(p.name, "deploy.sh");
        assert!(!p.agent);
    }

    #[test]
    fn an_interactive_shell_is_still_a_shell() {
        // A flag is not a script, so nothing is unwrapped and the shell keeps its name.
        let p = identify("/bin/zsh", &argv(["-zsh", "-i"].as_slice()));
        assert_eq!(p.name, "zsh");
        assert!(!p.agent);
    }

    #[test]
    fn a_command_string_is_not_unwrapped() {
        // The false positive the competitor's own regression tests are about: a program
        // named inside a command string is not the program that is running.
        let p = identify(
            "/bin/sh",
            &argv(["sh", "-c", "sleep 60; /tmp/codex"].as_slice()),
        );
        assert_eq!(p.name, "sh");
        assert!(!p.agent);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(args: &[&str]) -> Vec<String> {
        args.iter().map(|a| a.to_string()).collect()
    }

    #[test]
    fn a_plain_executable_is_its_last_path_segment() {
        let shell = identify("/bin/zsh", &argv(&["zsh"]));
        assert_eq!(shell.name, "zsh");
        assert!(!shell.agent);

        let agent = identify("/opt/homebrew/bin/claude", &argv(&["claude"]));
        assert_eq!(agent.name, "claude");
        assert!(agent.agent);
    }

    #[test]
    fn an_alias_resolves_to_the_canonical_spelling() {
        for (launched, shown) in [
            ("claude-code", "claude"),
            ("cursor-agent", "cursor"),
            ("opencode2", "opencode"),
            ("open-code", "opencode"),
            ("amp-local", "amp"),
        ] {
            let program = identify(&format!("/usr/local/bin/{launched}"), &[]);
            assert_eq!(program.name, shown, "{launched} should be shown as {shown}");
            assert!(program.agent);
        }
    }

    #[test]
    fn a_windows_script_suffix_is_stripped_before_the_lookup() {
        // Suffixes are stripped before matching: the Cursor agent runs as `cursor-agent.cmd` on Windows, and the same
        // file name resolves here too. Paths stay Unix-style: this crate never
        // sees a backslash separator on its platforms.
        for (launched, shown) in [
            ("cursor-agent.cmd", "cursor"),
            ("CURSOR-AGENT.CMD", "cursor"),
            ("opencode2.exe", "opencode"),
            ("claude-code.bat", "claude"),
            ("codex.ps1", "codex"),
        ] {
            let program = identify(&format!("/usr/local/bin/{launched}"), &[]);
            assert_eq!(program.name, shown, "{launched} should be shown as {shown}");
            assert!(program.agent);
        }
    }

    #[test]
    fn a_js_suffix_is_not_stripped_into_an_agent() {
        // `.js` goes through the package-name channel, not suffix stripping:
        // a script file that happens to share a stem with an agent is not one.
        let program = identify("/tmp/claude.js", &[]);
        assert!(!program.agent);
    }

    #[test]
    fn every_agent_in_the_table_is_recognised_under_every_spelling() {
        for row in AGENT_ALIASES {
            for alias in row.iter() {
                let program = identify(alias, &[]);
                assert!(program.agent, "{alias} should be an agent");
                assert_eq!(program.name, row[0]);
            }
        }
    }

    #[test]
    fn a_program_outside_the_table_is_not_an_agent() {
        let program = identify("/usr/bin/make", &argv(&["make", "test"]));
        assert_eq!(program.name, "make");
        assert!(!program.agent);
    }

    #[test]
    fn a_runtime_wrapper_is_unwrapped_to_the_program_it_runs() {
        let program = identify(
            "/usr/local/bin/node",
            &argv(&["node", "/home/u/.npm/node_modules/.bin/claude", "--resume"]),
        );
        assert_eq!(program.name, "claude");
        assert!(program.agent);
    }

    #[test]
    fn every_supported_runtime_is_unwrapped() {
        for runtime in ["node", "bun", "deno", "python", "python3", "python3.12"] {
            let program = identify(runtime, &argv(&[runtime, "/opt/tools/bin/aider"]));
            assert_eq!(program.name, "aider", "{runtime} should be seen through");
            assert!(program.agent);
        }
    }

    #[test]
    fn a_runtime_flag_before_the_script_is_stepped_over() {
        let program = identify(
            "/usr/bin/node",
            &argv(&["node", "--enable-source-maps", "/opt/codex/bin/codex"]),
        );
        assert_eq!(program.name, "codex");
    }

    #[test]
    fn an_argv_that_omits_the_runtime_is_unwrapped_the_same_way() {
        let program = identify("/usr/bin/node", &argv(&["/opt/gemini/bin/gemini", "chat"]));
        assert_eq!(program.name, "gemini");
        assert!(program.agent);
    }

    /// The false positive this guards against:
    /// a payload passed to an eval flag is source text, and a program name inside it names
    /// nothing that is running. Reporting `codex` here would put an agent in the index that
    /// a person cannot find, cannot attach to, and did not start.
    #[test]
    fn an_eval_payload_is_never_unwrapped() {
        let python = identify(
            "/usr/bin/python3",
            &argv(&["python3", "-c", "import codex"]),
        );
        assert_eq!(python.name, "python3");
        assert!(!python.agent);

        let node = identify(
            "/usr/bin/node",
            &argv(&["node", "-e", "require('claude-code')"]),
        );
        assert_eq!(node.name, "node");
        assert!(!node.agent);
    }

    #[test]
    fn a_shell_is_never_the_program_named_inside_its_command_string() {
        for shell in ["sh", "bash", "zsh", "fish"] {
            let program = identify(
                &format!("/bin/{shell}"),
                &argv(&[shell, "-c", "sleep 60; /tmp/codex"]),
            );
            assert_eq!(program.name, shell);
            assert!(!program.agent, "{shell} is a shell, not what it launches");
        }
    }

    #[test]
    fn a_package_layout_names_the_package_rather_than_the_file() {
        let program = identify(
            "/usr/bin/node",
            &argv(&["node", "/x/node_modules/@some-scope/some-agent/dist/cli.js"]),
        );
        assert_eq!(program.name, "some-agent");
        assert!(
            !program.agent,
            "a better name to show, never a promotion into the index"
        );
    }

    #[test]
    fn a_package_layout_still_resolves_a_known_agent() {
        let program = identify(
            "/usr/bin/node",
            &argv(&["node", "/x/node_modules/@anthropic-ai/claude-code/cli.js"]),
        );
        assert_eq!(program.name, "claude");
        assert!(program.agent);
    }

    #[test]
    fn a_nested_package_names_the_innermost_one() {
        let program = identify(
            "/usr/bin/node",
            &argv(&["node", "/x/node_modules/host/node_modules/opencode/cli.js"]),
        );
        assert_eq!(program.name, "opencode");
        assert!(program.agent);
    }

    #[test]
    fn a_shim_under_dot_bin_falls_back_to_the_file_name() {
        let program = identify("/x/node_modules/.bin/goose", &[]);
        assert_eq!(program.name, "goose");
        assert!(program.agent);
    }

    #[test]
    fn degenerate_input_answers_without_panicking() {
        assert_eq!(identify("", &[]).name, "");
        assert_eq!(identify("/usr/bin/", &[]).name, "bin");
        assert_eq!(identify("/", &[]).name, "");
        // A runtime with nothing to run is the runtime.
        assert_eq!(identify("/usr/bin/node", &[]).name, "node");
        assert_eq!(identify("/usr/bin/node", &argv(&["node"])).name, "node");
        assert_eq!(
            identify("/usr/bin/node", &argv(&["node", "--version"])).name,
            "node"
        );
    }

    #[test]
    fn an_alias_matches_whatever_case_it_was_launched_under() {
        let program = identify("/Applications/Claude", &[]);
        assert_eq!(program.name, "claude", "shown under the canonical spelling");
        assert!(program.agent);
    }

    #[test]
    fn an_unknown_program_keeps_its_own_spelling() {
        let program = identify("/Users/u/bin/DeployAll", &[]);
        assert_eq!(program.name, "DeployAll");
        assert!(!program.agent);
    }
}
