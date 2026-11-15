//! Connects the optional Lua registry to ordinary Shell state and dispatch.
use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::rc::Rc;
use std::{collections::BTreeSet, mem};

use crate::plugin_lua::{CallContext, CommandMode, Effect};
use crate::plugins;
use crate::shell::{Flow, Shell, diagnostic, is_name, output, show};

impl Shell {
    pub fn plugin_context(&self, history: bool) -> CallContext {
        let mut commands = self
            .functions
            .keys()
            .chain(self.aliases.keys())
            .cloned()
            .collect::<Vec<_>>();
        if let Some(plugins) = &self.plugins {
            commands.extend(plugins.command_names());
        }
        let mut context = CallContext {
            cwd: std::env::current_dir().unwrap_or_default(),
            status: self.status,
            commands: commands.into(),
            history: if history {
                self.session.history().to_vec().into()
            } else {
                Rc::default()
            },
            ..Default::default()
        };
        for (name, variable) in &self.vars {
            if let Some(value) = &variable.value {
                Rc::make_mut(&mut context.vars).insert(name.clone(), value.clone());
                if variable.exported {
                    Rc::make_mut(&mut context.env).insert(name.clone(), value.clone());
                }
            }
        }
        context
    }

    fn plugin_reserved(&self, name: &[u8]) -> bool {
        crate::builtins::builtin(name)
            || matches!(name, b"printf" | b"test" | b"[")
            || self.functions.contains_key(name)
            || self.aliases.contains_key(name)
            || self.search_paths(name).any(|path| {
                crate::sys::access(&path, libc::X_OK)
                    && Path::new(OsStr::from_bytes(&path)).is_file()
            })
    }

    fn reconcile_plugins(
        &mut self,
        requested: BTreeSet<String>,
        reload: Option<&str>,
    ) -> Result<(), String> {
        if self.plugins.is_none() && requested.is_empty() {
            return Ok(());
        }
        let context = self.plugin_context(false);
        let mut current = self.plugins.take().unwrap_or_default();
        let result = (|| {
            let mut stage = current.stage(requested, reload, context.clone(), |name| {
                self.plugin_reserved(name)
            })?;
            self.apply_effects(mem::take(&mut stage.context.effects), false)?;
            current.commit(stage, &context);
            Ok(())
        })();
        if !current.requested.is_empty() {
            self.plugins = Some(current);
        }
        result
    }

    fn requested_plugins(&self) -> BTreeSet<String> {
        self.plugin_pending
            .as_ref()
            .cloned()
            .or_else(|| self.plugins.as_ref().map(|p| p.requested.clone()))
            .unwrap_or_default()
    }

    pub fn load_saved_plugins(&mut self) {
        if !self.plugin_allowed {
            return;
        }
        let wanted = match plugins::saved() {
            Ok(wanted) => wanted,
            Err(error) => {
                diagnostic(&error);
                return;
            }
        };
        if wanted.is_empty() {
            return;
        }
        let _input = match crate::shell::Fds::startup_stdio() {
            Ok(guard) => guard,
            Err(error) => {
                diagnostic(&format!("cannot isolate plugin startup input: {error}"));
                return;
            }
        };
        if self.reconcile_plugins(wanted.clone(), None).is_ok() {
            return;
        }
        // Recover independent plugins if a saved plugin or dependency failed.
        for name in wanted {
            let mut requested = self.requested_plugins();
            requested.insert(name);
            if let Err(error) = self.reconcile_plugins(requested, None) {
                diagnostic(&error);
            }
        }
    }

    pub fn commit_plugin_config(&mut self) -> Result<(), String> {
        if let Some(requested) = self.plugin_pending.take() {
            self.reconcile_plugins(requested, None)?;
        }
        Ok(())
    }

    pub fn plugin_manage(&mut self, args: &[Vec<u8>]) -> Flow {
        let result = self.plugin_management(args);
        match result {
            Ok(Some(text)) => output(text.as_bytes()),
            Ok(None) => Flow::Status(0),
            Err(error) => self.error(1, &error, false),
        }
    }

    fn plugin_management(&mut self, args: &[Vec<u8>]) -> Result<Option<String>, String> {
        let (verb, name) = match args {
            [verb, name] if matches!(verb.as_slice(), b"enable" | b"disable" | b"reload") => {
                (verb, name)
            }
            _ => return plugins::inspect(args, Some(&self.requested_plugins())).map(Some),
        };
        if !self.plugin_allowed {
            if self.startup_failure.is_some() {
                return Ok(None);
            }
            return Err(
                "plugins disabled in this session; use command nsh plugin to manage saved plugins"
                    .into(),
            );
        }
        if self.is_fork_worker() {
            return Err("plugin management requires the parent NSH session".into());
        }
        let name = std::str::from_utf8(name).map_err(|_| "plugin name must be UTF-8")?;
        let mut requested = self.requested_plugins();
        let reload = (verb == b"reload").then_some(name);
        if verb == b"disable" {
            if !requested.remove(name) {
                return Err(format!(
                    "plugin {name} is not explicitly enabled (check dependent plugins)"
                ));
            }
        } else if reload.is_some() {
            if !requested.contains(name) && !self.plugins.as_ref().is_some_and(|p| p.contains(name))
            {
                return Err(format!("enable plugin {name} before reloading it"));
            }
        } else {
            requested.insert(name.to_owned());
        }
        if self.startup_failure.is_some() {
            plugins::validate(&requested)?;
            self.plugin_pending = Some(requested);
        } else {
            self.reconcile_plugins(requested, reload)?;
        }
        Ok(None)
    }

