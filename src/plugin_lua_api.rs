use std::collections::BTreeMap;
use std::ffi::{CString, OsStr};
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, Instant};

use mlua::{
    FromLuaMulti, Function, IntoLuaMulti, Lua, String as LuaString, Table, Value, Variadic,
};
use unicode_segmentation::UnicodeSegmentation;

use super::{
    CallContext, CommandMode, Effect, MAX_EDITOR_TEXT, MAX_ITEMS, MEMORY_LIMIT, Phase, Provider,
    State, error,
};
use crate::plugin_editor::{
    EditorColor, EditorHighlight, EditorMenu, EditorMenuItem, EditorStyle, EditorVirtualText,
};
use crate::session::TerminalColors;
use crate::sys;

pub(super) fn install(lua: &Lua, state: &Rc<State>) -> mlua::Result<()> {
    let api = lua.create_table()?;
    api.set("api_version", 1)?;
    api.set("version", env!("CARGO_PKG_VERSION"))?;
    install_options(lua, state, &api)?;
    install_commands(lua, state, &api)?;
    install_shell(lua, state, &api)?;
    install_output(lua, state, &api)?;
    install_editor(lua, state, &api)?;
    install_providers(lua, state, &api)?;
    install_fs(lua, state, &api)?;
    bind(
        lua,
        state,
        &api,
        "on",
        |lua, state, (name, callback): (String, Function)| {
            if !matches!(
                name.as_str(),
                "load"
                    | "enable"
                    | "disable"
                    | "reload"
                    | "unload"
                    | "error"
                    | "shutdown"
                    | "startup"
                    | "before-prompt"
                    | "pre-command"
                    | "post-command"
                    | "before-dispatch"
                    | "after-dispatch"
                    | "directory-changed"
                    | "exit"
            ) {
                return Err(error(format!("unknown plugin event '{name}'")));
            }
            let key = lua.create_registry_value(callback)?;
            state.register(|handlers| {
                handlers.events.entry(name).or_default().push(key);
                Ok(())
            })
        },
    )?;
    let events = lua.create_table()?;
    events.set("on", api.get::<Function>("on")?)?;
    api.set("events", events)?;

    let globals = lua.globals();
    for name in [
        "dofile",
        "loadfile",
        "load",
        "require",
        "package",
        "collectgarbage",
        "pcall",
        "xpcall",
        "warn",
    ] {
        globals.set(name, Value::Nil)?;
    }
    globals.set("print", api.get::<Function>("print")?)?;
    globals.set("nsh", api)?;
    Ok(())
}

fn bind<A, R, F>(
    lua: &Lua,
    state: &Rc<State>,
    table: &Table,
    name: &str,
    callback: F,
) -> mlua::Result<()>
where
    A: FromLuaMulti,
    R: IntoLuaMulti,
    F: Fn(&Lua, &State, A) -> mlua::Result<R> + 'static,
{
    let state = Rc::clone(state);
    table.set(
        name,
        lua.create_function(move |lua, args| callback(lua, &state, args))?,
    )
}

fn install_options(lua: &Lua, state: &Rc<State>, api: &Table) -> mlua::Result<()> {
    bind(lua, state, api, "require", |lua, state, name: String| {
        state.require(lua, name)
    })?;
    let config = lua.create_table()?;
    bind(
        lua,
        state,
        &config,
        "get",
        |lua, state, (key, fallback): (Option<String>, Value)| {
            const DEPTH: usize = 64;
            match key {
                Some(key) => state
                    .options
                    .get(&key)
                    .map_or(Ok(fallback), |value| config_value(lua, value, DEPTH)),
                None => config_table(lua, &state.options, DEPTH).map(Value::Table),
            }
        },
    )?;
    api.set("config", config)
}

fn config_table(lua: &Lua, values: &toml::Table, depth: usize) -> mlua::Result<Table> {
    let table = lua.create_table_with_capacity(0, values.len())?;
    for (key, value) in values {
        table.raw_set(key.as_str(), config_value(lua, value, depth)?)?;
    }
    Ok(table)
}

fn config_value(lua: &Lua, value: &toml::Value, depth: usize) -> mlua::Result<Value> {
    if depth == 0 {
        return Err(error("plugin options exceed the nesting limit"));
    }
    match value {
        toml::Value::String(value) => lua.create_string(value).map(Value::String),
        toml::Value::Integer(value) => Ok(Value::Integer(*value)),
        toml::Value::Float(value) => Ok(Value::Number(*value)),
        toml::Value::Boolean(value) => Ok(Value::Boolean(*value)),
        toml::Value::Datetime(value) => lua.create_string(value.to_string()).map(Value::String),
        toml::Value::Array(values) => {
            let array = lua.create_table_with_capacity(values.len(), 0)?;
            for (index, value) in values.iter().enumerate() {
                array.raw_set(index + 1, config_value(lua, value, depth - 1)?)?;
            }
            Ok(Value::Table(array))
        }
        toml::Value::Table(values) => config_table(lua, values, depth - 1).map(Value::Table),
    }
}

