//! Application entrypoint.

use std::convert::identity;
use std::path::{self, PathBuf};

use gtk::{gdk, gio, glib, prelude::*};
use relm4::actions::{RelmAction, RelmActionGroup};
use relm4::factory::FactoryVecDeque;
use relm4::prelude::*;
use tracing::*;

use crate::config::{self, State};
use crate::ops::Progress;

use super::alert::{AlertModel, AlertMsg, ERROR_BROKER};
use super::directory_list::{
    refresh_hidden_filters, refresh_sorters, Directory, DirectoryMessage, Selection,
};
use super::file_preview::{FilePreviewModel, FilePreviewMsg};
use super::mount::{Mount, MountMsg};
use super::places_sidebar::PlacesSidebarModel;
use super::transfer_progress::{NewTransfer, TransferProgress, TransferProgressMsg};

#[derive(Debug)]
pub struct AppModel {
    /// The directory listed by the leftmost column.
    root: gio::File,

    /// The directory listings. This factory acts as a stack, where new directories are pushed and
    /// popped relative to the root as the user clicks on new directory entries.
    directories: FactoryVecDeque<Directory>,

    /// Displays the progress of ongoing file operations.
    progress: FactoryVecDeque<TransferProgress>,

    error_alert: Controller<AlertModel>,
    file_preview: Controller<FilePreviewModel>,
    mount: Controller<Mount>,
    _places_sidebar: Controller<PlacesSidebarModel>,

    /// Whether the directory panes scroll window should update its scroll position to the upper
    /// bound on the next view update.
    update_directory_scroll_position: bool,

    /// The index of the directory panel an active search applies to.
    search_panel: Option<usize>,

    /// Monotonic id of the latest corner toast; expiry timers only hide the
    /// toast if theirs is still the newest.
    toast_epoch: std::rc::Rc<std::cell::Cell<u64>>,

    state: State,
}

impl AppModel {
    /// Returns the deepest directory that is listed (the rightmost listing).
    pub fn last_dir(&self) -> gio::File {
        self.directories
            .back()
            .expect("there must be at least one directory listed")
            .dir()
    }

    /// Shows `message` in the bottom-left corner toast for a few seconds.
    fn show_toast(&self, widgets: &AppWidgets, message: &str) {
        widgets.corner_toast_label.set_text(message);
        widgets.corner_toast.set_reveal_child(true);

        let epoch = self.toast_epoch.get() + 1;
        self.toast_epoch.set(epoch);

        let current_epoch = self.toast_epoch.clone();
        let revealer = widgets.corner_toast.downgrade();
        glib::timeout_add_seconds_local(4, move || {
            if current_epoch.get() == epoch {
                if let Some(revealer) = revealer.upgrade() {
                    revealer.set_reveal_child(false);
                }
            }
            glib::ControlFlow::Break
        });
    }

    /// Returns the index of the deepest panel holding the cursor (a selection).
    fn cursor_panel(&self) -> Option<usize> {
        (0..self.directories.len()).rev().find(|&idx| {
            self.directories
                .get(idx)
                .is_some_and(|dir| matches!(dir.selection(), Selection::Files(_)))
        })
    }
}

#[derive(Debug)]
pub enum Transfer {
    New { id: u64, description: String },
    Progress(Progress),
}

#[derive(Debug)]
pub enum AppMsg {
    /// Display an arbitrary error in an alert dialog.
    Error(Box<dyn std::error::Error + Send>),

    /// The file root has changed. Existing directory trees are now invalid and must be popped off
    /// the stack.
    NewRoot(gio::File),

    /// A new selection was made within the existing directory listings. This can result in a
    /// number of possible changes:
    ///
    /// - If the new selection is higher in the directory tree than the old selection, the lower
    ///   listings must be removed.
    /// - If the new selection is a directory, a new directory listing is pushed onto the listing
    ///   stack.
    /// - If the new selection is a file, the preview must be updated.
    NewSelection(Selection),

    /// Update the file transfer progress.
    Transfer(Transfer),

    /// Display a toast.
    Toast(String),

    /// Display the about window.
    About,

    /// Launch a dialog to mount a new mountable.
    Mount,

    /// Open the search bar for the deepest directory panel.
    SearchOpen,

    /// The search term changed.
    SearchChanged(String),

