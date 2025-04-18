use std::{
    collections::HashMap,
    fs,
    path::Path,
    rc::Rc,
    sync::{Arc, Condvar, Mutex},
};

use dashmap::DashMap;
use rayon::{iter::ParallelBridge, prelude::ParallelIterator};
use rustc_hash::FxHashSet;

use oxc_allocator::Allocator;
use oxc_diagnostics::{DiagnosticSender, DiagnosticService, Error, FailedToOpenFileError};
use oxc_parser::Parser;
use oxc_resolver::{ResolveOptions, Resolver};
use oxc_semantic::{ModuleRecord, SemanticBuilder};
use oxc_span::{SourceType, VALID_EXTENSIONS};

use crate::{
    partial_loader::{JavaScriptSource, PartialLoader, LINT_PARTIAL_LOADER_EXT},
    Fixer, LintContext, Linter, Message,
};

#[derive(Clone)]
pub struct LintService {
    runtime: Arc<Runtime>,
}

impl LintService {
    pub fn new(cwd: Box<Path>, paths: &[Box<Path>], linter: Linter) -> Self {
        let runtime = Arc::new(Runtime::new(cwd, paths, linter));
        Self { runtime }
    }

    #[cfg(test)]
    pub(crate) fn from_linter(cwd: Box<Path>, paths: &[Box<Path>], linter: Linter) -> Self {
        let runtime = Arc::new(Runtime::new(cwd, paths, linter));
        Self { runtime }
    }

    pub fn linter(&self) -> &Linter {
        &self.runtime.linter
    }

    pub fn number_of_dependencies(&self) -> usize {
        self.runtime.module_map.len() - self.runtime.paths.len()
    }

    /// # Panics
    pub fn run(&self, tx_error: &DiagnosticSender) {
        self.runtime
            .paths
            .iter()
            .par_bridge()
            .for_each_with(&self.runtime, |runtime, path| runtime.process_path(path, tx_error));
        tx_error.send(None).unwrap();
    }

    /// For tests
    #[cfg(test)]
    pub(crate) fn run_source<'a>(
        &self,
        allocator: &'a Allocator,
        source_text: &'a str,
        check_syntax_errors: bool,
        tx_error: &DiagnosticSender,
    ) -> Vec<Message<'a>> {
        self.runtime
            .paths
            .iter()
            .flat_map(|path| {
                let source_type = SourceType::from_path(path).unwrap();
                self.runtime.init_cache_state(path);
                self.runtime.process_source(
                    path,
                    allocator,
                    source_text,
                    source_type,
                    check_syntax_errors,
                    tx_error,
                )
            })
            .collect::<Vec<_>>()
    }
}

/// `CacheState` and `CacheStateEntry` are used to fix the problem where
/// there is a brief moment when a concurrent fetch can miss the cache.
///
/// Given `ModuleMap` is a `DashMap`, which conceptually is a `RwLock<HashMap>`.
/// When two requests read the map at the exact same time from different threads,
/// both will miss the cache so both thread will make a request.
///
/// See the "problem section" in <https://medium.com/@polyglot_factotum/rust-concurrency-patterns-condvars-and-locks-e278f18db74f>
/// and the solution is copied here to fix the issue.
type CacheState = Mutex<HashMap<Box<Path>, Arc<(Mutex<CacheStateEntry>, Condvar)>>>;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CacheStateEntry {
    ReadyToConstruct,
    PendingStore(usize),
}

/// Keyed by canonicalized path
type ModuleMap = DashMap<Box<Path>, Arc<ModuleRecord>>;

pub struct Runtime {
    cwd: Box<Path>,
    /// All paths to lint
    paths: FxHashSet<Box<Path>>,
    linter: Linter,
    resolver: Resolver,
    module_map: ModuleMap,
    cache_state: CacheState,
}

impl Runtime {
    fn new(cwd: Box<Path>, paths: &[Box<Path>], linter: Linter) -> Self {
        Self {
            cwd,
            paths: paths.iter().cloned().collect(),
            linter,
            resolver: Self::resolver(),
            module_map: ModuleMap::default(),
            cache_state: CacheState::default(),
        }
    }

    fn resolver() -> Resolver {
        Resolver::new(ResolveOptions {
            extensions: VALID_EXTENSIONS.iter().map(|ext| format!(".{ext}")).collect(),
            ..ResolveOptions::default()
        })
    }

    fn get_source_type_and_text(
        path: &Path,
        ext: &str,
    ) -> Option<Result<(SourceType, String), Error>> {
        let read_file = |path: &Path| -> Result<String, Error> {
            fs::read_to_string(path)
                .map_err(|e| Error::new(FailedToOpenFileError(path.to_path_buf(), e)))
        };
        let source_type = SourceType::from_path(path);
        let not_supported_yet =
            source_type.as_ref().is_err_and(|_| !LINT_PARTIAL_LOADER_EXT.contains(&ext));
        if not_supported_yet {
            return None;
        }
        let source_type = source_type.unwrap_or_default();
        let source_text = match read_file(path) {
            Ok(source_text) => source_text,
            Err(e) => {
                return Some(Err(e));
            }
        };

        Some(Ok((source_type, source_text)))
    }

