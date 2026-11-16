//! The folder a channel's new agents open in, where Herdr can't say. Herdr
//! keeps a workspace's checkout, but a pane's folder follows every `cd`, so
//! the folder a channel was created in, or first got an agent in, is kept
//! here. The server's mutation gate serializes every write.
use super::server_files::ServerFiles;
use crate::StateNamespace;
use serde::{Deserialize, Serialize};
use std::{fs, io, path::Path};

const FILE: &str = "channel-folders.json";

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct Recorded {
    socket: String,
    workspace: String,
    folder: String,
}

#[derive(Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct ChannelFolders {
    channels: Vec<Recorded>,
}

impl Recorded {
    fn is(&self, namespace: &StateNamespace) -> bool {
        self.socket == namespace.session_socket_id && self.workspace == namespace.space_id.as_str()
    }
}

impl ChannelFolders {
    /// A file that is missing or can't be used reads as no folders: each
    /// channel then falls back to what Herdr reports, as before it was kept.
    pub fn read(state: &Path) -> Self {
        let path = state.join("dashboard").join(FILE);
        match fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_else(|error| {
                eprintln!("dashboard: ignoring {}: {error}", path.display());
                Self::default()
            }),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Self::default(),
            Err(error) => {
                eprintln!("dashboard: could not read {}: {error}", path.display());
                Self::default()
            }
        }
    }

    pub fn of(&self, namespace: &StateNamespace) -> Option<&str> {
        self.channels
            .iter()
            .find(|recorded| recorded.is(namespace))
            .map(|recorded| recorded.folder.as_str())
    }

    pub fn record(&mut self, namespace: &StateNamespace, folder: &str) {
        self.forget(namespace);
        self.channels.push(Recorded {
            socket: namespace.session_socket_id.clone(),
            workspace: namespace.space_id.as_str().to_owned(),
            folder: folder.to_owned(),
        });
    }

    pub fn forget(&mut self, namespace: &StateNamespace) {
        self.channels.retain(|recorded| !recorded.is(namespace));
    }

    /// Drops the folders of this Herdr session's closed workspaces, so a
    /// workspace id Herdr gives out again never inherits one.
    pub fn keep_live<'a>(&mut self, socket: &str, live: impl IntoIterator<Item = &'a str>) {
        let live: Vec<&str> = live.into_iter().collect();
        self.channels
            .retain(|recorded| recorded.socket != socket || live.contains(&&*recorded.workspace));
    }

    pub fn save(&self, files: &ServerFiles) -> Result<(), String> {
        let bytes = serde_json::to_vec(self).map_err(|error| error.to_string())?;
        files.write_atomic(FILE, &bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_folder_is_kept_per_workspace_and_leaves_with_a_closed_one() {
        let (a1, a2, b1) = (
            StateNamespace::for_space("socket-a", "w1"),
            StateNamespace::for_space("socket-a", "w2"),
            StateNamespace::for_space("socket-b", "w1"),
        );
        let mut folders = ChannelFolders::default();
        folders.record(&a1, "/one");
        folders.record(&a2, "/two");
        folders.record(&b1, "/other");
        folders.record(&a1, "/one-again");
        assert_eq!(folders.of(&a1), Some("/one-again"));
        assert_eq!(folders.of(&a2), Some("/two"));
        folders.keep_live("socket-a", ["w2", "w9"]);
        assert_eq!(folders.of(&a1), None, "w1 closed on socket-a");
        assert_eq!(folders.of(&a2), Some("/two"));
        assert_eq!(
            folders.of(&b1),
            Some("/other"),
            "another Herdr session's workspaces are not this one's to judge"
        );
        folders.forget(&a2);
        assert_eq!(folders.of(&a2), None);
    }

    #[test]
    fn a_missing_or_broken_file_reads_as_no_folders() {
        let state = std::env::temp_dir().join(crate::IdGenerator::next_id(
            &crate::ProcessIds::default(),
            "channel-folders",
        ));
        assert_eq!(ChannelFolders::read(&state), ChannelFolders::default());
        fs::create_dir_all(state.join("dashboard")).unwrap();
        fs::write(state.join("dashboard").join(FILE), "{").unwrap();
        assert_eq!(ChannelFolders::read(&state), ChannelFolders::default());
        fs::write(
            state.join("dashboard").join(FILE),
            r#"{"channels":[{"socket":"s","workspace":"w1","folder":"/kept"}]}"#,
        )
        .unwrap();
        assert_eq!(
            ChannelFolders::read(&state).of(&StateNamespace::for_space("s", "w1")),
            Some("/kept")
        );
        fs::remove_dir_all(state).unwrap();
    }
}
