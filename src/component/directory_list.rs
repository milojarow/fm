//! Factory widget that displays a listing of the contents of a directory.

use std::cell::RefCell;
use std::collections::HashMap;
use std::fmt::{self, Debug};
use std::sync::{Arc, Mutex};

use anyhow::bail;
use educe::Educe;
use futures::prelude::*;
use glib::clone;
use glib::translate::{from_glib_full, IntoGlib};
use relm4::actions::{ActionGroupName, RelmAction, RelmActionGroup};
use relm4::factory::{DynamicIndex, FactoryComponent, FactorySender};
use relm4::gtk::{gdk, gio, glib, pango, prelude::*};
use relm4::prelude::*;
use relm4::view;
use tracing::*;

use super::app::AppMsg;
use super::new_folder_dialog::{NewFolderDialog, NewFolderDialogMsg};
use crate::config;
use crate::ops;
use crate::util::{self, fmt_files_as_uris, GFileInfoExt};

mod actions;

use actions::*;

/// The requested minimum width of the widget.
const WIDTH: i32 = 200;

/// The spacing between elements of a list item.
const SPACING: i32 = 2;

/// Button number identifying the right click button on a mouse.
const BUTTON_RIGHT_CLICK: u32 = 3;

#[derive(Educe)]
#[educe(Debug)]
pub struct Directory {
    /// The sorted list model (with a selection) that is displayed in the list view.
    list_model: gtk::MultiSelection,

    new_folder_dialog: Option<Controller<NewFolderDialog>>,

    /// The active search term for this panel (empty when no search is active).
    search_term: String,

    /// Positions of the entries matching the search term, in list order.
    search_matches: Vec<u32>,

    /// Index into `search_matches` of the match the cursor is on.
    search_current: usize,

    /// Weak handles to the currently bound list rows, used to reach a row's
    /// widget (and its per-row actions, e.g. rename) from a keyboard shortcut.
    #[educe(Debug(ignore))]
    bound_rows: std::rc::Rc<RefCell<Vec<(glib::WeakRef<gtk::ListItem>, glib::WeakRef<gtk::Widget>)>>>,

    /// Root panels select their first row once loaded, so the cursor always
    /// exists and `l`/`Enter` work without a `j` first. Panels spawned by
    /// descending stay unselected — the cursor lives in their parent.
    select_first_on_load: bool,

    /// Position of the keyboard cursor row. Shared with the selection-changed
    /// closure so the preview can follow the cursor; cleared when items shift.
    #[educe(Debug(ignore))]
    cursor: std::rc::Rc<std::cell::Cell<Option<u32>>>,

    /// URI of the file under the cursor, so the cursor can follow the file
    /// rather than an index when the listing shifts underneath it.
    #[educe(Debug(ignore))]
    cursor_uri: std::rc::Rc<RefCell<Option<String>>>,

    /// URIs marked with Space. Marks live outside the GTK selection (the
    /// selection is the cursor bar alone), render in their own style, and
    /// being URI-keyed they survive sorting and refreshes.
    #[educe(Debug(ignore))]
    marks: std::rc::Rc<RefCell<std::collections::HashSet<String>>>,

    /// True while this panel owns the keyboard cursor. Only that panel's
    /// row wears the glow; ancestors keep their selection rendered quietly.
    is_cursor_panel: std::rc::Rc<std::cell::Cell<bool>>,

    /// True while this panel is too narrow to show its listing. Shared with the
    /// loading handler so a refresh cannot bounce a sliver back to the listing.
    sliver: std::rc::Rc<std::cell::Cell<bool>>,
}

impl Directory {
    /// Returns the listed directory.
    pub fn dir(&self) -> gio::File {
        self.directory_list().file().unwrap()
    }

    /// Whether the keyboard cursor lives in this panel.
    ///
    /// Marks deliberately do not count. A marked row is a selected row, so a
    /// panel holding marks keeps reporting `Selection::Files` long after the
    /// cursor left it — which used to pin the app's idea of the cursor panel
    /// there and make `h` a dead key once anything was marked.
    pub fn has_cursor(&self) -> bool {
        self.cursor
            .get()
            .is_some_and(|pos| self.list_model.is_selected(pos))
    }

    /// Returns the underlying directory list model.
    fn directory_list(&self) -> gtk::DirectoryList {
        directory_list_of(&self.list_model)
    }

    /// Recomputes the positions of the entries matching the current search term.
    fn recompute_matches(&mut self) {
        self.search_matches.clear();

        let term = self.search_term.to_ascii_lowercase();
        if term.is_empty() {
            return;
        }

        for pos in 0..self.list_model.n_items() {
            if let Some(info) = self.list_model.item(pos).and_downcast::<gio::FileInfo>() {
                if info
                    .display_name()
                    .to_ascii_lowercase()
                    .contains(term.as_str())
                {
                    self.search_matches.push(pos);
                }
            }
        }
    }

    /// Moves the keyboard cursor to `pos` and scrolls it into view. The row
    /// the cursor leaves keeps its selection only if it was marked (Space);
    /// other selected rows are never touched, so marks survive navigation.
    ///
    /// Scrolling drives the outer scrolled window directly: the list view sits
    /// inside a Box (which hosts the context menu popover), so its own
    /// Scrollable interface dangles and `list.scroll-to-item` is a no-op.
    /// Rows are uniform, so the row extent is plain arithmetic.
    fn select_and_scroll(&mut self, scroller: &gtk::ScrolledWindow, pos: u32) {
        let n = self.list_model.n_items();
        if n == 0 || pos >= n {
            return;
        }

        // The GTK selection is the cursor bar alone; marks render separately.
        self.cursor.set(Some(pos));
        *self.cursor_uri.borrow_mut() = self
            .list_model
            .item(pos)
            .and_downcast::<gio::FileInfo>()
            .and_then(|info| info.file())
            .map(|file| file.uri().to_string());
        self.list_model.select_item(pos, true);
        self.restyle_cursor();

        let vadj = scroller.vadjustment();
        let row_height = vadj.upper() / f64::from(n);
        let (row_top, row_bottom) = (f64::from(pos) * row_height, f64::from(pos + 1) * row_height);

        if row_top < vadj.value() {
            vadj.set_value(row_top);
        } else if row_bottom > vadj.value() + vadj.page_size() {
            vadj.set_value(row_bottom - vadj.page_size());
        }
    }

    /// Returns the file info an operation applies to: the marked entries, or
    /// the row under the cursor when nothing is marked.
    ///
    /// This function does not perform any I/O.
    fn selected_file_info(&self) -> Vec<gio::FileInfo> {
        let marks = self.marks.borrow();
        let mut marked = Vec::new();
        let mut under_cursor = Vec::new();

        for pos in 0..self.list_model.n_items() {
            if let Some(info) = self.list_model.item(pos).and_downcast::<gio::FileInfo>() {
                if marks.contains(info.file().unwrap().uri().as_str()) {
                    marked.push(info);
                } else if self.list_model.is_selected(pos) {
                    under_cursor.push(info);
                }
            }
        }

        operation_rows(marked, under_cursor)
    }

    /// Returns the file info under the keyboard cursor, if any.
    fn cursor_file_info(&self) -> Option<gio::FileInfo> {
        self.cursor
            .get()
            .and_then(|pos| self.list_model.item(pos))
            .and_downcast::<gio::FileInfo>()
    }

