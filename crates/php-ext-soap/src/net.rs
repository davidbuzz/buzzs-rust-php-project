//! The HTTP transport's network layer: `php-soap`'s [`HttpStream`] over
//! socket streams opened with `stream_socket_client()` (so every transport
//! registered with php-ext-streams — `tcp`, `ssl`, `tls`, … — is
//! available).

use std::rc::Rc;

use php_ext_streams::res::{cell_of, StreamCell};
use php_runtime::{PhpRef, Value};
use php_soap::host::{ConnectTarget, HttpStream};
use php_vm::Vm;

/// A connected socket stream.
struct SocketStream {
    /// The stream resource (kept alive while the connection is used).
    _resource: Value,
    cell: Rc<StreamCell>,
}

impl HttpStream for SocketStream {
    fn write(&mut self, data: &[u8]) -> usize {
        let Some(mut s) = self.cell.get() else {
            return 0;
        };
        let mut done = 0;
        while done < data.len() {
            match s.write(&data[done..]) {
                Ok(0) | Err(_) => break,
                Ok(n) => done += n,
            }
        }
        done
    }

    fn read_line(&mut self) -> Option<Vec<u8>> {
        self.cell.get()?.get_line(None)
    }

    fn read(&mut self, n: usize) -> Vec<u8> {
        self.cell
            .get()
            .and_then(|mut s| s.read_vec(n))
            .unwrap_or_default()
    }

    fn eof(&mut self) -> bool {
        self.cell.get().is_none_or(|mut s| s.eof())
    }
}

/// `php_stream_xport_create` for a SOAP connection (`http_connect`).
pub(crate) fn connect(vm: &mut Vm, t: &ConnectTarget) -> Option<Box<dyn HttpStream>> {
    let proto = if t.proxy_tunnel.is_some() {
        "tcp"
    } else {
        t.protocol.as_str()
    };
    let host = if t.host.contains(':') && !t.host.starts_with('[') {
        format!("[{}]", t.host)
    } else {
        t.host.clone()
    };
    let addr = format!("{proto}://{host}:{}", t.port);
    #[allow(clippy::cast_precision_loss)]
    let timeout = t.timeout.map_or_else(
        || vm.ini_long("default_socket_timeout") as f64,
        |s| s as f64,
    );
    let errno = Value::Reference(PhpRef::new(Value::Null));
    let errstr = Value::Reference(PhpRef::new(Value::Null));
    let r = crate::host::silenced(vm, |vm| {
        vm.call_value(
            &Value::from("stream_socket_client"),
            vec![Value::from(addr.as_str()), errno, errstr, Value::Float(timeout)],
        )
    });
    let res = match r {
        Ok(v @ Value::Resource(_)) => v,
        _ => return None,
    };
    let Value::Resource(r) = &res else {
        return None;
    };
    let cell = cell_of(r)?;
    let mut stream = SocketStream {
        _resource: res.clone(),
        cell,
    };
    if let Some((request, _method)) = &t.proxy_tunnel {
        let mut req = request.clone();
        req.extend_from_slice(b"\r\n");
        if stream.write(&req) != req.len() {
            return None;
        }
        let status = stream.read_line()?;
        let ok = status.starts_with(b"HTTP/1.") && status.get(9..12) == Some(b"200");
        loop {
            match stream.read_line() {
                Some(l) if l != b"\r\n" && l != b"\n" => {}
                _ => break,
            }
        }
        if !ok {
            return None;
        }
        let method = vm
            .constants
            .get(b"STREAM_CRYPTO_METHOD_TLS_CLIENT".as_slice())
            .cloned()
            .unwrap_or(Value::Int(121));
        let enabled = crate::host::silenced(vm, |vm| {
            vm.call_value(
                &Value::from("stream_socket_enable_crypto"),
                vec![res.clone(), Value::Bool(true), method],
            )
        });
        if !matches!(enabled, Ok(Value::Bool(true))) {
            return None;
        }
    }
    Some(Box::new(stream))
}
