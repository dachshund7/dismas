// SPDX-FileCopyrightText: 2026  John Hamilton
// SPDX-License-Identifier: GPL-3.0-or-later

use std::{
    cell::{Cell, OnceCell, RefCell},
    collections::HashSet,
    path::{Path, PathBuf},
};

use adw::{prelude::*, subclass::prelude::*};
use glib::clone;
use gtk::{CompositeTemplate, gdk, gio, glib};

use crate::{
    application::Application,
    i18n::{i18n, i18n_k, ni18n_k},
    library::{self, MusicFolder},
    utils,
};

mod imp {
    use super::*;

    #[derive(Debug, Default, CompositeTemplate)]
    #[template(resource = "/io/github/dachshund7/Dismas/preferences-dialog.ui")]
    pub struct PreferencesDialog {
        #[template_child]
        pub music_folders_group: TemplateChild<adw::PreferencesGroup>,
        #[template_child]
        pub add_folder_row: TemplateChild<adw::ButtonRow>,
        #[template_child]
        pub background_play_row: TemplateChild<adw::SwitchRow>,
        #[template_child]
        pub restore_playlist_row: TemplateChild<adw::SwitchRow>,
        #[template_child]
        pub recoloring_row: TemplateChild<adw::SwitchRow>,
        #[template_child]
        pub waveform_row: TemplateChild<adw::SwitchRow>,
        #[template_child]
        pub zen_mode_row: TemplateChild<adw::SwitchRow>,
        #[template_child]
        pub header_bars_row: TemplateChild<adw::SwitchRow>,

