//! PyO3 bindings: the `carla_pack_registry._core` extension module used by the Python frontend.
//! Thin wrappers only; network calls release the GIL and JSON values cross the
//! boundary as Python dicts/lists.
use std::path::PathBuf;
use std::sync::Mutex;

use pyo3::exceptions::PyException;
use pyo3::prelude::*;
use serde::Serialize;
use serde_json::Value;

use crate::auth::DeviceFlow;
use crate::client::{AclChange, AclRef, Client as CoreClient, display_ref};
use crate::error::{Error, Result};
use crate::store::{Login, Store};

pyo3::create_exception!(_core, CarlaPackError, PyException, "Error from the carla-pack core. args = (message, code).");

fn to_py(e: Error) -> PyErr {
    CarlaPackError::new_err((e.to_string(), e.code().to_string()))
}

fn to_object<'py>(py: Python<'py>, v: &impl Serialize) -> PyResult<Bound<'py, PyAny>> {
    let s = serde_json::to_string(v).map_err(|e| to_py(e.into()))?;
    py.import("json")?.call_method1("loads", (s,))
}

fn from_object(obj: &Bound<'_, PyAny>) -> PyResult<Value> {
    let s: String = obj.py().import("json")?.call_method1("dumps", (obj,))?.extract()?;
    serde_json::from_str(&s).map_err(|e| to_py(e.into()))
}

/// Runs `f` without the GIL and converts its result to Python objects.
fn call<'py, T: Serialize + Send>(
    py: Python<'py>,
    f: impl FnOnce() -> Result<T> + Send,
) -> PyResult<Bound<'py, PyAny>> {
    let v = py.detach(f).map_err(to_py)?;
    to_object(py, &v)
}

#[pyclass(frozen)]
struct Client {
    inner: CoreClient,
}

#[pymethods]
impl Client {
    /// Registry from the argument / CARLA_PACK_REGISTRY / last login; token from
    /// CARLA_PACK_TOKEN / the saved login. Raises not_logged_in when `require_auth`.
    #[new]
    #[pyo3(signature = (registry=None, require_auth=true))]
    fn new(registry: Option<&str>, require_auth: bool) -> PyResult<Self> {
        Ok(Self { inner: CoreClient::from_store(registry, require_auth).map_err(to_py)? })
    }

    fn with_token(&self, token: String) -> Self {
        Self { inner: self.inner.with_token(token) }
    }

    #[getter]
    fn registry_url(&self) -> &str {
        self.inner.registry_url()
    }