fn install_commands(lua: &Lua, state: &Rc<State>, api: &Table) -> mlua::Result<()> {
    let commands = lua.create_table()?;
    for (method, mode) in [
        ("register", CommandMode::Register),
        ("replace", CommandMode::Replace),
        ("wrap", CommandMode::Wrap),
    ] {
        bind(
            lua,
            state,
            &commands,
            method,
            move |lua, state, (name, callback): (LuaString, Function)| {
                let name = name.as_bytes().to_vec();
                command_name(&name)?;
                if mode != CommandMode::Register {
                    state.allow("override")?;
                }
                let key = lua.create_registry_value(callback)?;
                state.register(|handlers| {
                    if handlers.commands.contains_key(&name) {
                        return Err(error(format!(
                            "duplicate Lua command '{}'",
                            String::from_utf8_lossy(&name)
                        )));
                    }
                    handlers.commands.insert(name, (mode, key));
                    Ok(())
                })
            },
        )?;
    }
    bind(
        lua,
        state,
        &commands,
        "unregister",
        |_, state, name: LuaString| {
            let name = name.as_bytes();
            state.call(|call| {
                if matches!(call.phase, Phase::ReadOnly) {
                    return Err(error(
                        "unregister is unavailable during editor or provider callback",
                    ));
                }
                Ok(())
            })?;
            Ok(state
                .registrations
                .borrow_mut()
                .commands
                .remove(name.as_ref())
                .is_some())
        },
    )?;
    bind(
        lua,
        state,
        &commands,
        "exists",
        |_, state, name: LuaString| {
            let name = name.as_bytes();
            if name.is_empty() || name.contains(&0) {
                return Ok(false);
            }
            if name.as_ref() == b"nsh"
                || crate::builtins::builtin(&name)
                || state
                    .registrations
                    .borrow()
                    .commands
                    .contains_key(name.as_ref())
            {
                return Ok(true);
            }
            state.call(|call| {
                if call
                    .shell
                    .commands
                    .iter()
                    .any(|command| command == name.as_ref())
                {
                    return Ok(true);
                }
                let path = call
                    .shell
                    .vars
                    .get(b"PATH".as_slice())
                    .map(Vec::as_slice)
                    .unwrap_or(b"/usr/bin:/bin");
                Ok(state
                    .command_cache
                    .borrow_mut()
                    .contains(&name, path, &call.shell.cwd))
            })
        },
    )?;
    api.set("command", commands)
}

fn command_name(name: &[u8]) -> mlua::Result<()> {
    if name.is_empty()
        || name.len() > 128
        || name.contains(&b'/')
        || name
            .iter()
            .any(|byte| byte.is_ascii_whitespace() || byte.is_ascii_control())
    {
        return Err(error("invalid Lua command name"));
    }
    Ok(())
}

#[derive(Default)]
pub(super) struct CommandCache {
    path: Vec<u8>,
    cwd: PathBuf,
    entries: BTreeMap<Vec<u8>, (Instant, bool)>,
}

impl CommandCache {
    fn contains(&mut self, name: &[u8], path: &[u8], cwd: &Path) -> bool {
        if path != self.path || cwd != self.cwd {
            self.path = path.to_vec();
            self.cwd = cwd.to_owned();
            self.entries.clear();
        }
        let now = Instant::now();
        if let Some((checked, result)) = self.entries.get(name) {
            if now.duration_since(*checked) < Duration::from_secs(2) {
                return *result;
            }
        }
        if self.entries.len() >= 256 {
            self.entries.clear();
        }
        let exists = |path: &Path| {
            sys::access(path.as_os_str().as_bytes(), libc::X_OK)
                && std::fs::metadata(path).is_ok_and(|metadata| metadata.is_file())
        };
        let name_path = Path::new(OsStr::from_bytes(name));
        let result = if name.contains(&b'/') {
            exists(&cwd.join(name_path))
        } else {
            path.split(|&byte| byte == b':')
                .any(|directory| exists(&cwd.join(OsStr::from_bytes(directory)).join(name_path)))
        };
        self.entries.insert(name.to_vec(), (now, result));
        result
    }
}

