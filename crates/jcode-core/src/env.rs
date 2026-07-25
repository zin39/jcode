use std::ffi::OsStr;

/// Mutate the process environment for jcode runtime configuration.
///
/// Rust 2024 makes environment mutation unsafe because it can race with
/// concurrent environment access in foreign code. jcode intentionally mutates
/// process-local env vars to coordinate provider/runtime bootstrap before or
/// during task execution. We centralize that unsafety here so call sites remain
/// auditable.
pub fn set_var<K, V>(key: K, value: V)
where
    K: AsRef<OsStr>,
    V: AsRef<OsStr>,
{
    #[cfg(any(test, feature = "test-support"))]
    mirror_home_override_on_set(key.as_ref(), value.as_ref());
    #[cfg(any(test, feature = "test-support"))]
    mirror_runtime_provider_on_set(key.as_ref(), value.as_ref());

    // SAFETY: jcode treats these mutations as process-global configuration.
    // They are a pre-existing design choice used throughout startup, auth,
    // provider bootstrap, tests, and self-dev flows. Centralizing the unsafe
    // operation here makes the Rust 2024 requirement explicit without
    // scattering unsafe blocks across hundreds of call sites.
    unsafe {
        std::env::set_var(key, value);
    }
}

/// Remove a process environment variable used by jcode runtime configuration.
pub fn remove_var<K>(key: K)
where
    K: AsRef<OsStr>,
{
    #[cfg(any(test, feature = "test-support"))]
    mirror_home_override_on_remove(key.as_ref());
    #[cfg(any(test, feature = "test-support"))]
    mirror_runtime_provider_on_remove(key.as_ref());

    // SAFETY: see `set_var` above; this is the corresponding centralized
    // removal operation for the same process-global configuration surface.
    unsafe {
        std::env::remove_var(key);
    }
}

/// Environment variable naming the jcode home directory.
pub const JCODE_HOME_VAR: &str = "JCODE_HOME";

/// The jcode home scoped to the current thread, if one is active.
///
/// Always `None` outside test builds, where the process environment is the only
/// source of truth.
#[cfg(not(any(test, feature = "test-support")))]
#[inline]
pub fn home_override() -> Option<std::path::PathBuf> {
    None
}

/// The jcode home scoped to the current thread, if one is active.
///
/// `JCODE_HOME` is process-global, so tests that point it at their own temp dir
/// are mutating state every other test can see. Under the default parallel
/// runner that races: one test sets the var, another overwrites or clears it,
/// and the first then resolves paths against a home it never wrote.
///
/// Cargo runs each test on its own thread, so scoping the home per thread makes
/// isolation structural rather than a convention every test has to remember.
/// [`set_var`] and [`remove_var`] maintain this automatically for `JCODE_HOME`,
/// which is why existing tests need no changes.
#[cfg(any(test, feature = "test-support"))]
pub fn home_override() -> Option<std::path::PathBuf> {
    HOME_OVERRIDE.with(|slot| slot.borrow().clone())
}

#[cfg(any(test, feature = "test-support"))]
thread_local! {
    static HOME_OVERRIDE: std::cell::RefCell<Option<std::path::PathBuf>> =
        const { std::cell::RefCell::new(None) };
}

/// Keep the per-thread home in sync when a caller sets `JCODE_HOME` directly.
///
/// The process env var is still written, so subprocesses and any code reading
/// the raw environment behave as before. Path resolution prefers the per-thread
/// value, so the setter gets isolation without opting in.
#[cfg(any(test, feature = "test-support"))]
fn mirror_home_override_on_set(key: &OsStr, value: &OsStr) {
    if key == OsStr::new(JCODE_HOME_VAR) {
        let path = std::path::PathBuf::from(value);
        HOME_OVERRIDE.with(|slot| *slot.borrow_mut() = Some(path));
    }
}

#[cfg(any(test, feature = "test-support"))]
fn mirror_home_override_on_remove(key: &OsStr) {
    if key == OsStr::new(JCODE_HOME_VAR) {
        HOME_OVERRIDE.with(|slot| *slot.borrow_mut() = None);
    }
}

/// Environment variable pinning the active runtime provider.
pub const JCODE_RUNTIME_PROVIDER_VAR: &str = "JCODE_RUNTIME_PROVIDER";

/// The runtime provider pin, reading the process environment.
#[cfg(not(any(test, feature = "test-support")))]
#[inline]
pub fn runtime_provider() -> Option<String> {
    std::env::var(JCODE_RUNTIME_PROVIDER_VAR).ok()
}

/// The runtime provider pin, preferring a value scoped to the current thread.
///
/// Like [`home_override`], this exists because the var is process-global while
/// tests treat it as local setup. Pinning `claude-api` in one test flipped
/// pricing and credential-mode resolution for every test rendering
/// concurrently, so cost assertions failed depending on interleaving.
#[cfg(any(test, feature = "test-support"))]
pub fn runtime_provider() -> Option<String> {
    if let Some(pinned) = RUNTIME_PROVIDER_OVERRIDE.with(|slot| slot.borrow().clone()) {
        return Some(pinned);
    }
    // A thread that has never touched the var falls back to the process
    // environment so single-threaded runs and real binaries behave alike.
    if RUNTIME_PROVIDER_TOUCHED.with(|slot| slot.get()) {
        return None;
    }
    std::env::var(JCODE_RUNTIME_PROVIDER_VAR).ok()
}