    /// Re-applies the cursor style across the visible rows.
    ///
    /// The glow cannot ride the GTK selection: a provider rule on
    /// `listview > row` never reaches those nodes, measured with a magenta
    /// test rule that painted nothing while a class rule on our own widget
    /// painted fine. So the cursor bar wears a CSS class on the row's own box,
    /// exactly the way marks already do.
    fn restyle_cursor(&self) {
        // Ancestor panels keep their selection as a quiet breadcrumb; lighting
        // all of them up would defeat the point of finding the cursor.
        let cursor = self.is_cursor_panel.get().then(|| self.cursor.get()).flatten();
        for (list_item, widget) in self.bound_rows.borrow().iter() {
            let (Some(list_item), Some(widget)) = (list_item.upgrade(), widget.upgrade()) else {
                continue;
            };
            if cursor == Some(list_item.position()) {
                widget.add_css_class("cursor-row");
            } else {
                widget.remove_css_class("cursor-row");
            }
        }
    }

    /// Re-applies the marked style to the visible row at `pos`.
    fn restyle_row(&self, pos: u32) {
        let marks = self.marks.borrow();
        for (list_item, widget) in self.bound_rows.borrow().iter() {
            let (Some(list_item), Some(widget)) = (list_item.upgrade(), widget.upgrade()) else {
                continue;
            };
            if list_item.position() != pos {
                continue;
            }
            if let Some(info) = list_item.item().and_downcast::<gio::FileInfo>() {
                if marks.contains(info.file().unwrap().uri().as_str()) {
                    widget.add_css_class("marked");
                } else {
                    widget.remove_css_class("marked");
                }
            }
            break;
        }
    }
}

/// Used to communicate the file selection status to the parent widget.
#[derive(Educe)]
#[educe(Debug)]
pub enum Selection {
    /// A selection of at least one file.
    Files(FileSelection),

    /// No file is selected.
    None,
}

/// A selection of at least one file.
#[derive(Educe)]
#[educe(Debug)]
pub struct FileSelection {
    /// The shared parent of the selected files.
    #[educe(Debug(method = "util::fmt_file_as_uri"))]
    pub parent: gio::File,

    /// The selected files.
    #[educe(Debug(method = "util::fmt_files_as_uris"))]
    pub files: Vec<gio::File>,

    /// The file under the keyboard cursor, when one exists — previews and
    /// descend logic follow it while several rows are marked.
    #[educe(Debug(ignore))]
    pub cursor_file: Option<gio::File>,
}

#[derive(Debug)]
pub enum DirectoryMessage {
    OpenItemAtPosition(u32),

    /// Open the application launcher dialog for the given file.
    ChooseAndLaunchApp(gio::File),

    /// Send the files in the current selection to the trash.
    TrashSelection,

    /// Restore files in the current selection from the trash.
    RestoreSelectionFromTrash,

    ShowNewFolderDialog,

    /// Set the search term: recompute matches, highlight them, jump to the first.
    SetSearch(String),

    /// Move the cursor to the next search match.
    SearchNext,

    /// Move the cursor to the previous search match.
    SearchPrev,

    /// Clear the search term and its highlights.
    ClearSearch,

    /// Open the rename popover for the currently selected entry.
    RenameSelected,

    /// Move the cursor (selection) by the given delta within this panel.
    MoveCursor(i32),

    /// Put the cursor on the first entry.
    SelectFirst,

    /// Put the cursor on the last entry.
    SelectLast,

    /// Unselect everything, moving the cursor out of this panel.
    UnselectAll,

    /// Open the currently selected entry with its default application.
    OpenSelected,

    /// Select the first row once the listing has loaded, if this panel was
    /// created wanting an initial cursor (root panels).
    AutoSelectIfPending,

    /// Toggle the mark on the cursor row and advance the cursor (Space).
    ToggleMark,

    /// Permanently delete the selected entries (Shift+Delete). No trash.
    DeleteSelectionPermanent,

    /// Items shifted (load, sort, external changes): stored cursor position is
    /// no longer trustworthy.
    InvalidateCursor,

    /// Put this panel's operation set on the system clipboard.
    ClipboardCopy(crate::clipboard::ClipboardOp),

    /// Paste the clipboard into this panel's directory.
    ClipboardPaste,

    /// Whether this panel currently owns the keyboard cursor.
    SetCursorPanel(bool),

    /// Take the width and page the app's column layout computed for this panel.
    SetLayout(crate::layout::PanelLayout),

    /// Fall back to the uniform column width; the window is too narrow to lay
    /// out and the view scrolls instead.
    ResetLayout,
}

#[relm4::factory(pub)]
impl FactoryComponent for Directory {
    type ParentWidget = panel::Paned;
    type Widgets = DirectoryWidgets;
    type Init = (gio::File, bool);
    type Input = DirectoryMessage;
    type Output = AppMsg;
    type CommandOutput = ();

    view! {
        root = gtk::Stack {
            set_width_request: WIDTH,

            // Only the visible page's width is requested, so a sliver can go
            // below the 58px a listing needs.
            set_hhomogeneous: false,

            add_child = &gtk::Spinner {
                set_halign: gtk::Align::Center,
                set_valign: gtk::Align::Center,
                set_spinning: true,
            } -> { set_name: "spinner" },

            #[name = "sliver_label"]
            add_child = &gtk::Label {
                add_css_class: "column-sliver",
                set_vexpand: true,
                set_yalign: 0.0,
                set_margin_top: 8,
            } -> { set_name: "sliver" },

            #[name = "scroller"]
            add_child = &gtk::ScrolledWindow {
                set_hscrollbar_policy: gtk::PolicyType::Never,

                #[wrap(Some)]
                set_child = &gtk::Box {
                    set_layout_manager: Some(gtk::BinLayout::new()),

                    #[name = "list_view"]
                    gtk::ListView {
                        set_factory: Some(&factory),
                        set_model: Some(&self.list_model),

                        connect_activate[sender] => move |_, position| {
                            sender.input(DirectoryMessage::OpenItemAtPosition(position))
                        },
                    },

                    #[name = "context_menu"]
                    gtk::PopoverMenu::from_model(gio::MenuModel::NONE) {
                        set_has_arrow: false,
                    },
                },
            } -> { set_name: "listing" },
        }
    }

    fn init_model(
        (dir, select_first_on_load): Self::Init,
        _index: &DynamicIndex,
        _sender: FactorySender<Self>,
    ) -> Self {
        debug_assert!(
            dir.query_file_type(gio::FileQueryInfoFlags::NONE, gio::Cancellable::NONE)
                == gio::FileType::Directory
        );

        let directory_list = gtk::DirectoryList::new(
            Some(
                &[
                    &**gio::FILE_ATTRIBUTE_STANDARD_NAME,
                    &**gio::FILE_ATTRIBUTE_STANDARD_DISPLAY_NAME,
                    &**gio::FILE_ATTRIBUTE_STANDARD_ICON,
                    &**gio::FILE_ATTRIBUTE_STANDARD_TYPE,
                    &**gio::FILE_ATTRIBUTE_STANDARD_CONTENT_TYPE,
                    &**gio::FILE_ATTRIBUTE_STANDARD_IS_SYMLINK,
                    &**gio::FILE_ATTRIBUTE_STANDARD_IS_HIDDEN,
                    &**gio::FILE_ATTRIBUTE_TIME_MODIFIED,
                ]
                .join(","),
            ),
            Some(&dir),
        );

        let list_model =
            gtk::FilterListModel::new(Some(directory_list.clone()), Some(hidden_filter()));

        let list_model = gtk::SortListModel::new(Some(list_model), Some(file_sorter()));

        let list_model = gtk::MultiSelection::new(Some(list_model));

        Directory {
            list_model,

            // This can't be initialized here, since we need make the dialog transient for
            // something but we don't have a reference to a widget here.
            new_folder_dialog: None,

            search_term: String::new(),
            search_matches: Vec::new(),
            search_current: 0,
            bound_rows: Default::default(),
            select_first_on_load,
            cursor: Default::default(),
            cursor_uri: Default::default(),
            marks: Default::default(),
            is_cursor_panel: Default::default(),
            sliver: Default::default(),
        }
    }