fn install_shell(lua: &Lua, state: &Rc<State>, api: &Table) -> mlua::Result<()> {
    let shell = lua.create_table()?;
    let env = lua.create_table()?;
    for (table, environment) in [(&shell, false), (&env, true)] {
        bind(
            lua,
            state,
            table,
            "get",
            move |lua, state, name: LuaString| {
                state.call(|call| {
                    let vars = if environment {
                        &call.shell.env
                    } else {
                        &call.shell.vars
                    };
                    vars.get(name.as_bytes().as_ref())
                        .map(|value| lua.create_string(value))
                        .transpose()
                })
            },
        )?;
        bind(
            lua,
            state,
            table,
            "set",
            move |_, state, (name, value): (LuaString, Option<LuaString>)| {
                state.allow("shell-state")?;
                let name = name.as_bytes().to_vec();
                if !valid_name(&name) {
                    return Err(error("invalid shell variable name"));
                }
                let value = value.map(|value| value.as_bytes().to_vec());
                if value.as_ref().is_some_and(|value| value.contains(&0)) {
                    return Err(error("shell variable contains NUL"));
                }
                state.call(|call| {
                    call.effect(if environment {
                        Effect::SetEnv(name.clone(), value.clone())
                    } else {
                        Effect::SetVar(name.clone(), value.clone())
                    })?;
                    update_var(Rc::make_mut(&mut call.shell.vars), &name, &value);
                    if environment || call.shell.env.contains_key(name.as_slice()) {
                        update_var(Rc::make_mut(&mut call.shell.env), &name, &value);
                    }
                    Ok(())
                })
            },
        )?;
    }
    bind(lua, state, &shell, "cwd", |lua, state, ()| {
        state.call(|call| lua.create_string(call.shell.cwd.as_os_str().as_bytes()))
    })?;
    bind(lua, state, &shell, "status", |_, state, ()| {
        state.call(|call| Ok(call.shell.status))
    })?;
    bind(lua, state, &shell, "cd", |_, state, path: LuaString| {
        state.allow("shell-state")?;
        state.call(|call| {
            call.io()?;
            let bytes = path.as_bytes();
            let path = call.shell.cwd.join(OsStr::from_bytes(&bytes));
            let destination = std::fs::canonicalize(&path).map_err(mlua::Error::external)?;
            if !destination.is_dir() {
                return Err(error(format!("{}: not a directory", path.display())));
            }
            call.effect(Effect::Cd(destination.clone()))?;
            let old = Some(call.shell.cwd.as_os_str().as_bytes().to_vec());
            let new = Some(destination.as_os_str().as_bytes().to_vec());
            for table in [&mut call.shell.vars, &mut call.shell.env] {
                let table = Rc::make_mut(table);
                update_var(table, b"OLDPWD", &old);
                update_var(table, b"PWD", &new);
            }
            call.shell.cwd = destination;
            Ok(())
        })
    })?;
    let process = lua.create_table()?;
    bind(
        lua,
        state,
        &process,
        "run",
        |_, state, (program, args): (LuaString, Option<Table>)| {
            state.allow("process")?;
            let name = program.as_bytes();
            let args = args.map(byte_list).transpose()?.unwrap_or_default();
            check_program(&name, &args)?;
            state.call(|call| {
                call.io()?;
                run_process(&name, &args, &call.shell).map_err(mlua::Error::external)
            })
        },
    )?;
    bind(
        lua,
        state,
        &process,
        "exec",
        |_, state, (program, args): (LuaString, Option<Table>)| {
            state.allow("exec")?;
            let name = program.as_bytes().to_vec();
            let args = args.map(byte_list).transpose()?.unwrap_or_default();
            check_program(&name, &args)?;
            let command = std::iter::once(name).chain(args).collect();
            state.call(|call| call.effect(Effect::Exec(command)))
        },
    )?;
    api.set("process", process)?;
    api.set("shell", shell)?;
    api.set("env", env)
}

fn valid_name(name: &[u8]) -> bool {
    name.first()
        .is_some_and(|byte| byte.is_ascii_alphabetic() || *byte == b'_')
        && name
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || *byte == b'_')
}

fn check_program(name: &[u8], args: &[Vec<u8>]) -> mlua::Result<()> {
    if name.is_empty() || name.contains(&0) || args.iter().any(|arg| arg.contains(&0)) {
        return Err(error(
            "process requires a command name and arguments without NUL",
        ));
    }
    Ok(())
}

