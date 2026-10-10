//! The page renders untrusted collaborator text, so the escaping rule (text
//! goes in through `textContent` only) and the no-CDN rule are build failures.
const ASSETS: [(&str, &str); 3] = [
    (
        "index.html",
        include_str!("../src/dashboard/assets/index.html"),
    ),
    ("app.js", include_str!("../src/dashboard/assets/app.js")),
    ("app.css", include_str!("../src/dashboard/assets/app.css")),
];

#[test]
fn assets_never_parse_strings_as_markup_or_code() {
    for (name, source) in ASSETS {
        for forbidden in [
            "innerHTML",
            "outerHTML",
            "insertAdjacentHTML",
            "document.write",
            "eval(",
            "new Function",
            "setTimeout(\"",
            "setInterval(\"",
        ] {
            assert!(!source.contains(forbidden), "{name} uses {forbidden}");
        }
    }
}

#[test]
fn assets_load_nothing_from_the_network() {
    for (name, source) in ASSETS {
        for forbidden in ["http://", "https://", "//cdn", "@import"] {
            assert!(!source.contains(forbidden), "{name} references {forbidden}");
        }
    }
}

#[test]
fn the_page_has_no_inline_script_or_style() {
    let (_, html) = ASSETS[0];
    for tag in html.split("<script").skip(1) {
        let open = &tag[..tag.find('>').unwrap()];
        assert!(open.contains("src="), "inline <script{open}>");
    }
    for forbidden in ["<style", " style=", " onclick=", " onload=", " onerror="] {
        assert!(!html.contains(forbidden), "index.html has {forbidden}");
    }
}
