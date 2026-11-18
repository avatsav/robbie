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
    fn every_suggested_name_is_executable() {
        for definition in COMMANDS {
            for name in definition.names() {
                assert_eq!(parse(name), Ok(Some(definition.command)), "{name}");
            }
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
