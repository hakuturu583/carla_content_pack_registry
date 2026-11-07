//! PyO3 bindings: the `carla_pack_registry._core` extension module used by the Python frontend.
//! Thin wrappers only; network calls release the GIL and values cross the boundary as
//! Python dicts/lists.
use std::path::PathBuf;
use std::sync::Mutex;

use pyo3::exceptions::PyException;
use pyo3::prelude::*;
use serde::Serialize;

use crate::auth::DeviceFlow;
use crate::client::{
    Client as CoreClient, PublishOptions, Release, append_entry as core_append, plan_publish as core_plan,
    publish as core_publish,
};
use crate::error::{Error, Result};
use crate::manifest::PackEntry;
use crate::manifest::{Filter, PackRef};
use crate::pack::InstallOptions;
use crate::sigv4::Credentials;
use crate::store::{Login, Store};

pyo3::create_exception!(_core, CarlaPackError, PyException, "Error from the carla-pack core. args = (message, code).");

fn to_py(e: Error) -> PyErr {
    CarlaPackError::new_err((e.to_string(), e.code().to_string()))
}

fn to_object<'py>(py: Python<'py>, v: &impl Serialize) -> PyResult<Bound<'py, PyAny>> {
    let s = serde_json::to_string(v).map_err(|e| to_py(e.into()))?;
    py.import("json")?.call_method1("loads", (s,))
}

/// Runs `f` without the GIL and converts its result to Python objects.
fn call<'py, T: Serialize + Send>(
    py: Python<'py>,
    f: impl FnOnce() -> Result<T> + Send,
) -> PyResult<Bound<'py, PyAny>> {
    let v = py.detach(f).map_err(to_py)?;
    to_object(py, &v)
}

/// A release as Python sees it: the manifest entry plus `repo` and `ref`.
#[derive(Serialize)]
struct ReleaseView<'a> {
    #[serde(flatten)]
    release: &'a Release,
    #[serde(rename = "ref")]
    reference: String,
}

fn view(r: &Release) -> ReleaseView<'_> {
    ReleaseView { release: r, reference: r.reference() }
}

fn filter(base_release: Option<String>, platform: Option<String>, sha256: Option<String>) -> Filter {
    Filter { base_release, platform, sha256 }
}

#[pyclass(frozen)]
struct Client {
    inner: CoreClient,
}

impl Client {
    /// A release dict (from resolve / find_map / list_packs) or a reference string.
    fn release(&self, py: Python<'_>, release: &Bound<'_, PyAny>, f: Filter) -> PyResult<Release> {
        if let Ok(text) = release.extract::<String>() {
            let reference = PackRef::parse(&text).map_err(to_py)?;
            return py.detach(|| self.inner.resolve(&reference, &f)).map_err(to_py);
        }
        let json: String = py.import("json")?.call_method1("dumps", (release,))?.extract()?;
        serde_json::from_str::<Release>(&json).map_err(|e| to_py(Error::Usage(format!("not a pack release: {e}"))))
    }
}

#[pymethods]
impl Client {
    /// `token`: a GitHub token; default CARLA_PACK_TOKEN / GH_TOKEN / GITHUB_TOKEN / the saved
    /// login / `gh auth token`. `repos`: repositories searched by map name; default
    /// CARLA_PACK_REPOS / the saved list.
    #[new]
    #[pyo3(signature = (token=None, repos=None))]
    fn new(token: Option<String>, repos: Option<Vec<String>>) -> PyResult<Self> {
        Ok(Self { inner: CoreClient::new(token, repos).map_err(to_py)? })
    }

