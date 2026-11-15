mod arith;
mod builtins;
mod editor;
mod expand;
mod fc;
mod hash;
mod jobs;
mod login;
mod plugin_editor;
mod plugin_fs;
mod plugin_host;
mod plugin_lua;
mod plugins;
mod printf;
mod regular;
mod resources;
mod session;
mod shell;
mod syntax;
mod sys;
mod test_builtin;

use std::ffi::OsStr;
use std::fs::File;
use std::io;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::fs::MetadataExt;
use std::path::Path;

use editor::{Editor, Read};
use shell::{Flow, Options, Shell, diagnostic, show};

enum Input {
    Command(Vec<u8>),
    File(Vec<u8>),
    Stdin,
}

struct Invocation {
    input: Input,
    interactive: bool,
    login: bool,
    vanilla: bool,
    posix: bool,
    plugins: bool,
    no_plugins: bool,
    profile: Option<String>,
    options: Options,
    arg0: Vec<u8>,
    args: Vec<Vec<u8>>,
}

fn parse_args(program: Vec<u8>, args: &[Vec<u8>]) -> Result<Invocation, String> {
    let mut login = program.first() == Some(&b'-');
    let mut interactive = false;
    let mut vanilla = false;
    let mut posix = false;
    let mut plugins = false;
    let mut no_plugins = false;
    let mut from_stdin = false;
    let mut command = false;
    let mut profile = None;
    let mut options = Options::default();
    let mut i = 0;
    while let Some(arg) = args.get(i) {
        if arg == b"--" {
            i += 1;
            break;
        }
        if arg == b"-" || arg == b"+" || !arg.starts_with(b"-") && !arg.starts_with(b"+") {
            break;
        }
        match arg.as_slice() {
            b"--vanilla" => vanilla = true,
            b"--posix" => posix = true,
            b"--plugins" => plugins = true,
            b"--no-plugins" => no_plugins = true,
            b"--login" => login = true,
            b"--profile" => {
                i += 1;
                let name = args.get(i).ok_or("--profile requires a name")?;
                profile =
                    Some(String::from_utf8(name.clone()).map_err(|_| "invalid profile name")?);
            }
            _ if arg.starts_with(b"--") => return Err(format!("unknown option {}", show(arg))),
            _ => {
                let enabled = arg[0] == b'-';
                for &opt in &arg[1..] {
                    match opt {
                        b'l' if enabled => login = true,
                        b'i' => interactive = enabled,
                        b's' if enabled => from_stdin = true,
                        b'c' if enabled => {
                            command = true;
                        }
                        b'o' => {
                            i += 1;
                            let name = args.get(i).ok_or("-o requires an option name")?;
                            builtins::named_option(&mut options, name, enabled)?;
                        }
                        _ => builtins::set_letter(&mut options, opt, enabled)?,
                    }
                }
            }
        }
        i += 1;
    }

    let mut operands = args[i..].to_vec();
    let (arg0, input) = if command {
        if operands.is_empty() {
            return Err("-c requires a command string".into());
        }
        let command = operands.remove(0);
        let arg0 = if operands.is_empty() {
            program
        } else {
            operands.remove(0)
        };
        (arg0, Input::Command(command))
    } else if !operands.is_empty() && !from_stdin {
        let script = operands.remove(0);
        (script.clone(), Input::File(script))
    } else {
        interactive |= sys::tty(0) && sys::tty(2);
        (program, Input::Stdin)
    };
    if profile.is_some() && !interactive && !vanilla && !posix {
        return Err("--profile requires an interactive invocation".into());
    }
    if interactive {
        options.noexec = false;
    }
    Ok(Invocation {
        input,
        interactive,
        login,
        vanilla,
        posix,
        plugins,
        no_plugins,
        profile,
        options,
        arg0,
        args: operands,
    })
}

fn read_stdin(line: &mut Vec<u8>) -> io::Result<usize> {
    let start = line.len();
    while let Some(b) = sys::byte(0)? {
        line.push(b);
        if b == b'\n' {
            break;
        }
    }
    Ok(line.len() - start)
}

fn trusted_source(path: &[u8], owner: u32) -> io::Result<File> {
    use std::fs;
    use std::path::Component;
    let path = Path::new(OsStr::from_bytes(path));
    if !path.is_absolute() || path.components().any(|p| matches!(p, Component::ParentDir)) {
        return Err(io::Error::other(
            "startup path must be an absolute path without parent traversal",
        ));
    }
    for dir in path.ancestors().skip(1) {
        let meta = fs::symlink_metadata(dir)?;
        let sticky_root = meta.uid() == 0 && meta.mode() & 0o1000 != 0;
        if !meta.is_dir()
            || !(meta.uid() == 0 || meta.uid() == owner)
            || meta.mode() & 0o022 != 0 && !sticky_root
        {
            return Err(io::Error::other("untrusted startup directory"));
        }
    }
    let file = File::from(sys::open(
        path.as_os_str().as_bytes(),
        libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_NONBLOCK,
    )?);
    let meta = file.metadata()?;
    if !meta.is_file() || meta.uid() != owner || meta.mode() & 0o022 != 0 {
        return Err(io::Error::other("untrusted startup file"));
    }
    Ok(file)
}