    /// The search term was confirmed: move focus away so `n`/`N` navigate matches.
    SearchConfirm,

    /// Cancel the search and clear its highlights.
    SearchCancel,

    /// Move to the next search match.
    SearchNext,

    /// Move to the previous search match.
    SearchPrev,

    /// Sort by modification time or name; selecting the active key reverses the order.
    SetSort { by_modified: bool },

    /// Open the rename popover for the selected entry.
    RenameSelected,

    /// Move the cursor down (`j`) or up (`k`) within the current panel.
    NavMove(i32),

    /// Jump to the first row (`gg`).
    NavFirst,

    /// Jump to the last row (`G`).
    NavLast,

    /// Descend into the selected directory, or open the selected file (`l`).
    NavInto,

    /// Move the cursor to the parent panel; at the root column, go up one level (`h`).
    NavParent,
}

#[relm4::component(pub)]
impl Component for AppModel {
    type Widgets = AppWidgets;
    type Init = PathBuf;
    type Input = AppMsg;
    type Output = ();
    type CommandOutput = ();

    view! {
        #[name = "main_window"]
        adw::Window {
            set_default_size: (state.width, state.height),
            set_title: Some("fm"),

            gtk::Overlay {
                gtk::Box {
                    set_orientation: gtk::Orientation::Vertical,

                    adw::HeaderBar {
                        pack_end = &gtk::MenuButton {
                            set_icon_name: "open-menu-symbolic",
                            set_menu_model: Some(&primary_menu),
                        },

                        #[name = "transfer_progress_button"]
                        pack_end = &gtk::MenuButton {
                            set_visible: false,

                            #[wrap(Some)]
                            set_child = &gtk::Spinner {
                                start: (),
                            },

                            #[wrap(Some)]
                            set_popover = &gtk::Popover {
                                #[name = "transfer_progress"]
                                gtk::ListBox {
                                    set_selection_mode: gtk::SelectionMode::None,
                                },
                            }
                        },
                    },

                    adw::Flap {
                        #[wrap(Some)]
                        set_flap = &gtk::Box {
                            set_orientation: gtk::Orientation::Vertical,
                            append: places_sidebar.widget(),
                        },

                        #[wrap(Some)]
                        set_separator = &gtk::Separator {},

                        #[wrap(Some)]
                        set_content = &gtk::Box {
                            set_orientation: gtk::Orientation::Vertical,

                            #[name = "directory_panes_scroller"]
                            gtk::ScrolledWindow {
                                set_hexpand: true,
                                set_vexpand: true,

                                #[name = "directory_panes"]
                                panel::Paned {
                                    append: file_preview.widget(),
                                },
                            },

                            #[name = "search_bar"]
                            gtk::SearchBar {
                                #[wrap(Some)]
                                #[name = "search_entry"]
                                set_child = &gtk::SearchEntry {
                                    set_placeholder_text: Some("Search this directory..."),

                                    connect_search_changed[sender] => move |entry| {
                                        sender.input(AppMsg::SearchChanged(entry.text().to_string()));
                                    },

                                    connect_activate[sender] => move |_| {
                                        sender.input(AppMsg::SearchConfirm);
                                    },

                                    connect_stop_search[sender] => move |_| {
                                        sender.input(AppMsg::SearchCancel);
                                    },
                                },
                            },
                        },
                    },
                },

                #[name = "corner_toast"]
                add_overlay = &gtk::Revealer {
                    set_halign: gtk::Align::Start,
                    set_valign: gtk::Align::End,
                    set_margin_start: 12,
                    set_margin_bottom: 12,
                    set_transition_type: gtk::RevealerTransitionType::Crossfade,
                    set_can_target: false,

                    #[wrap(Some)]
                    #[name = "corner_toast_label"]
                    set_child = &gtk::Label {
                        add_css_class: "corner-toast",
                    },
                },
            },

            connect_close_request => move |this| {
                let (width, height) = this.default_size();
                let is_maximized = this.is_maximized();

                let new_state = State {
                    width,
                    height,
                    is_maximized,
                    show_hidden: config::show_hidden(),
                    sort_by_modified: config::sort_by_modified(),
                    sort_reversed: config::sort_reversed(),
                };

                if let Err(e) = new_state.write() {
                    warn!("unable to write application state: {}", e);
                }

                glib::signal::Propagation::Proceed
            }
        }
    }