    #[getter]
    fn token_source(&self) -> Option<&'static str> {
        self.inner.token_source()
    }

    #[getter]
    fn repos(&self) -> Vec<String> {
        self.inner.repos().to_vec()
    }

    #[getter]
    fn cache_dir(&self) -> PathBuf {
        self.inner.cache_dir().to_path_buf()
    }

    fn whoami<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        call(py, || self.inner.whoami())
    }

    fn list_packs<'py>(&self, py: Python<'py>, repo: &str) -> PyResult<Bound<'py, PyAny>> {
        let releases = py.detach(|| self.inner.list_packs(repo)).map_err(to_py)?;
        to_object(py, &releases.iter().map(view).collect::<Vec<_>>())
    }

    #[pyo3(signature = (reference, base_release=None, platform=None, sha256=None))]
    fn resolve<'py>(
        &self,
        py: Python<'py>,
        reference: &str,
        base_release: Option<String>,
        platform: Option<String>,
        sha256: Option<String>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let reference = PackRef::parse(reference).map_err(to_py)?;
        let f = filter(base_release, platform, sha256);
        let r = py.detach(|| self.inner.resolve(&reference, &f)).map_err(to_py)?;
        to_object(py, &view(&r))
    }

    #[pyo3(signature = (map_name, repos=None, base_release=None, platform=None, sha256=None))]
    fn find_map<'py>(
        &self,
        py: Python<'py>,
        map_name: &str,
        repos: Option<Vec<String>>,
        base_release: Option<String>,
        platform: Option<String>,
        sha256: Option<String>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let f = filter(base_release, platform, sha256);
        let r = py.detach(|| self.inner.find_map(map_name, repos.as_deref(), &f)).map_err(to_py)?;
        to_object(py, &view(&r))
    }

    /// Downloads into the cache (verified) and returns the tarball's path.
    #[pyo3(signature = (release, base_release=None, platform=None, sha256=None))]
    fn fetch(
        &self,
        py: Python<'_>,
        release: &Bound<'_, PyAny>,
        base_release: Option<String>,
        platform: Option<String>,
        sha256: Option<String>,
    ) -> PyResult<PathBuf> {
        let r = self.release(py, release, filter(base_release, platform, sha256))?;
        py.detach(|| self.inner.fetch(&r)).map_err(to_py)
    }

    #[pyo3(signature = (release, packs_dir, base_release=None, platform=None, sha256=None, force=false))]
    #[allow(clippy::too_many_arguments)] // mirrors the Python keyword arguments
    fn install<'py>(
        &self,
        py: Python<'py>,
        release: &Bound<'py, PyAny>,
        packs_dir: PathBuf,
        base_release: Option<String>,
        platform: Option<String>,
        sha256: Option<String>,
        force: bool,
    ) -> PyResult<Bound<'py, PyAny>> {
        let r = self.release(py, release, filter(base_release.clone(), platform, sha256))?;
        let opts = InstallOptions { base_release, force };
        call(py, || self.inner.install(&r, &packs_dir, &opts))
    }

    fn credentials_status<'py>(&self, py: Python<'py>, repo: &str) -> PyResult<Bound<'py, PyAny>> {
        call(py, || self.inner.credentials_status(repo))
    }
}

/// Appends the pack `tarball` to the manifest at `manifest` and (with `upload`) uploads it with
/// CARLA_PACK_UPLOAD_* / AWS_* credentials (multipart above `part_size` bytes, default
/// 64 MiB). Returns the new manifest entry.
#[pyfunction]
#[pyo3(signature = (manifest, tarball, key=None, upload=true, overwrite=false, description=None, part_size=None))]
#[allow(clippy::too_many_arguments)] // mirrors the Python keyword arguments
fn publish<'py>(
    py: Python<'py>,
    manifest: PathBuf,
    tarball: PathBuf,
    key: Option<String>,
    upload: bool,
    overwrite: bool,
    description: Option<String>,
    part_size: Option<u64>,
) -> PyResult<Bound<'py, PyAny>> {
    let opts = PublishOptions { key, upload, overwrite, description, part_size };
    let creds = Credentials::upload_from_env();
    call(py, || core_publish(&manifest, &tarball, &opts, creds.as_ref()))
}

/// Checks that `tarball` can be published to the manifest at `manifest` and returns
/// {entry, storage, object_key} without changing anything (the SHA-256 is computed here).
#[pyfunction]
#[pyo3(signature = (manifest, tarball, key=None, description=None))]
fn plan_publish<'py>(
    py: Python<'py>,
    manifest: PathBuf,
    tarball: PathBuf,
    key: Option<String>,
    description: Option<String>,
) -> PyResult<Bound<'py, PyAny>> {
    let opts = PublishOptions { key, description, ..Default::default() };
    call(py, || core_plan(&manifest, &tarball, &opts))
}