fn source_if_present(shell: &mut Shell, path: &[u8], owner: u32) -> Flow {
    match trusted_source(path, owner) {
        Ok(file) => {
            shell.call_depth += 1;
            let flow = shell.run_file_at(file, Path::new(OsStr::from_bytes(path)), false);
            shell.call_depth -= 1;
            match flow {
                Flow::Return(n) => Flow::Status(n),
                other => other,
            }
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => Flow::Status(0),
        Err(e) => shell.error(1, &format!("{}: {e}", show(path)), false),
    }
}

fn apply_startup_configuration(
    shell: &mut Shell,
    scripts: &[session::StartupScript],
) -> Option<Flow> {
    if scripts.is_empty() {
        return None;
    }
    for (path, script) in scripts {
        if let Some((line, command)) = syntax::configuration_fence(script) {
            diagnostic(&format!(
                "{}:{line}: {:?}: Markdown code fence; paste the shell commands without the fence",
                path.display(),
                show(command)
            ));
            diagnostic(&format!(
                "{}: configuration transaction discarded",
                path.display()
            ));
            return None;
        }
    }
    // Empty and comment-only files make no changes. With -v their input is
    // still observable, so keep the usual interpreter path in that mode.
    if !shell.options.verbose && scripts.iter().all(|(_, source)| comment_only(source)) {
        return None;
    }
    let _stdio = match shell::Fds::startup_stdio() {
        Ok(guard) => guard,
        Err(error) => {
            diagnostic(&format!("cannot isolate startup input: {error}"));
            return None;
        }
    };
    let check = match sys::StartupFailure::new() {
        Ok(check) => check,
        Err(error) => {
            diagnostic(&format!("cannot stage configuration: {error}"));
            return None;
        }
    };
    let staged = shell.session.clone();
    let previous = std::mem::replace(&mut shell.session, staged);
    shell.startup_failure = Some(check);
    let mut exit = None;
    let mut rejected = false;
    for (path, script) in scripts {
        shell.reset_source_failure();
        shell.call_depth += 1;
        let flow = shell.code_at(path, script, false);
        shell.call_depth -= 1;
        if flow.code() != 0
            || shell
                .startup_failure
                .as_ref()
                .is_some_and(sys::StartupFailure::failed)
        {
            shell.report_source_failure(path, flow.code());
            rejected = true;
            break;
        }
        if matches!(flow, Flow::Exit(_)) {
            exit = Some(flow);
            break;
        }
    }
    if rejected {
        shell.plugin_pending = None;
    } else if let Err(error) = shell.commit_plugin_config() {
        diagnostic(&error);
        if let Some((path, _)) = scripts.last() {
            shell.report_source_failure(path, 1);
        }
        rejected = true;
        exit = None;
    }
    shell.startup_failure = None;
    shell.reset_source_failure();
    if rejected {
        shell.session = previous;
    }
    exit
}

fn comment_only(source: &[u8]) -> bool {
    source.split(|&byte| byte == b'\n').all(|line| {
        line.iter()
            .find(|&&byte| !matches!(byte, b' ' | b'\t'))
            .is_none_or(|&byte| byte == b'#')
    })
}

fn startups(shell: &mut Shell, login: bool, posix: bool, profile: Option<&str>) -> Option<Flow> {
    if login {
        let flow = source_if_present(shell, b"/etc/profile", 0);
        if !matches!(flow, Flow::Status(_)) {
            return Some(flow);
        }
        if sys::privilege_matches() {
            if let Some(home) = shell.get(b"HOME").filter(|home| home.starts_with(b"/")) {
                let path = [home, b"/.profile"].concat();
                let flow = source_if_present(shell, &path, sys::uid());
                if !matches!(flow, Flow::Status(_)) {
                    return Some(flow);
                }
            }
        }
    }
    if shell.interactive && posix && sys::privilege_matches() {
        if let Some(raw) = shell
            .get(b"ENV")
            .filter(|v| !v.is_empty())
            .map(<[u8]>::to_vec)
        {
            match expand::env_path(shell, &raw) {
                Ok(path) if !path.is_empty() => {
                    let flow = source_if_present(shell, &path, sys::uid());
                    if !matches!(flow, Flow::Status(_)) {
                        return Some(flow);
                    }
                }
                Ok(_) => {}
                Err(e) => diagnostic(&format!("ENV: {e}")),
            }
        }
    }
    if shell.config {
        match session::Session::load_scripts(profile) {
            Ok(scripts) => {
                match scripts.profiles {
                    Ok(profiles) => {
                        if let Some(flow) = apply_startup_configuration(shell, &profiles) {
                            return Some(flow);
                        }
                    }
                    Err(error) => diagnostic(&error),
                }
                match scripts.config {
                    Ok(Some(config)) => {
                        if let Some(flow) = apply_startup_configuration(shell, &[config]) {
                            return Some(flow);
                        }
                    }
                    Err(error) => diagnostic(&error),
                    Ok(None) => {}
                }
            }
            Err(e) => diagnostic(&e),
        }
        if let Err(e) = shell.session.load_history() {
            diagnostic(&e);
        }
    }
    None
}

fn prompt(shell: &Shell, continuation: bool) -> String {
    if continuation {
        return show(shell.get(b"PS2").unwrap_or(b"> ")).into_owned();
    }
    if let Some(ps1) = shell.get(b"PS1") {
        return show(ps1).into_owned();
    }
    let default = if sys::uid() == 0 { "# " } else { "$ " };
    if shell.config {
        shell.session.prompt(default, shell.status)
    } else {
        default.to_owned()
    }
}

fn interactive(shell: &mut Shell) -> Flow {
    let mut editor = Editor::new();
    let mut line = Vec::new();
    loop {
        if let Some(flow) = shell.dispatch_signals() {
            return flow;
        }
        if line.is_empty() {
            shell.run_hooks("before-prompt", &[]);
        }
        let mut rendered = prompt(shell, !line.is_empty());
        if line.is_empty() {
            if let Some(plugins) = shell
                .plugins
                .as_ref()
                .filter(|plugins| plugins.has_prompt())
            {
                if let Some(replacement) =
                    plugins.prompt(&rendered, shell.plugin_context(false), &shell.session)
                {
                    rendered = replacement;
                }
            }
        }
        let input = if let Some(plugins) = shell
            .plugins
            .as_ref()
            .filter(|plugins| plugins.has_editor())
        {
            let context = shell.plugin_context(true);
            let mut bridge = plugins::Bridge::new(plugins, context);
            editor.read_line(&rendered, &shell.session, Some(&mut bridge))
        } else {
            editor.read_line(&rendered, &shell.session, None)
        };
        match input {
            Ok(Read::Text(part)) => {
                line.extend(part);
                if !line.ends_with(b"\n") {
                    line.push(b'\n');
                }
            }
            Ok(Read::Interrupt) => {
                shell.status = 128 + libc::SIGINT;
                line.clear();
                if let Some(handler) = shell
                    .traps
                    .get(&libc::SIGINT)
                    .filter(|action| !action.is_empty())
                    .cloned()
                {
                    let flow = shell.invoke_trap(&handler);
                    if !matches!(flow, Flow::Status(_)) {
                        return flow;
                    }
                }
                continue;
            }
            Ok(Read::Eof) => {
                if !line.is_empty() {
                    diagnostic("unexpected EOF in command");
                    return Flow::Status(2);
                }
                return Flow::Status(shell.status);
            }
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return shell.error(1, &e.to_string(), false),
        }
        match syntax::parse_next(&line, &shell.aliases) {
            Err(e) if e.incomplete => continue,
            Err(e) => {
                diagnostic(&e.message);
                shell.status = 2;
            }
            Ok((list, _)) if list.is_empty() => {}
            Ok(_) => {
                let command = line.strip_suffix(b"\n").unwrap_or(&line).to_vec();
                let before = expand::cwd();
                shell.run_hooks("pre-command", std::slice::from_ref(&command));
                shell.suppress_history = false;
                let flow = shell.code(&line, false);
                if !shell.suppress_history {
                    if let Err(error) = shell.session.record(&command) {
                        diagnostic(&error);
                    }
                }
                let after = expand::cwd();
                shell.status = flow.code();
                if before != after {
                    shell.run_hooks("directory-changed", &[after, before]);
                }
                shell.run_hooks(
                    "post-command",
                    &[command, flow.code().to_string().into_bytes()],
                );
                if !matches!(flow, Flow::Status(_)) {
                    return flow;
                }
            }
        }
        line.clear();
    }
}

fn settings_cli(args: &[Vec<u8>]) -> Option<i32> {
    let [name, rest @ ..] = args else { return None };
    if name == b"plugin" {
        return Some(plugin_host::cli(rest));
    }
    let file_command = match name.as_slice() {
        b"config" => rest
            .first()
            .is_some_and(|a| matches!(a.as_slice(), b"list" | b"describe" | b"check")),
        b"profile" => rest
            .first()
            .is_some_and(|a| matches!(a.as_slice(), b"list" | b"create" | b"use" | b"delete")),
        b"set" | b"get" | b"unset" | b"bind" | b"unbind" | b"bindings" | b"module" | b"hook" => {
            false
        }
        _ => return None,
    };
    if file_command && sys::privilege_matches() {
        let mut shell = Shell::new(
            b"nsh".to_vec(),
            Vec::new(),
            Options::default(),
            false,
            true,
            true,
        );
        Some(shell.setting(args, false).code())
    } else {
        diagnostic("this setting command requires an interactive NSH session");
        Some(2)
    }
}

fn run() -> i32 {
    let mut raw = std::env::args_os().map(OsStringExt::into_vec);
    let program = raw.next().unwrap_or_else(|| b"nsh".to_vec());
    let args = raw.collect::<Vec<_>>();
    if let [mode, path] = args.as_slice() {
        if mode == b"--plugin-fs" {
            return if sys::privilege_matches() {
                plugin_fs::worker(Path::new(OsStr::from_bytes(path)))
            } else {
                1
            };
        }
    }
    if args.first().is_some_and(|a| a == b"--set-default") {
        return match args.as_slice() {
            [_] => login::run(false),
            [_, opt] if opt == b"--dry-run" => login::run(true),
            _ => {
                diagnostic("usage: nsh --set-default [--dry-run]");
                2
            }
        };
    }
    if args.len() == 1 && args[0] == b"--version" {
        println!("NSH (Neo SH) {}", env!("CARGO_PKG_VERSION"));
        return 0;
    }
    if args.len() == 1 && args[0] == b"--help" {
        print!(
            "NSH — Neo SH. Just Vanilla SH.\n\
                Usage: nsh [-ilns] [--posix|--vanilla] [--profile NAME] [SCRIPT [ARG ...]]\n\
                       nsh [-iln] [--posix|--vanilla] -c COMMAND [NAME [ARG ...]]\n\
                       nsh --plugins | --no-plugins\n\
                       nsh plugin list|enable|disable|info|doctor\n\
                       nsh --set-default [--dry-run]\n\
                       nsh --help | --version\n\
                See nsh(1) for configuration, profiles and login shell rollback.\n"
        );
        return 0;
    }
    if let Some(status) = settings_cli(&args) {
        return status;
    }
    let Invocation {
        input,
        interactive: is_interactive,
        login,
        vanilla,
        posix,
        plugins,
        no_plugins,
        profile,
        options,
        arg0,
        args,
    } = match parse_args(program, &args) {
        Ok(invocation) => invocation,
        Err(error) => {
            diagnostic(&error);
            return 2;
        }
    };
    // POSIX locale selection. The empty C string asks libc to read its locale variables.
    unsafe {
        libc::setlocale(libc::LC_ALL, c"".as_ptr());
    }
    let config =
        is_interactive && !vanilla && !posix && sys::privilege_matches() && !options.noexec;
    let mut shell = Shell::new(arg0, args, options, is_interactive, config, config);
    shell.plugin_allowed = (config || plugins)
        && !no_plugins
        && !vanilla
        && !posix
        && !shell.options.noexec
        && sys::privilege_matches();
    if is_interactive {
        if let Err(e) = shell.jobs.interactive() {
            diagnostic(&format!("job control: {e}"));
        }
        let _ = sys::catch(libc::SIGHUP);
    }
    shell.load_saved_plugins();
    let startup = if !vanilla && !shell.options.noexec {
        startups(&mut shell, login, posix, profile.as_deref())
    } else {
        None
    };
    let flow = startup.unwrap_or_else(|| {
        shell.status = 0;
        shell.run_hooks("startup", &[]);
        match input {
            Input::Command(source) => shell.code(&source, false),
            Input::File(path) => match File::open(OsStr::from_bytes(&path)) {
                Ok(file) => shell.run_file(file, false),
                Err(e) => shell.error(127, &format!("{}: {e}", show(&path)), false),
            },
            Input::Stdin if is_interactive => interactive(&mut shell),
            Input::Stdin => shell.input(read_stdin),
        }
    });
    shell.status = flow.code();
    shell.run_hooks("exit", &[flow.code().to_string().into_bytes()]);
    shell.shutdown_plugins();
    shell.terminate_jobs();
    shell.exit_trap(flow.code())
}

fn main() -> std::process::ExitCode {
    std::process::ExitCode::from(run() as u8)
}
