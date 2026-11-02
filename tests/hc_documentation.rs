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
            let (command, input) = match line.split_once(" --input-json '") {
                Some((command, input)) => (command, Some(input.strip_suffix('\'').unwrap())),
                None => (line, None),
            };
            let mut words: Vec<String> = command.split_whitespace().map(str::to_owned).collect();
            if let Some(input) = input {
                words.push("--input-json".to_owned());
                words.push(input.to_owned());
            }
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
    assert!(
        commands.iter().any(|(_, cli)| {
            cli.operation() == "name"
                && cli.agent_name() == Some("@build")
                && cli.name_pane_type() == Some(namedrop::PaneType::Terminal)
        }),
        "README needs a parseable hc plain-terminal naming example"
    );
    assert!(
        commands.iter().any(|(_, cli)| {
            cli.operation() == "send"
                && cli
                    .input()
                    .ok()
                    .and_then(|input| input["source"].as_str().map(str::to_owned))
                    == Some("@build printf hello-from-Namedrop".to_owned())
        }),
        "README needs a parseable hc send example with valid JSON and a shell command"
    );
    assert!(
        readme.contains("Its reply includes the command output and exit status."),
        "README needs to explain the terminal reply"
    );
    for (_, cli) in commands.iter().filter(|(_, cli)| cli.operation() == "send") {
        namedrop::validate_message_input("send", cli.input().unwrap())
            .expect("README send example must pass operation validation");
    }
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
