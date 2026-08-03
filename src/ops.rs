//! Filesystem operations.
//!
//! This module contains functions that abstract filesystem operations at a higher level than
//! raw gio.

use std::sync::atomic::{AtomicU64, Ordering};

use futures::prelude::*;
use gtk::{gio, glib, prelude::*};
use relm4::{gtk, Sender};
use tracing::*;

use crate::component::app::{AppMsg, Transfer};

static ID: AtomicU64 = AtomicU64::new(0);

/// File transfer progress update.
#[derive(Debug)]
pub struct Progress {
    /// Uniquely identifies the ongoing operation.
    pub id: u64,

    pub current: i64,
    pub total: i64,
}

impl Progress {
    /// Returns true if this is the final update that will be sent for this operation.
    pub fn is_complete(&self) -> bool {
        self.current == self.total
    }
}

use std::path::PathBuf;

/// One entry found under a copy source, and where it lands relative to the
/// destination root. A plain file yields a single node with an empty relative
/// path.
#[derive(Debug)]
pub struct Node {
    pub source: gio::File,
    pub relative: PathBuf,
    pub is_dir: bool,
    pub size: i64,
}

/// Lists everything under `root`, directories included.
///
/// Directories are listed in their own right rather than implied by the files
/// inside them: an empty directory has no files to imply it and would otherwise
/// be dropped from the copy. A directory is always emitted before anything
/// inside it, so the copy can create it before writing into it.
///
/// Symlinks are reported as the links they are, never followed — following them
/// duplicates whole trees silently and can loop forever.
pub async fn walk(root: &gio::File) -> Result<Vec<Node>, glib::Error> {
    const ATTRIBUTES: &str = "standard::name,standard::type,standard::size";

    let info = root
        .query_info_future(
            ATTRIBUTES,
            gio::FileQueryInfoFlags::NOFOLLOW_SYMLINKS,
            glib::Priority::DEFAULT,
        )
        .await?;

    if info.file_type() != gio::FileType::Directory {
        return Ok(vec![Node {
            source: root.clone(),
            relative: PathBuf::new(),
            is_dir: false,
            size: info.size(),
        }]);
    }

    // The root itself is a node, exactly as a plain file is: `walk` promises
    // everything that must exist at the destination, and the destination
    // directory is the first thing that must. Without it every child copies
    // into a parent that was never created.
    let mut nodes = vec![Node {
        source: root.clone(),
        relative: PathBuf::new(),
        is_dir: true,
        size: 0,
    }];
    let mut pending = vec![(root.clone(), PathBuf::new())];

    while let Some((directory, prefix)) = pending.pop() {
        let enumerator = directory
            .enumerate_children_future(
                ATTRIBUTES,
                gio::FileQueryInfoFlags::NOFOLLOW_SYMLINKS,
                glib::Priority::DEFAULT,
            )
            .await?;

        loop {
            let batch = enumerator
                .next_files_future(32, glib::Priority::DEFAULT)
                .await?;

            if batch.is_empty() {
                break;
            }

            for info in batch {
                let name = info.name();
                let child = directory.child(&name);
                let relative = prefix.join(&name);
                let is_dir = info.file_type() == gio::FileType::Directory;

                nodes.push(Node {
                    source: child.clone(),
                    relative: relative.clone(),
                    is_dir,
                    size: info.size(),
                });

                if is_dir {
                    pending.push((child, relative));
                }
            }
        }
    }

    Ok(nodes)
}

/// Copies `source` — a file or a whole tree — to `destination`, reporting
/// progress through the same transfer UI the drag-and-drop move feeds.
///
/// `destination` is the final path: the caller has already resolved a
/// collision-free name. Nothing here overwrites — the flags omit
/// `FileCopyFlags::OVERWRITE`, so a name that slipped through fails loudly
/// instead of destroying a file.
pub async fn copy_tree(source: gio::File, destination: gio::File, sender: Sender<AppMsg>) {
    info!("copying {} to {}", source.uri(), destination.uri());

    let nodes = match walk(&source).await {
        Ok(nodes) => nodes,
        Err(err) => {
            let _ = sender.send(AppMsg::Error(Box::new(err)));
            return;
        }
    };

    let id = ID.fetch_add(1, Ordering::SeqCst);
    let total: i64 = nodes
        .iter()
        .filter(|node| !node.is_dir)
        .map(|node| node.size)
        .sum();
    let description = format!(
        "Copying '{}'",
        source
            .basename()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| "file".to_owned()),
    );

    let _ = sender.send(AppMsg::Transfer(Transfer::New { id, description }));

    // A plain file walks to a single node with an empty relative path, so one
    // loop covers both shapes.
    let mut done: i64 = 0;

    for node in &nodes {
        let target = if node.relative.as_os_str().is_empty() {
            destination.clone()
        } else {
            destination.child(&node.relative)
        };

        let result = if node.is_dir {
            match target.make_directory_future(glib::Priority::DEFAULT).await {
                // A directory this same walk created earlier is not an error.
                Err(err) if err.matches(gio::IOErrorEnum::Exists) => Ok(()),
                other => other,
            }
        } else {
            let (operation, mut progress) = node.source.copy_future(
                &target,
                gio::FileCopyFlags::NOFOLLOW_SYMLINKS,
                glib::Priority::DEFAULT,
            );

            // Read the stream in its own task and simply await the copy, which
            // is what `move_` below has always done. Joining the two instead
            // looks tidier and hangs: the progress channel does not close when
            // the copy finishes, so a combinator that waits for the stream to
            // end waits forever, and the whole paste never reports done.
            //
            // Bytes are offset by everything this walk already finished, so the
            // bar tracks the whole tree instead of restarting at every file.
            let finished_before = done;
            let reporter_sender = sender.clone();
            relm4::spawn_local(async move {
                while let Some((current, _file_total)) = progress.next().await {
                    let _ = reporter_sender.send(AppMsg::Transfer(Transfer::Progress(Progress {
                        id,
                        current: finished_before + current,
                        total,
                    })));
                }
            });

            operation.await
        };

        if let Err(err) = result {
            let _ = sender.send(AppMsg::Error(Box::new(err)));
            continue;
        }

        if !node.is_dir {
            done += node.size;
            let _ = sender.send(AppMsg::Transfer(Transfer::Progress(Progress {
                id,
                current: done,
                total,
            })));
        }
    }

    // Close the transfer. Without this the row lives for the life of the
    // process and the header keeps a spinner over work that finished long ago.
    let _ = sender.send(AppMsg::Transfer(Transfer::Done { id }));
}

