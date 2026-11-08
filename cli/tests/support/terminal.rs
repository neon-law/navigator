use std::fs::File;
use std::io::Write;
use std::process::{Command, Output, Stdio};

/// Run a command with scripted input on a real terminal.
pub fn output(command: &mut Command, answers: &str) -> Output {
    let terminal = nix::pty::openpty(None, None).unwrap();
    let mut input = File::from(terminal.master);
    input.write_all(answers.as_bytes()).unwrap();
    command.stdin(Stdio::from(terminal.slave)).output().unwrap()
}