    fn cli_config<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        call(py, || self.inner.cli_config())
    }

    fn me<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        call(py, || self.inner.me())
    }

    fn list_buckets<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        call(py, || self.inner.list_buckets())
    }

    /// Full bucket view for "owner/name", "name" or a UUID.
    fn resolve_bucket<'py>(&self, py: Python<'py>, reference: &str) -> PyResult<Bound<'py, PyAny>> {
        call(py, || self.inner.resolve_bucket(reference))
    }

    fn create_bucket<'py>(&self, py: Python<'py>, body: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
        let body = from_object(body)?;
        call(py, || self.inner.create_bucket(&body))
    }

    fn update_bucket<'py>(&self, py: Python<'py>, id: &str, body: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
        let body = from_object(body)?;
        call(py, || self.inner.update_bucket(id, &body))
    }

    fn delete_bucket(&self, py: Python<'_>, id: &str) -> PyResult<()> {
        py.detach(|| self.inner.delete_bucket(id)).map_err(to_py)
    }

    fn verify_bucket<'py>(&self, py: Python<'py>, id: &str) -> PyResult<Bound<'py, PyAny>> {
        call(py, || self.inner.verify_bucket(id))
    }

    fn put_credentials<'py>(&self, py: Python<'py>, id: &str, body: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
        let body = from_object(body)?;
        call(py, || self.inner.put_credentials(id, &body))
    }

    fn get_acl<'py>(&self, py: Python<'py>, id: &str) -> PyResult<Bound<'py, PyAny>> {
        call(py, || self.inner.get_acl(id))
    }

    /// mode: "set" | "add" | "rm"; teams as "ORG/SLUG".
    #[pyo3(signature = (id, mode, users=vec![], orgs=vec![], teams=vec![]))]
    fn update_acl<'py>(
        &self,
        py: Python<'py>,
        id: &str,
        mode: &str,
        users: Vec<String>,
        orgs: Vec<String>,
        teams: Vec<String>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let change = match mode {
            "set" => AclChange::Set,
            "add" => AclChange::Add,
            "rm" => AclChange::Remove,
            _ => return Err(to_py(Error::Usage(format!("unknown ACL mode {mode:?}")))),
        };
        let refs = [("user", users), ("org", orgs), ("team", teams)]
            .into_iter()
            .flat_map(|(kind, values)| values.into_iter().map(move |v| AclRef::parse(kind, &v)))
            .collect::<Result<Vec<_>>>()
            .map_err(to_py)?;
        call(py, || self.inner.update_acl(id, change, refs))
    }

    /// Every object under `prefix`: [{key, size}].
    #[pyo3(signature = (id, prefix=None))]
    fn list_objects<'py>(&self, py: Python<'py>, id: &str, prefix: Option<&str>) -> PyResult<Bound<'py, PyAny>> {
        call(py, || self.inner.list_objects(id, prefix))
    }

    /// Downloads into `out_dir`; `on_event(dict)` is called per object. Returns bytes downloaded.
    #[pyo3(signature = (id, targets, out_dir, force=false, jobs=4, on_event=None))]
    #[allow(clippy::too_many_arguments)] // mirrors the Python keyword arguments
    fn pull(
        &self,
        py: Python<'_>,
        id: &str,
        targets: Vec<String>,
        out_dir: PathBuf,
        force: bool,
        jobs: usize,
        on_event: Option<Py<PyAny>>,
    ) -> PyResult<u64> {
        let callback_error: Mutex<Option<PyErr>> = Mutex::new(None);
        let total = py
            .detach(|| {
                self.inner.pull(id, &targets, &out_dir, force, jobs, |event| {
                    if let Some(cb) = &on_event {
                        Python::attach(|py| {
                            if let Err(e) = to_object(py, &event).and_then(|ev| cb.call1(py, (ev,))) {
                                callback_error.lock().unwrap().get_or_insert(e);
                            }
                        });
                    }
                })
            })
            .map_err(to_py)?;
        match callback_error.into_inner().unwrap() {
            Some(e) => Err(e),
            None => Ok(total),
        }
    }
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

/// "owner/name" for a bucket view.
#[pyfunction]
fn bucket_ref(bucket: &Bound<'_, PyAny>) -> PyResult<String> {
    Ok(display_ref(&from_object(bucket)?))
}

#[pyfunction]
fn save_login(registry: &str, token: String, login: String) -> PyResult<()> {
    let mut s = Store::load().map_err(to_py)?;
    s.set_login(registry, Login { token, login });
    s.save().map_err(to_py)
}

/// Forgets the token for the selected registry; returns (registry, removed).
#[pyfunction]
#[pyo3(signature = (registry=None))]
fn remove_login(registry: Option<&str>) -> PyResult<(String, bool)> {
    let mut s = Store::load().map_err(to_py)?;
    let registry = s.registry(registry);
    let removed = s.remove_login(&registry);
    s.save().map_err(to_py)?;
    Ok((registry, removed))
}

#[pymodule]
fn _core(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add("CarlaPackError", m.py().get_type::<CarlaPackError>())?;
    m.add("__version__", env!("CARGO_PKG_VERSION"))?;
    m.add_class::<Client>()?;
    m.add_function(wrap_pyfunction!(device_login, m)?)?;
    m.add_function(wrap_pyfunction!(bucket_ref, m)?)?;
    m.add_function(wrap_pyfunction!(save_login, m)?)?;
    m.add_function(wrap_pyfunction!(remove_login, m)?)?;
    Ok(())
}
