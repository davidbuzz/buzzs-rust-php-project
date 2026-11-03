//! `CURLOPT_POSTFIELDS` arrays: `build_mime_structure_from_hash()` with
//! `CURLFile`/`CURLStringFile` uploads.

use crate::setopt::basedir_denied;
use crate::state::Curl;
use php_curl::{MimePart, MimeSource};
use php_runtime::{ArrayKey, Value};
use php_streams::stream::Stream;
use php_vm::ext::VmResult;
use php_vm::{object, Vm};

/// `CURLE_BAD_FUNCTION_ARGUMENT`.
const CURLE_BAD_FUNCTION_ARGUMENT: i32 = 43;
/// `CURLE_ABORTED_BY_CALLBACK`.
const CURLE_ABORTED_BY_CALLBACK: i32 = 42;

/// The body of a `CURLFile` part: the file is (re)opened lazily through
/// the stream layer (`read_cb`/`seek_cb`/`free_cb` in interface.c).
struct FileSource {
    filename: Vec<u8>,
    stream: Option<Stream>,
}

impl MimeSource for FileSource {
    fn read(&mut self, buf: &mut [u8]) -> Option<usize> {
        if self.stream.is_none() {
            let mut opened = None;
            self.stream =
                php_streams::open::open_wrapper(&self.filename, "rb", 0, &mut opened, None);
        }
        let s = self.stream.as_mut()?;
        if let Ok(n) = s.read(buf) {
            Some(n)
        } else {
            if let Some(mut s) = self.stream.take() {
                s.close();
            }
            None
        }
    }

    fn seek(&mut self, offset: i64, origin: i32) -> bool {
        match self.stream.as_mut() {
            Some(s) => s.seek(offset, origin) == 0,
            None => false,
        }
    }

    fn close(&mut self) {
        if let Some(mut s) = self.stream.take() {
            s.close();
        }
    }
}

fn instance_of(o: &php_runtime::Obj, lc: &[u8]) -> bool {
    object::class_of(o).instance_of_name(lc)
}

fn prop_string(vm: &mut Vm, o: &php_runtime::Obj, name: &str) -> VmResult<Option<Vec<u8>>> {
    let v = vm.get_property(o, name)?;
    Ok(match v.deref() {
        Value::Str(s) => Some(s.as_bytes().to_vec()),
        _ => None,
    })
}

/// `add_form_field()`: `Ok(Err(code))` aborts building.
fn add_form_field(
    vm: &mut Vm,
    fname: &str,
    curl: &Curl,
    parts: &mut Vec<MimePart>,
    key: &[u8],
    current: &Value,
    postfields: &Value,
) -> VmResult<Result<(), i32>> {
    let current = current.deref();
    if let Value::Array(_) = current {
        curl.err_no.set(CURLE_BAD_FUNCTION_ARGUMENT);
        return Err(vm.throw(
            "TypeError",
            &format!(
                "{fname}(): Nested arrays are not supported for key {}",
                String::from_utf8_lossy(key)
            ),
        ));
    }
    if let Value::Object(o) = &current {
        if instance_of(o, b"curlfile") {
            let Some(name) = prop_string(vm, o, "name")? else {
                vm.php_warning(&format!(
                    "Invalid filename for key {}",
                    String::from_utf8_lossy(key)
                ))?;
                return Ok(Ok(()));
            };
            if basedir_denied(vm, &name)? {
                return Ok(Err(CURLE_ABORTED_BY_CALLBACK));
            }
            let mime_type = prop_string(vm, o, "mime")?.filter(|m| !m.is_empty());
            let postname = prop_string(vm, o, "postname")?.filter(|m| !m.is_empty());
            curl.st.borrow_mut().postfields = Some(postfields.clone());
            let mut opened = None;
            let stream = php_ext_streams::bridge::run(vm, || {
                php_streams::open::open_wrapper(
                    &name,
                    "rb",
                    php_streams::wrapper::STREAM_MUST_SEEK,
                    &mut opened,
                    None,
                )
            })?;
            let mut size = -1;
            let seekable = stream.is_some();
            let stream = stream.map(|mut s| {
                if !s.is_filtered() {
                    if let Some(st) = s.stat() {
                        size = st.size;
                    }
                }
                s
            });
            parts.push(MimePart::Stream {
                name: key.to_vec(),
                filename: postname.unwrap_or_else(|| name.clone()),
                mime_type: mime_type.unwrap_or_else(|| b"application/octet-stream".to_vec()),
                size,
                seekable,
                source: Box::new(FileSource {
                    filename: name,
                    stream,
                }),
            });
            return Ok(Ok(()));
        }
        if instance_of(o, b"curlstringfile") {
            let postname = prop_string(vm, o, "postname")?.unwrap_or_default();
            let mime_type = prop_string(vm, o, "mime")?.unwrap_or_default();
            let data = prop_string(vm, o, "data")?.unwrap_or_default();
            curl.st.borrow_mut().postfields = Some(postfields.clone());
            parts.push(MimePart::Buffer {
                name: key.to_vec(),
                data,
                filename: postname,
                mime_type,
            });
            return Ok(Ok(()));
        }
    }
    let s = vm.value_to_string(&current)?;
    parts.push(MimePart::Data {
        name: key.to_vec(),
        data: s.as_bytes().to_vec(),
    });
    Ok(Ok(()))
}

/// `build_mime_structure_from_hash()`.
pub(crate) fn build_from_hash(
    vm: &mut Vm,
    fname: &str,
    curl: &Curl,
    postfields: &Value,
) -> VmResult<bool> {
    let Value::Array(arr) = postfields else {
        return Ok(false);
    };
    let mut parts = Vec::new();
    for (k, v) in arr.iter() {
        let key = match k {
            ArrayKey::Int(i) => i.to_string().into_bytes(),
            ArrayKey::Str(s) => s.as_bytes().to_vec(),
        };
        let r = if let Value::Array(inner) = v.deref() {
            let mut r = Ok(());
            for e in inner.values() {
                r = add_form_field(vm, fname, curl, &mut parts, &key, e, postfields)?;
                if r.is_err() {
                    break;
                }
            }
            r
        } else {
            add_form_field(vm, fname, curl, &mut parts, &key, v, postfields)?
        };
        if let Err(code) = r {
            curl.err_no.set(code);
            return Ok(false);
        }
    }
    let rc = curl.easy.set_mime(parts);
    curl.err_no.set(rc);
    Ok(rc == 0)
}