    menu! {
        primary_menu: {
            section! {
                "Show hidden files" => ToggleHiddenAction,
            },
            section! {
                "Connect to server..." => MountAction,
            },
            section! {
                "About" => AboutAction,
            },
        }
    }

    fn init(dir: PathBuf, root: Self::Root, sender: ComponentSender<Self>) -> ComponentParts<Self> {
        let dir = if !dir.is_dir() {
            dir.parent().unwrap_or(&dir)
        } else {
            &dir
        };

        let dir = gio::File::for_path(dir);

        let state = State::read()
            .map_err(|e| {
                warn!("unable to read application state: {}", e);
                e
            })
            .unwrap_or_default();

        info!("starting with application state: {:?}", state);

        config::set_show_hidden(state.show_hidden);
        config::set_sort_by_modified(state.sort_by_modified);
        config::set_sort_reversed(state.sort_reversed);

        let file_preview = FilePreviewModel::builder().launch(()).detach();

        let places_sidebar = PlacesSidebarModel::builder()
            .launch(dir.clone())
            .forward(sender.input_sender(), identity);

        let widgets = view_output!();

        let mut model = AppModel {
            root: dir.clone(),
            directories: FactoryVecDeque::builder()
                .launch(widgets.directory_panes.clone())
                .forward(sender.input_sender(), identity),
            progress: FactoryVecDeque::builder()
                .launch(widgets.transfer_progress.clone())
                .forward(sender.input_sender(), identity),
            mount: Mount::builder()
                .transient_for(&widgets.main_window)
                .launch(())
                .forward(sender.input_sender(), identity),
            error_alert: AlertModel::builder()
                .transient_for(widgets.main_window.clone())
                .launch_with_broker((), &ERROR_BROKER)
                .detach(),
            file_preview,
            _places_sidebar: places_sidebar,
            update_directory_scroll_position: false,
            search_panel: None,
            toast_epoch: Default::default(),
            state,
        };

        model.directories.guard().push_back(dir);

        let mut group = RelmActionGroup::<WindowActionGroup>::new();

        let sender_ = sender.clone();
        let about_action: RelmAction<AboutAction> = RelmAction::new_stateless(move |_| {
            sender_.input(AppMsg::About);
        });
        group.add_action(about_action);

        let toggle_sender = sender.clone();
        let toggle_hidden_action: RelmAction<ToggleHiddenAction> =
            RelmAction::new_stateful(&config::show_hidden(), move |_, show_hidden: &mut bool| {
                *show_hidden = !*show_hidden;
                config::set_show_hidden(*show_hidden);
                refresh_hidden_filters();
                toggle_sender.input(AppMsg::Toast(
                    if *show_hidden {
                        "Showing hidden files"
                    } else {
                        "Hiding hidden files"
                    }
                    .to_owned(),
                ));
            });
        group.add_action(toggle_hidden_action);

        let key_sender = sender.clone();

        let mount_action: RelmAction<MountAction> = RelmAction::new_stateless(move |_| {
            sender.input(AppMsg::Mount);
        });
        group.add_action(mount_action);

        widgets
            .main_window
            .insert_action_group("win", Some(&group.into_action_group()));

        // Also a ranger default (`<C-h>`), alongside Backspace below.
        relm4::main_application()
            .set_accels_for_action("win.toggle-hidden", &["<Control>h"]);

        // ranger-style keys: Backspace (hidden files), / n N (search), o+m / o+n
        // (sort by modified / name), F2 (rename).
        let key_controller = gtk::EventControllerKey::new();
        let pending_sort = std::rc::Rc::new(std::cell::Cell::new(false));
        let pending_g = std::rc::Rc::new(std::cell::Cell::new(false));
        let window = widgets.main_window.downgrade();
        key_controller.connect_key_pressed(move |_, keyval, _, state| {
            let Some(window) = window.upgrade() else {
                return glib::Propagation::Proceed;
            };

            // Let text entries (rename, search, ...) and open popovers (menus,
            // rename) keep their keys.
            if gtk::prelude::GtkWindowExt::focus(&window).is_some_and(|focus| {
                focus.is::<gtk::Text>()
                    || focus.is::<gtk::Entry>()
                    || focus.ancestor(gtk::Popover::static_type()).is_some()
            }) {
                pending_sort.set(false);
                pending_g.set(false);
                return glib::Propagation::Proceed;
            }

            if state.intersects(gdk::ModifierType::CONTROL_MASK | gdk::ModifierType::ALT_MASK) {
                return glib::Propagation::Proceed;
            }

            if pending_sort.take() {
                match keyval {
                    gdk::Key::m => key_sender.input(AppMsg::SetSort { by_modified: true }),
                    gdk::Key::n => key_sender.input(AppMsg::SetSort { by_modified: false }),
                    // A repeated prefix re-arms instead of cancelling.
                    gdk::Key::o => pending_sort.set(true),
                    _ => {}
                }
                return glib::Propagation::Stop;
            }

            if pending_g.take() {
                if keyval == gdk::Key::g {
                    key_sender.input(AppMsg::NavFirst);
                }
                return glib::Propagation::Stop;
            }

            match keyval {
                gdk::Key::BackSpace => {
                    let _ = window.activate_action("win.toggle-hidden", None);
                    glib::Propagation::Stop
                }
                gdk::Key::slash => {
                    key_sender.input(AppMsg::SearchOpen);
                    glib::Propagation::Stop
                }
                gdk::Key::o => {
                    pending_sort.set(true);
                    glib::Propagation::Stop
                }
                gdk::Key::g => {
                    pending_g.set(true);
                    glib::Propagation::Stop
                }
                gdk::Key::G => {
                    key_sender.input(AppMsg::NavLast);
                    glib::Propagation::Stop
                }
                gdk::Key::n => {
                    key_sender.input(AppMsg::SearchNext);
                    glib::Propagation::Stop
                }
                gdk::Key::N => {
                    key_sender.input(AppMsg::SearchPrev);
                    glib::Propagation::Stop
                }
                gdk::Key::j => {
                    key_sender.input(AppMsg::NavMove(1));
                    glib::Propagation::Stop
                }
                gdk::Key::k => {
                    key_sender.input(AppMsg::NavMove(-1));
                    glib::Propagation::Stop
                }
                gdk::Key::l | gdk::Key::Return | gdk::Key::KP_Enter => {
                    key_sender.input(AppMsg::NavInto);
                    glib::Propagation::Stop
                }
                gdk::Key::h => {
                    key_sender.input(AppMsg::NavParent);
                    glib::Propagation::Stop
                }
                gdk::Key::F2 => {
                    key_sender.input(AppMsg::RenameSelected);
                    glib::Propagation::Stop
                }
                _ => glib::Propagation::Proceed,
            }
        });
        // Capture phase: act before the focused widget does. Keyboard focus can
        // sit on the header's menu button while j/k move the model selection,
        // and in bubble phase the button would swallow Return (opening the menu).
        key_controller.set_propagation_phase(gtk::PropagationPhase::Capture);
        widgets.main_window.add_controller(key_controller);

        widgets.search_bar.connect_entry(&widgets.search_entry);

        // TODO: There's sometimes a delay in updating the adjustment upper bound when a new pane
        // is added, causing this code to not trigger at the right time. Needs more investigation.
        widgets
            .directory_panes_scroller
            .hadjustment()
            .connect_notify(Some("upper"), |this, _| {
                set_adjustment_to_upper_bound(this);
            });

        ComponentParts { model, widgets }
    }