fn run_process(name: &[u8], args: &[Vec<u8>], shell: &CallContext) -> io::Result<i32> {
    let path = shell
        .vars
        .get(b"PATH".as_slice())
        .map_or(b"/usr/bin:/bin".as_slice(), Vec::as_slice);
    let program = Path::new(OsStr::from_bytes(name));
    let candidates = if name.contains(&b'/') {
        vec![shell.cwd.join(program)]
    } else {
        path.split(|&byte| byte == b':')
            .map(|directory| shell.cwd.join(OsStr::from_bytes(directory)).join(program))
            .collect()
    };
    let argv = std::iter::once(name)
        .chain(args.iter().map(Vec::as_slice))
        .map(CString::new)
        .collect::<Result<Vec<_>, _>>()?;
    let env = shell
        .env
        .iter()
        .map(|(name, value)| CString::new([name.as_slice(), b"=", value.as_slice()].concat()))
        .collect::<Result<Vec<_>, _>>()?;
    let candidates = candidates
        .into_iter()
        .map(|path| CString::new(path.as_os_str().as_bytes()))
        .collect::<Result<Vec<_>, _>>()?;
    let args = sys::ExecArgs::new(&argv, &env);
    let (pid, mask) = sys::fork()?;
    if pid == 0 {
        let _ = sys::signal(libc::SIGPIPE, libc::SIG_DFL);
        drop(mask);
        if std::env::set_current_dir(&shell.cwd).is_err() {
            sys::exit(126);
        }
        let mut denied = false;
        for candidate in &candidates {
            match args.exec(candidate).raw_os_error() {
                Some(libc::EACCES) => denied = true,
                Some(libc::ENOENT | libc::ENOTDIR) => {}
                _ => sys::exit(126),
            }
        }
        sys::exit(if denied { 126 } else { 127 });
    }
    drop(mask);
    loop {
        let mut status = 0;
        // The fork returned a positive, owned child PID. The status pointer
        // stays valid for this blocking wait and unrelated jobs aren't reaped.
        if unsafe { libc::waitpid(pid, &mut status, 0) } == pid {
            return Ok(if libc::WIFEXITED(status) {
                libc::WEXITSTATUS(status)
            } else {
                128 + libc::WTERMSIG(status)
            });
        }
        let cause = io::Error::last_os_error();
        if cause.kind() != io::ErrorKind::Interrupted {
            return Err(cause);
        }
    }
}

fn update_var(vars: &mut BTreeMap<Vec<u8>, Vec<u8>>, name: &[u8], value: &Option<Vec<u8>>) {
    if let Some(value) = value {
        vars.insert(name.to_vec(), value.clone());
    } else {
        vars.remove(name);
    }
}

fn install_output(lua: &Lua, state: &Rc<State>, api: &Table) -> mlua::Result<()> {
    bind(
        lua,
        state,
        api,
        "print",
        |lua, state, values: Variadic<Value>| {
            state.call(|call| call.io())?;
            let mut output = Vec::new();
            for (index, value) in values.into_iter().enumerate() {
                if index != 0 {
                    output.push(b'\t');
                }
                match value {
                    Value::Boolean(true) => output.extend_from_slice(b"true"),
                    Value::Boolean(false) => output.extend_from_slice(b"false"),
                    Value::Nil => output.extend_from_slice(b"nil"),
                    value => {
                        let string = lua
                            .coerce_string(value)?
                            .ok_or_else(|| error("print requires primitive values"))?;
                        output.extend_from_slice(&string.as_bytes());
                    }
                }
            }
            output.push(b'\n');
            sys::write(1, &output).map_err(mlua::Error::external)
        },
    )?;
    for (stream, fd) in [("stdout", 1), ("stderr", 2)] {
        let handle = lua.create_table()?;
        bind(
            lua,
            state,
            &handle,
            "write",
            move |_, state, values: Variadic<LuaString>| {
                state.allow("stdio")?;
                state.call(|call| call.io())?;
                for value in values {
                    sys::write(fd, &value.as_bytes()).map_err(mlua::Error::external)?;
                }
                Ok(())
            },
        )?;
        api.set(stream, handle)?;
    }
    let stdin = lua.create_table()?;
    bind(lua, state, &stdin, "read", |lua, state, count: i64| {
        state.allow("stdio")?;
        state.call(|call| call.io())?;
        if !(1..=65_536).contains(&count) {
            return Err(error("stdin read size must be between 1 and 65536"));
        }
        let mut bytes = vec![0; count as usize];
        let read = sys::read(0, &mut bytes).map_err(mlua::Error::external)?;
        if read == 0 {
            Ok(None)
        } else {
            lua.create_string(&bytes[..read]).map(Some)
        }
    })?;
    bind(lua, state, &stdin, "read_line", |lua, state, ()| {
        state.allow("stdio")?;
        state.call(|call| call.io())?;
        let mut line = Vec::new();
        loop {
            match sys::byte(0).map_err(mlua::Error::external)? {
                Some(b'\n') => break,
                Some(byte) if line.len() < 64 * 1024 => line.push(byte),
                Some(_) => return Err(error("plugin stdin line exceeds 64 KiB")),
                None if line.is_empty() => return Ok(None),
                None => break,
            }
        }
        lua.create_string(&line).map(Some)
    })?;
    api.set("stdin", stdin)
}

