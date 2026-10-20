//! Frozen checkout choices shared by the graph and branch browser.
use crate::branches::Target;
use crate::remote::Remote;
use gix::ObjectId;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Request {
    Branch(Target),
    /// A remote-only graph tip: fetch its branch and fast-forward its tracker.
    FetchBranch(Target),
    Commit {
        id: ObjectId,
        remote: Option<Remote>,
    },
}

impl Request {
    pub fn label(&self) -> String {
        match self {
            Self::Branch(t) | Self::FetchBranch(t) => t.label(),
            Self::Commit { id, .. } => format!("{} (detached HEAD)", id.to_hex_with_len(12)),
        }
    }

    pub fn is_remote(&self) -> bool {
        match self {
            Self::Branch(t) | Self::FetchBranch(t) => t.remote.is_some(),
            Self::Commit { remote, .. } => remote.is_some(),
        }
    }
}

pub struct Confirmation {
    pub choices: Vec<Request>,
    pub selected: usize,
    /// Enter only confirms after a readable popup has actually been drawn.
    pub ready: bool,
}

impl Confirmation {
    pub fn new(choices: Vec<Request>) -> Self {
        debug_assert!(!choices.is_empty());
        Self {
            choices,
            selected: 0,
            ready: false,
        }
    }

    pub fn chosen(&self) -> &Request {
        &self.choices[self.selected]
    }

    pub fn move_by(&mut self, delta: isize) {
        self.selected = self
            .selected
            .saturating_add_signed(delta)
            .min(self.choices.len() - 1);
    }
}

#[cfg(test)]
mod tests {
    #[test]
    #[ignore = "writes a disposable native checkout benchmark fixture"]
    fn prepare_native_fixture() {
        let path = std::path::PathBuf::from(
            std::env::var_os("GIT_RUSTLE_CHECKOUT_FIXTURE").expect("fixture path required"),
        );
        assert!(!path.exists(), "fixture must be fresh");
        std::fs::create_dir_all(&path).unwrap();
        let git = |args: &[&str]| crate::git::command::checked(&path, args).unwrap();
        git(&["init", "-q", "-b", "main"]);
        std::fs::write(
            path.join(".git/rustle-checkout-benchmark"),
            "git-rustle checkout benchmark fixture v1",
        )
        .unwrap();
        git(&["config", "core.hooksPath", "disabled-test-hooks"]);
        for n in 0..3 {
            std::fs::write(path.join("file.txt"), format!("version {n}\n")).unwrap();
            git(&["add", "file.txt"]);
            git(&[
                "-c",
                "user.name=Checkout Test",
                "-c",
                "user.email=test@example.invalid",
                "-c",
                "commit.gpgSign=false",
                "commit",
                "-qm",
                &format!("Commit {n}"),
            ]);
        }
        git(&["branch", "topic", "HEAD~1"]);
        println!("Native checkout fixture: {}", path.display());
    }
}
