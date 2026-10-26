use clap::{Arg, Command};

pub fn print_tree(cmd: &Command, depth: usize) {
    let before_after = match depth {
        1 => format!("\n{}\n", "====".repeat(20)),
        _ => String::new(),
    };

    let indent = match depth {
        1 => String::from(">>"),
        _ => "--".repeat(depth)
    };

    let about = cmd.get_about().map(|a| a.to_string()).unwrap_or_default();

    let sep = match depth {
        0 => "",
        1 => "  -  ",
        _ => "\t\t\t| "
    };

    println!("{before_after}{indent} {}{sep}{about}{before_after}", cmd.get_name());

    let args = cmd.get_arguments().filter(|a| !a.is_hide_set()).collect::<Vec<_>>();
    let subcommands = cmd.get_subcommands().collect::<Vec<_>>();

    for arg in &args {
        let help = arg.get_help().map(|h| h.to_string()).unwrap_or_default();

        print!("{}    {:<20}\t| {help}", "  ".repeat(depth), arg_label(arg));

        if !arg.get_possible_values().is_empty() {
            let values = arg.get_possible_values()
                .iter()
                .map(|v| v.get_name())
                .collect::<Vec<&str>>()
                .join(", ");

            println!(". Values: [{values}]");
        }
        else {
            println!();
        }
    }

    if !args.is_empty() && !subcommands.is_empty() && depth == 1 {
        println!();
    }

    for sub in &subcommands {
        print_tree(sub, depth + 1);
    }
}

fn arg_label(arg: &Arg) -> String {
    if arg.is_positional() {
        return format!("<{}>", arg.get_id());
    }
    let mut parts = Vec::new();
    if let Some(s) = arg.get_short() {
        parts.push(format!("-{s}"));
    }
    if let Some(l) = arg.get_long() {
        parts.push(format!("--{l}"));
    }
    parts.join(", ")
}