    fn init_widgets(
        &mut self,
        _index: &DynamicIndex,
        root: Self::Root,
        _returned_widget: &gtk::Widget,
        sender: FactorySender<Self>,
    ) -> Self::Widgets {
        let factory = gtk::SignalListItemFactory::new();

        factory.connect_setup(clone!(
            #[strong]
            sender,
            #[weak(rename_to = selection)]
            self.list_model,
            move |_, item| {
                let item = item.downcast_ref::<gtk::ListItem>().unwrap();
                build_list_item_view(&selection, item, &sender);
            }
        ));

        // Store the drop controllers we add by widget so that we can remove them on unbind.
        #[allow(clippy::arc_with_non_send_sync)]
        let controllers = Arc::new(Mutex::new(HashMap::new()));

        let bound_rows = self.bound_rows.clone();
        let marks = self.marks.clone();
        let cursor = self.cursor.clone();
        let is_cursor_panel = self.is_cursor_panel.clone();
        factory.connect_bind(clone!(
            #[strong]
            sender,
            #[strong]
            controllers,
            #[strong]
            bound_rows,
            #[strong]
            marks,
            #[strong]
            cursor,
            #[strong]
            is_cursor_panel,
            move |_, list_item| {
                let list_item = list_item.downcast_ref::<gtk::ListItem>().unwrap();
                let widget = list_item.child().unwrap();

                let info = list_item.item().and_downcast::<gio::FileInfo>().unwrap();

                bound_rows
                    .borrow_mut()
                    .push((list_item.downgrade(), widget.downgrade()));

                // Recycled row widgets carry stale style state: re-derive it.
                if marks.borrow().contains(info.file().unwrap().uri().as_str()) {
                    widget.add_css_class("marked");
                } else {
                    widget.remove_css_class("marked");
                }

                if is_cursor_panel.get() && cursor.get() == Some(list_item.position()) {
                    widget.add_css_class("cursor-row");
                } else {
                    widget.remove_css_class("cursor-row");
                }

                if matches!(info.file_type(), gio::FileType::Directory) {
                    let dir = info.file().unwrap();
                    let target = new_drop_target_for_dir(dir, sender.clone());
                    widget.add_controller(target.clone());
                    controllers.lock().unwrap().insert(widget, target);
                }
            }
        ));

        let bound_rows = self.bound_rows.clone();
        factory.connect_unbind(move |_, list_item| {
            let list_item = list_item.downcast_ref::<gtk::ListItem>().unwrap();
            let widget = list_item.child().unwrap();

            bound_rows
                .borrow_mut()
                .retain(|(_, w)| w.upgrade().is_some_and(|w| w != widget));

            if let Some(controller) = controllers.lock().unwrap().remove(&widget) {
                widget.remove_controller(&controller);
            }
        });

        let sender_ = sender.clone();
        let cursor_ = self.cursor.clone();
        let marks_ = self.marks.clone();
        self.list_model
            .connect_selection_changed(move |selection, _, _| {
                send_new_selection(selection, &sender_, cursor_.get(), &marks_);
            });
        let sender_ = sender.clone();
        let cursor_ = self.cursor.clone();
        let marks_ = self.marks.clone();
        self.list_model
            .connect_items_changed(move |selection, _, _, _| {
                sender_.input(DirectoryMessage::InvalidateCursor);
                sender_.input(DirectoryMessage::AutoSelectIfPending);
                send_new_selection(selection, &sender_, cursor_.get(), &marks_);
            });

        let widgets = view_output!();

        let click_controller = gtk::GestureClick::builder()
            .button(BUTTON_RIGHT_CLICK)
            .build();
        let dir = self.dir();

        let menu = &widgets.context_menu;

        click_controller.connect_pressed(clone!(
            #[strong]
            menu,
            move |_, _, x, y| {
                let model = populate_directory_menu_model();

                menu.set_menu_model(Some(&model));
                menu.set_pointing_to(Some(&gdk::Rectangle::new(x as i32, y as i32, 1, 1)));
                menu.popup();
            }
        ));
        register_directory_context_actions(widgets.list_view.upcast_ref(), sender.clone());
        widgets.list_view.add_controller(click_controller);

        let name = self
            .dir()
            .basename()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        widgets
            .sliver_label
            .set_text(&crate::path_title::initial(&name));

        let directory_list = self.directory_list();
        directory_list.connect_loading_notify(clone!(
            #[weak(rename_to = stack)]
            widgets.root,
            #[strong(rename_to = sliver)]
            self.sliver,
            move |list| apply_page(&stack, list.is_loading(), sliver.get())
        ));
        apply_page(
            &widgets.root,
            directory_list.is_loading(),
            self.sliver.get(),
        );

        let drop_target = new_drop_target_for_dir(self.dir(), sender);
        widgets.list_view.add_controller(drop_target);

        self.new_folder_dialog = Some(
            NewFolderDialog::builder()
                .transient_for(&widgets.list_view)
                .launch(dir)
                .detach(),
        );

        widgets
    }