    fn process_path(&self, path: &Path, tx_error: &DiagnosticSender) {
        if self.init_cache_state(path) {
            return;
        }
        let Some(ext) = path.extension().and_then(std::ffi::OsStr::to_str) else {
            return;
        };
        let Some(source_type_and_text) = Self::get_source_type_and_text(path, ext) else { return };
        let (source_type, source_text) = match source_type_and_text {
            Ok(source_text) => source_text,
            Err(e) => {
                tx_error.send(Some((path.to_path_buf(), vec![e]))).unwrap();
                return;
            }
        };

        let sources = PartialLoader::parse(ext, &source_text)
            .unwrap_or_else(|| vec![JavaScriptSource::new(&source_text, source_type, 0)]);

        if sources.is_empty() {
            return;
        }

        for JavaScriptSource { source_text, source_type, .. } in sources {
            let allocator = Allocator::default();
            let mut messages =
                self.process_source(path, &allocator, source_text, source_type, true, tx_error);

            if self.linter.options().fix {
                let fix_result = Fixer::new(source_text, messages).fix();
                fs::write(path, fix_result.fixed_code.as_bytes()).unwrap();
                messages = fix_result.messages;
            }

            if !messages.is_empty() {
                let errors = messages.into_iter().map(|m| m.error).collect();
                let path = path.strip_prefix(&self.cwd).unwrap_or(path);
                let diagnostics = DiagnosticService::wrap_diagnostics(path, source_text, errors);
                tx_error.send(Some(diagnostics)).unwrap();
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn process_source<'a>(
        &self,
        path: &Path,
        allocator: &'a Allocator,
        source_text: &'a str,
        source_type: SourceType,
        check_syntax_errors: bool,
        tx_error: &DiagnosticSender,
    ) -> Vec<Message<'a>> {
        let ret = Parser::new(allocator, source_text, source_type)
            .allow_return_outside_function(true)
            .parse();

        if !ret.errors.is_empty() {
            return ret.errors.into_iter().map(|err| Message::new(err, None)).collect();
        };

        let program = allocator.alloc(ret.program);

        // Build the module record to unblock other threads from waiting for too long.
        // The semantic model is not built at this stage.
        let semantic_builder = SemanticBuilder::new(source_text, source_type)
            .with_trivias(ret.trivias)
            .with_check_syntax_error(check_syntax_errors)
            .build_module_record(path.to_path_buf(), program);
        let module_record = semantic_builder.module_record();

        if self.linter.options().import_plugin {
            self.module_map
                .insert(path.to_path_buf().into_boxed_path(), Arc::clone(&module_record));
            self.update_cache_state(path);

            // Retrieve all dependency modules from this module.
            let dir = path.parent().unwrap();
            module_record
                .requested_modules
                .keys()
                .par_bridge()
                .map_with(&self.resolver, |resolver, specifier| {
                    resolver.resolve(dir, specifier).ok().map(|r| (specifier, r))
                })
                .flatten()
                .for_each_with(tx_error, |tx_error, (specifier, resolution)| {
                    let path = resolution.path();
                    self.process_path(path, tx_error);
                    if let Some(target_module_record) = self.module_map.get(path) {
                        module_record
                            .loaded_modules
                            .insert(specifier.clone(), Arc::clone(&target_module_record));
                    }
                });

            // Stop if the current module is not marked for lint.
            if !self.paths.contains(path) {
                return vec![];
            }
        }

        let semantic_ret = semantic_builder.build(program);

        if !semantic_ret.errors.is_empty() {
            return semantic_ret.errors.into_iter().map(|err| Message::new(err, None)).collect();
        };

        let lint_ctx =
            LintContext::new(path.to_path_buf().into_boxed_path(), &Rc::new(semantic_ret.semantic));
        self.linter.run(lint_ctx)
    }

    fn init_cache_state(&self, path: &Path) -> bool {
        if !self.linter.options().import_plugin {
            return false;
        }

        let (lock, cvar) = {
            let mut state_map = self.cache_state.lock().unwrap();
            &*Arc::clone(state_map.entry(path.to_path_buf().into_boxed_path()).or_insert_with(
                || Arc::new((Mutex::new(CacheStateEntry::ReadyToConstruct), Condvar::new())),
            ))
        };
        let mut state = cvar
            .wait_while(lock.lock().unwrap(), |state| {
                matches!(*state, CacheStateEntry::PendingStore(_))
            })
            .unwrap();

        let cache_hit = if self.module_map.contains_key(path) {
            true
        } else {
            let i = if let CacheStateEntry::PendingStore(i) = *state { i } else { 0 };
            *state = CacheStateEntry::PendingStore(i + 1);
            false
        };

        if *state == CacheStateEntry::ReadyToConstruct {
            cvar.notify_one();
        }

        drop(state);
        cache_hit
    }

    fn update_cache_state(&self, path: &Path) {
        let (lock, cvar) = {
            let mut state_map = self.cache_state.lock().unwrap();
            &*Arc::clone(
                state_map
                    .get_mut(path)
                    .expect("Entry in http-cache state to have been previously inserted"),
            )
        };
        let mut state = lock.lock().unwrap();
        if let CacheStateEntry::PendingStore(i) = *state {
            let new = i - 1;
            if new == 0 {
                *state = CacheStateEntry::ReadyToConstruct;
                // Notify the next thread waiting in line, if there is any.
                cvar.notify_one();
            } else {
                *state = CacheStateEntry::PendingStore(new);
            }
        }
    }
}