fn install_editor(lua: &Lua, state: &Rc<State>, api: &Table) -> mlua::Result<()> {
    install_styles(lua, state, api)?;
    let editor = lua.create_table()?;
    for (name, tick) in [("on_change", false), ("on_tick", true)] {
        bind(
            lua,
            state,
            &editor,
            name,
            move |lua, state, callback: Function| {
                let key = lua.create_registry_value(callback)?;
                state.register(|handlers| {
                    if tick {
                        &mut handlers.tick
                    } else {
                        &mut handlers.change
                    }
                    .push(key);
                    Ok(())
                })
            },
        )?;
    }
    bind(lua, state, &editor, "line", |_, state, ()| {
        state.edit(|edit| Ok(edit.line.clone()))
    })?;
    bind(lua, state, &editor, "cursor", |_, state, ()| {
        state.edit(|edit| Ok(edit.cursor))
    })?;
    bind(
        lua,
        state,
        &editor,
        "set_line",
        |_, state, (line, cursor): (LuaString, Option<usize>)| {
            if line.as_bytes().len() > MAX_EDITOR_TEXT {
                return Err(error("editor line exceeds 32 KiB"));
            }
            let line = line.to_str()?;
            let cursor = cursor.unwrap_or(line.len());
            if !boundary(&line, cursor) {
                return Err(error("editor cursor must align with a grapheme boundary"));
            }
            state.edit(|edit| {
                edit.line = line.to_owned();
                edit.cursor = cursor;
                edit.changed = true;
                Ok(())
            })
        },
    )?;
    bind(
        lua,
        state,
        &editor,
        "replace",
        |_, state, (start, end, text): (usize, usize, LuaString)| {
            state.edit(|edit| {
                if start > end || !boundary(&edit.line, start) || !boundary(&edit.line, end) {
                    return Err(error(
                        "editor replacement must align with grapheme boundaries",
                    ));
                }
                if edit.line.len() - (end - start) + text.as_bytes().len() > MAX_EDITOR_TEXT {
                    return Err(error("editor replacement exceeds 32 KiB"));
                }
                let text = text.to_str()?;
                edit.line.replace_range(start..end, &text);
                edit.cursor = if edit.cursor <= start {
                    edit.cursor
                } else if edit.cursor >= end {
                    edit.cursor - (end - start) + text.len()
                } else {
                    start + text.len()
                };
                if !boundary(&edit.line, edit.cursor) {
                    edit.cursor = edit
                        .line
                        .grapheme_indices(true)
                        .map(|(index, _)| index)
                        .take_while(|&index| index < edit.cursor)
                        .last()
                        .unwrap_or(0);
                }
                edit.changed = true;
                Ok(())
            })
        },
    )?;
    bind(
        lua,
        state,
        &editor,
        "set_cursor",
        |_, state, cursor: usize| {
            state.edit(|edit| {
                if !boundary(&edit.line, cursor) {
                    return Err(error("editor cursor must align with a grapheme boundary"));
                }
                edit.cursor = cursor;
                edit.cursor_changed = true;
                Ok(())
            })
        },
    )?;
    bind(
        lua,
        state,
        &editor,
        "set_ghost_text",
        |lua, state, (text, style): (Option<LuaString>, Value)| {
            let text = text
                .map(|text| {
                    if text.as_bytes().len() > MAX_EDITOR_TEXT {
                        return Err(error("editor ghost text exceeds 32 KiB"));
                    }
                    Ok::<_, mlua::Error>(text.to_str()?.to_owned())
                })
                .transpose()?;
            let style = parse_style(lua, style)?;
            state.edit(|edit| {
                edit.render.ghost = text;
                edit.render.ghost_style = style;
                edit.render_changed = true;
                Ok(())
            })
        },
    )?;
    bind(
        lua,
        state,
        &editor,
        "set_virtual_text",
        |lua, state, entries: Table| {
            let values = virtual_text(lua, entries)?;
            state.edit(|edit| {
                edit.render.virtual_text = values;
                edit.render_changed = true;
                Ok(())
            })
        },
    )?;
    bind(
        lua,
        state,
        &editor,
        "set_highlights",
        |lua, state, entries: Table| {
            let values = highlights(lua, entries)?;
            state.edit(|edit| {
                edit.render.highlights = Some(values);
                edit.render_changed = true;
                Ok(())
            })
        },
    )?;
    bind(
        lua,
        state,
        &editor,
        "set_menu",
        |lua, state, (entries, options): (Option<Table>, Option<Table>)| {
            let values = entries
                .map(|rows| menu(lua, rows, options.as_ref()))
                .transpose()?;
            state.edit(|edit| {
                edit.render.menu = values;
                edit.render_changed = true;
                Ok(())
            })
        },
    )?;
    api.set("editor", editor)?;

    let keys = lua.create_table()?;
    bind(
        lua,
        state,
        &keys,
        "bind",
        |lua, state, (name, callback): (String, Function)| {
            if name.is_empty() || name.len() > 32 || name.chars().any(char::is_control) {
                return Err(error("invalid key label"));
            }
            let key = lua.create_registry_value(callback)?;
            state.register(|handlers| {
                if handlers.keys.contains_key(&name) {
                    return Err(error(format!("duplicate binding '{name}'")));
                }
                handlers.keys.insert(name, key);
                Ok(())
            })
        },
    )?;
    keys.set("wrap", keys.get::<Function>("bind")?)?;
    api.set("keys", keys)
}