    fn update_with_view(
        &mut self,
        widgets: &mut Self::Widgets,
        msg: Self::Input,
        sender: FactorySender<Self>,
    ) {
        match msg {
            DirectoryMessage::OpenItemAtPosition(pos) => {
                let file_info = self
                    .list_model
                    .item(pos)
                    .and_downcast::<gio::FileInfo>()
                    .unwrap();

                debug!(
                    "opening item at position {}: {}",
                    pos,
                    file_info.display_name()
                );

                open_application_for_file(&file_info.file().unwrap(), &sender);
            }
            DirectoryMessage::ChooseAndLaunchApp(file) => {
                let dialog = gtk::AppChooserDialog::new(
                    widgets.root.toplevel_window().as_ref(),
                    gtk::DialogFlags::MODAL,
                    &file,
                );

                dialog.connect_response(clone!(
                    #[strong]
                    file,
                    move |this, response| {
                        if let gtk::ResponseType::Ok = response {
                            if let Some(app_info) = this.app_info() {
                                let _ =
                                    app_info.launch(&[file.clone()], gio::AppLaunchContext::NONE);
                            }
                        }

                        this.hide();
                    }
                ));

                dialog.show();
            }
            DirectoryMessage::TrashSelection => {
                let selected_file_info = self.selected_file_info();

                info!("trashing files: {:?}", fmt_file_info(&selected_file_info));

                let sender = sender.clone();
                relm4::spawn_local(async move {
                    let results = future::join_all(selected_file_info.iter().map(|f| {
                        f.file()
                            .unwrap()
                            .trash_future(glib::Priority::DEFAULT)
                            .map(move |res| (res, f))
                    }))
                    .await;

                    let trashed_files = results
                        .into_iter()
                        .flat_map(|(result, info)| match result {
                            Ok(_) => Some(info),
                            Err(e) => {
                                sender.output(AppMsg::Error(Box::new(e))).unwrap();
                                None
                            }
                        })
                        .collect::<Vec<_>>();

                    if !trashed_files.is_empty() {
                        sender
                            .output(AppMsg::Toast(match &trashed_files[..] {
                                [info] => format!("'{}' moved to trash", info.display_name()),
                                _ => format!("{} files moved to trash", trashed_files.len()),
                            }))
                            .unwrap();
                    }
                });
            }
            DirectoryMessage::RestoreSelectionFromTrash => {
                let selected_file_info = self.selected_file_info();

                info!("restoring files: {:?}", fmt_file_info(&selected_file_info));

                let sender = sender.clone();
                relm4::spawn_local(async move {
                    future::join_all(selected_file_info.iter().map(|info| async {
                        let file = info.file().unwrap();

                        let info = file
                            .query_info_future(
                                gio::FILE_ATTRIBUTE_TRASH_ORIG_PATH,
                                gio::FileQueryInfoFlags::empty(),
                                glib::Priority::DEFAULT,
                            )
                            .await;

                        let info = match info {
                            Ok(info) => info,
                            Err(err) => {
                                sender.output(AppMsg::Error(Box::new(err))).unwrap();
                                return;
                            }
                        };

                        let original_path = info
                            .attribute_byte_string(gio::FILE_ATTRIBUTE_TRASH_ORIG_PATH)
                            .unwrap();
                        let original_path = gio::File::for_parse_name(&original_path);

                        ops::move_(file, original_path, sender.output_sender().clone()).await;
                    }))
                    .await;
                });
            }
            DirectoryMessage::ShowNewFolderDialog => self
                .new_folder_dialog
                .as_ref()
                .unwrap()
                .emit(NewFolderDialogMsg::Show),
            DirectoryMessage::SetSearch(term) => {
                self.search_term = term;
                set_search_term(&self.search_term);
                self.recompute_matches();
                self.search_current = 0;

                if let Some(&pos) = self.search_matches.first() {
                    self.select_and_scroll(&widgets.scroller, pos);
                }

                refresh_highlights(&widgets.list_view);
            }
            DirectoryMessage::SearchNext => {
                self.recompute_matches();
                if !self.search_matches.is_empty() {
                    self.search_current = (self.search_current + 1) % self.search_matches.len();
                    self.select_and_scroll(&widgets.scroller, self.search_matches[self.search_current]);
                }
            }
            DirectoryMessage::SearchPrev => {
                self.recompute_matches();
                if !self.search_matches.is_empty() {
                    self.search_current = self
                        .search_current
                        .checked_sub(1)
                        .unwrap_or(self.search_matches.len() - 1);
                    self.select_and_scroll(&widgets.scroller, self.search_matches[self.search_current]);
                }
            }
            DirectoryMessage::ClearSearch => {
                self.search_term.clear();
                set_search_term("");
                self.search_matches.clear();
                self.search_current = 0;
                refresh_highlights(&widgets.list_view);
            }
            DirectoryMessage::RenameSelected => {
                if let Some(info) = self.cursor_file_info().as_ref() {
                    let uri = info.file().unwrap().uri().to_string();

                    // Reach the selected entry's bound row widget: its per-row action
                    // group owns the rename popover, anchored at the row itself.
                    let row_widget = self.bound_rows.borrow().iter().find_map(|(li, w)| {
                        let item = li.upgrade()?.item().and_downcast::<gio::FileInfo>()?;
                        (item == *info).then(|| w.upgrade())?
                    });

                    if let Some(widget) = row_widget {
                        let _ = widget
                            .activate_action("directory-list.rename", Some(&uri.to_variant()));
                    }
                }
            }
            DirectoryMessage::MoveCursor(delta) => {
                let n = self.list_model.n_items();
                if n > 0 {
                    let current = self.cursor.get().filter(|&c| c < n).or_else(|| {
                        let selected = self.list_model.selection();
                        (!selected.is_empty()).then(|| selected.minimum())
                    });

                    let pos = match current {
                        Some(current) => (i64::from(current) + i64::from(delta))
                            .clamp(0, i64::from(n - 1))
                            as u32,
                        None if delta >= 0 => 0,
                        None => n - 1,
                    };

                    self.select_and_scroll(&widgets.scroller, pos);
                }
            }
            DirectoryMessage::SelectFirst => {
                if self.list_model.n_items() > 0 {
                    self.select_and_scroll(&widgets.scroller, 0);
                }
            }
            DirectoryMessage::SelectLast => {
                let n = self.list_model.n_items();
                if n > 0 {
                    self.select_and_scroll(&widgets.scroller, n - 1);
                }
            }
            DirectoryMessage::UnselectAll => {
                self.list_model.unselect_all();
            }
            DirectoryMessage::OpenSelected => {
                if let Some(info) = self.cursor_file_info().as_ref() {
                    // The panel already holds the content type, so it decides
                    // here rather than making the app ask for data it has.
                    let is_audio = info
                        .content_type()
                        .and_then(|content_type| gio::content_type_get_mime_type(&content_type))
                        .is_some_and(|mime| mime.starts_with("audio/"));

                    if is_audio {
                        sender.output(AppMsg::ToggleAudioPreview).unwrap();
                    } else {
                        open_application_for_file(&info.file().unwrap(), &sender);
                    }
                }
            }
            DirectoryMessage::AutoSelectIfPending => {
                if self.select_first_on_load && self.list_model.n_items() > 0 {
                    self.select_first_on_load = false;
                    self.select_and_scroll(&widgets.scroller, 0);
                }
            }
            DirectoryMessage::ToggleMark => {
                if let (Some(pos), Some(info)) = (self.cursor.get(), self.cursor_file_info()) {
                    let uri = info.file().unwrap().uri().to_string();
                    {
                        let mut marks = self.marks.borrow_mut();
                        if !marks.remove(&uri) {
                            marks.insert(uri);
                        }
                    }
                    self.restyle_row(pos);

                    // The operation set changed; let the app re-derive previews.
                    send_new_selection(&self.list_model, &sender, self.cursor.get(), &self.marks);

                    // ranger advances after marking.
                    if pos + 1 < self.list_model.n_items() {
                        self.select_and_scroll(&widgets.scroller, pos + 1);
                    }
                }
            }
            DirectoryMessage::DeleteSelectionPermanent => {
                let selected = self.selected_file_info();
                if selected.is_empty() {
                    return;
                }

                info!(
                    "permanently deleting files: {:?}",
                    fmt_file_info(&selected)
                );

                for file_info in &selected {
                    let file = file_info.file().unwrap();
                    let Some(path) = file.path() else { continue };

                    let result = if path.is_dir() {
                        std::fs::remove_dir_all(&path)
                    } else {
                        std::fs::remove_file(&path)
                    };

                    if let Err(e) = result {
                        sender.output(AppMsg::Error(Box::new(e))).unwrap();
                    }
                }
            }
            DirectoryMessage::InvalidateCursor => {
                // Positions shifted; marks are URI-keyed and survive on their own.
                //
                // The cursor must not simply be dropped here. A panel without a
                // cursor stops being the cursor panel, so clearing it made the
                // app believe the cursor had moved to the parent: deleting a
                // file looked like jumping a level up the tree.
                //
                // Follow the file rather than the index. A re-sort moves it
                // somewhere else entirely, and after a delete it is gone — then
                // the old index is the honest fallback, because the row that
                // took its place is where the eye already is.
                let n = self.list_model.n_items();

                let Some(previous) = self.cursor.get() else {
                    return;
                };

                if n == 0 {
                    self.cursor.set(None);
                    self.restyle_cursor();
                    return;
                }

                let wanted = self.cursor_uri.borrow().clone();
                let found = wanted.and_then(|uri| {
                    (0..n).find(|&pos| {
                        self.list_model
                            .item(pos)
                            .and_downcast::<gio::FileInfo>()
                            .and_then(|info| info.file())
                            .is_some_and(|file| file.uri() == uri)
                    })
                });

                let landed = found.unwrap_or_else(|| previous.min(n - 1));
                self.select_and_scroll(&widgets.scroller, landed);

                // Say so out loud. `select_item` emits nothing when the row it
                // lands on is one GTK had already kept selected, so the app
                // would never re-evaluate which panel holds the cursor and
                // would keep drawing the title, the glow and the layout one
                // level up — which is exactly what a delete looked like.
                send_new_selection(&self.list_model, &sender, self.cursor.get(), &self.marks);
            }
            DirectoryMessage::ClipboardCopy(op) => {
                let uris: Vec<String> = self
                    .selected_file_info()
                    .iter()
                    .filter_map(|info| info.file().map(|file| file.uri().to_string()))
                    .collect();

                if uris.is_empty() {
                    sender
                        .output(AppMsg::Toast("Nothing to copy".to_owned()))
                        .unwrap();
                    return;
                }

                let count = uris.len();

                // Both types at once: the GNOME one carries copy-vs-cut, the
                // uri-list is what file pickers and upload dialogs understand.
                let gnome = gdk::ContentProvider::for_bytes(
                    crate::clipboard::GNOME_MIME,
                    &glib::Bytes::from_owned(crate::clipboard::encode(op, &uris).into_bytes()),
                );
                let uri_list = gdk::ContentProvider::for_bytes(
                    crate::clipboard::URI_LIST_MIME,
                    &glib::Bytes::from_owned(
                        crate::clipboard::encode_uri_list(&uris).into_bytes(),
                    ),
                );

                if let Err(err) = widgets
                    .root
                    .clipboard()
                    .set_content(Some(&gdk::ContentProvider::new_union(&[gnome, uri_list])))
                {
                    warn!("unable to set the clipboard: {}", err);
                }

                let verb = match op {
                    crate::clipboard::ClipboardOp::Copy => "copied",
                    crate::clipboard::ClipboardOp::Cut => "cut",
                };
                sender
                    .output(AppMsg::Toast(match count {
                        1 => format!("1 file {verb}"),
                        n => format!("{n} files {verb}"),
                    }))
                    .unwrap();
            }
            DirectoryMessage::ClipboardPaste => {
                let destination_dir = self.dir();
                let clipboard = widgets.root.clipboard();
                let sender = sender.clone();

                relm4::spawn_local(async move {
                    let Some((op, uris)) = read_file_clipboard(&clipboard).await else {
                        sender
                            .output(AppMsg::Toast("Nothing to paste".to_owned()))
                            .unwrap();
                        return;
                    };

                    let mut pasted = 0usize;
                    let mut skipped = 0usize;

                    for uri in uris {
                        let source = gio::File::for_uri(&uri);

                        let Some(name) = source.basename() else {
                            skipped += 1;
                            continue;
                        };

                        // Never paste a directory into itself or below itself:
                        // the walk would feed itself and fill the disk. This is
                        // the one failure here that damages the machine rather
                        // than a file, so it is checked before anything else.
                        if destination_dir.equal(&source) || destination_dir.has_prefix(&source) {
                            warn!("refusing to paste {} into itself", source.uri());
                            skipped += 1;
                            continue;
                        }

                        // A cut landing where it already lives is a no-op, not
                        // an error.
                        if op == crate::clipboard::ClipboardOp::Cut
                            && source.parent().as_ref() == Some(&destination_dir)
                        {
                            skipped += 1;
                            continue;
                        }

                        let name = name.to_string_lossy().into_owned();
                        let target_name = crate::clipboard::free_name(&name, |candidate| {
                            destination_dir
                                .child(candidate)
                                .query_exists(gio::Cancellable::NONE)
                        });
                        let target = destination_dir.child(&target_name);

                        match op {
                            crate::clipboard::ClipboardOp::Copy => {
                                ops::copy_tree(source, target, sender.output_sender().clone()).await;
                            }
                            crate::clipboard::ClipboardOp::Cut => {
                                ops::move_(source, target, sender.output_sender().clone()).await;
                            }
                        }

                        pasted += 1;
                    }

                    // A cut clipboard is spent: its sources have moved, and a
                    // second paste would fail on every one of them.
                    if op == crate::clipboard::ClipboardOp::Cut && pasted > 0 {
                        let _ = clipboard.set_content(None::<&gdk::ContentProvider>);
                    }

                    let message = match (pasted, skipped) {
                        (0, 0) => "Nothing to paste".to_owned(),
                        (0, n) => format!("{n} skipped"),
                        (1, 0) => "1 file pasted".to_owned(),
                        (n, 0) => format!("{n} files pasted"),
                        (n, s) => format!("{n} pasted, {s} skipped"),
                    };
                    sender.output(AppMsg::Toast(message)).unwrap();
                });
            }
            DirectoryMessage::SetCursorPanel(owns_cursor) => {
                self.is_cursor_panel.set(owns_cursor);
                self.restyle_cursor();
            }
            DirectoryMessage::SetLayout(plan) => {
                widgets.root.set_visible(plan.visible);
                if plan.visible {
                    widgets.root.set_width_request(plan.width);
                    self.sliver.set(plan.sliver);
                    apply_page(
                        &widgets.root,
                        self.directory_list().is_loading(),
                        plan.sliver,
                    );
                }
            }
            DirectoryMessage::ResetLayout => {
                widgets.root.set_visible(true);
                widgets.root.set_width_request(WIDTH);
                self.sliver.set(false);
                apply_page(&widgets.root, self.directory_list().is_loading(), false);
            }
        }

        self.update_view(widgets, sender);
    }
}

