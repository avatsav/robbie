// Commands are frontend input, never messages for the agent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Command {
    Help,
    Compact,
    Exit,
}

pub(super) struct Definition {
    pub command: Command,
    pub name: &'static str,
    pub aliases: &'static [&'static str],
    pub description: &'static str,
}

impl Definition {
    // Primary name first; aliases parse and complete just like it.
    pub(super) fn names(&self) -> impl Iterator<Item = &'static str> {
        std::iter::once(self.name).chain(self.aliases.iter().copied())
    }

    pub(super) fn label(&self) -> String {
        self.names().collect::<Vec<_>>().join(", ")
    }
}

// One fixed list for execution, help, and completion; not a runtime registry.
pub(super) const COMMANDS: &[Definition] = &[
    Definition {
        command: Command::Help,
        name: "/help",
        aliases: &[],
        description: "show this help (no model request)",
    },
    Definition {
        command: Command::Compact,
        name: "/compact",
        aliases: &[],
        description: "compact context (may make a billed summary request)",
    },
    Definition {
        command: Command::Exit,
        name: "/exit",
        aliases: &["/quit"],
        description: "exit Robbie",
    },
];

pub(super) fn help() -> String {
    let mut text = String::from("Commands (case-sensitive):\n");
    for definition in COMMANDS {
        text.push_str(&format!(
            "{} — {}\n",
            definition.label(),
            definition.description
        ));
    }
    text.push_str("\n↑/↓ choose a suggestion; Tab or Enter fills it; Enter runs a complete command; Esc closes suggestions.\nA leading / selects a command. To discuss a path, start with text, e.g. explain /tmp/file.\nCtrl+C also exits; Ctrl+O cycles tool output; Ctrl+G toggles the mascot.");
    text
}

// None means an ordinary prompt; an error is still local command input.
pub(super) fn parse(input: &str) -> Result<Option<Command>, &'static str> {
    let input = input.trim();
    if !input.starts_with('/') {
        return Ok(None);
    }
    let mut words = input.split_whitespace();
    let name = words.next();
    let definition = COMMANDS
        .iter()
        .find(|definition| definition.names().any(|candidate| Some(candidate) == name))
        .ok_or("Unknown command. Type /help for available commands.")?;
    if words.next().is_some() {
        return Err(match definition.command {
            Command::Help => "Usage: /help",
            Command::Compact => "Usage: /compact",
            Command::Exit => "Usage: /exit (or /quit)",
        });
    }
    Ok(Some(definition.command))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_suggested_command_is_executable_and_described_in_help() {
        let help = help();
        for definition in COMMANDS {
            for name in definition.names() {
                assert_eq!(parse(name), Ok(Some(definition.command)), "{name}");
                assert!(help.contains(name), "{name}");
            }
            assert!(help.contains(&format!(
                "{} — {}",
                definition.label(),
                definition.description
            )));
        }
    }

    #[test]
    fn help_accepts_surrounding_whitespace_but_not_arguments() {
        for input in ["/help", "  /help  ", "\t/help\n"] {
            assert_eq!(parse(input), Ok(Some(Command::Help)));
        }
        for input in ["/help extra", "/help\nextra"] {
            assert_eq!(parse(input), Err("Usage: /help"));
        }
    }

    #[test]
    fn compact_has_no_arguments_and_accepts_surrounding_whitespace() {
        for input in ["/compact", "  /compact\t", "\n/compact\n"] {
            assert_eq!(parse(input), Ok(Some(Command::Compact)));
        }
        for input in ["/compact now", "/compact\nextra"] {
            assert_eq!(parse(input), Err("Usage: /compact"));
        }
    }

    #[test]
    fn exit_and_quit_are_the_same_argument_free_command() {
        for input in ["/exit", "  /quit\t", "\n/exit\n"] {
            assert_eq!(parse(input), Ok(Some(Command::Exit)));
        }
        for input in ["/exit now", "/quit\nextra"] {
            assert_eq!(parse(input), Err("Usage: /exit (or /quit)"));
        }
    }

    #[test]
    fn aliases_are_listed_in_help_without_duplicate_command_rows() {
        let help = help();
        assert!(help.contains("/exit, /quit — exit Robbie"));
        assert_eq!(help.matches(" — exit Robbie").count(), 1);
    }

    #[test]
    fn all_unrecognized_slash_input_stays_local() {
        for input in [
            "/",
            "/HELP",
            "/helpx",
            "/COMPACT",
            "/unknown arg",
            "/tmp/file",
            "/EXIT",
            "/exitx",
            "/quits",
        ] {
            assert!(parse(input).is_err(), "{input:?}");
        }
    }

    #[test]
    fn ordinary_prompts_are_not_commands() {
        for input in [
            "",
            "hello",
            "explain /tmp/file",
            "what does /help do?",
            "`/help`",
        ] {
            assert_eq!(parse(input), Ok(None), "{input:?}");
        }
    }
}