const STYLE_PALETTE: &str = "nsh.editor.style_palette";

struct StoredStyle(EditorStyle);

impl mlua::UserData for StoredStyle {}

fn install_styles(lua: &Lua, state: &Rc<State>, api: &Table) -> mlua::Result<()> {
    lua.set_named_registry_value(STYLE_PALETTE, lua.create_table()?)?;
    let styles = lua.create_table()?;
    bind(
        lua,
        state,
        &styles,
        "define",
        |lua, _, (name, spec): (String, Value)| {
            if name.is_empty()
                || name.len() > 64
                || name == "dim"
                || !name
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b"-_.".contains(&byte))
            {
                return Err(error(
                    "invalid style name; use up to 64 ASCII letters, digits, '-', '_' or '.'",
                ));
            }
            let style =
                parse_style(lua, spec)?.ok_or_else(|| error("style specification required"))?;
            let palette = lua.named_registry_value::<Table>(STYLE_PALETTE)?;
            if !palette.contains_key(&*name)?
                && palette.pairs::<Value, Value>().take(MAX_ITEMS).count() == MAX_ITEMS
            {
                return Err(error("style palette exceeds 512 names"));
            }
            palette.raw_set(&*name, lua.create_userdata(StoredStyle(style))?)?;
            Ok(())
        },
    )?;
    bind(lua, state, &styles, "defined", |lua, _, name: String| {
        lua.named_registry_value::<Table>(STYLE_PALETTE)?
            .contains_key(&*name)
    })?;
    bind(lua, state, &styles, "remove", |lua, _, name: String| {
        let palette = lua.named_registry_value::<Table>(STYLE_PALETTE)?;
        let found = palette.contains_key(&*name)?;
        palette.raw_set(&*name, Value::Nil)?;
        Ok(found)
    })?;
    api.set("style", styles)
}

fn parse_style(lua: &Lua, spec: Value) -> mlua::Result<Option<EditorStyle>> {
    let style = match spec {
        Value::Nil => return Ok(None),
        Value::String(value) => {
            if value.as_bytes().len() > 64 {
                return Err(error("style name exceeds 64 bytes"));
            }
            let value = value.to_str()?;
            if value == "dim" {
                EditorStyle::ghost()
            } else if let Some(fg) = EditorColor::hex(&value) {
                EditorStyle {
                    fg: Some(fg),
                    ..EditorStyle::default()
                }
            } else {
                let palette = lua.named_registry_value::<Table>(STYLE_PALETTE)?;
                let stored: Option<mlua::AnyUserData> = palette.raw_get(&*value)?;
                stored
                    .ok_or_else(|| error(format!("unknown style '{}'", value.escape_debug())))?
                    .borrow::<StoredStyle>()?
                    .0
            }
        }
        Value::Table(fields) => {
            let color = |key: &str| {
                fields
                    .raw_get::<Option<LuaString>>(key)?
                    .map(|color| {
                        let invalid = || error(format!("style {key} must be #RRGGBB"));
                        if color.as_bytes().len() != 7 {
                            return Err(invalid());
                        }
                        EditorColor::hex(&color.to_str()?).ok_or_else(invalid)
                    })
                    .transpose()
            };
            let flag = |key: &str| match fields.raw_get::<Value>(key)? {
                Value::Nil => Ok(false),
                Value::Boolean(value) => Ok(value),
                _ => Err(error(format!("style {key} must be a boolean"))),
            };
            for field in fields.clone().pairs::<LuaString, Value>() {
                let (field, _) = field?;
                if field.as_bytes().len() > 9 {
                    return Err(error("unknown style field"));
                }
                let field = field.to_str()?;
                if !matches!(
                    &*field,
                    "fg" | "bg" | "bold" | "dim" | "italic" | "underline" | "inverse"
                ) {
                    return Err(error(format!(
                        "unknown style field '{}'",
                        field.escape_debug()
                    )));
                }
            }
            EditorStyle {
                fg: color("fg")?,
                bg: color("bg")?,
                bold: flag("bold")?,
                dim: flag("dim")?,
                italic: flag("italic")?,
                underline: flag("underline")?,
                inverse: flag("inverse")?,
            }
        }
        _ => return Err(error("style must be a color, a named style, or a table")),
    };
    Ok(Some(style))
}