    pub fn plugin_command(&self, name: &[u8]) -> Option<CommandMode> {
        self.plugins.as_ref()?.command(name)
    }

    pub fn run_plugin(&mut self, args: &[Vec<u8>], wrapped: Option<i32>) -> Flow {
        let context = self.plugin_context(true);
        let result = self.plugins.as_ref().expect("plugin dispatch").invoke(
            &args[0],
            &args[1..],
            context,
            wrapped,
        );
        match result {
            Ok((status, mut context)) => {
                match self.apply_effects(mem::take(&mut context.effects), true) {
                    Ok(Some(flow)) => flow,
                    Ok(None) => Flow::Status(status),
                    Err(error) => self.error(1, &error, false),
                }
            }
            Err(error) => self.error(1, &error, false),
        }
    }

    pub fn run_plugin_event(&mut self, event: &str, args: &[Vec<u8>]) {
        let Some(plugins) = self
            .plugins
            .as_ref()
            .filter(|plugins| plugins.has_event(event))
        else {
            return;
        };
        let context = plugins.event(event, args, self.plugin_context(true));
        if let Err(error) = self.apply_effects(context.effects, false) {
            diagnostic(&error);
        }
    }

    pub fn shutdown_plugins(&mut self) {
        if let Some(mut plugins) = self.plugins.take() {
            plugins.shutdown(&self.plugin_context(false));
        }
    }

    fn apply_effects(
        &mut self,
        effects: Vec<Effect>,
        may_exec: bool,
    ) -> Result<Option<Flow>, String> {
        let mut directory = None;
        let mut exec = None;
        for effect in &effects {
            match effect {
                Effect::SetVar(name, value) | Effect::SetEnv(name, value) => {
                    if !is_name(name) || value.as_ref().is_some_and(|v| v.contains(&0)) {
                        return Err(format!(
                            "invalid plugin variable assignment for {:?}",
                            show(name)
                        ));
                    }
                    if self
                        .vars
                        .get(name)
                        .is_some_and(|variable| variable.readonly)
                    {
                        return Err(format!("{}: readonly variable", show(name)));
                    }
                }
                Effect::Cd(path) => directory = Some(path),
                Effect::Exec(args) => {
                    if !may_exec
                        || self.owns_startup_transaction()
                        || args.is_empty()
                        || exec.is_some()
                    {
                        return Err("process replacement is only allowed once from a plugin command outside startup".into());
                    }
                    exec = Some(args.clone());
                }
            }
        }
        let mut previous = if let Some(directory) = directory {
            if self.vars.get(b"PWD".as_slice()).is_some_and(|v| v.readonly)
                || self
                    .vars
                    .get(b"OLDPWD".as_slice())
                    .is_some_and(|v| v.readonly)
            {
                return Err("cannot change directory: PWD or OLDPWD is readonly".into());
            }
            let old = crate::expand::cwd();
            std::env::set_current_dir(directory)
                .map_err(|error| format!("{}: {error}", directory.display()))?;
            self.hashed_commands
                .retain(|_, path| path.starts_with(b"/"));
            old
        } else {
            Vec::new()
        };
        for effect in effects {
            let (name, value, exported) = match effect {
                Effect::SetVar(name, value) => (name, value, false),
                Effect::SetEnv(name, value) => (name, value, true),
                Effect::Cd(path) => {
                    let next = path.as_os_str().as_bytes().to_vec();
                    self.assign(b"OLDPWD", mem::replace(&mut previous, next.clone()), true)?;
                    self.assign(b"PWD", next, true)?;
                    continue;
                }
                _ => continue,
            };
            if let Some(value) = value {
                self.assign(&name, value, exported)?;
            } else {
                self.vars.remove(&name);
                if name == b"PATH" {
                    self.hashed_commands.clear();
                }
            }
        }
        Ok(exec.map(|args| self.exec_external(&args, false)))
    }
}

pub fn cli(words: &[Vec<u8>]) -> i32 {
    if !crate::sys::privilege_matches() {
        return 1;
    }
    let result = match words {
        [verb, name] if matches!(verb.as_slice(), b"enable" | b"disable") => {
            std::str::from_utf8(name)
                .map_err(|_| "plugin name must be UTF-8".into())
                .and_then(|name| plugins::save(name, verb == b"enable"))
                .map(|()| String::new())
        }
        [verb, _] if verb == b"reload" => {
            Err("reload runs inside the NSH session that owns the plugin".into())
        }
        _ => plugins::inspect(words, None),
    };
    match result {
        Ok(text) => output(text.as_bytes()).code(),
        Err(error) => {
            diagnostic(&error);
            1
        }
    }
}
