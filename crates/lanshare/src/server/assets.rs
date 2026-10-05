//! 页面静态资源：编译时嵌进 exe，运行时不读磁盘。都不含任何秘密。

use axum::http::{HeaderValue, header};
use axum::response::{IntoResponse, Response};

macro_rules! asset {
    ($name:ident, $file:literal, $mime:literal) => {
        pub(crate) async fn $name() -> Response {
            let body: &'static [u8] = include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/web/", $file));
            let mut response = body.into_response();
            response.headers_mut().insert(header::CONTENT_TYPE, HeaderValue::from_static($mime));
            response
        }
    };
}

asset!(index, "index.html", "text/html; charset=utf-8");
asset!(app_js, "app.js", "text/javascript; charset=utf-8");
asset!(proto_js, "proto.js", "text/javascript; charset=utf-8");
asset!(style_css, "style.css", "text/css; charset=utf-8");
asset!(wasm, "lanshare.wasm", "application/wasm");
asset!(favicon, "icon.ico", "image/x-icon");