fn install_providers(lua: &Lua, state: &Rc<State>, api: &Table) -> mlua::Result<()> {
    for (name, provider) in [
        ("prompt", Provider::Prompt),
        ("history", Provider::History),
        ("highlight", Provider::Highlight),
        ("completion", Provider::Completion),
    ] {
        let table = lua.create_table()?;
        bind(
            lua,
            state,
            &table,
            "register",
            move |lua, state, callback: Function| {
                let key = lua.create_registry_value(callback)?;
                state.register(|handlers| {
                    if handlers.providers.contains_key(&provider) {
                        return Err(error(format!("duplicate {name} provider")));
                    }
                    handlers.providers.insert(provider, key);
                    Ok(())
                })
            },
        )?;
        if provider == Provider::History {
            bind(lua, state, &table, "entries", |lua, state, ()| {
                state.call(|call| string_table(lua, &call.shell.history))
            })?;
        }
        api.set(name, table)?;
    }
    Ok(())
}

fn install_fs(lua: &Lua, state: &Rc<State>, api: &Table) -> mlua::Result<()> {
    let fs = lua.create_table()?;
    let scan = |lua: &Lua, state: &State, (path, prefix): (LuaString, Option<LuaString>)| {
        state.allow("fs")?;
        let path =
            state.call(|call| Ok(call.shell.cwd.join(OsStr::from_bytes(&path.as_bytes()))))?;
        let mut worker = state.fs.borrow_mut();
        let entries = match prefix {
            Some(prefix) => worker.request_matching(&path, &prefix.as_bytes()),
            None => worker.request(&path),
        };
        drop(worker);
        let entries = match entries {
            Ok(Some(entries)) => entries,
            Ok(None) => return Ok((None, None)),
            Err(cause) => return Ok((None, Some(cause.to_string()))),
        };
        let ret = lua.create_table_with_capacity(entries.len(), 0)?;
        for (index, entry) in entries.into_iter().enumerate() {
            let row = lua.create_table()?;
            row.set("name", lua.create_string(entry.name)?)?;
            row.set("is_dir", entry.is_dir)?;
            ret.raw_set(index + 1, row)?;
        }
        Ok((Some(ret), None))
    };
    bind(lua, state, &fs, "read_dir", scan)?;
    fs.set("request", fs.get::<Function>("read_dir")?)?;
    api.set("fs", fs)
}

fn boundary(text: &str, offset: usize) -> bool {
    offset == text.len()
        || text
            .grapheme_indices(true)
            .any(|(start, _)| start == offset)
}

pub(super) fn string_table(lua: &Lua, values: &[Vec<u8>]) -> mlua::Result<Table> {
    let table = lua.create_table_with_capacity(values.len(), 0)?;
    for (index, value) in values.iter().enumerate() {
        table.raw_set(index + 1, lua.create_string(value)?)?;
    }
    Ok(table)
}

fn limited<T: mlua::FromLua>(table: Table) -> mlua::Result<Vec<T>> {
    if table.raw_len() > MAX_ITEMS {
        return Err(error("plugin list exceeds 512 items"));
    }
    let mut values = Vec::with_capacity(table.raw_len());
    for row in table.sequence_values() {
        if values.len() == MAX_ITEMS {
            return Err(error("plugin list exceeds 512 items"));
        }
        values.push(row?);
    }
    Ok(values)
}

pub(super) fn byte_list(table: Table) -> mlua::Result<Vec<Vec<u8>>> {
    let mut remaining = MEMORY_LIMIT;
    limited::<LuaString>(table)?
        .into_iter()
        .map(|string| {
            let bytes = string.as_bytes();
            charge(&mut remaining, bytes.len(), "plugin history exceeds 8 MiB")?;
            Ok(bytes.to_vec())
        })
        .collect()
}

fn charge(remaining: &mut usize, bytes: usize, message: &'static str) -> mlua::Result<()> {
    *remaining = remaining.checked_sub(bytes).ok_or_else(|| error(message))?;
    Ok(())
}

fn display_item(lua: &Lua, value: Value) -> mlua::Result<(LuaString, Value)> {
    if let Value::Table(row) = value {
        return Ok((row.raw_get("text")?, row.raw_get("style")?));
    }
    let text = lua
        .coerce_string(value)?
        .ok_or_else(|| error("display item must be a string or a {text, style} table"))?;
    Ok((text, Value::Nil))
}

fn menu(lua: &Lua, rows: Table, options: Option<&Table>) -> mlua::Result<EditorMenu> {
    let mut remaining = MAX_EDITOR_TEXT;
    let items = limited::<Value>(rows)?
        .into_iter()
        .map(|value| {
            let (text, style) = display_item(lua, value)?;
            charge(
                &mut remaining,
                text.as_bytes().len(),
                "editor menu exceeds 32 KiB",
            )?;
            Ok(EditorMenuItem {
                text: text.to_str()?.to_owned(),
                style: parse_style(lua, style)?,
            })
        })
        .collect::<mlua::Result<Vec<_>>>()?;
    let mut result = EditorMenu {
        items,
        ..EditorMenu::default()
    };
    if let Some(options) = options {
        result.selected = options
            .raw_get::<Option<usize>>("selected")?
            .map(|one_based| {
                one_based
                    .checked_sub(1)
                    .filter(|&index| index < result.items.len())
                    .ok_or_else(|| {
                        error("selected menu index must name a visible item (one-based)")
                    })
            })
            .transpose()?;
        result.style = parse_style(lua, options.raw_get("style")?)?;
        result.selected_style = parse_style(lua, options.raw_get("selected_style")?)?;
    }
    Ok(result)
}

