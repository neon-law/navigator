//! `navigator`: Projects and their Jujutsu repositories. The command tree
//! lives in the `cli` library so the menu-bar app links the same rules.

fn main() -> std::process::ExitCode {
    cli::commands::main()
}