/// Construct the view for an uninitialized list item, and set it as the item's child.
///
/// This view displays an icon, the name of the file, and an arrow indicating if the item is a file
/// or directory.
fn build_list_item_view(
    selection: &gtk::MultiSelection,
    list_item: &gtk::ListItem,
    sender: &FactorySender<Directory>,
) {
    view! {
        #[name = "root"]
        gtk::Box {
            set_orientation: gtk::Orientation::Horizontal,
            set_hexpand: true,
            set_spacing: SPACING,

            #[name = "icon"]
            gtk::Image {},

            #[name = "file_name"]
            gtk::Label {
                // `.marked .entry-label` in styles.css colours marked entries;
                // without this class that rule matched nothing and a marked row
                // only ever got its indent.
                add_css_class: "entry-label",
                set_ellipsize: pango::EllipsizeMode::Middle,
            },

            #[name = "directory_icon"]
            gtk::Image {
                set_halign: gtk::Align::End,
                set_hexpand: true,
            },

            #[name = "menu"]
            gtk::PopoverMenu::from_model(gio::MenuModel::NONE) {
                set_has_arrow: false,
            },

            #[name = "rename_popover"]
            gtk::Popover {
                gtk::Box {
                    set_orientation: gtk::Orientation::Horizontal,
                    set_spacing: 12,

                    #[name = "entry"]
                    gtk::Entry {},

                    gtk::Button {
                        set_label: "Rename",
                        add_css_class: "suggested-action",
                        connect_clicked[entry] => move |_| {
                            entry.emit_activate();
                        }
                    }
                }
            },
        }
    }

    list_item
        .bind_property("item", &icon, "paintable")
        .transform_to(|_, item: Option<gio::FileInfo>| {
            item.map(|info| {
                // FIXME: How inefficient is it to query this every time?
                let icon_theme = gtk::IconTheme::for_display(&gdk::Display::default().unwrap());

                util::icon_for_file(&icon_theme, 16, &info)
            })
        })
        .build();

    list_item
        .bind_property("item", &file_name, "label")
        .transform_to(|_, item: Option<gio::FileInfo>| item.map(|info| info.display_name()))
        .build();

    list_item
        .bind_property("item", &file_name, "attributes")
        .transform_to(|_, item: Option<gio::FileInfo>| {
            item.map(|info| search_highlight_attrs(&info.display_name()))
        })
        .build();

    list_item
        .bind_property("item", &directory_icon, "gicon")
        .transform_to(|_, item: Option<gio::FileInfo>| {
            item.and_then(|info| match info.file_type() {
                gio::FileType::Directory => {
                    Some(gio::Icon::for_string("go-next-symbolic").unwrap())
                }
                _ => None,
            })
        })
        .build();

    let click_controller = gtk::GestureClick::builder()
        .button(BUTTON_RIGHT_CLICK)
        .build();
    click_controller.connect_pressed(clone!(
        #[weak]
        selection,
        #[weak]
        list_item,
        #[weak]
        menu,
        move |_, _, x, y| {
            // If the clicked item isn't part of the selection, select it.
            let position = list_item.position();

            if !list_item.is_selected() {
                selection.select_item(position, true);
            }

            let item = list_item.item().unwrap();
            let info = item.downcast_ref::<gio::FileInfo>().unwrap();

            let model = populate_entry_menu_model(info);

            menu.set_menu_model(Some(&model));
            menu.set_pointing_to(Some(&gdk::Rectangle::new(x as i32, y as i32, 1, 1)));
            menu.popup();
        }
    ));
    root.add_controller(click_controller);

    let drag_source_controller = gtk::DragSource::builder()
        .actions(gdk::DragAction::MOVE)
        .build();

    // TODO: The documentation seems pretty adamant that you need to listen to `drag-end` if you're
    // supporting `DragAction::MOVE`, but everything seems to work as expected if you don't, at
    // least with Nautilus...
    list_item
        .bind_property("item", &drag_source_controller, "content")
        .transform_to(|_, item: Option<gio::FileInfo>| {
            item.map(|item| {
                let file_info = item.downcast_ref::<gio::FileInfo>().unwrap();
                let file = file_info.file().unwrap();

                // Dip into FFI here since the Rust bindings don't currently provide a way to
                // construct the content provider from a GFile.
                let content_provider: gdk::ContentProvider = unsafe {
                    from_glib_full(gdk::ffi::gdk_content_provider_new_typed(
                        gio::File::static_type().into_glib(),
                        file,
                    ))
                };

                content_provider
            })
        })
        .build();
    root.add_controller(drag_source_controller);

    register_entry_context_actions(root.upcast_ref(), &rename_popover, sender.clone());

    list_item.set_child(Some(&root));
}

