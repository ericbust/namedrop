// Claude Bash matching treats '*' as any characters, including spaces. Keep
// assignments in the command string so this check cannot hide injection.
pub fn bash_rule_matches(rule: &str, command: &str) -> bool {
    // A rule for another tool, such as the Read rule for sent images, matches no command.
    let Some(pattern) = rule
        .strip_prefix("Bash(")
        .and_then(|rule| rule.strip_suffix(')'))
    else {
        return false;
    };
    let command = command.as_bytes();
    let mut matched = vec![false; command.len() + 1];
    matched[0] = true;
    for byte in pattern.bytes() {
        if byte == b'*' {
            for i in 1..matched.len() {
                matched[i] |= matched[i - 1];
            }
        } else {
            for i in (1..matched.len()).rev() {
                matched[i] = matched[i - 1] && command[i - 1] == byte;
            }
            matched[0] = false;
        }
    }
    matched[command.len()]
}
