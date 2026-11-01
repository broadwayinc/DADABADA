//! The web admin UI: static files built into the binary.

use super::http::{full, Body};
use hyper::Response;

const INDEX: &str = include_str!("../ui/index.html");
const CSS: &str = include_str!("../ui/app.css");
const JS: &str = include_str!("../ui/app.js");
const THEME_JS: &str = include_str!("../ui/theme.js");
const FONT_LATIN: &[u8] = include_bytes!("../ui/jetbrains-mono-latin.woff2");
const FONT_LATIN_EXT: &[u8] = include_bytes!("../ui/jetbrains-mono-latin-ext.woff2");
const FAVICON: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 64 64" fill="none" stroke="#FFFFFF" stroke-width="6" stroke-linecap="round" stroke-linejoin="round"><rect width="64" height="64" rx="14" fill="#111111" stroke="none"/><path d="M16 20 C21 13 27 13 32 20 S43 27 48 20"/><path d="M16 32 C21 25 27 25 32 32 S43 39 48 32"/><path d="M16 44 C21 37 27 37 32 44 S43 51 48 44"/></svg>"##;

const CSP: &str = "default-src 'none'; script-src 'self'; style-src 'self'; font-src 'self'; img-src 'self' data:; connect-src 'self' https://cdn.broadwayinc.com; base-uri 'none'; form-action 'none'; frame-ancestors 'none'";

pub fn serve(path: &str) -> Option<Response<Body>> {
    let (ctype, body, cache): (&str, Vec<u8>, &str) = match path {
        "/" | "/index.html" => ("text/html; charset=utf-8", INDEX.as_bytes().to_vec(), "no-store"),
        "/ui/app.css" => ("text/css; charset=utf-8", CSS.as_bytes().to_vec(), "no-cache"),
        "/ui/app.js" => ("text/javascript; charset=utf-8", JS.as_bytes().to_vec(), "no-cache"),
        "/ui/theme.js" => ("text/javascript; charset=utf-8", THEME_JS.as_bytes().to_vec(), "no-cache"),
        "/ui/fonts/jetbrains-mono-latin.woff2" => ("font/woff2", FONT_LATIN.to_vec(), "public, max-age=31536000, immutable"),
        "/ui/fonts/jetbrains-mono-latin-ext.woff2" => ("font/woff2", FONT_LATIN_EXT.to_vec(), "public, max-age=31536000, immutable"),
        "/favicon.svg" | "/favicon.ico" => ("image/svg+xml", FAVICON.as_bytes().to_vec(), "public, max-age=86400"),
        _ => return None,
    };
    Some(
        Response::builder()
            .status(200)
            .header("content-type", ctype)
            .header("cache-control", cache)
            .header("content-security-policy", CSP)
            .header("x-content-type-options", "nosniff")
            .header("x-frame-options", "DENY")
            .header("referrer-policy", "no-referrer")
            .body(full(body))
            .unwrap(),
    )
}