/// Register right-click context menu actions and handlers.
fn register_entry_context_actions(
    list_item_view: &gtk::Widget,
    rename_popover: &gtk::Popover,
    sender: FactorySender<Directory>,
) {
    let mut group = RelmActionGroup::<DirectoryListRightClickActionGroup>::new();

    group.add_action(RelmAction::<OpenDefaultAction>::new_with_target_value(
        move |_, uri: String| {
            let _ = gio::AppInfo::launch_default_for_uri(&uri, None::<&gio::AppLaunchContext>);
        },
    ));

    group.add_action(RelmAction::<OpenChooserAction>::new_with_target_value(
        clone!(
            #[strong]
            sender,
            move |_, uri: String| {
                let file = gio::File::for_uri(&uri);
                sender.input(DirectoryMessage::ChooseAndLaunchApp(file));
            }
        ),
    ));

    // This is a bit nasty: we create a new handler each time that the action is activated so that
    // we don't rely on the view alone to provide the file path, instead relying on the action
    // parameter. We have to disconnect the old handler each time because registering a new handler
    // is additive.
    let previous_handler_id = RefCell::new(None);
    group.add_action(RelmAction::<RenameAction>::new_with_target_value(clone!(
        #[weak]
        rename_popover,
        #[strong]
        sender,
        move |_, uri: String| {
            let root = rename_popover
                .child()
                .unwrap()
                .downcast::<gtk::Box>()
                .unwrap();
            let entry = root
                .first_child()
                .unwrap()
                .downcast::<gtk::Entry>()
                .unwrap();

            if let Some(id) = previous_handler_id.borrow_mut().take() {
                glib::signal_handler_disconnect(&entry, id);
            }

            let file = gio::File::for_uri(&uri);
            if let Ok(edit_name) = file
                .query_info(
                    gio::FILE_ATTRIBUTE_STANDARD_EDIT_NAME,
                    gio::FileQueryInfoFlags::NONE,
                    gio::Cancellable::NONE,
                )
                .map(|info| info.edit_name())
            {
                entry.set_text(&edit_name);
            }

            let signal_handler_id = entry.connect_activate(clone!(
                #[weak]
                rename_popover,
                #[strong]
                file,
                #[strong]
                sender,
                move |this| {
                    let new_name = this.text();
                    info!("renaming {} to {}", file.uri(), new_name);

                    let res = (|| -> anyhow::Result<()> {
                        if new_name.is_empty() {
                            bail!("File name cannot be empty.");
                        }

                        file.set_display_name(&new_name, gio::Cancellable::NONE)?;

                        Ok(())
                    })();

                    if let Err(err) = res {
                        sender.output(AppMsg::Error(err.into())).unwrap();
                    }

                    rename_popover.popdown();
                }
            ));

            *previous_handler_id.borrow_mut() = Some(signal_handler_id);

            rename_popover.popup();
        }
    )));

    let sender_ = sender.clone();
    group.add_action(RelmAction::<TrashSelectionAction>::new_stateless(
        move |_| sender_.input(DirectoryMessage::TrashSelection),
    ));

    group.add_action(
        RelmAction::<RestoreSelectionFromTrashAction>::new_stateless(move |_| {
            sender.input(DirectoryMessage::RestoreSelectionFromTrash)
        }),
    );

    let actions = group.into_action_group();
    list_item_view.insert_action_group(
        <DirectoryListRightClickActionGroup as ActionGroupName>::NAME,
        Some(&actions),
    );
}

fn register_directory_context_actions(
    directory_list_view: &gtk::Widget,
    sender: FactorySender<Directory>,
) {
    let mut group = RelmActionGroup::<DirectoryListRightClickActionGroup>::new();

    group.add_action(RelmAction::<NewFolderAction>::new_stateless(move |_| {
        sender.input(DirectoryMessage::ShowNewFolderDialog)
    }));

    directory_list_view.insert_action_group(
        <DirectoryListRightClickActionGroup as ActionGroupName>::NAME,
        Some(&group.into_action_group()),
    );
}