/// Appends an entry from plan_publish to the manifest (after its file was uploaded).
#[pyfunction]
fn append_entry(py: Python<'_>, manifest: PathBuf, entry: &Bound<'_, PyAny>) -> PyResult<()> {
    let json: String = py.import("json")?.call_method1("dumps", (entry,))?.extract()?;
    let entry: PackEntry =
        serde_json::from_str(&json).map_err(|e| to_py(Error::Usage(format!("not a manifest entry: {e}"))))?;
    py.detach(|| core_append(&manifest, &entry)).map_err(to_py)
}

/// GitHub device-flow login: calls `on_code(verification_uri, user_code)`, then waits
/// for the browser step (Ctrl-C aborts). Returns the access token.
#[pyfunction]
fn device_login(py: Python<'_>, client_id: &str, scope: &str, on_code: Bound<'_, PyAny>) -> PyResult<String> {
    let flow = DeviceFlow::from_env(client_id).map_err(to_py)?;
    let code = py.detach(|| flow.start(scope)).map_err(to_py)?;
    on_code.call1((&code.verification_uri, &code.user_code))?;
    let interrupted: Mutex<Option<PyErr>> = Mutex::new(None);
    let token = py.detach(|| {
        flow.wait(&code, || {
            Python::attach(|py| py.check_signals()).map_err(|e| *interrupted.lock().unwrap() = Some(e)).is_ok()
        })
    });
    match (token, interrupted.into_inner().unwrap()) {
        (_, Some(e)) => Err(e),
        (t, None) => t.map_err(to_py),
    }
}

#[pyfunction]
fn save_login(token: String, login: String) -> PyResult<()> {
    let mut s = Store::load().map_err(to_py)?;
    s.login = Some(Login { token, login });
    s.save().map_err(to_py)
}

/// Forgets the saved token; returns whether there was one.
#[pyfunction]
fn remove_login() -> PyResult<bool> {
    let mut s = Store::load().map_err(to_py)?;
    let removed = s.login.take().is_some();
    s.save().map_err(to_py)?;
    Ok(removed)
}

/// The saved repositories (not CARLA_PACK_REPOS).
#[pyfunction]
fn saved_repos() -> PyResult<Vec<String>> {
    Ok(Store::load().map_err(to_py)?.repos)
}

/// Returns false when the repository was already saved.
#[pyfunction]
fn add_repo(repo: &str) -> PyResult<bool> {
    let mut s = Store::load().map_err(to_py)?;
    let added = s.add_repo(repo).map_err(to_py)?;
    s.save().map_err(to_py)?;
    Ok(added)
}

#[pyfunction]
fn remove_repo(repo: &str) -> PyResult<bool> {
    let mut s = Store::load().map_err(to_py)?;
    let removed = s.remove_repo(repo);
    s.save().map_err(to_py)?;
    Ok(removed)
}

#[pymodule]
fn _core(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add("CarlaPackError", m.py().get_type::<CarlaPackError>())?;
    m.add("__version__", env!("CARGO_PKG_VERSION"))?;
    m.add_class::<Client>()?;
    m.add_function(wrap_pyfunction!(publish, m)?)?;
    m.add_function(wrap_pyfunction!(plan_publish, m)?)?;
    m.add_function(wrap_pyfunction!(append_entry, m)?)?;
    m.add_function(wrap_pyfunction!(device_login, m)?)?;
    m.add_function(wrap_pyfunction!(save_login, m)?)?;
    m.add_function(wrap_pyfunction!(remove_login, m)?)?;
    m.add_function(wrap_pyfunction!(saved_repos, m)?)?;
    m.add_function(wrap_pyfunction!(add_repo, m)?)?;
    m.add_function(wrap_pyfunction!(remove_repo, m)?)?;
    Ok(())
}
