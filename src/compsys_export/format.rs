use crate::spec::{ArgSpec, ArgTemplate, GeneratorSpec, OptionSpec};

pub(super) fn format_option(opt: &OptionSpec) -> String {
    let desc = opt
        .description
        .as_deref()
        .map(escape_zsh_string)
        .unwrap_or_default();

    let arg_suffix = if opt.takes_arg {
        if let Some(ref generator) = opt.arg_generator {
            format!("::{}", format_generator_action(generator))
        } else {
            ": :".to_string()
        }
    } else {
        String::new()
    };

    match (opt.short.as_deref(), opt.long.as_deref()) {
        (Some(short), Some(long)) => {
            let eq = if opt.takes_arg { "=" } else { "" };
            format!("'({short} {long})'{{{short},{long}{eq}}}'[{desc}]'{arg_suffix}")
        }
        (None, Some(long)) => {
            let eq = if opt.takes_arg { "=" } else { "" };
            format!("'{long}{eq}[{desc}]'{arg_suffix}")
        }
        (Some(short), None) => {
            format!("'{short}[{desc}]'{arg_suffix}")
        }
        (None, None) => String::new(),
    }
}

pub(super) fn format_arg(arg: &ArgSpec) -> String {
    let prefix = if arg.variadic { "*" } else { "" };

    if let Some(ref template) = arg.template {
        return match template {
            ArgTemplate::FilePaths => format!("'{prefix}:file:_files'"),
            ArgTemplate::Directories => format!("'{prefix}:directory:_files -/'"),
            ArgTemplate::EnvVars => {
                format!("'{prefix}:variable:_parameters -g \"*(export)\"'")
            }
            ArgTemplate::History => format!("'{prefix}:arg:'"),
        };
    }

    if !arg.suggestions.is_empty() {
        let values = arg
            .suggestions
            .iter()
            .map(|suggestion| escape_zsh_string(suggestion))
            .collect::<Vec<_>>()
            .join(" ");
        let name = if arg.name.is_empty() {
            "arg"
        } else {
            &arg.name
        };
        return format!("'{prefix}:{name}:({values})'");
    }

    if let Some(ref generator) = arg.generator {
        let action = format_generator_action(generator).replace('\'', "'\\''");
        let name = if arg.name.is_empty() {
            "arg"
        } else {
            &arg.name
        };
        return format!("'{prefix}:{name}:{action}'");
    }

    let name = if arg.name.is_empty() {
        "arg"
    } else {
        &arg.name
    };
    format!("'{prefix}:{name}:'")
}

pub(super) fn format_generator_action(generator: &GeneratorSpec) -> String {
    let cmd_escaped = escape_double_quote_string(&generator.command);
    let mut synapse_cmd = format!("synapse run-generator \"{cmd_escaped}\" --cwd \"$PWD\"");

    if let Some(ref prefix) = generator.strip_prefix {
        let prefix_escaped = escape_double_quote_string(prefix);
        synapse_cmd.push_str(&format!(" --strip-prefix \"{prefix_escaped}\""));
    }

    if generator.split_on != "\n" {
        let split_escaped = escape_double_quote_string(&generator.split_on);
        synapse_cmd.push_str(&format!(" --split-on \"{split_escaped}\""));
    }

    format!("{{local -a vals; vals=(${{(f)\"$({synapse_cmd} 2>/dev/null)\"}}); compadd -a vals}}")
}

fn escape_double_quote_string(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('$', "\\$")
        .replace('`', "\\`")
}

pub(super) fn escape_zsh_string(value: &str) -> String {
    value
        .replace('\'', "'\\''")
        .replace('[', "\\[")
        .replace(']', "\\]")
}
