//! PyO3 bindings: the `carla_pack._core` extension module used by the Python frontend.
//! Network calls release the GIL. JSON values cross the boundary as Python dicts/lists.
use std::path::PathBuf;

use pyo3::exceptions::PyException;
use pyo3::prelude::*;
use serde_json::Value;

use crate::auth::{DeviceFlow as CoreDeviceFlow, Poll};
use crate::client::Client as CoreClient;
use crate::error::Error;
use crate::store::{Login, Store};

pyo3::create_exception!(
    _core,
    CarlaPackError,
    PyException,
    "Error raised by the carla-pack core. args = (message, code, status)."
);

fn to_py(e: Error) -> PyErr {
    match &e {
        Error::Api { status, code, message } => CarlaPackError::new_err((message.clone(), code.clone(), *status)),
        Error::NotLoggedIn(_) => CarlaPackError::new_err((e.to_string(), "not_logged_in", 0u16)),
        _ => CarlaPackError::new_err((e.to_string(), "client_error", 0u16)),
    }
}

fn to_object<'py>(py: Python<'py>, v: &Value) -> PyResult<Bound<'py, PyAny>> {
    py.import("json")?.call_method1("loads", (v.to_string(),))
}

fn from_object(obj: &Bound<'_, PyAny>) -> PyResult<Value> {
    let s: String = obj.py().import("json")?.call_method1("dumps", (obj,))?.extract()?;
    serde_json::from_str(&s).map_err(|e| to_py(e.into()))
}

#[pyclass(frozen)]
struct Client {
    inner: CoreClient,
}

#[pymethods]
impl Client {
    #[new]
    #[pyo3(signature = (registry_url, token=None))]
    fn new(registry_url: &str, token: Option<String>) -> PyResult<Self> {
        Ok(Self { inner: CoreClient::new(registry_url, token).map_err(to_py)? })
    }

    #[getter]
    fn registry_url(&self) -> &str {
        self.inner.registry_url()
    }

    fn cli_config<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let v = py.detach(|| self.inner.cli_config()).map_err(to_py)?;
        to_object(py, &v)
    }

    fn me<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let v = py.detach(|| self.inner.me()).map_err(to_py)?;
        to_object(py, &v)
    }

    fn list_buckets<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let v = py.detach(|| self.inner.list_buckets()).map_err(to_py)?;
        to_object(py, &v["buckets"])
    }

    fn get_bucket<'py>(&self, py: Python<'py>, id: &str) -> PyResult<Bound<'py, PyAny>> {
        let v = py.detach(|| self.inner.get_bucket(id)).map_err(to_py)?;
        to_object(py, &v)
    }

    /// Resolves "owner/name", "name" or a UUID; returns the full bucket view.
    fn resolve_bucket<'py>(&self, py: Python<'py>, reference: &str) -> PyResult<Bound<'py, PyAny>> {
        let v = py
            .detach(|| self.inner.resolve_bucket(reference).and_then(|b| self.inner.get_bucket(&b.id)))
            .map_err(to_py)?;
        to_object(py, &v)
    }

    fn create_bucket<'py>(&self, py: Python<'py>, body: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
        let body = from_object(body)?;
        let v = py.detach(|| self.inner.create_bucket(&body)).map_err(to_py)?;
        to_object(py, &v)
    }

    fn update_bucket<'py>(&self, py: Python<'py>, id: &str, body: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
        let body = from_object(body)?;
        let v = py.detach(|| self.inner.update_bucket(id, &body)).map_err(to_py)?;
        to_object(py, &v)
    }

    fn delete_bucket(&self, py: Python<'_>, id: &str) -> PyResult<()> {
        py.detach(|| self.inner.delete_bucket(id)).map_err(to_py)
    }

    fn verify_bucket<'py>(&self, py: Python<'py>, id: &str) -> PyResult<Bound<'py, PyAny>> {
        let v = py.detach(|| self.inner.verify_bucket(id)).map_err(to_py)?;
        to_object(py, &v)
    }

    fn put_credentials<'py>(&self, py: Python<'py>, id: &str, body: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
        let body = from_object(body)?;
        let v = py.detach(|| self.inner.put_credentials(id, &body)).map_err(to_py)?;
        to_object(py, &v)
    }

    fn get_acl<'py>(&self, py: Python<'py>, id: &str) -> PyResult<Bound<'py, PyAny>> {
        let v = py.detach(|| self.inner.get_acl(id)).map_err(to_py)?;
        to_object(py, &v["entries"])
    }

    fn put_acl<'py>(&self, py: Python<'py>, id: &str, entries: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
        let entries = from_object(entries)?;
        let v = py.detach(|| self.inner.put_acl(id, &entries)).map_err(to_py)?;
        to_object(py, &v["entries"])
    }

    #[pyo3(signature = (id, prefix=None, cursor=None, limit=100))]
    fn list_objects<'py>(
        &self,
        py: Python<'py>,
        id: &str,
        prefix: Option<&str>,
        cursor: Option<&str>,
        limit: u32,
    ) -> PyResult<Bound<'py, PyAny>> {
        let v = py.detach(|| self.inner.list_objects(id, prefix, cursor, limit)).map_err(to_py)?;
        to_object(py, &v)
    }

    /// Every object under `prefix` as (key, size) tuples.
    #[pyo3(signature = (id, prefix=None))]
    fn list_all_objects(&self, py: Python<'_>, id: &str, prefix: Option<&str>) -> PyResult<Vec<(String, u64)>> {
        let objs = py.detach(|| self.inner.list_all_objects(id, prefix)).map_err(to_py)?;
        Ok(objs.into_iter().map(|o| (o.key, o.size)).collect())
    }

    /// Downloads one object to `dest`; returns bytes written.
    fn download(&self, py: Python<'_>, id: &str, key: &str, dest: PathBuf) -> PyResult<u64> {
        py.detach(|| self.inner.download(id, key, &dest)).map_err(to_py)
    }
}