        pub settings: OnceCell<gio::Settings>,
        // The music folders, as `MusicFolder`s; each change is stored right
        // away, and the window reads them again once the dialog is closed
        pub folders: OnceCell<gio::ListStore>,
        // A folder in the list was added again, e.g. one whose access was
        // taken back and given again, so they're read again all the same
        pub rescan: Cell<bool>,
        // The paths of the folders that are missing or empty, which their
        // rows say
        pub unavailable: RefCell<HashSet<PathBuf>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for PreferencesDialog {
        const NAME: &'static str = "DismasPreferencesDialog";
        type Type = super::PreferencesDialog;
        type ParentType = adw::PreferencesDialog;

        fn class_init(klass: &mut Self::Class) {
            klass.bind_template();

            // Ctrl+comma opens the dialog, and closes it again: the
            // window's shortcuts don't run while a dialog is open
            klass.add_binding(gdk::Key::comma, gdk::ModifierType::CONTROL_MASK, |dialog| {
                dialog.close();
                glib::Propagation::Stop
            });
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for PreferencesDialog {
        fn constructed(&self) {
            self.parent_constructed();

            // What depends on the settings watches them, or reads them when
            // it needs them, so the switches only write them
            let obj = self.obj();
            let settings = obj.settings();
            for (key, row) in [
                ("background-play", &self.background_play_row),
                ("restore-playlist", &self.restore_playlist_row),
                ("enable-recoloring", &self.recoloring_row),
                ("show-waveform", &self.waveform_row),
                ("zen-mode", &self.zen_mode_row),
                ("move-from-header-bars", &self.header_bars_row),
            ] {
                settings.bind(key, &**row, "active").build();
            }

            obj.setup_folders();
        }
    }

    impl WidgetImpl for PreferencesDialog {}
    impl AdwDialogImpl for PreferencesDialog {}
    impl PreferencesDialogImpl for PreferencesDialog {}
}

glib::wrapper! {
    pub struct PreferencesDialog(ObjectSubclass<imp::PreferencesDialog>)
        @extends gtk::Widget, adw::Dialog, adw::PreferencesDialog,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl Default for PreferencesDialog {
    fn default() -> Self {
        glib::Object::new()
    }
}

impl PreferencesDialog {
    // Whether the music folders should be read again even if they're the
    // same
    pub fn rescan_requested(&self) -> bool {
        self.imp().rescan.get()
    }

    fn settings(&self) -> &gio::Settings {
        self.imp().settings.get_or_init(utils::settings_manager)
    }

    fn folders(&self) -> &gio::ListStore {
        self.imp()
            .folders
            .get()
            .expect("The folders are set up at construction")
    }

    fn folder_list(&self) -> Vec<MusicFolder> {
        self.folders()
            .iter::<glib::BoxedAnyObject>()
            .flatten()
            .map(|item| item.borrow::<MusicFolder>().clone())
            .collect()
    }

    fn store_folders(&self) {
        library::store_music_folders(self.settings(), &self.folder_list());
    }

    fn setup_folders(&self) {
        let imp = self.imp();

        // Rows come and go one at a time, so the others keep the focus
        let store = gio::ListStore::new::<glib::BoxedAnyObject>();
        for folder in library::music_folders(self.settings()) {
            store.append(&glib::BoxedAnyObject::new(folder));
        }
        let _ = imp.folders.set(store.clone());
        imp.music_folders_group.bind_model(
            Some(&store),
            clone!(
                #[weak(rename_to = dialog)]
                self,
                #[upgrade_or_else]
                || gtk::ListBoxRow::new().upcast(),
                move |item| dialog.folder_row(item)
            ),
        );

        imp.add_folder_row.connect_activated(clone!(
            #[weak(rename_to = dialog)]
            self,
            move |_| {
                glib::spawn_future_local(clone!(
                    #[weak]
                    dialog,
                    async move { dialog.add_folder().await }
                ));
            }
        ));
    }

    fn folder_row(&self, item: &glib::Object) -> gtk::Widget {
        let item = item
            .downcast_ref::<glib::BoxedAnyObject>()
            .expect("The folders are boxed");
        let folder = item.borrow::<MusicFolder>().clone();
        // The folders it leaves out are listed when it's expanded, so they
        // don't make the page longer
        let row = adw::ExpanderRow::builder()
            .title(folder_name(folder.shown_path()))
            .use_markup(false)
            .build();

        let remove = gtk::Button::builder()
            .icon_name("list-remove-symbolic")
            .tooltip_text(i18n("Remove Folder"))
            .valign(gtk::Align::Center)
            .css_classes(["flat"])
            .build();
        // The last folder stays
        self.folders()
            .bind_property("n-items", &remove, "sensitive")
            .transform_to(|_, n_items: u32| Some(n_items > 1))
            .sync_create()
            .build();
        remove.connect_clicked(clone!(
            #[weak(rename_to = dialog)]
            self,
            #[weak]
            item,
            move |_| dialog.remove_folder(item.upcast_ref())
        ));
        row.add_suffix(&remove);

        let leave_out = adw::ButtonRow::builder()
            .title(i18n("_Leave Out a Folder…"))
            .start_icon_name("list-add-symbolic")
            .use_underline(true)
            .build();
        leave_out.connect_activated(clone!(
            #[weak(rename_to = dialog)]
            self,
            #[weak]
            item,
            #[weak]
            row,
            move |leave_out| {
                glib::spawn_future_local(clone!(
                    #[weak]
                    dialog,
                    #[weak]
                    item,
                    #[weak]
                    row,
                    #[weak]
                    leave_out,
                    async move { dialog.exclude_folder(&item, &row, &leave_out).await }
                ));
            }
        ));
        self.fill_exclusions(item, &row, &leave_out);
        self.update_folder_subtitle(item, &row);

        // Finding out whether it's there can take a moment, e.g. on a
        // network drive. Its albums are kept, but hidden, until it's back
        glib::spawn_future_local(clone!(
            #[weak(rename_to = dialog)]
            self,
            #[weak]
            item,
            #[weak]
            row,
            async move {
                let path = folder.path.clone();
                let available = gio::spawn_blocking(move || library::is_available(&path))
                    .await
                    .unwrap_or(true);
                let unavailable = &dialog.imp().unavailable;
                if available {
                    unavailable.borrow_mut().remove(&folder.path);
                } else {
                    unavailable.borrow_mut().insert(folder.path.clone());
                    row.add_prefix(
                        &gtk::Image::builder()
                            .icon_name("dialog-warning-symbolic")
                            .css_classes(["warning"])
                            .build(),
                    );
                }
                dialog.update_folder_subtitle(&item, &row);
            }
        ));

        row.upcast()
    }

    // The path of a folder's row, whether it's there, and how many folders
    // it leaves out
    fn update_folder_subtitle(&self, item: &glib::BoxedAnyObject, row: &adw::ExpanderRow) {
        let folder = item.borrow::<MusicFolder>();
        let mut lines = vec![home_path(folder.shown_path())];
        if self.imp().unavailable.borrow().contains(&folder.path) {
            lines.push(i18n("Not found or empty; its albums are hidden"));
        }
        if !folder.excluded.is_empty() {
            let n = folder.excluded.len() as u32;
            lines.push(ni18n_k(
                // Translators: the `{n}` must be left unmodified
                "{n} folder left out",
                "{n} folders left out",
                n,
                &[("n", &n.to_string())],
            ));
        }
        row.set_subtitle(&lines.join("\n"));
    }

    // Lists the folders a folder leaves out, in its row, in their order,
    // with the button that leaves out another one last
    fn fill_exclusions(
        &self,
        item: &glib::BoxedAnyObject,
        row: &adw::ExpanderRow,
        leave_out: &adw::ButtonRow,
    ) {
        let mut child = leave_out.parent().and_then(|list| list.first_child());
        while let Some(widget) = child {
            child = widget.next_sibling();
            row.remove(&widget);
        }
        let excluded = item.borrow::<MusicFolder>().excluded.clone();
        for relative in excluded {
            row.add_row(&self.exclusion_row(item, row, relative));
        }
        row.add_row(leave_out);
    }

    fn exclusion_row(
        &self,
        item: &glib::BoxedAnyObject,
        row: &adw::ExpanderRow,
        relative: PathBuf,
    ) -> adw::ActionRow {
        let exclusion = adw::ActionRow::builder()
            .title(relative.to_string_lossy())
            .use_markup(false)
            .build();
        let remove = gtk::Button::builder()
            .icon_name("list-remove-symbolic")
            .tooltip_text(i18n("Include Again"))
            .valign(gtk::Align::Center)
            .css_classes(["flat"])
            .build();
        remove.connect_clicked(clone!(
            #[weak(rename_to = dialog)]
            self,
            #[weak]
            item,
            #[weak]
            row,
            #[weak]
            exclusion,
            move |_| dialog.remove_exclusion(&item, &row, &exclusion, &relative)
        ));
        exclusion.add_suffix(&remove);
        exclusion
    }

    fn toast(&self, title: &str) {
        self.add_toast(plain_toast(title));
    }

    async fn add_folder(&self) {
        let file_dialog = gtk::FileDialog::builder()
            .title(i18n("Add Music Folder"))
            .modal(true)
            .initial_folder(&gio::File::for_path(glib::home_dir()))
            .build();
        let parent = self.root().and_downcast::<gtk::Window>();
        let Ok(file) = file_dialog.select_folder_future(parent.as_ref()).await else {
            return;
        };
        // E.g. a network location without a local path
        let Some(path) = file.path() else {
            self.toast(&i18n("Only local folders can be added"));
            return;
        };

        // This blocks, as the files of the document portal can be slow
        let folders = self.folder_list();
        let Ok((folder, key, keys)) = gio::spawn_blocking(move || {
            let folder = library::resolve_folder(&path);
            let key = library::folder_key(&folder);
            let keys: Vec<PathBuf> = folders.iter().map(library::folder_key).collect();
            (folder, key, keys)
        })
        .await
        else {
            return;
        };

        let store = self.folders();
        if keys.len() != store.n_items() as usize {
            return;
        }

        // The same folder again: through the portal, it has a new path if
        // it was made again, and it's back if its access was taken back and
        // given again. Either way, its row and its albums are brought up to
        // date
        let mut folder = folder;
        if let Some(pos) = keys.iter().position(|other| *other == key) {
            let old = store
                .item(pos as u32)
                .and_downcast::<glib::BoxedAnyObject>();
            // It leaves out the same folders as before
            if let Some(old) = &old {
                folder.excluded = old.borrow::<MusicFolder>().excluded.clone();
            }
            let same = old.is_some_and(|old| *old.borrow::<MusicFolder>() == folder);
            store.splice(pos as u32, 1, &[glib::BoxedAnyObject::new(folder)]);
            self.store_folders();
            self.imp().rescan.set(true);
            if same {
                self.toast(&i18n("The folder is already included"));
            }
            return;
        }
        if keys.iter().any(|other| key.starts_with(other)) {
            self.toast(&i18n("The folder is already included"));
            return;
        }

        // The folders in it go, as it has their albums, and it leaves out
        // the folders they left out
        for pos in (0..keys.len()).rev() {
            let Ok(inner) = keys[pos].strip_prefix(&key) else {
                continue;
            };
            if let Some(item) = store
                .item(pos as u32)
                .and_downcast::<glib::BoxedAnyObject>()
            {
                let excluded = &item.borrow::<MusicFolder>().excluded;
                folder
                    .excluded
                    .splice(0..0, excluded.iter().map(|other| inner.join(other)));
            }
            store.remove(pos as u32);
        }
        store.append(&glib::BoxedAnyObject::new(folder));
        self.store_folders();
    }

    async fn exclude_folder(
        &self,
        item: &glib::BoxedAnyObject,
        row: &adw::ExpanderRow,
        leave_out: &adw::ButtonRow,
    ) {
        let folder = item.borrow::<MusicFolder>().clone();
        let file_dialog = gtk::FileDialog::builder()
            .title(i18n("Leave Out a Folder"))
            .modal(true)
            .initial_folder(&gio::File::for_path(&folder.path))
            .build();
        let parent = self.root().and_downcast::<gtk::Window>();
        let Ok(file) = file_dialog.select_folder_future(parent.as_ref()).await else {
            return;
        };

        // This blocks, as the files of the document portal can be slow
        let name = folder_name(folder.shown_path());
        let relative = match file.path() {
            Some(path) => gio::spawn_blocking(move || {
                library::relative_to(&library::resolve_folder(&path), &folder)
            })
            .await
            .ok()
            .flatten(),
            None => None,
        };
        let Some(relative) = relative else {
            self.toast(&i18n_k(
                // Translators: the `{folder}` must be left unmodified; it's
                // the name of a music folder
                "Only folders in “{folder}” can be left out",
                &[("folder", &name)],
            ));
            return;
        };
        // It might have been removed in the meantime
        if self.folders().find(item).is_none() {
            return;
        }

        {
            let mut folder = item.borrow_mut::<MusicFolder>();
            if folder
                .excluded
                .iter()
                .any(|other| relative.starts_with(other))
            {
                drop(folder);
                self.toast(&i18n("The folder is already left out"));
                return;
            }
            // The folders in it go, as it has them
            folder
                .excluded
                .retain(|other| !other.starts_with(&relative));
            folder.excluded.push(relative);
        }
        self.fill_exclusions(item, row, leave_out);
        self.update_folder_subtitle(item, row);
        leave_out.grab_focus();
        self.store_folders();
    }

    fn remove_exclusion(
        &self,
        item: &glib::BoxedAnyObject,
        row: &adw::ExpanderRow,
        exclusion: &adw::ActionRow,
        relative: &Path,
    ) {
        item.borrow_mut::<MusicFolder>()
            .excluded
            .retain(|other| other != relative);
        // Its row is gone with the focus, which goes to the row after it,
        // the button that leaves out another folder if it was the last
        let next = exclusion.next_sibling();
        row.remove(exclusion);
        if let Some(next) = next {
            next.grab_focus();
        }
        self.update_folder_subtitle(item, row);
        self.store_folders();
    }

    // The name of the selected playlist, empty for the queue, if it has
    // tracks of a folder
    fn selected_playlist_with_tracks_in(&self, folder: &Path) -> Option<String> {
        let app = gio::Application::default().and_downcast::<Application>()?;
        let has_tracks = app
            .player()
            .queue()
            .files()
            .iter()
            .filter_map(gio::prelude::FileExt::path)
            .any(|path| path.starts_with(folder));
        has_tracks.then(|| self.settings().string("selected-playlist").to_string())
    }

    fn remove_folder(&self, item: &glib::Object) {
        let imp = self.imp();
        let store = self.folders();
        let Some(pos) = store.find(item) else {
            return;
        };
        let (name, path) = item
            .downcast_ref::<glib::BoxedAnyObject>()
            .map(|item| {
                let folder = item.borrow::<MusicFolder>();
                (folder_name(folder.shown_path()), folder.path.clone())
            })
            .unwrap_or_default();
        store.remove(pos);
        self.store_folders();

        // Its row is gone with the focus, which goes to the row that took
        // its place
        let next = pos.min(store.n_items().saturating_sub(1));
        match imp.music_folders_group.row(next) {
            Some(row) => row.grab_focus(),
            None => imp.add_folder_row.grab_focus(),
        };

        // The folders only make up the album grid: the queue and the
        // playlists are lists of files, which play as long as they're there,
        // so the toast says so when the selected one has tracks of the
        // folder. Reading it again, once it's left the library, would take a
        // while, so there's an Undo
        let message = match self.selected_playlist_with_tracks_in(&path) {
            None => i18n_k(
                // Translators: the `{folder}` must be left unmodified; it's
                // the name of a music folder
                "Removed “{folder}”",
                &[("folder", &name)],
            ),
            Some(playlist) if playlist.is_empty() => i18n_k(
                // Translators: the `{folder}` must be left unmodified; it's
                // the name of a music folder
                "Removed “{folder}” from the album grid; the queue keeps its tracks",
                &[("folder", &name)],
            ),
            Some(playlist) => i18n_k(
                // Translators: the `{folder}` and `{playlist}` must be left
                // unmodified; they're the names of a music folder and of a
                // playlist
                "Removed “{folder}” from the album grid; “{playlist}” keeps its tracks",
                &[
                    ("folder", &name),
                    ("playlist", &utils::short_playlist_name(&playlist)),
                ],
            ),
        };
        let toast = plain_toast(&message);
        toast.set_button_label(Some(&i18n("Undo")));
        toast.connect_button_clicked(clone!(
            #[weak(rename_to = dialog)]
            self,
            #[strong]
            item,
            move |_| {
                let store = dialog.folders();
                if store.find(&item).is_none() {
                    store.insert(pos.min(store.n_items()), &item);
                    dialog.store_folders();
                }
            }
        ));
        self.add_toast(toast);
    }
}

// A toast whose title is plain text: the titles of toasts are markup by
// default, and the names of folders can have "&"
fn plain_toast(title: &str) -> adw::Toast {
    adw::Toast::builder().title(title).use_markup(false).build()
}

// The name of a folder, to show
fn folder_name(path: &Path) -> String {
    path.file_name()
        .map_or_else(|| path.to_string_lossy(), |name| name.to_string_lossy())
        .into_owned()
}

// A path to show, with the home folder as "~"
fn home_path(path: &Path) -> String {
    match path.strip_prefix(glib::home_dir()) {
        Ok(rest) if rest.as_os_str().is_empty() => "~".to_string(),
        Ok(rest) => format!("~/{}", rest.display()),
        Err(_) => path.display().to_string(),
    }
}