/// Builds a new drop target that copies files to the given directory.
///
/// The drop target accepts [`gio::File`]s and rejects files that are already in the same
/// directory.
fn new_drop_target_for_dir(dir: gio::File, sender: FactorySender<Directory>) -> gtk::DropTarget {
    let drop_target = gtk::DropTarget::builder()
        .actions(gdk::DragAction::MOVE)
        .preload(true)
        .build();

    drop_target.set_types(&[gio::File::static_type()]);

    drop_target.connect_value_notify(clone!(
        #[strong]
        dir,
        move |this| {
            if let Some(value) = this.value() {
                let file = value.get::<gio::File>().unwrap();

                info!("attempting to drop file {}", file.uri());

                if file.parent().as_ref() == Some(&dir) {
                    info!("rejecting drop; file is already in directory");
                    this.reject();
                }
            }
        }
    ));

    drop_target.connect_drop(clone!(
        #[strong]
        dir,
        move |_, value, _, _| {
            ops::handle_drop(value, &dir, sender.output_sender().clone());

            true
        }
    ));

    drop_target
}

/// Chooses a panel's stack page: the spinner while the listing loads, then
/// either the listing or the thin sliver strip.
fn apply_page(stack: &gtk::Stack, loading: bool, sliver: bool) {
    stack.set_visible_child_name(if loading {
        "spinner"
    } else if sliver {
        "sliver"
    } else {
        "listing"
    });
}

/// Walks the list model chain (multi selection → sorter → hidden-files filter)
/// down to the underlying [`gtk::DirectoryList`]. Must mirror the chain built
/// in [`Directory`]'s `init_model`.
fn directory_list_of(selection: &gtk::MultiSelection) -> gtk::DirectoryList {
    selection
        .model()
        .and_downcast::<gtk::SortListModel>()
        .unwrap()
        .model()
        .and_downcast::<gtk::FilterListModel>()
        .unwrap()
        .model()
        .and_downcast()
        .unwrap()
}

/// Construct a new [`Selection`] from the given list model: the GTK-selected
/// rows (cursor bar or mouse selection) plus every marked entry, with the
/// cursor file leading the list.
fn build_selection(
    selection: &gtk::MultiSelection,
    cursor: Option<u32>,
    marks: &std::rc::Rc<RefCell<std::collections::HashSet<String>>>,
) -> Selection {
    let marks = marks.borrow();

    let cursor_file = cursor
        .filter(|&pos| selection.is_selected(pos))
        .and_then(|pos| selection.item(pos))
        .map(|item| item.downcast::<gio::FileInfo>().unwrap().file().unwrap());

    let mut files: Vec<gio::File> = Vec::new();
    if let Some(cursor_file) = &cursor_file {
        files.push(cursor_file.clone());
    }

    for pos in 0..selection.n_items() {
        let Some(file) = selection
            .item(pos)
            .map(|item| item.downcast::<gio::FileInfo>().unwrap().file().unwrap())
        else {
            continue;
        };

        if files.iter().any(|f| f.equal(&file)) {
            continue;
        }

        if selection.is_selected(pos) || marks.contains(file.uri().as_str()) {
            files.push(file);
        }
    }

    if files.is_empty() {
        Selection::None
    } else {
        let directory_list = directory_list_of(selection);
        let dir = directory_list.file().unwrap();

        Selection::Files(FileSelection {
            parent: dir,
            files,
            cursor_file,
        })
    }
}

/// Notifies the main component of the path of a new selection.
fn send_new_selection(
    selection: &gtk::MultiSelection,
    sender: &FactorySender<Directory>,
    cursor: Option<u32>,
    marks: &std::rc::Rc<RefCell<std::collections::HashSet<String>>>,
) {
    sender
        .output(AppMsg::NewSelection(build_selection(
            selection, cursor, marks,
        )))
        .unwrap();
}

/// Constructs a new menu model for a directory entry's right-click context menu.
fn populate_entry_menu_model(file_info: &gio::FileInfo) -> gio::Menu {
    let file = file_info.file().unwrap();
    let uri = file.uri().to_string();

    let menu_model = gio::Menu::new();

    let open_section = gio::Menu::new();

    menu_model.append_section(None, &open_section);

    if let Some(app_info) =
        gio::AppInfo::default_for_type(&file_info.content_type().unwrap(), false)
    {
        let menu_item = RelmAction::<OpenDefaultAction>::to_menu_item_with_target_value(
            &format!("Open with {}", app_info.display_name()),
            &uri,
        );

        if let Some(icon) = &app_info.icon() {
            menu_item.set_icon(icon);
        }

        open_section.append_item(&menu_item);
    }

    open_section.append_item(
        &RelmAction::<OpenChooserAction>::to_menu_item_with_target_value("Open with...", &uri),
    );

    let modify_section = gio::Menu::new();

    menu_model.append_section(None, &modify_section);

    modify_section.append_item(&RelmAction::<RenameAction>::to_menu_item_with_target_value(
        "Rename...",
        &uri,
    ));

    if !file.has_uri_scheme("trash") {
        modify_section.append_item(&RelmAction::<TrashSelectionAction>::to_menu_item(
            "Move to Trash",
        ));
    } else {
        modify_section.append_item(
            &RelmAction::<RestoreSelectionFromTrashAction>::to_menu_item("Restore from Trash"),
        );
    }

    menu_model.freeze();

    menu_model
}

/// Constructs a new menu model for a directory's right-click context menu.
fn populate_directory_menu_model() -> gio::Menu {
    let model = gio::Menu::new();

    let open_section = gio::Menu::new();

    model.append_section(None, &open_section);

    open_section.append_item(&RelmAction::<NewFolderAction>::to_menu_item(
        "New Folder...",
    ));

    model.freeze();
    model
}

/// Opens the default application for the given file.
fn open_application_for_file(file: &gio::File, sender: &FactorySender<Directory>) {
    info!("opening {} in external application", file.uri());

    if let Err(e) =
        gio::AppInfo::launch_default_for_uri(file.uri().as_str(), None::<&gio::AppLaunchContext>)
    {
        sender.output(AppMsg::Error(Box::new(e))).unwrap();
    }
}

thread_local! {
    /// Weak handles to the hidden-file filter of every live directory panel,
    /// poked by [`refresh_hidden_filters`] when the setting toggles.
    static HIDDEN_FILTERS: RefCell<Vec<glib::WeakRef<gtk::CustomFilter>>> =
        const { RefCell::new(Vec::new()) };

    /// Weak handles to the sorter of every live directory panel, poked by
    /// [`refresh_sorters`] when the sort settings change.
    static SORTERS: RefCell<Vec<glib::WeakRef<gtk::CustomSorter>>> =
        const { RefCell::new(Vec::new()) };

    /// The search term whose matches are highlighted in entry labels.
    static SEARCH_TERM: RefCell<String> = const { RefCell::new(String::new()) };
}

/// Sets the search term used to highlight matches in entry labels.
fn set_search_term(term: &str) {
    SEARCH_TERM.with(|t| term.clone_into(&mut t.borrow_mut()));
}

/// Forces the visible rows of a list view to re-bind, refreshing their labels'
/// search highlights.
fn refresh_highlights(list_view: &gtk::ListView) {
    let factory = list_view.factory();
    list_view.set_factory(None::<&gtk::ListItemFactory>);
    list_view.set_factory(factory.as_ref());
}

