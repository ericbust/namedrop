"""Mutation checks for the literals that Words must send to classification."""
import tempfile
from pathlib import Path

import importlib.util
_spec = importlib.util.spec_from_file_location("words_literals", Path(__file__).with_name("words_literals.py"))
_module = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(_module)
scan = _module.scan


def check(root, name, path, source, expected):
    target = root / path
    target.parent.mkdir(parents=True, exist_ok=True)
    target.write_text(source)
    try:
        literals = scan(root)
        rejected = [item for item in literals if item["role"] == "plain"]
        assert any(expected in item["text"] for item in rejected), (name, literals)
        print(f"rejected unclassified mutation: {name}")
    finally:
        target.unlink()


def main():
    with tempfile.TemporaryDirectory(prefix="nd-words-") as directory:
        root = Path(directory)
        (root / "src").mkdir()
        (root / "scripts").mkdir()
        cases = [
            ("tuple", "src/new.rs", 'fn f() { let _ = ("Close the Herdr popup", "pane"); }', "Herdr"),
            ("three-tuple", "src/new.rs", 'fn f() { let _ = ("Close the Herdr popup", "pane", 0); }', "Herdr"),
            ("Some tuple", "src/new.rs", 'fn f() { let _ = Some(("Close the Herdr popup", "pane")); }', "Herdr"),
            ("negated test-support", "src/new.rs", '#[cfg(not(feature = "test-support"))] fn f() { println!("Herdr popup"); }', "Herdr"),
            ("production tests filename", "src/new_tests.rs", 'fn f() { println!("Herdr popup"); }', "Herdr"),
            ("production tests.rs filename", "src/tests.rs", 'fn f() { println!("Herdr popup"); }', "Herdr"),
            ("shell unquoted", "scripts/new.sh", "echo Close the Herdr popup\n", "Herdr"),
            ("shell Rex space", "scripts/new.sh", "echo Rex space\n", "Rex space"),
            ("shell PY text heredoc", "scripts/new.sh", "cat <<PY\nHerdr popup\nPY\n", "Herdr"),
            ("shell unquoted heredoc", "scripts/new.sh", "cat <<EOF\nClose the Herdr popup\nEOF\n", "Herdr"),
            ("shell quoted heredoc", "scripts/new.sh", "cat <<'EOF'\nClose the Herdr popup\nEOF\n", "Herdr"),
            ("shell tab-stripped heredoc", "scripts/new.sh", "cat <<-EOF\n\tClose the Herdr popup\n\tEOF\n", "Herdr"),
            ("Python parenthesized string", "scripts/new.py", 'message = (\n"""Close the Herdr popup"""\n)\n', "Herdr"),
            ("Python same-line docstring", "scripts/new.py", '\"\"\"Docs\"\"\"; print("Herdr popup")\n', "Herdr"),
            ("Python non-docstring expression", "scripts/new.py", 'x = 1\n"""Close the Herdr popup"""\n', "Herdr"),
            ("Prefix", "src/new.rs", 'fn f() { println!("Press Prefix"); }', "Prefix"),
            ("Rex space", "src/new.rs", 'fn f() { println!("Rex space"); }', "Rex space"),
            ("Rex workspace", "src/new.rs", 'fn f() { println!("Rex workspace"); }', "Rex workspace"),
            ("Lua quoted", "src/rex_ui/module.lua", 'error("Herdr popup")', "Herdr"),
            ("Lua long string", "src/rex_ui/module.lua", 'error([=[Herdr popup]=])', "Herdr"),
            # A brace inside a string is no brace of the test module's body,
            # so the literal after the module is still read.
            ("brace in a test string", "src/new.rs", '#[cfg(test)]\nmod tests { fn t() { let _ = "{"; } }\nfn f() { println!("Herdr popup"); }', "Herdr"),
        ]
        for case in cases:
            check(root, *case)
        (root / "src/lib.rs").write_text('#[cfg(test)]\n#[path = "actual_tests.rs"]\nmod tests;\n#[cfg(test)] mod nested;\n')
        (root / "src/actual_tests.rs").write_text('fn f() { println!("Herdr popup"); }')
        (root / "src/nested.rs").write_text('fn f() { println!("Herdr popup"); }')
        (root / "scripts/doc.py").write_text('"""Herdr module documentation."""\ndef f():\n    """Herdr function documentation."""\n')
        (root / "src/rex_ui/module.lua").write_text('-- Herdr popup\n--[=[Herdr popup]=]\n')
        assert scan(root) == [], scan(root)
        print(f"{len(cases)} mutations rejected; declared test modules, docstrings and Lua comments excluded")


if __name__ == "__main__":
    main()