/// Move a file to a destination.
pub async fn move_(file: gio::File, destination: gio::File, sender: Sender<AppMsg>) {
    info!("moving {} to {}", file.uri(), destination.uri());

    let (file_display_name, destination_display_name) = futures::join!(
        file.query_info_future(
            gio::FILE_ATTRIBUTE_STANDARD_DISPLAY_NAME,
            gio::FileQueryInfoFlags::NONE,
            glib::Priority::DEFAULT,
        )
        .map_ok(|info| info.display_name()),
        destination
            .parent()
            .unwrap()
            .query_info_future(
                gio::FILE_ATTRIBUTE_STANDARD_DISPLAY_NAME,
                gio::FileQueryInfoFlags::NONE,
                glib::Priority::DEFAULT,
            )
            .map_ok(|info| info.display_name()),
    );

    let id = ID.fetch_add(1, Ordering::SeqCst);
    let description = format!(
        "Moving '{}' to '{}'",
        file_display_name.unwrap_or_else(|_| "file".into()),
        destination_display_name.unwrap_or_else(|_| "destination".into()),
    );

    sender
        .send(AppMsg::Transfer(Transfer::New { id, description }))
        .unwrap();

    let (res, mut progress) = file.move_future(
        &destination,
        gio::FileCopyFlags::NONE,
        glib::source::Priority::DEFAULT,
    );

    let sender_ = sender.clone();
    relm4::spawn_local(async move {
        while let Some((current, total)) = progress.next().await {
            let _ = sender_.send(AppMsg::Transfer(Transfer::Progress(Progress {
                id,
                current,
                total,
            })));
        }
    });

    if let Err(err) = res.await {
        let _ = sender.send(AppMsg::Error(Box::new(err)));
    }

    // Close it whether it succeeded or failed. A failed move used to leave its
    // row up for the life of the process, claiming work that had already
    // stopped.
    let _ = sender.send(AppMsg::Transfer(Transfer::Done { id }));
}

/// Move a dropped file into the destination directory.
pub fn handle_drop(value: &glib::Value, destination: &gio::File, sender: Sender<AppMsg>) {
    let file = value.get::<gio::File>().unwrap();

    let destination_file = destination.child(file.basename().unwrap());

    if destination_file.equal(&file) {
        return;
    }

    relm4::spawn_local(move_(file, destination_file, sender));
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// Builds a small tree under a unique directory and returns its path.
    /// `/tmp` is fine here: copying works on tmpfs, only trashing does not.
    fn fixture(name: &str) -> std::path::PathBuf {
        let root = std::env::temp_dir().join(format!("fm-ops-test-{name}"));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("sub/deeper")).unwrap();
        fs::create_dir_all(root.join("empty")).unwrap();
        fs::write(root.join("top.txt"), b"top").unwrap();
        fs::write(root.join("sub/middle.txt"), b"middle").unwrap();
        fs::write(root.join("sub/deeper/bottom.txt"), b"bottom!").unwrap();
        root
    }

    fn relatives(nodes: &[Node]) -> Vec<String> {
        let mut out: Vec<String> = nodes
            .iter()
            .map(|node| node.relative.to_string_lossy().into_owned())
            .collect();
        out.sort();
        out
    }

    #[test]
    fn walking_a_tree_finds_every_file_and_directory() {
        let root = fixture("walk");
        let nodes = // A fresh context per test: the default one can only be owned by a
        // single thread, and GLib aborts the process when the parallel test
        // runner has a second thread try to acquire it.
        glib::MainContext::new()
            .block_on(walk(&gio::File::for_path(&root)))
            .expect("the fixture is readable");

        // The empty string is the root itself: the destination directory that
        // has to exist before anything lands inside it.
        assert_eq!(
            relatives(&nodes),
            vec![
                "",
                "empty",
                "sub",
                "sub/deeper",
                "sub/deeper/bottom.txt",
                "sub/middle.txt",
                "top.txt",
            ]
        );

        // Empty directories are nodes too, or they would vanish in the copy.
        let empty = nodes
            .iter()
            .find(|n| n.relative.ends_with("empty"))
            .unwrap();
        assert!(empty.is_dir);

        let bottom = nodes
            .iter()
            .find(|n| n.relative.ends_with("bottom.txt"))
            .unwrap();
        assert!(!bottom.is_dir);
        assert_eq!(bottom.size, 7);

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn walking_a_plain_file_yields_that_file_alone() {
        let root = fixture("walk-file");
        let nodes = // A fresh context per test: the default one can only be owned by a
        // single thread, and GLib aborts the process when the parallel test
        // runner has a second thread try to acquire it.
        glib::MainContext::new()
            .block_on(walk(&gio::File::for_path(root.join("top.txt"))))
            .expect("the fixture is readable");

        assert_eq!(nodes.len(), 1);
        assert!(!nodes[0].is_dir);
        assert_eq!(nodes[0].relative, std::path::Path::new(""));

        let _ = fs::remove_dir_all(&root);
    }
}