/// Builds the pango attributes highlighting occurrences of the active search
/// term in the given entry name.
fn search_highlight_attrs(name: &str) -> pango::AttrList {
    let attrs = pango::AttrList::new();

    SEARCH_TERM.with(|term| {
        let term = term.borrow().to_ascii_lowercase();
        if term.is_empty() {
            return;
        }

        // ASCII lowercasing never changes byte offsets, so match indices are
        // valid in the original string.
        for (start, matched) in name.to_ascii_lowercase().match_indices(term.as_str()) {
            let (start, end) = (start as u32, (start + matched.len()) as u32);

            let mut background = pango::AttrColor::new_background(0, 0xd5d5, 0xd5d5);
            background.set_start_index(start);
            background.set_end_index(end);
            attrs.insert(background);

            let mut foreground = pango::AttrColor::new_foreground(0, 0, 0);
            foreground.set_start_index(start);
            foreground.set_end_index(end);
            attrs.insert(foreground);

            let mut weight = pango::AttrInt::new_weight(pango::Weight::Bold);
            weight.set_start_index(start);
            weight.set_end_index(end);
            attrs.insert(weight);
        }
    });

    attrs
}

/// Re-sorts every live directory panel. Call after the sort settings change.
pub fn refresh_sorters() {
    SORTERS.with(|sorters| {
        sorters.borrow_mut().retain(|weak| match weak.upgrade() {
            Some(sorter) => {
                sorter.changed(gtk::SorterChange::Different);
                true
            }
            None => false,
        });
    });
}

/// Constructs a filter that hides hidden files unless [`config::show_hidden`] is set.
fn hidden_filter() -> gtk::CustomFilter {
    let filter = gtk::CustomFilter::new(|obj| {
        config::show_hidden()
            || !obj
                .downcast_ref::<gio::FileInfo>()
                .map(|info| info.is_hidden())
                .unwrap_or(false)
    });

    HIDDEN_FILTERS.with(|filters| filters.borrow_mut().push(filter.downgrade()));

    filter
}

/// Re-evaluates the hidden-file filter of every live directory panel. Call after
/// [`config::set_show_hidden`] changes the setting.
pub fn refresh_hidden_filters() {
    let change = if config::show_hidden() {
        gtk::FilterChange::LessStrict
    } else {
        gtk::FilterChange::MoreStrict
    };

    HIDDEN_FILTERS.with(|filters| {
        filters.borrow_mut().retain(|weak| match weak.upgrade() {
            Some(filter) => {
                filter.changed(change);
                true
            }
            None => false,
        });
    });
}

/// Constructs a new sorter used to sort directory entries. The sort key and
/// direction follow the global sort settings (see [`config`]).
fn file_sorter() -> gtk::Sorter {
    let sorter = gtk::CustomSorter::new(move |a, b| {
        let a = a.downcast_ref::<gio::FileInfo>().unwrap();
        let b = b.downcast_ref::<gio::FileInfo>().unwrap();

        let name = |info: &gio::FileInfo| info.display_name().to_lowercase();

        let ordering = match config::sort_key() {
            config::SortKey::Name => name(a).cmp(&name(b)),
            config::SortKey::Modified => {
                let modified = |info: &gio::FileInfo| {
                    info.modification_date_time().map_or(0, |d| d.to_unix())
                };

                modified(a).cmp(&modified(b))
            }
            config::SortKey::Type => {
                // Directories first, then grouped by content type, name as the
                // tie-breaker.
                let file_rank =
                    |info: &gio::FileInfo| u8::from(info.file_type() != gio::FileType::Directory);
                let content_type = |info: &gio::FileInfo| info.content_type().unwrap_or_default();

                file_rank(a)
                    .cmp(&file_rank(b))
                    .then_with(|| content_type(a).cmp(&content_type(b)))
                    .then_with(|| name(a).cmp(&name(b)))
            }
        };

        let ordering = if config::sort_reversed() {
            ordering.reverse()
        } else {
            ordering
        };

        ordering.into()
    });

    SORTERS.with(|sorters| sorters.borrow_mut().push(sorter.downgrade()));

    sorter.upcast()
}

/// Returns a formattable object for a list of [`gio::FileInfo`] objects. Used to log the return
/// value of [`Directory::selected_file_info`].
fn fmt_file_info(info: &[gio::FileInfo]) -> impl Debug + '_ {
    struct Formatter<'a>(&'a [gio::FileInfo]);

    impl Debug for Formatter<'_> {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            let files = self.0.iter().map(|i| i.file().unwrap()).collect::<Vec<_>>();
            fmt_files_as_uris(&files, f)
        }
    }

    Formatter(info)
}

/// Reads a file list off the clipboard.
///
/// The GNOME type is preferred because it carries the operation word. A bare
/// `text/uri-list` — what a browser or a file picker offers — has no such word,
/// so it is read as a copy: assuming a cut would delete another app's files.
async fn read_file_clipboard(
    clipboard: &gdk::Clipboard,
) -> Option<(crate::clipboard::ClipboardOp, Vec<String>)> {
    if let Ok((stream, _)) = clipboard
        .read_future(&[crate::clipboard::GNOME_MIME], glib::Priority::DEFAULT)
        .await
    {
        if let Some(payload) = read_stream_to_string(stream).await {
            if let Some(parsed) = crate::clipboard::decode(&payload) {
                return Some(parsed);
            }
        }
    }

    let (stream, _) = clipboard
        .read_future(&[crate::clipboard::URI_LIST_MIME], glib::Priority::DEFAULT)
        .await
        .ok()?;
    let payload = read_stream_to_string(stream).await?;
    let uris = crate::clipboard::decode_uri_list(&payload);

    (!uris.is_empty()).then_some((crate::clipboard::ClipboardOp::Copy, uris))
}

/// Drains a clipboard stream into a string, discarding anything that is not
/// valid UTF-8 — a clipboard payload that is not text is not a file list.
async fn read_stream_to_string(stream: gio::InputStream) -> Option<String> {
    let mut collected: Vec<u8> = Vec::new();

    loop {
        let (buffer, read) = stream
            .read_future(vec![0u8; 4096], glib::Priority::DEFAULT)
            .await
            .ok()?;

        if read == 0 {
            break;
        }

        collected.extend_from_slice(&buffer[..read]);
    }

    String::from_utf8(collected).ok()
}

/// Picks the rows an operation applies to.
///
/// Marks win outright. Once entries are explicitly marked, the cursor is only
/// where you happen to be standing — folding it into the batch means a Delete
/// takes a file you never marked. Only with nothing marked does an operation
/// fall back to the row under the cursor.
///
/// Same rule as ranger, which this fork follows: `Directory.get_selection`
/// returns the marked items if there are any, and the pointed-at object
/// otherwise (`ranger/container/directory.py:248`).
fn operation_rows<T>(marked: Vec<T>, under_cursor: Vec<T>) -> Vec<T> {
    if marked.is_empty() {
        under_cursor
    } else {
        marked
    }
}

#[cfg(test)]
mod tests {
    use super::operation_rows;

    #[test]
    fn marked_entries_win_over_the_cursor() {
        // The reported bug: with entries marked, Delete also trashed the row
        // the cursor happened to be resting on.
        let rows = operation_rows(vec!["a.txt", "b.txt"], vec!["under-cursor.txt"]);
        assert_eq!(rows, vec!["a.txt", "b.txt"]);
    }

    #[test]
    fn the_cursor_acts_alone_when_nothing_is_marked() {
        let rows = operation_rows(Vec::new(), vec!["under-cursor.txt"]);
        assert_eq!(rows, vec!["under-cursor.txt"]);
    }

    #[test]
    fn an_empty_listing_has_nothing_to_operate_on() {
        assert!(operation_rows::<&str>(Vec::new(), Vec::new()).is_empty());
    }
}
