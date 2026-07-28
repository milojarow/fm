//! Application entrypoint.

use std::convert::identity;
use std::path::{self, PathBuf};

use gtk::{gdk, gio, glib, pango, prelude::*};
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

    /// The index of the directory panel an active search applies to.
    search_panel: Option<usize>,

    /// Monotonic id of the latest corner toast; expiry timers only hide the
    /// toast if theirs is still the newest.
    toast_epoch: std::rc::Rc<std::cell::Cell<u64>>,

    /// True while the columns area is too narrow for a computed layout and the
    /// panels fall back to uniform widths. Those overflow the scroller, so the
    /// view has to follow the newest column the way it did before the tapering
    /// layout existed. Shared with the scroller's `upper` hook.
    columns_overflow: std::rc::Rc<std::cell::Cell<bool>>,

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

    /// Tells each panel whether it owns the keyboard cursor. Only that panel
    /// glows its cursor row; the ancestors keep their selection as a quiet
    /// breadcrumb, so exactly one row on screen says "you are here".
    fn mark_cursor_panel(&self) {
        let cursor = self
            .cursor_panel()
            .unwrap_or_else(|| self.directories.len().saturating_sub(1));

        for index in 0..self.directories.len() {
            self.directories
                .send(index, DirectoryMessage::SetCursorPanel(index == cursor));
        }
    }

    /// Applies the tapering column layout: ancestors thin out to the left, the
    /// cursor's column stays centred, and the preview absorbs the remainder.
    fn relayout(&self, widgets: &AppWidgets) {
        let area = widgets.directory_panes_scroller.width();
        let cursor = self
            .cursor_panel()
            .unwrap_or_else(|| self.directories.len().saturating_sub(1));

        match crate::layout::solve(area, self.directories.len(), cursor) {
            Some(plan) => {
                self.columns_overflow.set(false);
                widgets.directory_panes.set_margin_start(plan.gutter);
                for (index, panel) in plan.panels.iter().enumerate() {
                    self.directories
                        .send(index, DirectoryMessage::SetLayout(*panel));
                }
            }
            None => {
                // Uniform columns overflow a narrow area, so the pre-layout
                // behaviour comes back with them: keep the rightmost panels in
                // view. The panels resize on a later main loop turn, so this
                // only covers the case where the adjustment never changes (a
                // new selection inside an existing panel); the `upper` hook in
                // `init` catches the rest.
                self.columns_overflow.set(true);
                widgets.directory_panes.set_margin_start(0);
                for index in 0..self.directories.len() {
                    self.directories.send(index, DirectoryMessage::ResetLayout);
                }

                let adjustment = widgets.directory_panes_scroller.hadjustment();
                adjustment.set_value(adjustment.upper());
            }
        }
    }

    /// Retitles the header with the path the cursor is in, abbreviating
    /// ancestor names from the left until the label's width accepts it.
    fn retitle(&self, widgets: &AppWidgets) {
        let cursor = self
            .cursor_panel()
            .unwrap_or_else(|| self.directories.len().saturating_sub(1));

        let Some(dir) = self.directories.get(cursor).map(|panel| panel.dir()) else {
            return;
        };

        let label = &widgets.path_title;

        let Some(path) = dir.path() else {
            // A gvfs location — `trash:///`, `smb://…`, a phone over MTP — has
            // no local path. Leaving the previous directory's path on screen
            // would claim the user is somewhere they are not, so name the
            // location itself.
            label.set_markup(&crate::path_title::uri_markup(&dir.uri()));
            return;
        };

        let segments = crate::path_title::segments(&path, Some(&glib::home_dir()));

        let available = label.width();
        let layout = label.create_pango_layout(None);
        let fits = |candidate: &str| {
            // Before the first allocation there is nothing to measure against;
            // show the full path and let the next relayout shorten it.
            if available <= 0 {
                return true;
            }
            layout.set_text(candidate);
            layout.pixel_size().0 <= available
        };

        label.set_markup(&crate::path_title::markup(&crate::path_title::shorten(
            &segments, fits,
        )));
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

    /// Put the cursor panel's operation set on the clipboard (`Ctrl+C`, `Ctrl+X`).
    ClipboardCopy(crate::clipboard::ClipboardOp),

    /// Paste the clipboard into the cursor panel's directory (`Ctrl+V`).
    ClipboardPaste,

    /// The columns area changed size; recompute the column widths.
    Relayout,

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

    /// Sort by the given key; selecting the already-active key reverses the order.
    SetSort(config::SortKey),

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

    /// Toggle the mark on the cursor row and advance (`Space`).
    ToggleMark,

    /// Send the selected entries to the trash (`Delete`).
    TrashSelected,

    /// Permanently delete the selected entries (`Shift+Delete`).
    DeletePermanentSelected,
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
                        #[wrap(Some)]
                        #[name = "path_title"]
                        set_title_widget = &gtk::Label {
                            add_css_class: "title",
                            set_hexpand: true,
                            set_single_line_mode: true,
                            // Last resort when even the fully abbreviated path
                            // overflows a very narrow window.
                            set_ellipsize: pango::EllipsizeMode::Middle,
                        },

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
                    sort_key: config::sort_key(),
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
        config::set_sort_key(state.sort_key);
        config::set_sort_reversed(state.sort_reversed);

        let file_preview = FilePreviewModel::builder().launch(()).detach();

        // Every column has a computed width; the preview takes whatever is
        // left, so rounding can never produce a scrollbar.
        file_preview.widget().set_hexpand(true);

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
            search_panel: None,
            toast_epoch: Default::default(),
            columns_overflow: Default::default(),
            state,
        };

        model.directories.guard().push_back((dir, true));

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
        let relayout_sender = sender.clone();

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

            // Ctrl+C / Ctrl+X, ahead of the modifier bail below so every other
            // accelerator (Ctrl+H for hidden files) still passes through. The
            // focus guard above has already run, so these never fire while a
            // text entry holds the caret — there, Ctrl+C copies text.
            if state.contains(gdk::ModifierType::CONTROL_MASK)
                && !state.contains(gdk::ModifierType::ALT_MASK)
            {
                match keyval {
                    gdk::Key::c | gdk::Key::C => {
                        key_sender
                            .input(AppMsg::ClipboardCopy(crate::clipboard::ClipboardOp::Copy));
                        return glib::Propagation::Stop;
                    }
                    gdk::Key::x | gdk::Key::X => {
                        key_sender
                            .input(AppMsg::ClipboardCopy(crate::clipboard::ClipboardOp::Cut));
                        return glib::Propagation::Stop;
                    }
                    gdk::Key::v | gdk::Key::V => {
                        key_sender.input(AppMsg::ClipboardPaste);
                        return glib::Propagation::Stop;
                    }
                    _ => {}
                }
            }

            if state.intersects(gdk::ModifierType::CONTROL_MASK | gdk::ModifierType::ALT_MASK) {
                return glib::Propagation::Proceed;
            }

            if pending_sort.take() {
                match keyval {
                    gdk::Key::m => key_sender.input(AppMsg::SetSort(config::SortKey::Modified)),
                    gdk::Key::n => key_sender.input(AppMsg::SetSort(config::SortKey::Name)),
                    gdk::Key::t => key_sender.input(AppMsg::SetSort(config::SortKey::Type)),
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
                gdk::Key::space => {
                    key_sender.input(AppMsg::ToggleMark);
                    glib::Propagation::Stop
                }
                gdk::Key::Delete => {
                    if state.contains(gdk::ModifierType::SHIFT_MASK) {
                        key_sender.input(AppMsg::DeletePermanentSelected);
                    } else {
                        key_sender.input(AppMsg::TrashSelected);
                    }
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

        // page-size is the viewport width: it changes on window resize and when
        // the places sidebar is folded away.
        widgets
            .directory_panes_scroller
            .hadjustment()
            .connect_notify_local(Some("page-size"), {
                let sender = relayout_sender;
                move |_, _| sender.input(AppMsg::Relayout)
            });

        // A computed layout never overflows, but the uniform fallback for a
        // too-narrow area does, and a panel pushed onto the stack widens the
        // paned one main loop turn after the relayout ran. Follow the newest
        // column then, and only then.
        widgets
            .directory_panes_scroller
            .hadjustment()
            .connect_notify_local(Some("upper"), {
                let overflow = model.columns_overflow.clone();
                move |adjustment: &gtk::Adjustment, _| {
                    if overflow.get() {
                        adjustment.set_value(adjustment.upper());
                    }
                }
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
        match msg {
            AppMsg::Error(err) => {
                self.error_alert.emit(AlertMsg::Show {
                    text: err.to_string(),
                });
            }
            AppMsg::NewSelection(Selection::Files(selection)) => {
                let mut last_dir = self.last_dir();

                // With several rows marked, panel pushes and the preview follow
                // the keyboard cursor rather than the whole batch.
                let file = selection
                    .cursor_file
                    .as_ref()
                    .or_else(|| (selection.files.len() == 1).then(|| selection.files.first().unwrap()))
                    .unwrap_or(&selection.parent);

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
                                directories.push_back((component_file.clone(), false));
                                last_dir = component_file;
                            }
                        }
                        _ => unreachable!("unexpected path component: {:?}", component),
                    }
                }

                self.file_preview
                    .emit(FilePreviewMsg::NewSelection(selection));
            }
            AppMsg::NewSelection(Selection::None) => {
                self.file_preview.emit(FilePreviewMsg::Hide);
            }
            AppMsg::NewRoot(new_root) => {
                info!("new root: {:?}", new_root);

                let mut directories = self.directories.guard();

                directories.clear();

                self.root = new_root;
                directories.push_back((self.root.clone(), true));

                self.file_preview.emit(FilePreviewMsg::Hide);
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
            // Handled by the relayout at the end of this function.
            AppMsg::Relayout => {}
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
                // Search the panel the cursor lives in — the deepest listing may
                // be an unselected child spawned by the cursor sitting on a
                // directory (root panels auto-select their first row).
                self.search_panel = Some(
                    self.cursor_panel()
                        .unwrap_or(self.directories.len().saturating_sub(1)),
                );
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
            AppMsg::SetSort(key) => {
                if config::sort_key() == key {
                    config::set_sort_reversed(!config::sort_reversed());
                } else {
                    config::set_sort_key(key);
                    // Modified starts newest-first; name and type start ascending.
                    config::set_sort_reversed(key == config::SortKey::Modified);
                }
                refresh_sorters();

                let description = match (key, config::sort_reversed()) {
                    (config::SortKey::Modified, true) => "Sort: modified (newest first)",
                    (config::SortKey::Modified, false) => "Sort: modified (oldest first)",
                    (config::SortKey::Name, false) => "Sort: name (A\u{2192}Z)",
                    (config::SortKey::Name, true) => "Sort: name (Z\u{2192}A)",
                    (config::SortKey::Type, false) => "Sort: type (dirs first)",
                    (config::SortKey::Type, true) => "Sort: type (reversed)",
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
            AppMsg::ToggleMark => {
                if let Some(idx) = self.cursor_panel() {
                    self.directories.send(idx, DirectoryMessage::ToggleMark);
                }
            }
            AppMsg::ClipboardCopy(op) => {
                let idx = self
                    .cursor_panel()
                    .unwrap_or_else(|| self.directories.len().saturating_sub(1));
                self.directories
                    .send(idx, DirectoryMessage::ClipboardCopy(op));
            }
            AppMsg::ClipboardPaste => {
                // An empty directory has no rows, so no panel holds a cursor —
                // and pasting into a directory you just made empty is the most
                // ordinary paste there is. Fall back to the deepest panel, the
                // same way SearchOpen, NavFirst and NavLast already do.
                let idx = self
                    .cursor_panel()
                    .unwrap_or_else(|| self.directories.len().saturating_sub(1));
                self.directories.send(idx, DirectoryMessage::ClipboardPaste);
            }
            AppMsg::TrashSelected => {
                if let Some(idx) = self.cursor_panel() {
                    self.directories.send(idx, DirectoryMessage::TrashSelection);
                }
            }
            AppMsg::DeletePermanentSelected => {
                if let Some(idx) = self.cursor_panel() {
                    self.directories
                        .send(idx, DirectoryMessage::DeleteSelectionPermanent);
                }
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

        self.mark_cursor_panel();
        self.relayout(widgets);
        self.retitle(widgets);
    }

    fn post_view(&self, widgets: &mut Self::Widgets) {
        if self.state.is_maximized {
            widgets.main_window.maximize();
        }
    }
}

relm4::new_action_group!(WindowActionGroup, "win");
relm4::new_stateless_action!(AboutAction, WindowActionGroup, "about");
relm4::new_stateless_action!(MountAction, WindowActionGroup, "mount");
relm4::new_stateful_action!(ToggleHiddenAction, WindowActionGroup, "toggle-hidden", (), bool);
