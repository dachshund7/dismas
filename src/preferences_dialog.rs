// SPDX-FileCopyrightText: 2026  John Hamilton
// SPDX-License-Identifier: GPL-3.0-or-later

use adw::{prelude::*, subclass::prelude::*};
use gtk::{CompositeTemplate, gdk, glib};

use crate::utils;

mod imp {
    use super::*;

    #[derive(Debug, Default, CompositeTemplate)]
    #[template(resource = "/io/github/dachshund7/Dismas/preferences-dialog.ui")]
    pub struct PreferencesDialog {
        #[template_child]
        pub background_play_row: TemplateChild<adw::SwitchRow>,
        #[template_child]
        pub restore_playlist_row: TemplateChild<adw::SwitchRow>,
        #[template_child]
        pub recoloring_row: TemplateChild<adw::SwitchRow>,
        #[template_child]
        pub waveform_row: TemplateChild<adw::SwitchRow>,
        #[template_child]
        pub header_bars_row: TemplateChild<adw::SwitchRow>,
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
            let settings = utils::settings_manager();
            for (key, row) in [
                ("background-play", &self.background_play_row),
                ("restore-playlist", &self.restore_playlist_row),
                ("enable-recoloring", &self.recoloring_row),
                ("show-waveform", &self.waveform_row),
                ("move-from-header-bars", &self.header_bars_row),
            ] {
                settings.bind(key, &**row, "active").build();
            }
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
