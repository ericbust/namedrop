use clap::Parser;
use namedrop::cli::Cli;

fn documented_commands(document: &str) -> Vec<(&str, Cli)> {
    document
        .lines()
        .filter(|line| {
            line.starts_with("namedrop --host hc ")
                || line.starts_with(
                    "/absolute/path/namedrop-<version>-<target>/bin/namedrop --host hc ",
                )
        })
        .map(|line| {
            let mut words: Vec<String> = line
                .split_whitespace()
                .map(|word| word.trim_matches('\'').to_owned())
                .collect();
            words[0] = "namedrop".to_owned();
            let cli = Cli::try_parse_from(words).unwrap_or_else(|error| panic!("{line}: {error}"));
            (line, cli)
        })
        .collect()
}

#[test]
fn app_install_examples_parse_as_the_documented_operations() {
    let readme = include_str!("../README.md");
    let commands = documented_commands(readme);
    for operation in ["activate", "name", "roster"] {
        assert!(
            commands.iter().any(|(_, cli)| cli.operation() == operation),
            "README needs a parseable hc {operation} example"
        );
    }
    let (_, activation) = commands
        .iter()
        .find(|(_, cli)| cli.operation() == "activate")
        .unwrap();
    assert_eq!(activation.input().unwrap()["confirmed"], true);
    assert!(commands.iter().any(|(_, cli)| {
        cli.operation() == "name"
            && cli.agent_name() == Some("build")
            && cli.name_pane_type() == Some(namedrop::PaneType::Terminal)
    }), "README needs a parseable hc plain-terminal naming example");
    assert!(commands.iter().any(|(_, cli)| {
        cli.operation() == "send"
            && cli.input().ok().and_then(|input| input["source"].as_str().map(str::to_owned))
                == Some("@build printf 'hello from Namedrop'".to_owned())
    }), "README needs a parseable hc send example with valid JSON and a shell command");
    assert!(readme.contains("exit status") && readme.contains("output"), "README needs to explain the terminal reply");
}

#[test]
fn app_operations_examples_parse_as_hc_commands() {
    let operations = include_str!("../docs/operations.md");
    let commands = documented_commands(operations);
    for operation in ["roster", "events"] {
        assert!(
            commands.iter().any(|(_, cli)| cli.operation() == operation),
            "operations needs a parseable hc {operation} example"
        );
    }
    let (_, ensure) = commands
        .iter()
        .find(|(_, cli)| cli.operation() == "events")
        .unwrap();
    assert!(ensure.events_ensure());
}