#[cfg(any(test, feature = "test-support"))]
thread_local! {
    static RUNTIME_PROVIDER_OVERRIDE: std::cell::RefCell<Option<String>> =
        const { std::cell::RefCell::new(None) };
    static RUNTIME_PROVIDER_TOUCHED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[cfg(any(test, feature = "test-support"))]
fn mirror_runtime_provider_on_set(key: &OsStr, value: &OsStr) {
    if key == OsStr::new(JCODE_RUNTIME_PROVIDER_VAR) {
        let value = value.to_string_lossy().into_owned();
        RUNTIME_PROVIDER_TOUCHED.with(|slot| slot.set(true));
        RUNTIME_PROVIDER_OVERRIDE.with(|slot| *slot.borrow_mut() = Some(value));
    }
}

#[cfg(any(test, feature = "test-support"))]
fn mirror_runtime_provider_on_remove(key: &OsStr) {
    if key == OsStr::new(JCODE_RUNTIME_PROVIDER_VAR) {
        RUNTIME_PROVIDER_TOUCHED.with(|slot| slot.set(true));
        RUNTIME_PROVIDER_OVERRIDE.with(|slot| *slot.borrow_mut() = None);
    }
}

/// Scope the jcode home to the current thread until the returned guard drops,
/// without touching the process environment. See [`home_override`].
#[cfg(any(test, feature = "test-support"))]
pub fn scoped_home_override(path: impl Into<std::path::PathBuf>) -> ScopedHomeOverride {
    let previous = HOME_OVERRIDE.with(|slot| slot.borrow_mut().replace(path.into()));
    ScopedHomeOverride { previous }
}

/// Restores the previous per-thread home on drop, including on panic.
#[cfg(any(test, feature = "test-support"))]
pub struct ScopedHomeOverride {
    previous: Option<std::path::PathBuf>,
}

#[cfg(any(test, feature = "test-support"))]
impl Drop for ScopedHomeOverride {
    fn drop(&mut self) {
        let previous = self.previous.take();
        HOME_OVERRIDE.with(|slot| *slot.borrow_mut() = previous);
    }
}

/// Wrap a future so it observes the spawning thread's home override wherever it
/// is polled.
///
/// A future spawned onto a runtime runs on a worker thread, and may migrate
/// between them, so a thread-local set on the spawning thread is invisible to
/// it. Without this, background work would resolve paths against the
/// developer's real `~/.jcode` while the test believes it is sandboxed. The
/// override is installed for the duration of each `poll` and removed
/// afterwards, so the worker thread is left exactly as it was found.
///
/// Compiles to the identity function outside test builds.
#[cfg(not(any(test, feature = "test-support")))]
#[inline]
pub fn inherit_home<F: std::future::Future>(future: F) -> F {
    future
}

#[cfg(any(test, feature = "test-support"))]
pub fn inherit_home<F: std::future::Future>(future: F) -> InheritHome<F> {
    InheritHome {
        home: home_override(),
        future,
    }
}

/// Future returned by [`inherit_home`].
#[cfg(any(test, feature = "test-support"))]
pub struct InheritHome<F> {
    home: Option<std::path::PathBuf>,
    future: F,
}

#[cfg(any(test, feature = "test-support"))]
impl<F: std::future::Future> std::future::Future for InheritHome<F> {
    type Output = F::Output;

    fn poll(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Self::Output> {
        // SAFETY: standard pin projection. `home` is `Unpin` and never moved
        // out; `future` is only ever exposed as a `Pin<&mut F>`.
        let (home, future) = unsafe {
            let this = self.get_unchecked_mut();
            (&this.home, std::pin::Pin::new_unchecked(&mut this.future))
        };
        let _scoped = home.clone().map(scoped_home_override);
        future.poll(cx)
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn setting_jcode_home_scopes_it_to_this_thread() {
        let _guard = super::scoped_home_override("/tmp/outer");
        super::set_var(super::JCODE_HOME_VAR, "/tmp/inner");
        assert_eq!(
            super::home_override(),
            Some(std::path::PathBuf::from("/tmp/inner"))
        );

        // A thread that never set the var must not inherit the setter's home,
        // even though the process env var is still exported for subprocesses.
        let observed = std::thread::spawn(super::home_override).join().unwrap();
        assert_eq!(observed, None);
    }

    #[test]
    fn removing_jcode_home_clears_the_thread_override() {
        super::set_var(super::JCODE_HOME_VAR, "/tmp/removed");
        super::remove_var(super::JCODE_HOME_VAR);
        assert_eq!(super::home_override(), None);
    }

    #[test]
    fn unrelated_vars_do_not_touch_the_home_override() {
        let _guard = super::scoped_home_override("/tmp/kept");
        super::set_var("JCODE_SOME_OTHER_VAR", "x");
        super::remove_var("JCODE_SOME_OTHER_VAR");
        assert_eq!(
            super::home_override(),
            Some(std::path::PathBuf::from("/tmp/kept"))
        );
    }
}