    fn update_with_view(
        &mut self,
        widgets: &mut Self::Widgets,
        msg: Self::Input,
        sender: ComponentSender<Self>,
        _: &Self::Root,
    ) {
        self.update_directory_scroll_position = false;

        match msg {
            AppMsg::Error(err) => {
                self.error_alert.emit(AlertMsg::Show {
                    text: err.to_string(),
                });
            }
            AppMsg::NewSelection(Selection::Files(selection)) => {
                let mut last_dir = self.last_dir();

                let file = if selection.files.len() == 1 {
                    selection.files.first().unwrap()
                } else {
                    &selection.parent
                };

                let file_path = match glib::Uri::split(&file.uri(), glib::UriFlags::NONE) {
                    Ok((_, _, _, _, path, _, _)) => PathBuf::from(&path),
                    Err(e) => {
                        warn!("unable to parse URI: {}", e);
                        return;
                    }
                };

                let last_dir_path = glib::Uri::split(&last_dir.uri(), glib::UriFlags::NONE)
                    .map(|(_, _, _, _, path, _, _)| path)
                    .expect("last visited directory must be a valid URI");

                let diff = pathdiff::diff_paths(file_path, &last_dir_path)
                    .expect("new selection must be relative to the listed directories");

                info!(
                    "new selection: {:?}, last dir: {}, diff: {}",
                    selection,
                    last_dir.uri(),
                    diff.display()
                );

                let mut directories = self.directories.guard();

                for component in diff.components() {
                    match component {
                        path::Component::ParentDir => {
                            directories.pop_back();
                            last_dir = last_dir.parent().unwrap();
                        }
                        path::Component::Normal(name) => {
                            let component_file = last_dir.child(name);
                            if component_file.query_file_type(
                                gio::FileQueryInfoFlags::NONE,
                                gio::Cancellable::NONE,
                            ) == gio::FileType::Directory
                            {
                                directories.push_back(component_file.clone());
                                last_dir = component_file;
                            }
                        }
                        _ => unreachable!("unexpected path component: {:?}", component),
                    }
                }

                self.file_preview
                    .emit(FilePreviewMsg::NewSelection(selection));

                self.update_directory_scroll_position = true;
            }
            AppMsg::NewSelection(Selection::None) => {
                self.file_preview.emit(FilePreviewMsg::Hide);

                self.update_directory_scroll_position = true;
            }
            AppMsg::NewRoot(new_root) => {
                info!("new root: {:?}", new_root);

                let mut directories = self.directories.guard();

                directories.clear();

                self.root = new_root;
                directories.push_back(self.root.clone());

                self.file_preview.emit(FilePreviewMsg::Hide);

                self.update_directory_scroll_position = true;
            }
            AppMsg::Transfer(transfer) => {
                match transfer {
                    Transfer::New { id, description } => {
                        self.progress
                            .guard()
                            .push_back(NewTransfer { id, description });
                    }
                    Transfer::Progress(progress) => {
                        let idx = self
                            .progress
                            .iter()
                            .position(|child| child.id == progress.id);

                        if let Some(idx) = idx {
                            self.progress
                                .send(idx, TransferProgressMsg::Update(progress));
                        }
                    }
                }

                if !self.progress.is_empty() {
                    widgets.transfer_progress_button.set_visible(true);
                }
            }
            AppMsg::Toast(message) => {
                self.show_toast(widgets, &message);
            }
            AppMsg::About => {
                gtk::AboutDialog::builder()
                    .authors(
                        env!("CARGO_PKG_AUTHORS")
                            .split(':')
                            .map(String::from)
                            .collect::<Vec<_>>(),
                    )
                    .comments(env!("CARGO_PKG_DESCRIPTION"))
                    .copyright("© 2021 Andy Russell")
                    .license_type(gtk::License::MitX11)
                    .logo_icon_name("folder-symbolic")
                    .program_name(env!("CARGO_PKG_NAME"))
                    .version(env!("CARGO_PKG_VERSION"))
                    .website(env!("CARGO_PKG_HOMEPAGE"))
                    .build()
                    .show();
            }
            AppMsg::Mount => self.mount.emit(MountMsg::Mount),
            AppMsg::SearchOpen => {
                self.search_panel = Some(self.directories.len().saturating_sub(1));
                widgets.search_bar.set_search_mode(true);
                widgets.search_entry.grab_focus();
                // A previous term stays in the entry; select it so typing replaces it.
                widgets.search_entry.select_region(0, -1);
            }
            AppMsg::SearchChanged(term) => {
                if let Some(idx) = self.search_panel {
                    if idx < self.directories.len() {
                        self.directories.send(idx, DirectoryMessage::SetSearch(term));
                    }
                }
            }
            AppMsg::SearchConfirm => {
                gtk::prelude::GtkWindowExt::set_focus(&widgets.main_window, None::<&gtk::Widget>);
            }
            AppMsg::SearchCancel => {
                if let Some(idx) = self.search_panel.take() {
                    if idx < self.directories.len() {
                        self.directories.send(idx, DirectoryMessage::ClearSearch);
                    }
                }
                widgets.search_bar.set_search_mode(false);
                gtk::prelude::GtkWindowExt::set_focus(&widgets.main_window, None::<&gtk::Widget>);
            }
            AppMsg::SearchNext => {
                if let Some(idx) = self.search_panel {
                    if idx < self.directories.len() {
                        self.directories.send(idx, DirectoryMessage::SearchNext);
                    }
                }
            }
            AppMsg::SearchPrev => {
                if let Some(idx) = self.search_panel {
                    if idx < self.directories.len() {
                        self.directories.send(idx, DirectoryMessage::SearchPrev);
                    }
                }
            }
            AppMsg::SetSort { by_modified } => {
                if config::sort_by_modified() == by_modified {
                    config::set_sort_reversed(!config::sort_reversed());
                } else {
                    config::set_sort_by_modified(by_modified);
                    // Modified starts newest-first; name starts A -> Z.
                    config::set_sort_reversed(by_modified);
                }
                refresh_sorters();

                let description = match (by_modified, config::sort_reversed()) {
                    (true, true) => "Sort: modified (newest first)",
                    (true, false) => "Sort: modified (oldest first)",
                    (false, false) => "Sort: name (A\u{2192}Z)",
                    (false, true) => "Sort: name (Z\u{2192}A)",
                };
                self.show_toast(widgets, description);
            }
            AppMsg::RenameSelected => {
                if let Some(idx) = self.cursor_panel() {
                    self.directories.send(idx, DirectoryMessage::RenameSelected);
                }
            }
            AppMsg::NavMove(delta) => {
                match self.cursor_panel() {
                    Some(idx) => self.directories.send(idx, DirectoryMessage::MoveCursor(delta)),
                    // No cursor yet: enter the deepest listing at one of its ends.
                    None => {
                        let idx = self.directories.len().saturating_sub(1);
                        let msg = if delta >= 0 {
                            DirectoryMessage::SelectFirst
                        } else {
                            DirectoryMessage::SelectLast
                        };
                        self.directories.send(idx, msg);
                    }
                }
            }
            AppMsg::NavFirst => {
                let idx = self
                    .cursor_panel()
                    .unwrap_or(self.directories.len().saturating_sub(1));
                self.directories.send(idx, DirectoryMessage::SelectFirst);
            }
            AppMsg::NavLast => {
                let idx = self
                    .cursor_panel()
                    .unwrap_or(self.directories.len().saturating_sub(1));
                self.directories.send(idx, DirectoryMessage::SelectLast);
            }
            AppMsg::NavInto => {
                if let Some(idx) = self.cursor_panel() {
                    if idx + 1 < self.directories.len() {
                        // The selection is a directory: its listing is the next panel.
                        self.directories.send(idx + 1, DirectoryMessage::SelectFirst);
                    } else {
                        // The selection is a file (files never push a panel).
                        self.directories.send(idx, DirectoryMessage::OpenSelected);
                    }
                }
            }
            AppMsg::NavParent => {
                match self.cursor_panel() {
                    Some(idx) if idx > 0 => {
                        self.directories.send(idx, DirectoryMessage::UnselectAll);
                    }
                    // Cursor on the root column (or nowhere): go up one level.
                    _ => {
                        if let Some(parent) = self.root.parent() {
                            sender.input(AppMsg::NewRoot(parent));
                        }
                    }
                }
            }
        }
    }

    fn post_view(&self, widgets: &mut Self::Widgets) {
        if self.state.is_maximized {
            widgets.main_window.maximize();
        }

        if self.update_directory_scroll_position {
            // Although this function is already called whenever the hadjustment changes, we also
            // sometimes want to scroll when the adjustment doesn't change.
            //
            // Consider the user selecting a new directory entry on a partially obscured panel. The
            // adjustment won't change, because the total number of panels is the same. However,
            // we still want to scroll over to it because it's new information that the user wants
            // to see.
            set_adjustment_to_upper_bound(&widgets.directory_panes_scroller.hadjustment());
        }
    }
}

relm4::new_action_group!(WindowActionGroup, "win");
relm4::new_stateless_action!(AboutAction, WindowActionGroup, "about");
relm4::new_stateless_action!(MountAction, WindowActionGroup, "mount");
relm4::new_stateful_action!(ToggleHiddenAction, WindowActionGroup, "toggle-hidden", (), bool);

/// Updates the value of an adjustment to its upper bound.
///
/// This is used to keep new directories and file information visible inside the directory panes
/// scroll window as user interacts with the application.
fn set_adjustment_to_upper_bound(adjustment: &gtk::Adjustment) {
    adjustment.set_value(adjustment.upper());
}
