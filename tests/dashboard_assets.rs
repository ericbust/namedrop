//! The dashboard renders untrusted collaborator text, and the server sends
//! `script-src 'self'; style-src 'self'`. These assertions keep the embedded
//! assets inside that boundary: no HTML-parsing DOM sinks, no dynamic code,
//! no inline script or style, and nothing loaded from the network.

const INDEX_HTML: &str = include_str!("../src/dashboard/assets/index.html");
const APP_JS: &str = include_str!("../src/dashboard/assets/app.js");
const APP_CSS: &str = include_str!("../src/dashboard/assets/app.css");

const ASSETS: [(&str, &str); 3] = [
    ("index.html", INDEX_HTML),
    ("app.js", APP_JS),
    ("app.css", APP_CSS),
];

/// createElementNS needs the SVG namespace name. It is an identifier, never fetched.
const SVG_NAMESPACE: &str = "http://www.w3.org/2000/svg";

fn offenders(source: &str, needles: &[&str]) -> Vec<String> {
    needles
        .iter()
        .filter(|needle| source.contains(*needle))
        .map(|needle| needle.to_string())
        .collect()
}

#[test]
fn script_never_uses_html_parsing_sinks_or_dynamic_code() {
    let forbidden = [
        "innerHTML",
        "outerHTML",
        "insertAdjacentHTML",
        "document.write",
        "eval(",
        "new Function",
        "setAttribute('style'",
        "setAttribute(\"style\"",
        "createContextualFragment",
        "DOMParser",
        "srcdoc",
    ];
    assert_eq!(
        offenders(APP_JS, &forbidden),
        Vec::<String>::new(),
        "app.js must build DOM through el()/svg() with textContent only"
    );
}

#[test]
fn string_timers_are_not_used_as_eval() {
    for timer in ["setTimeout(", "setInterval("] {
        for (index, _) in APP_JS.match_indices(timer) {
            let rest = APP_JS[index + timer.len()..].trim_start();
            assert!(
                !rest.starts_with('\'') && !rest.starts_with('"') && !rest.starts_with('`'),
                "{timer} at byte {index} is passed a string, which evaluates code"
            );
        }
    }
}

#[test]
fn no_asset_loads_anything_from_the_network() {
    for (name, source) in ASSETS {
        let scrubbed = source.replace(SVG_NAMESPACE, "");
        assert_eq!(
            offenders(&scrubbed, &["http://", "https://", "//cdn", "@import"]),
            Vec::<String>::new(),
            "{name} references a remote URL"
        );
    }
}

#[test]
fn index_page_has_no_inline_script_or_style() {
    assert_eq!(
        offenders(
            INDEX_HTML,
            &["<style", " style=", " onload=", " onclick=", "javascript:"]
        ),
        Vec::<String>::new()
    );
    let scripts: Vec<&str> = INDEX_HTML
        .match_indices("<script")
        .map(|(i, _)| &INDEX_HTML[i..])
        .collect();
    assert_eq!(scripts.len(), 1, "exactly one script tag");
    assert!(
        scripts[0].starts_with("<script src=\"/assets/app.js\""),
        "the only script is the external app.js"
    );
    assert!(INDEX_HTML.contains("<link rel=\"stylesheet\" href=\"/assets/app.css\">"));
}

#[test]
fn stylesheet_uses_system_fonts_only() {
    assert_eq!(
        offenders(APP_CSS, &["@font-face", "url("]),
        Vec::<String>::new()
    );
}
