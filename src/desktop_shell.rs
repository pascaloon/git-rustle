//! Launch installed Windows applications without invoking a command interpreter.
pub fn command(path: &str, terminal: bool) -> std::process::Command {
    let mut command = std::process::Command::new(if terminal { "wt.exe" } else { "explorer.exe" });
    if terminal {
        command.arg("-d");
    }
    command.arg(path);
    command
}
pub fn open(path: &str, terminal: bool) -> Result<(), String> {
    if path.is_empty() {
        return Err("Repository is still loading".into());
    }
    command(path, terminal).spawn().map(|_| ()).map_err(|e| {
        format!(
            "Unable to open {}: {e}",
            if terminal {
                "Windows Terminal"
            } else {
                "Explorer"
            }
        )
    })
}
#[cfg(test)]
mod tests {
    #[test]
    fn repository_path_is_one_literal_argument() {
        let path = r"F:\Projects\a & b; test";
        let cmd = super::command(path, true);
        assert_eq!(cmd.get_args().collect::<Vec<_>>(), vec!["-d", path]);
        assert_eq!(
            super::command(path, false).get_args().collect::<Vec<_>>(),
            vec![path]
        );
    }
}