#[pyclass(frozen)]
struct DeviceFlow {
    inner: CoreDeviceFlow,
}

#[pymethods]
impl DeviceFlow {
    #[new]
    fn new(github_url: &str, client_id: &str) -> PyResult<Self> {
        Ok(Self { inner: CoreDeviceFlow::new(github_url, client_id).map_err(to_py)? })
    }

    /// Returns {device_code, user_code, verification_uri, expires_in, interval}.
    fn start<'py>(&self, py: Python<'py>, scope: &str) -> PyResult<Bound<'py, PyAny>> {
        let code = py.detach(|| self.inner.start(scope)).map_err(to_py)?;
        to_object(py, &serde_json::to_value(code).map_err(|e| to_py(e.into()))?)
    }

    /// One poll: ("token", token) | ("pending", None) | ("slow_down", new_interval).
    fn poll(&self, py: Python<'_>, device_code: &str) -> PyResult<(String, Option<String>, Option<u64>)> {
        Ok(match py.detach(|| self.inner.poll(device_code)).map_err(to_py)? {
            Poll::Token(t) => ("token".into(), Some(t), None),
            Poll::Pending => ("pending".into(), None, None),
            Poll::SlowDown(i) => ("slow_down".into(), None, Some(i)),
        })
    }
}

/// Registry URL to use (argument, CARLA_PACK_REGISTRY, saved default, then localhost).
#[pyfunction]
#[pyo3(signature = (explicit=None))]
fn resolve_registry(explicit: Option<&str>) -> PyResult<String> {
    Ok(Store::load().map_err(to_py)?.registry(explicit))
}

/// Token for a registry (CARLA_PACK_TOKEN, then the saved login), or None.
#[pyfunction]
fn stored_token(registry: &str) -> PyResult<Option<String>> {
    Ok(Store::load().map_err(to_py)?.token(registry))
}

#[pyfunction]
fn save_login(registry: &str, token: String, login: String) -> PyResult<()> {
    let mut s = Store::load().map_err(to_py)?;
    s.set_login(registry, Login { token, login });
    s.save().map_err(to_py)
}

#[pyfunction]
fn remove_login(registry: &str) -> PyResult<bool> {
    let mut s = Store::load().map_err(to_py)?;
    let removed = s.remove_login(registry);
    s.save().map_err(to_py)?;
    Ok(removed)
}

#[pyfunction]
fn config_dir() -> PyResult<PathBuf> {
    crate::store::config_dir().map_err(to_py)
}

#[pymodule]
fn _core(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add("CarlaPackError", m.py().get_type::<CarlaPackError>())?;
    m.add("__version__", env!("CARGO_PKG_VERSION"))?;
    m.add_class::<Client>()?;
    m.add_class::<DeviceFlow>()?;
    m.add_function(wrap_pyfunction!(resolve_registry, m)?)?;
    m.add_function(wrap_pyfunction!(stored_token, m)?)?;
    m.add_function(wrap_pyfunction!(save_login, m)?)?;
    m.add_function(wrap_pyfunction!(remove_login, m)?)?;
    m.add_function(wrap_pyfunction!(config_dir, m)?)?;
    Ok(())
}