pub(super) fn completion_menu(lua: &Lua, result: Table) -> mlua::Result<EditorMenu> {
    match result.raw_get::<Option<Table>>("items")? {
        Some(rows) => menu(lua, rows, Some(&result)),
        None => menu(lua, result, None),
    }
}

pub(super) fn prompt_text(
    lua: &Lua,
    result: Value,
    colors: TerminalColors,
) -> mlua::Result<Option<String>> {
    const MESSAGE: &str = "editor prompt exceeds 32 KiB";
    if result.is_nil() {
        return Ok(None);
    }
    let mut remaining = MAX_EDITOR_TEXT;
    let Value::Table(segments) = result else {
        let (text, _) = display_item(lua, result)?;
        charge(&mut remaining, text.as_bytes().len(), MESSAGE)?;
        return Ok(Some(text.to_str()?.to_owned()));
    };

    let mut visible = String::new();
    let mut runs = Vec::new();
    for value in limited::<Value>(segments)? {
        let (segment, style) = display_item(lua, value)?;
        charge(&mut remaining, segment.as_bytes().len(), MESSAGE)?;
        let sgr = parse_style(lua, style)?.and_then(|spec| colors.structured(&spec));
        let start = visible.len();
        for character in segment.to_str()?.chars() {
            if character.is_control() && character != '\n' {
                let escaped = character.escape_default();
                charge(
                    &mut remaining,
                    escaped.len() - character.len_utf8(),
                    MESSAGE,
                )?;
                visible.extend(escaped);
            } else {
                visible.push(character);
            }
        }
        if start != visible.len() {
            runs.push((start, sgr));
        }
    }
    if runs.iter().all(|(_, style)| style.is_none()) {
        return Ok(Some(visible));
    }

    // A plugin may split a grapheme across segments. Use the style of its first
    // codepoint and keep escape sequences outside the entire grapheme.
    let mut runs = runs.iter().peekable();
    let mut text = String::with_capacity(visible.len());
    let mut active = None;
    let mut style = None;
    for (offset, grapheme) in visible.grapheme_indices(true) {
        while runs.peek().is_some_and(|(start, _)| *start <= offset) {
            style = runs.next().and_then(|(_, style)| style.as_deref());
        }
        if style != active {
            if let Some(sgr) = style {
                charge(&mut remaining, sgr.len() + "\x1b[0m".len(), MESSAGE)?;
            }
            if active.is_some() {
                text.push_str("\x1b[0m");
            }
            if let Some(sgr) = style {
                text.push_str(sgr);
            }
            active = style;
        }
        text.push_str(grapheme);
    }
    if active.is_some() {
        text.push_str("\x1b[0m");
    }
    Ok(Some(text))
}

fn legacy_color(entry: &Table) -> mlua::Result<Option<String>> {
    let Some(value) = entry.raw_get::<Option<LuaString>>("color")? else {
        return Ok(None);
    };
    if value.as_bytes().len() > 7 {
        return Ok(None);
    }
    let value = value.to_str()?;
    Ok((value == "dim" || EditorColor::hex(&value).is_some()).then(|| value.to_owned()))
}

pub(super) fn highlights(lua: &Lua, table: Table) -> mlua::Result<Vec<EditorHighlight>> {
    limited::<Table>(table)?
        .into_iter()
        .map(|entry| {
            Ok(EditorHighlight {
                start: entry.raw_get("start")?,
                end: entry.raw_get("end")?,
                color: legacy_color(&entry)?.unwrap_or_default(),
                style: parse_style(lua, entry.raw_get("style")?)?,
            })
        })
        .collect()
}

fn virtual_text(lua: &Lua, table: Table) -> mlua::Result<Vec<EditorVirtualText>> {
    let mut remaining = MAX_EDITOR_TEXT;
    limited::<Table>(table)?
        .into_iter()
        .map(|entry| {
            let text: LuaString = entry.raw_get("text")?;
            charge(
                &mut remaining,
                text.as_bytes().len(),
                "editor virtual text exceeds 32 KiB",
            )?;
            Ok(EditorVirtualText {
                offset: entry.raw_get("offset")?,
                text: text.to_str()?.to_owned(),
                color: legacy_color(&entry)?,
                style: parse_style(lua, entry.raw_get("style")?)?,
            })
        })
        .collect()
}
